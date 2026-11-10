import assert from "node:assert/strict";
import { type ChildProcess, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, startRustWebServer, testRoot } from "./support.ts";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../..");
type PollExportJob = { status: string; files: { name: string; bytes: number; url: string }[]; error?: unknown };

test(
	"Rust cst-server --standalone-web 全链路浏览器 E2E：首页乐观发送、工具流式回答、分支树、33 会话聚合流、BitLocker 复查卡片与 PNG/HTML 导出",
	{ timeout: 120_000 },
	async () => {
		const root = await mkdtemp(join(await testRoot(), "rust-web-e2e-"));
		const kitRoot = join(root, "kit");
		const workspace = join(root, "workspace");
		await mkdir(workspace, { recursive: true });
		await writeFile(join(workspace, "hello-rust.txt"), "RUST_SERVER_TOOL_OK");

		const llmPort = await freePort();
		const llmServer = createServer(async (req, res) => {
			let raw = "";
			for await (const chunk of req) raw += chunk;
			const body = JSON.parse(raw || "{}") as {
				messages?: { role: string; content: unknown }[];
				tools?: unknown[];
			};
			const messages = body.messages ?? [];
			const lastRole = messages.at(-1)?.role;
			const lastContent = JSON.stringify(messages.at(-1)?.content ?? "");
			const hasTools = Array.isArray(body.tools) && body.tools.length > 0;
			const shouldCallTool = hasTools && lastRole === "user" && lastContent.includes("请读取 hello-rust.txt");

			res.writeHead(200, {
				"Content-Type": "text/event-stream",
				"Cache-Control": "no-cache",
				Connection: "keep-alive",
			});

			if (shouldCallTool) {
				const chunk1 = {
					id: "chat-1",
					choices: [
						{
							index: 0,
							delta: {
								role: "assistant",
								tool_calls: [
									{
										index: 0,
										id: "call-read-1",
										type: "function",
										function: {
											name: "read",
											arguments: JSON.stringify({ path: "hello-rust.txt", offset: 1, limit: 20 }),
										},
									},
								],
							},
							finish_reason: null,
						},
					],
				};
				const chunk2 = {
					id: "chat-1",
					choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }],
				};
				res.end(`data: ${JSON.stringify(chunk1)}\n\ndata: ${JSON.stringify(chunk2)}\n\ndata: [DONE]\n\n`);
				return;
			}

			const replyChunk1 = {
				id: "chat-2",
				choices: [
					{ index: 0, delta: { role: "assistant", content: "Rust Web 端联调成功：" }, finish_reason: null },
				],
			};
			const replyChunk2 = {
				id: "chat-2",
				choices: [{ index: 0, delta: { content: "已读取工作区文件。" }, finish_reason: "stop" }],
			};
			res.end(`data: ${JSON.stringify(replyChunk1)}\n\ndata: ${JSON.stringify(replyChunk2)}\n\ndata: [DONE]\n\n`);
		});
		await new Promise<void>((resolve) => llmServer.listen(llmPort, "127.0.0.1", resolve));

		const home = join(kitRoot, "agent", "home");
		await mkdir(home, { recursive: true });
		await configureTestModel(home, `http://127.0.0.1:${llmPort}/v1`);
		await writeFile(
			join(home, "models.json"),
			JSON.stringify({
				providers: {
					"test-probe": {
						api: "openai-completions",
						baseUrl: `http://127.0.0.1:${llmPort}/v1`,
						apiKey: "test-only",
						models: [
							{
								id: "mock",
								name: "Mock Model",
								reasoning: false,
								input: ["text", "image"],
								contextWindow: 128000,
								maxTokens: 4096,
							},
						],
					},
				},
			}),
		);

		let server: Awaited<ReturnType<typeof startRustWebServer>> | undefined;
		let browser: BrowserProbe | undefined;
		try {
			server = await startRustWebServer({
				kitRoot,
				cwd: workspace,
				modelBaseUrl: `http://127.0.0.1:${llmPort}/v1`,
			});
			const origin = server.url;

			const postJson = async (path: string, value: unknown, key = randomUUID()) => {
				const resp = await fetch(`${origin}${path}`, {
					method: "POST",
					headers: {
						Origin: origin,
						"X-CST-Web-Request": "1",
						"Content-Type": "application/json",
						"Idempotency-Key": key,
					},
					body: JSON.stringify(value),
				});
				return { status: resp.status, body: (await resp.json().catch(() => null)) as Record<string, unknown> };
			};

			// 1. Check GET /api/state advertises multiplexStream: true
			const stateResp = (await (await fetch(`${origin}/api/state`)).json()) as {
				stage: string;
				channelMode: string;
				multiplexStream: boolean;
			};
			assert.equal(stateResp.stage, "ready");
			assert.equal(stateResp.channelMode, "web_active");
			assert.equal(stateResp.multiplexStream, true);

			// 2. Launch browser, open home page, optimistically send a message, and verify navigation + tool + reply
			browser = await BrowserProbe.launch(root);
			await browser.navigate(`${origin}/`);
			await browser.until(`document.body.innerText.includes("我该如何协助您？")`, "首页问候就绪");

			await browser.fill('[contenteditable="true"]', "请读取 hello-rust.txt");
			await browser.click('button[aria-label="发送消息"]');
			await browser.until(`location.pathname.startsWith("/s/")`, "自动跳转会话页");
			await browser.until(
				`document.body.innerText.includes("Rust Web 端联调成功：已读取工作区文件。")`,
				"助手流式回答与工具调用完成",
				20_000,
			);

			const chatSessionId = await browser.evaluate<string>(`location.pathname.split("/s/")[1].split("/")[0]`);
			assert.ok(chatSessionId);

			// 3. Navigate to /s/:id/tree and verify branch tree renders user & assistant nodes
			await browser.navigate(`${origin}/s/${chatSessionId}/tree`);
			await browser.until(
				`document.body.innerText.includes("请读取 hello-rust.txt") && document.body.innerText.includes("Rust Web 端联调成功")`,
				"分支树渲染用户与助手节点",
			);

			// 4. Verify manual compaction (200 -> 409 already_compacted -> 409 nothing_to_compact) & model scoped sync
			const firstCompact = await postJson(`/api/sessions/${chatSessionId}/compact`, {});
			assert.equal(firstCompact.status, 200, JSON.stringify(firstCompact.body));

			const secondCompact = await postJson(`/api/sessions/${chatSessionId}/compact`, {});
			assert.equal(secondCompact.status, 409, JSON.stringify(secondCompact.body));
			assert.equal(
				(secondCompact.body?.error as { message?: string } | undefined)?.message,
				"会话已压缩，继续对话后可再次压缩",
			);

			const emptyCreated = await postJson("/api/sessions", {});
			assert.equal(emptyCreated.status, 201);
			const emptyCompact = await postJson(`/api/sessions/${String(emptyCreated.body?.id)}/compact`, {});
			assert.equal(emptyCompact.status, 409, JSON.stringify(emptyCompact.body));
			assert.equal(
				(emptyCompact.body?.error as { message?: string } | undefined)?.message,
				"当前没有可压缩的较早内容",
			);

			const scopedRes = await postJson("/api/models/scoped", { patterns: ["test-probe/mock"] });
			assert.equal(scopedRes.status, 200);

			// 5. Verify business 404 session_not_found carries sessionId on multiplex subscription API
			const missingSubResp = await fetch(`${origin}/api/events/stream/e2e-probe-client/subscriptions`, {
				method: "PUT",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": randomUUID(),
				},
				body: JSON.stringify({ sessions: [chatSessionId, "deleted-e2e-session"] }),
			});
			assert.equal(missingSubResp.status, 404);
			const missingSubJson = (await missingSubResp.json()) as {
				error?: { code?: string; sessionId?: string };
			};
			assert.equal(missingSubJson.error?.code, "session_not_found");
			assert.equal(missingSubJson.error?.sessionId, "deleted-e2e-session");

			// 6. Create a session with crash-reconciled `cst.bitlocker-state-verification` and verify both Chat UI & Export (PNG + Offline HTML)
			const blSessionId = "bitlocker-reconcile-e2e";
			const blEntries = [
				JSON.stringify({
					type: "session",
					version: 3,
					id: blSessionId,
					timestamp: "2026-11-10T08:00:00.000Z",
					cwd: workspace,
				}),
				JSON.stringify({
					type: "message",
					id: "u-bl-1",
					parentId: null,
					timestamp: "2026-11-10T08:00:01.000Z",
					message: {
						role: "user",
						content: "解锁 D 盘加密卷",
						timestamp: 1762761601000,
					},
				}),
				JSON.stringify({
					type: "message",
					id: "a-bl-1",
					parentId: "u-bl-1",
					timestamp: "2026-11-10T08:00:02.000Z",
					message: {
						role: "assistant",
						content: [
							{
								type: "toolCall",
								id: "tc-bl-unlock",
								name: "bitlocker",
								arguments: { scope: "unlock", drive: "D:" },
							},
						],
						api: "openai-completions",
						provider: "test-probe",
						model: "mock",
						usage: {
							input: 10,
							output: 10,
							cacheRead: 0,
							cacheWrite: 0,
							totalTokens: 20,
							cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
						},
						stopReason: "toolUse",
						timestamp: 1762761602000,
					},
				}),
				JSON.stringify({
					type: "custom",
					id: "c-bl-verify-1",
					parentId: "a-bl-1",
					timestamp: "2026-11-10T08:00:05.000Z",
					customType: "cst.bitlocker-state-verification",
					data: {
						bitlocker: {
							scope: "status",
							operation: {
								status: "unknown",
								reason: "E2E_OLD_BITLOCKER_VERIFICATION",
							},
							degraded: true,
						},
					},
				}),
			];
			const blSessionPath = join(home, "sessions", `2026-11-10_${blSessionId}.jsonl`);
			await writeFile(blSessionPath, `${blEntries.join("\n")}\n`);
			const blResponse = await fetch(`${origin}/api/sessions/${blSessionId}`);
			assert.equal(blResponse.status, 200);
			await blResponse.arrayBuffer();
			const verifications = (await readFile(blSessionPath, "utf8"))
				.trim()
				.split("\n")
				.map(
					(line) =>
						JSON.parse(line) as {
							customType?: string;
							data?: { bitlocker?: { operation?: { status?: string; reason?: string } } };
						},
				)
				.filter((entry) => entry.customType === "cst.bitlocker-state-verification");
			assert.equal(verifications.length, 2, "打开中断会话应追加一次实时复查");
			const operation = verifications.at(-1)?.data?.bitlocker?.operation;
			assert.equal(operation?.status, "unknown", "复查不会确认或重放中断操作");
			const verificationReason = operation?.reason;
			assert.ok(
				verificationReason === "上次操作异常中断；实时查询失败，请重新查询。" ||
					verificationReason === "上次操作异常中断；已实时复查，未重试操作",
				`应记录本机实时复查结果：${verificationReason}`,
			);

			await browser.navigate(`${origin}/s/${blSessionId}`);
			await browser.until(
				`document.body.innerText.includes("结果未确认") && document.body.innerText.includes(${JSON.stringify(verificationReason)}) && !document.body.innerText.includes("E2E_OLD_BITLOCKER_VERIFICATION")`,
				"聊天页渲染强杀重启后的 BitLocker 未知复查状态",
			);

			// 7. Navigate to /s/:id/export and verify preview iframe + PNG & Offline HTML exports complete within 10s
			await browser.navigate(`${origin}/s/${blSessionId}/export`);
			await browser.until(
				`!!document.querySelector('iframe[title="会话导出预览"][data-ready="true"]')`,
				"导出预览页加载就绪",
			);

			for (const format of ["png", "html"] as const) {
				const startedAt = Date.now();
				const created = await postJson(`/api/sessions/${blSessionId}/exports`, {
					format,
					theme: "light",
					includeThinking: false,
					includeTools: true,
				});
				assert.equal(created.status, 202);
				const exportId = String(created.body?.id);

				let job: PollExportJob | undefined;
				while (Date.now() - startedAt < 15_000) {
					job = (await (await fetch(`${origin}/api/exports/${exportId}`)).json()) as PollExportJob;
					if (job?.status === "completed" || job?.status === "failed") break;
					await new Promise((r) => setTimeout(r, 60));
				}
				const elapsedMs = Date.now() - startedAt;
				assert.equal(job?.status, "completed", `export ${format} failed: ${JSON.stringify(job?.error)}`);
				assert.ok(elapsedMs < 10_000, `${format} 导出耗时应低于 10s，实际 ${elapsedMs}ms`);
				assert.ok((job?.files.length ?? 0) >= 1);

				const fileResp = await fetch(`${origin}${job!.files[0]!.url}`);
				assert.equal(fileResp.status, 200);
				const buf = Buffer.from(await fileResp.arrayBuffer());
				if (format === "png") {
					assert.equal(buf.readUInt32BE(16), 2480, "PNG 宽度应为 1240 CSS px * 2x = 2480px");
				} else {
					const htmlText = buf.toString("utf8");
					assert.ok(htmlText.startsWith("<!DOCTYPE html>"));
					assert.ok(htmlText.includes("Content-Security-Policy"));
					assert.ok(htmlText.includes("data:font/woff2;base64,"), "离线 HTML 内嵌 WOFF2 字体");
					assert.ok(htmlText.includes("结果未确认"), "离线 HTML 投影 BitLocker 未知复查状态");
					assert.ok(htmlText.includes(verificationReason), "离线 HTML 与聊天页显示同一次最新复查");
					assert.ok(!htmlText.includes("E2E_OLD_BITLOCKER_VERIFICATION"));
				}
			}
		} finally {
			await browser?.close();
			await server?.close();
			llmServer.closeAllConnections();
			await new Promise<void>((resolve) => llmServer.close(() => resolve()));
			await rm(root, { recursive: true, force: true }).catch(() => undefined);
		}
	},
);

test(
	"Rust cst-server 特殊诊断卡片、500 段长会话导出 (<10s) 与 AppActive 通道待机界面验收",
	{ timeout: 120_000 },
	async () => {
		const root = await mkdtemp(join(await testRoot(), "rust-web-cards-perf-"));
		const kitRoot = join(root, "kit");
		const home = join(kitRoot, "agent", "home");
		await mkdir(join(home, "sessions"), { recursive: true });
		await configureTestModel(home);

		// 1. Seed a session with special tool cards (`sys overview`, `runbook`, `web_search`) + a 500-paragraph long session
		const cardsSessionId = "special-cards-e2e";
		const cardsLines = [
			JSON.stringify({
				type: "session",
				version: 3,
				id: cardsSessionId,
				timestamp: "2026-11-10T09:00:00.000Z",
				cwd: kitRoot,
			}),
			JSON.stringify({
				type: "message",
				id: "u-cards",
				parentId: null,
				timestamp: "2026-11-10T09:00:01.000Z",
				message: { role: "user", content: "检查系统概览、修复清单与联网检索", timestamp: 1762765201000 },
			}),
			JSON.stringify({
				type: "message",
				id: "a-cards",
				parentId: "u-cards",
				timestamp: "2026-11-10T09:00:02.000Z",
				message: {
					role: "assistant",
					content: [
						{ type: "toolCall", id: "tc-sys", name: "sys", arguments: { scope: "overview" } },
						{
							type: "toolCall",
							id: "tc-runbook",
							name: "runbook",
							arguments: {
								title: "清理缓存清单",
								level: "safe",
								items: [
									{
										shell: "PowerShell",
										admin: false,
										summary: "查看临时目录",
										command: "Get-Item $env:TEMP",
									},
								],
							},
						},
						{
							type: "toolCall",
							id: "tc-search",
							name: "web_search",
							arguments: { queries: ["Windows 蓝屏 0x116"] },
						},
					],
					api: "openai-completions",
					provider: "test-probe",
					model: "mock",
					usage: {
						input: 1,
						output: 1,
						cacheRead: 0,
						cacheWrite: 0,
						totalTokens: 2,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
					},
					stopReason: "toolUse",
					timestamp: 1762765202000,
				},
			}),
			JSON.stringify({
				type: "message",
				id: "tr-sys",
				parentId: "a-cards",
				timestamp: "2026-11-10T09:00:03.000Z",
				message: {
					role: "toolResult",
					toolCallId: "tc-sys",
					toolName: "sys",
					content: [{ type: "text", text: "{}" }],
					details: {
						overview: {
							cpuTotalPct: 24,
							logicalCores: 16,
							mem: { usedPct: 62.5, usedMB: 20480, totalMB: 32768 },
							machine: { vendor: "ASUS", model: "TUF", cpu: "Core i7" },
							uptime: { text: "5小时10分", bootTime: "2026-11-10", totalHours: 5.1 },
							pagefile: [],
						},
					},
					isError: false,
					timestamp: 1762765203000,
				},
			}),
			JSON.stringify({
				type: "message",
				id: "tr-runbook",
				parentId: "tr-sys",
				timestamp: "2026-11-10T09:00:04.000Z",
				message: {
					role: "toolResult",
					toolCallId: "tc-runbook",
					toolName: "runbook",
					content: [{ type: "text", text: "{}" }],
					details: {
						runbook: {
							file: join(kitRoot, "artifacts/2026-11-10/runbook/01-test.txt"),
							name: "01-清理缓存清单.txt",
							items: 1,
							levelLabel: "安全查看",
						},
					},
					isError: false,
					timestamp: 1762765204000,
				},
			}),
			JSON.stringify({
				type: "message",
				id: "tr-search",
				parentId: "tr-runbook",
				timestamp: "2026-11-10T09:00:05.000Z",
				message: {
					role: "toolResult",
					toolCallId: "tc-search",
					toolName: "web_search",
					content: [{ type: "text", text: "{}" }],
					details: { queryCount: 1, successfulQueries: 1, totalResults: 3 },
					isError: false,
					timestamp: 1762765205000,
				},
			}),
		];
		await writeFile(join(home, "sessions", `2026-11-10_${cardsSessionId}.jsonl`), `${cardsLines.join("\n")}\n`);

		const longSessionId = "long-500-paragraphs";
		const paragraphs = Array.from(
			{ length: 500 },
			(_, idx) => `第 ${idx + 1} 段诊断记录：检查系统磁盘空间、驱动签名状态与事件日志摘要，确认读写延迟稳定。`,
		).join("\n\n");
		const longLines = [
			JSON.stringify({
				type: "session",
				version: 3,
				id: longSessionId,
				timestamp: "2026-11-10T09:10:00.000Z",
				cwd: kitRoot,
			}),
			JSON.stringify({
				type: "message",
				id: "u-long",
				parentId: null,
				timestamp: "2026-11-10T09:10:01.000Z",
				message: { role: "user", content: "输出 500 段完整巡检报告", timestamp: 1762765801000 },
			}),
			JSON.stringify({
				type: "message",
				id: "a-long",
				parentId: "u-long",
				timestamp: "2026-11-10T09:10:02.000Z",
				message: {
					role: "assistant",
					content: [{ type: "text", text: paragraphs }],
					api: "openai-completions",
					provider: "test-probe",
					model: "mock",
					usage: {
						input: 10,
						output: 500,
						cacheRead: 0,
						cacheWrite: 0,
						totalTokens: 510,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
					},
					stopReason: "stop",
					timestamp: 1762765802000,
				},
			}),
		];
		await writeFile(join(home, "sessions", `2026-11-10_${longSessionId}.jsonl`), `${longLines.join("\n")}\n`);

		let server: Awaited<ReturnType<typeof startRustWebServer>> | undefined;
		let appModeChild: ChildProcess | undefined;
		let browser: BrowserProbe | undefined;
		try {
			server = await startRustWebServer({ kitRoot });
			browser = await BrowserProbe.launch(root);

			// Verify special tool cards render in browser
			await browser.navigate(`${server.url}/s/${cardsSessionId}`);
			await browser.until(
				`document.body.innerText.includes("整机概览") && document.body.innerText.includes("清理缓存清单") && document.body.innerText.includes("已联网检索")`,
				"特殊工具卡片（整机概览、命令清单、联网检索）完整渲染",
			);

			// Verify 500-paragraph long session multi-page PNG & Offline HTML export both complete within 10s
			const activeUrl = server.url;
			for (const format of ["png", "html"] as const) {
				const startedAt = Date.now();
				const createResp = await fetch(`${activeUrl}/api/sessions/${longSessionId}/exports`, {
					method: "POST",
					headers: {
						Origin: activeUrl,
						"X-CST-Web-Request": "1",
						"Content-Type": "application/json",
						"Idempotency-Key": randomUUID(),
					},
					body: JSON.stringify({ format, theme: "dark", includeThinking: false, includeTools: false }),
				});
				assert.equal(createResp.status, 202);
				const { id: expId } = (await createResp.json()) as { id: string };

				let job: PollExportJob | undefined;
				while (Date.now() - startedAt < 15_000) {
					job = (await (await fetch(`${activeUrl}/api/exports/${expId}`)).json()) as PollExportJob;
					if (job?.status === "completed" || job?.status === "failed") break;
					await new Promise((r) => setTimeout(r, 60));
				}
				const elapsedMs = Date.now() - startedAt;
				assert.equal(job?.status, "completed", `500 段 ${format} 导出失败: ${JSON.stringify(job?.error)}`);
				assert.ok(elapsedMs < 10_000, `500 段 ${format} 导出应在 10s 内完成，实际 ${elapsedMs}ms`);
				if (format === "png") {
					assert.ok((job?.files.length ?? 0) > 1, "500 段长会话 PNG 自动切分为多张分页图片");
				}
			}

			await server.close();
			server = undefined;

			// Verify AppActive standby UI when cst-server runs in default desktop/AppActive mode
			const appKitRoot = join(root, "app-mode-kit");
			const appHome = join(appKitRoot, "agent", "home");
			await mkdir(join(appHome, "sessions"), { recursive: true });
			await cp(join(repoRoot, "agent/home/APPEND_SYSTEM.md"), join(appHome, "APPEND_SYSTEM.md"));
			await cp(join(repoRoot, "agent/home/skills"), join(appHome, "skills"), { recursive: true });
			await mkdir(join(appHome, "extensions/web"), { recursive: true });
			await symlink(
				join(repoRoot, "agent/home/extensions/web/static"),
				join(appHome, "extensions/web/static"),
				"junction",
			);
			await configureTestModel(appHome);

			const appPort = await freePort();
			const bin = join(repoRoot, "target", "debug", process.platform === "win32" ? "cst-server.exe" : "cst-server");
			appModeChild = spawn(bin, ["--port", String(appPort), "--kit-root", appKitRoot, "--cwd", appKitRoot], {
				cwd: appKitRoot,
				stdio: "ignore",
				windowsHide: true,
			});
			const appOrigin = `http://127.0.0.1:${appPort}`;
			for (let i = 0; i < 100; i++) {
				try {
					const r = await fetch(`${appOrigin}/api/health`, { signal: AbortSignal.timeout(300) });
					if (r.ok) break;
				} catch {
					/* wait */
				}
				await new Promise((r) => setTimeout(r, 40));
			}

			await browser.navigate(`${appOrigin}/`);
			await browser.until(
				`document.body.innerText.includes("软件端正在使用中") && document.body.innerText.includes("Web 界面处于待机状态")`,
				"AppActive 通道待机提示横幅呈现",
			);
		} finally {
			await browser?.close();
			await server?.close();
			if (appModeChild && appModeChild.exitCode === null) {
				appModeChild.kill();
			}
			await rm(root, { recursive: true, force: true }).catch(() => undefined);
		}
	},
);
