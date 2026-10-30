import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import type { ExportSnapshot, SessionExport } from "../shared/exports.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("会话导出 API 与网页：实际浅深 PNG、离线 HTML、选轮、分图和重启读取", { timeout: 180_000 }, async () => {
	const root = join(await testRoot(), "session-export");
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
	const slot = await pool.create();
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const artifactsRoot = join(kit, "artifacts");
	let api = createWebApi(pool, home, port, undefined, { artifactsRoot });
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "export-test" }),
		(...args) => api(...args),
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	const post = (path: string, value: unknown, key = crypto.randomUUID()) =>
		fetch(`${origin}${path}`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": key,
			},
			body: JSON.stringify(value),
		});
	const base = `/api/sessions/${slot.id}/exports`;
	async function waitJob(id: string): Promise<SessionExport> {
		for (let attempt = 0; attempt < 600; attempt++) {
			const result = (await (await fetch(`${origin}/api/exports/${id}`)).json()) as SessionExport;
			if (result.status === "failed") throw new Error(JSON.stringify(result.error));
			if (result.status === "completed") return result;
			await new Promise((resolve) => setTimeout(resolve, 100));
		}
		throw new Error("导出任务超时");
	}
	try {
		browser = await BrowserProbe.launch(root);
		const image = await browser.evaluate<string>(
			`(() => { const c=document.createElement('canvas');c.width=240;c.height=100;const x=c.getContext('2d');x.fillStyle='#e3ecf1';x.fillRect(0,0,240,100);x.fillStyle='#202020';x.font='18px sans-serif';x.fillText('CST · attachment',24,56);return c.toDataURL('image/png').split(',')[1]; })()`,
		);
		const now = Date.now();
		slot.session.sessionManager.appendMessage({
			role: "user",
			content: [
				{ type: "text", text: "检查磁盘和内存，附图仅用于测试。" },
				{ type: "image", mimeType: "image/png", data: image },
			],
			timestamp: now,
		});
		const firstTurn = slot.session.sessionManager.getLeafId()!;
		const usage = {
			input: 1,
			output: 1,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 2,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		};
		const assistant = {
			role: "assistant" as const,
			api: "openai-completions" as const,
			provider: "test-probe",
			model: "mock",
			usage,
			timestamp: now + 1,
		};
		slot.session.sessionManager.appendMessage({
			...assistant,
			content: [
				{ type: "thinking", thinking: "测试思考：先核对存储，再核对内存。" },
				{ type: "toolCall", id: "read-check", name: "read", arguments: { path: "fixture.txt" } },
			],
			stopReason: "toolUse",
		});
		slot.session.sessionManager.appendMessage({
			role: "toolResult",
			toolCallId: "read-check",
			toolName: "read",
			content: [{ type: "text", text: "工具结果：fixture output" }],
			isError: false,
			timestamp: now + 2,
		});
		slot.session.sessionManager.appendMessage({
			...assistant,
			timestamp: now + 3,
			content: [
				{
					type: "text",
					text:
						"## 检查结论\n\n磁盘空间充足，建议核对内存占用。\n\n| 项目 | 状态 |\n|---|---|\n| 磁盘 | 正常 |\n| 内存 | 需要检查 |\n\n```powershell\nGet-Process | Sort-Object WorkingSet -Descending\n```\n\n```typescript\nconst 中文 = 'fixture';\nconsole.log(中文);\n```\n\n```text\n" +
						"很长的代码内容_".repeat(80) +
						"\n```\n\n<script>alert('fixture')</script>",
				},
			],
			stopReason: "stop",
		});
		slot.session.sessionManager.appendMessage({
			role: "user",
			content: "生成长会话用于分图验证。",
			timestamp: now + 4,
		});
		slot.session.sessionManager.appendMessage({
			...assistant,
			timestamp: now + 5,
			content: [
				{
					type: "text",
					text: Array.from(
						{ length: 110 },
						(_, i) => `### 检查步骤 ${i + 1}\n\n这是第 ${i + 1} 段中文检查结果，包含保留完整信息的说明。`,
					).join("\n\n"),
				},
			],
			stopReason: "stop",
		});
		const config = { date: "2026-10-29", theme: "light", format: "png", turnIds: [firstTurn] };
		const preview = (await (await post(`${base}/preview`, config)).json()) as ExportSnapshot;
		assert.ok(preview.entries.some((entry) => entry.message.role === "user"));
		assert.equal(
			preview.entries.some((entry) => entry.message.role === "toolResult"),
			false,
		);
		const jobs: SessionExport[] = [];
		for (const format of ["png", "html"] as const)
			for (const theme of ["light", "dark"] as const) {
				const key = crypto.randomUUID();
				const response = await post(
					base,
					{ ...config, format, theme, includeThinking: true, includeTools: true },
					key,
				);
				assert.equal(response.status, 202);
				const created = (await response.json()) as SessionExport;
				assert.equal(
					(
						await (
							await post(base, { ...config, format, theme, includeThinking: true, includeTools: true }, key)
						).json()
					).id,
					created.id,
				);
				const result = await waitJob(created.id);
				jobs.push(result);
				assert.equal(result.files.length, 1);
				assert.equal(result.directory, join(artifactsRoot, config.date, "sessions"));
				const file = result.files[0];
				const download = await fetch(`${origin}${file.url}`);
				assert.equal(download.status, 200);
				const bytes = Buffer.from(await download.arrayBuffer());
				assert.deepEqual(bytes, await readFile(join(result.directory, file.name)));
				if (format === "png") {
					assert.equal(bytes.subarray(1, 4).toString(), "PNG");
					await writeFile(join(root, `actual-${theme}.png`), bytes);
				} else {
					const html = bytes.toString("utf8");
					assert.equal(html.includes("<script"), false);
					assert.equal(html.includes("data:font/woff2;base64,"), true);
					assert.equal(html.includes("data:image/png;base64,"), true);
					assert.equal(html.includes("fixture output"), true);
					assert.equal(html.includes("测试思考"), true);
					const offlineUrl = pathToFileURL(join(result.directory, file.name)).href;
					await browser.call("Network.enable");
					await browser.call("Network.emulateNetworkConditions", {
						offline: true,
						latency: 0,
						downloadThroughput: 0,
						uploadThroughput: 0,
					});
					await browser.call("Page.navigate", { url: offlineUrl });
					await browser.until(
						`location.href === ${JSON.stringify(offlineUrl)} && document.readyState==='complete' && !!document.querySelector('.export-document')`,
						"离线 HTML 文件加载",
					);
					await browser.evaluate("document.fonts.ready");
					assert.equal(
						await browser.evaluate(
							"(()=>{const fonts=[...document.fonts].filter(f=>f.family==='CST UI Sans');return fonts.length>0 && fonts.every(f=>f.status==='loaded')})()",
						),
						true,
					);
					assert.equal(await browser.evaluate("document.documentElement.scrollWidth <= innerWidth"), true);
					assert.equal(await browser.evaluate("document.querySelectorAll('script,button,input').length"), 0);
					assert.equal(
						await browser.evaluate("[...document.images].every(i=>i.complete && i.naturalWidth>0)"),
						true,
					);
					assert.equal(await browser.evaluate("!!document.querySelector('.hljs-built_in, .hljs-keyword')"), true);
					await browser.screenshot(join(root, `html-${theme}.png`));
					await browser.evaluate("scrollTo(0,document.documentElement.scrollHeight)");
					await browser.screenshot(join(root, `html-${theme}-bottom.png`));
					await browser.evaluate("document.querySelector('details').open=false");
					assert.equal(await browser.evaluate("document.querySelector('details').open"), false);
				}
			}
		await browser.call("Network.emulateNetworkConditions", {
			offline: false,
			latency: 0,
			downloadThroughput: -1,
			uploadThroughput: -1,
		});
		const long = (await (
			await post(base, { format: "png", theme: "light", date: config.date })
		).json()) as SessionExport;
		const split = await waitJob(long.id);
		assert.ok(split.files.length > 1);
		await writeFile(join(root, "actual-long-first.png"), await readFile(join(split.directory, split.files[0].name)));
		await writeFile(
			join(root, "actual-long-last.png"),
			await readFile(join(split.directory, split.files.at(-1)!.name)),
		);
		assert.ok(
			(await readdir(split.directory)).every((name) => /\.(png|html)$/.test(name)),
			"日期下的 sessions 只放 PNG / HTML",
		);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.click(".chat-menu-trigger");
		await browser.click(`a[href='/s/${slot.id}/export']`);
		await browser.until(
			"!!document.querySelector('.export-preview-viewport iframe') && !document.querySelector('.export-generate').disabled",
			"网页导出预览就绪",
		);
		await browser.screenshot(join(root, "export-controls-light.png"));
		await browser.evaluate(
			"(()=>{const e=document.querySelector('.export-controls select');e.value='html';e.dispatchEvent(new Event('change',{bubbles:true}));})()",
		);
		await browser.until("!document.querySelector('.export-generate').disabled", "切换格式后的预览");
		await browser.click(".export-generate");
		await browser.until("!!document.querySelector('.export-result a')", "网页点击生成并保存真实文件", 30_000);
		await browser.screenshot(join(root, "export-result-light.png"));
		await browser.evaluate(
			"document.documentElement.dataset.theme='dark';const e=document.querySelectorAll('.export-controls select')[1];e.value='dark';e.dispatchEvent(new Event('change',{bubbles:true}));",
		);
		await browser.until(
			"document.querySelector('iframe').contentDocument.documentElement.dataset.theme==='dark'",
			"夜间文档预览",
		);
		await browser.screenshot(join(root, "export-controls-dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		await browser.screenshot(join(root, "export-controls-narrow.png"));
		assert.equal((await post(base, { ...config, date: "2026-02-30" })).status, 400);
		assert.equal((await post(base, { ...config, turnIds: ["missing"] })).status, 400);
		assert.equal((await fetch(`${origin}/api/exports/${jobs[0].id}/snapshot`)).status, 404);
		assert.equal(
			(
				await fetch(`${origin}${base}`, {
					method: "POST",
					headers: { Origin: "https://untrusted.example", "Content-Type": "application/json" },
					body: JSON.stringify(config),
				})
			).status,
			403,
		);
		await api.close();
		api = createWebApi(pool, home, port, undefined, { artifactsRoot });
		assert.equal((await (await fetch(`${origin}/api/exports/${jobs[0].id}`)).json()).status, "completed");
		assert.equal((await fetch(`${origin}${jobs[0].files[0].url}`)).status, 200);
		assert.ok((await (await fetch(`${origin}${base}`)).json()).exports.length >= 5);
		await writeFile(join(jobs[0].directory, jobs[0].files[0].name), "changed");
		assert.equal((await fetch(`${origin}${jobs[0].files[0].url}`)).status, 409);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
