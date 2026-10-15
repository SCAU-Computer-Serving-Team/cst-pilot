import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("特殊工具混排与 Web 本地解锁：刷新恢复、隐藏输入、双主题、窄幅和历史失效", { timeout: 60000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "special-tools-browser-"));
	const home = join(root, "home");
	await mkdir(join(home, "extensions"), { recursive: true });
	const core = fileURLToPath(new URL("../../diagnostics/bitlocker-core.ts", import.meta.url)).replaceAll("\\", "/");
	const bridge = fileURLToPath(new URL("../../diagnostics/bitlocker-web.ts", import.meta.url)).replaceAll("\\", "/");
	await writeFile(
		join(home, "extensions", "fixture.ts"),
		`
 import {Type} from '@sinclair/typebox';import {executeBitLocker} from ${JSON.stringify(core)};import {bitLockerWebInteractions} from ${JSON.stringify(bridge)};
 export default function(pi){pi.registerTool({name:'bitlocker',label:'BitLocker',description:'Fixture only',parameters:Type.Object({scope:Type.String(),drive:Type.String()}),async execute(id,args,signal,update,ctx){const volume={volumeId:'fixture-volume',drive:'D:',volumeType:1,conversionStatus:1,encryptionPercentage:100,encryptionMethod:7,protectionStatus:1,lockStatus:1,autoUnlockEnabled:false,autoUnlockKeysStored:null,keyProtectors:[{type:3,id:'fixture-key-id'}],collectionErrors:[]};const run=async req=>{if(req.scope==='unlock')await new Promise(r=>setTimeout(r,500));return({snapshot:{collectedAt:new Date().toISOString(),admin:true,volumes:[{...volume,lockStatus:req.scope==='unlock'?0:1}],collectionErrors:[]},...(req.scope==='unlock'?{operation:{status:'accepted',reason:'Windows 已接受请求',returnCode:0,error:null}}:{})});};const details=await executeBitLocker(args,run,bitLockerWebInteractions.get(ctx.sessionManager.getSessionId()).forCall(id),signal,stage=>update({content:[],details:{bitlocker:{stage}}}));return{content:[{type:'text',text:JSON.stringify(details)}],details};}});}
 `,
	);
	let count = 0;
	const model = createServer(async (req, res) => {
		for await (const _ of req) {
		}
		const first = ++count === 1;
		res.writeHead(200, { "Content-Type": "text/event-stream" });
		const delta = first
			? {
					role: "assistant",
					content: "请在本地界面输入恢复密码。",
					tool_calls: [
						{
							index: 0,
							id: "unlock-fixture",
							type: "function",
							function: { name: "bitlocker", arguments: JSON.stringify({ scope: "unlock", drive: "D:" }) },
						},
					],
				}
			: { role: "assistant", content: "本次解锁测试已完成。" };
		res.end(
			`data: ${JSON.stringify({ id: "fixture", choices: [{ index: 0, delta, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ id: "fixture", choices: [{ index: 0, delta: {}, finish_reason: first ? "tool_calls" : "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const mp = await freePort();
	await new Promise<void>((r) => model.listen(mp, "127.0.0.1", r));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "fixture",
			defaultModel: "one",
			defaultTools: ["read", "ls"],
			retry: { enabled: false },
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				fixture: {
					baseUrl: `http://127.0.0.1:${mp}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "Fixture",
							reasoning: false,
							input: ["text"],
							contextWindow: 10000,
							maxTokens: 300,
						},
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (f) => f(),
	});
	const slot = await pool.create();
	const calls = [
		{ type: "toolCall" as const, id: "sys-fixture", name: "sys", arguments: {} },
		{ type: "toolCall" as const, id: "ls-fixture", name: "ls", arguments: { path: home } },
		{
			type: "toolCall" as const,
			id: "search-fixture",
			name: "web_search",
			arguments: { queries: ["fixture one", "fixture two"] },
		},
	];
	slot.session.sessionManager.appendMessage({
		role: "assistant",
		content: calls,
		api: "openai-completions",
		provider: "fixture",
		model: "one",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "toolUse",
		timestamp: Date.now(),
	});
	for (const call of calls) {
		const details =
			call.name === "sys"
				? {
						overview: {
							cpuTotalPct: 19,
							logicalCores: 16,
							mem: { usedPct: 83.6, usedMB: 27214, totalMB: 32557 },
							machine: { vendor: "Maxsun", cpu: "Intel Core i5" },
							uptime: { text: "3 天 8 小时" },
						},
					}
				: call.name === "ls"
					? { entries: [] }
					: { queryCount: 2, successfulQueries: 2, totalResults: 0 };
		slot.session.sessionManager.appendMessage({
			role: "toolResult",
			toolCallId: call.id,
			toolName: call.name,
			content: [{ type: "text", text: JSON.stringify(details) }],
			details,
			isError: false,
			timestamp: Date.now(),
		});
	}
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool').length===2", "历史特殊工具常驻");
		assert.equal(
			await browser.evaluate(
				"Array.from(document.querySelectorAll('.turn-process-head')).some(e=>e.textContent==='已完成 1 次工具调用')",
			),
			true,
		);
		assert.equal(await browser.evaluate("document.body.textContent.includes('检查过程')"), false);
		assert.equal(await browser.evaluate("!!document.querySelector('.special-tool').closest('.turn-process')"), false);
		await browser.fill(".composer-input", "解锁测试");
		await browser.click('[aria-label="发送消息"]');
		await browser.until("!!document.querySelector('.special-tool-password input')", "独立密码输入");
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.until("!!document.querySelector('.special-tool-password input')", "刷新恢复请求");
		assert.equal(await browser.evaluate("document.querySelector('.special-tool-password input').type"), "password");
		await browser.screenshot(join(root, "light-input.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.screenshot(join(root, "dark-input.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="收起侧栏"]');
		await browser.screenshot(join(root, "narrow-input.png"));
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=390"), true);
		const password = Array(8).fill("123456").join("-");
		await browser.fill(".special-tool-password input", password);
		await browser.click('.special-tool-actions button[type="submit"]');
		await browser.until(
			"document.querySelector('.special-tool--bitlocker').textContent.includes('解锁中')",
			"真实执行阶段",
		);
		await browser.until("document.body.innerText.includes('本次解锁测试已完成')", "解锁结果和续答");
		assert.equal(await browser.evaluate("!!document.querySelector('.special-tool-password input')"), false);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool').length===3", "历史结果恢复");
		assert.equal(await browser.evaluate("!!document.querySelector('.special-tool-password input')"), false);
		const sessionFile = slot.session.sessionFile;
		assert.ok(sessionFile);
		assert.equal((await readFile(sessionFile, "utf8")).includes(password), false);
		await browser.screenshot(join(root, "history-result.png"));
	} finally {
		await browser?.close();
		await pool.close();
		await api.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		await new Promise<void>((r) => model.close(() => r()));
	}
});
