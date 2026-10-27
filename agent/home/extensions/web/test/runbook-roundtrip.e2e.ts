import assert from "node:assert/strict";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { CommandPlanStore } from "../../diagnostics/command-plan-store.ts";
import { openDeliveredRunbook } from "../server/api/runbook.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("命令方案多轮修订、压缩后刷新与服务重启恢复，正文与输入框对齐", { timeout: 60000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "runbook-roundtrip-"));
	const home = join(root, "home"),
		outbox = join(root, "outbox");
	await mkdir(home);
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const options = {
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	};
	let pool = new WebSessionPool(options);
	let port = await freePort();
	const opened: string[] = [];
	const createApi = () =>
		createWebApi(pool, home, port, undefined, {
			openRunbook: (file) =>
				openDeliveredRunbook(
					file,
					async (target) => {
						opened.push(target);
					},
					outbox,
				),
		});
	let api = createApi();
	const createServer = () =>
		createWebServer(
			fileURLToPath(new URL("../static/", import.meta.url)),
			port,
			() => ({ sessions: pool.snapshot(), stage: "test" }),
			api,
		);
	let server = createServer();
	await listenWebServer(server, port);
	const slot = await pool.create();
	const store = new CommandPlanStore(outbox);
	const first = await store.plan({
		title: "命令方案",
		level: "safe",
		items: [{ shell: "cmd", summary: "输出第一批内容", command: "echo first" }],
	});
	const deliveredFirst = await store.deliver(first.runbookId, first.version);
	const second = await store.plan({
		title: "修订后的命令方案",
		level: "safe",
		runbookId: first.runbookId,
		baseVersion: first.version,
		items: [{ shell: "powershell", summary: "输出第二批内容", command: "Write-Output second" }],
	});
	const deliveredSecond = await store.deliver(second.runbookId, second.version);
	let timestamp = Date.now();
	const metadata = {
		api: "openai-completions" as const,
		provider: "fixture",
		model: "fixture",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
	};
	const append = (name: string, runbook: object, args: Record<string, unknown> = {}, textOnly = false) => {
		const entry = slot.session.sessionManager.appendMessage({
			role: "assistant",
			...metadata,
			content: [{ type: "toolCall", id: "reused-call", name, arguments: args }],
			stopReason: "toolUse",
			timestamp: timestamp++,
		});
		slot.session.sessionManager.appendMessage({
			role: "toolResult",
			toolCallId: "reused-call",
			toolName: name,
			content: [{ type: "text", text: JSON.stringify({ runbook }) }],
			...(textOnly ? {} : { details: { runbook } }),
			isError: false,
			timestamp: timestamp++,
		});
		return entry;
	};
	const user = slot.session.sessionManager.appendMessage({
		role: "user",
		content: "第一批命令",
		timestamp: timestamp++,
	});
	append("command_plan", { ...first, items: 1, action: "planned" });
	append("runbook_deliver", deliveredFirst, { runbookId: first.runbookId, version: 1 });
	slot.session.sessionManager.appendMessage({
		role: "assistant",
		...metadata,
		content: [{ type: "text", text: "第一批命令已交付。" }],
		stopReason: "stop",
		timestamp: timestamp++,
	});
	slot.session.sessionManager.appendMessage({ role: "user", content: "请修订命令", timestamp: timestamp++ });
	append("command_plan", { ...second, items: 1, action: "planned" });
	append("runbook_deliver", deliveredSecond, { runbookId: first.runbookId, version: 2 }, true);
	slot.session.sessionManager.appendMessage({
		role: "assistant",
		...metadata,
		content: [{ type: "text", text: "第二批命令已交付。\n\n| 项目 | 状态 |\n| --- | --- |\n| 清单 | 已修订 |" }],
		stopReason: "stop",
		timestamp: timestamp++,
	});
	slot.session.sessionManager.appendCompaction("命令方案已修订为版本 2，保留手动执行边界。", user, 1000);
	let browser: BrowserProbe | undefined;
	const close = async () => {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await api.close();
		await pool.close();
	};
	try {
		browser = await BrowserProbe.launch(root);
		const origin = () => `http://127.0.0.1:${port}`;
		await browser.navigate(`${origin()}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool--runbook').length===4", "四张命令卡常驻");
		const probe = browser;
		const commands = () =>
			probe.evaluate<string[]>(
				"[...document.querySelectorAll('.special-tool-command-line code')].map(e=>e.textContent)",
			);
		assert.deepEqual(await commands(), ["echo first", "echo first", "Write-Output second", "Write-Output second"]);
		await browser.evaluate(
			"window.__copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>window.__copied.push(text)}})",
		);
		await browser.evaluate("document.querySelectorAll('.special-tool-command-line button')[3].click()");
		assert.deepEqual(await browser.evaluate("window.__copied"), ["Write-Output second"]);
		await browser.evaluate("document.querySelectorAll('.special-tool-file-link')[0].click()");
		await browser.until("document.querySelectorAll('.special-tool-feedback').length===1", "第一轮文件打开");
		await browser.evaluate("document.querySelectorAll('.special-tool-file-link')[1].click()");
		await browser.until("document.querySelectorAll('.special-tool-feedback').length===2", "第二轮文件打开");
		assert.deepEqual(opened, [deliveredFirst.file, deliveredSecond.file]);
		for (const width of [1920, 1366, 960, 390]) {
			await browser.call("Emulation.setDeviceMetricsOverride", {
				width,
				height: 1000,
				deviceScaleFactor: 1,
				mobile: false,
			});
			if (width <= 960) await browser.evaluate("document.querySelector('[aria-label=\"收起侧栏\"]')?.click()");
			await browser.evaluate(
				"document.fonts.ready.then(()=>Promise.all(document.getAnimations().map(a=>a.finished.catch(()=>{}))))",
			);
			const geometry: { leftDelta: number; rightDelta: number; width: number; overflow: boolean } =
				await browser.evaluate(
					"(()=>{const m=document.querySelector('.markdown').getBoundingClientRect(),c=document.querySelector('.composer--chat').getBoundingClientRect();return {leftDelta:m.left-c.left,rightDelta:m.right-c.right,width:m.width,overflow:document.documentElement.scrollWidth>innerWidth}})()",
				);
			assert.ok(Math.abs(geometry.leftDelta) < 1, `宽 ${width} 正文与输入框左缘对齐`);
			assert.ok(Math.abs(geometry.rightDelta) < 1, `宽 ${width} 正文与输入框右缘对齐`);
			assert.ok(geometry.width <= 1144, `宽 ${width} 保留内容列宽度上限`);
			assert.equal(geometry.overflow, false);
			await browser.screenshot(join(root, `layout-${width}.png`));
		}
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.screenshot(join(root, "runbook-dark-mobile.png"));
		await browser.navigate(`${origin()}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool--runbook').length===4", "压缩后刷新命令仍在");
		assert.deepEqual(await commands(), ["echo first", "echo first", "Write-Output second", "Write-Output second"]);
		await browser.call("Page.navigate", { url: "about:blank" });
		await close();
		pool = new WebSessionPool(options);
		port = await freePort();
		api = createApi();
		server = createServer();
		await listenWebServer(server, port);
		await browser.navigate(`${origin()}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool--runbook').length===4", "服务重启后从文件恢复");
		assert.deepEqual(await commands(), ["echo first", "echo first", "Write-Output second", "Write-Output second"]);
		console.log("RUNBOOK_EVIDENCE", root);
	} finally {
		await browser?.close();
		await close();
	}
});
