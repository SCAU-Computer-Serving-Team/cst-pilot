import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createExportRenderer } from "../server/exports/render.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import type { SessionExport } from "../shared/exports.ts";
import { freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("真实导出性能：短长会话 PNG / HTML 冷启动与复用均在十秒内保存", { timeout: 240_000 }, async () => {
	const root = join(await testRoot(), "export-performance");
	await mkdir(root, { recursive: true });
	const kit = await mkdtemp(join(root, "kit-"));
	const home = join(kit, "agent", "home");
	await mkdir(home, { recursive: true });
	await configureTestModel(home);
	const pool = new WebSessionPool({
		cwd: kit,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const rendererOptions = { origin, workDir: join(home, "session-exports", "work", "browsers") };
	let render = createExportRenderer(rendererOptions);
	let renderMs = 0;
	const api = createWebApi(pool, home, port, undefined, {
		artifactsRoot: join(kit, "artifacts"),
		exportRenderer: {
			async render(...args: Parameters<typeof render.render>) {
				const start = performance.now();
				try {
					await render.render(...args);
				} catch (cause) {
					await writeFile(
						join(kit, "render-error.txt"),
						cause instanceof Error ? (cause.stack ?? cause.message) : String(cause),
					);
					throw cause;
				} finally {
					renderMs = performance.now() - start;
				}
			},
			close: () => render.close(),
		},
	});
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "export-performance" }),
		api,
	);
	await listenWebServer(server, port);
	const measurements: {
		size: string;
		format: string;
		mode: string;
		totalMs: number;
		renderMs: number;
		files: number;
		bytes: number;
	}[] = [];
	try {
		for (const size of ["short", "long"] as const) {
			const slot = await pool.create();
			const manager = slot.session.sessionManager;
			manager.appendMessage({ role: "user", content: "导出性能检查，保留中文与代码。", timestamp: Date.now() });
			manager.appendMessage({
				role: "assistant",
				api: "openai-completions",
				provider: "test-probe",
				model: "mock",
				stopReason: "stop",
				timestamp: Date.now(),
				usage: {
					input: 1,
					output: 1,
					cacheRead: 0,
					cacheWrite: 0,
					totalTokens: 2,
					cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
				},
				content: [
					{ type: "thinking", thinking: "性能夹具思考内容" },
					{
						type: "text",
						text: `${Array.from(
							{ length: size === "long" ? 500 : 2 },
							(_, i) =>
								`### 检查步骤 ${i + 1}\n\n这是第 ${i + 1} 段中文检查结果，包含保留完整信息的说明。\n\n\`\`\`typescript\nconst result${i} = '完整内容';\nconsole.log(result${i});\n\`\`\``,
						).join("\n\n")}\n\n导出正文结束标记`,
					},
				],
			});
			for (const format of ["png", "html"] as const) {
				for (const mode of ["cold", "warm"] as const) {
					if (mode === "cold") {
						await render.close();
						render = createExportRenderer(rendererOptions);
					}
					const start = performance.now();
					const response = await fetch(`${origin}/api/sessions/${slot.id}/exports`, {
						method: "POST",
						headers: {
							Origin: origin,
							"X-CST-Web-Request": "1",
							"Content-Type": "application/json",
							"Idempotency-Key": crypto.randomUUID(),
						},
						body: JSON.stringify({ format, theme: "light", includeThinking: true, includeTools: true }),
					});
					assert.equal(response.status, 202);
					let result = (await response.json()) as SessionExport;
					while (result.status === "queued" || result.status === "rendering") {
						assert.ok(performance.now() - start < 120_000, "导出任务超时");
						await new Promise((resolve) => setTimeout(resolve, 50));
						result = (await (await fetch(`${origin}/api/exports/${result.id}`)).json()) as SessionExport;
					}
					const totalMs = performance.now() - start;
					assert.equal(result.status, "completed", JSON.stringify(result.error));
					measurements.push({
						size,
						format,
						mode,
						totalMs,
						renderMs,
						files: result.files.length,
						bytes: result.files.reduce((sum, file) => sum + file.bytes, 0),
					});
					const pages = new Set<string>();
					let imageHeight = 0;
					for (const file of result.files) {
						const bytes = await readFile(join(result.directory, file.name));
						const download = await fetch(`${origin}${file.url}`);
						assert.equal(download.status, 200);
						assert.deepEqual(Buffer.from(await download.arrayBuffer()), bytes);
						if (format === "png") {
							assert.equal(bytes.subarray(1, 4).toString(), "PNG");
							assert.equal(bytes.readUInt32BE(16), 2480, "保留两倍清晰度");
							assert.ok(bytes.readUInt32BE(20) <= 6400);
							imageHeight += bytes.readUInt32BE(20);
							const hash = createHash("sha256").update(bytes).digest("hex");
							assert.equal(pages.has(hash), false, "分图内容各不相同");
							pages.add(hash);
						} else {
							const html = bytes.toString("utf8");
							assert.ok(html.includes("导出正文结束标记"));
							assert.ok(html.includes("性能夹具思考内容"));
							assert.ok(html.includes("data:font/woff2;base64,"));
							assert.equal(html.includes("<script"), false);
						}
					}
					if (size === "long" && format === "png") {
						assert.ok(result.files.length > 1);
						assert.ok(imageHeight > 140_000, "长会话全部高度均已截图");
					}
				}
			}
		}
	} finally {
		await writeFile(join(kit, "timings.json"), JSON.stringify(measurements, null, 2));
		console.log(JSON.stringify({ kit, measurements }));
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
	for (const measurement of measurements)
		assert.ok(
			measurement.totalMs < 10_000,
			`${measurement.size} ${measurement.format} ${measurement.mode}: ${Math.round(measurement.totalMs)}ms 超过十秒`,
		);
});
