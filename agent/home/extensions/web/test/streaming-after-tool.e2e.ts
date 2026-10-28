import assert from "node:assert/strict";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("长思考与特殊工具之后，正文逐段显示，不等待重连集中回放", { timeout: 30000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "streaming-tool-redo-")),
		home = join(root, "home");
	await mkdir(join(home, "extensions"), { recursive: true });
	await writeFile(
		join(home, "extensions", "fixture.ts"),
		`import {Type} from '@sinclair/typebox';export default function(pi){pi.registerTool({name:'sys',label:'sys',description:'Fixture',parameters:Type.Object({}),async execute(){const details={overview:{cpuTotalPct:19,logicalCores:16,mem:{usedPct:50,totalMB:32768,usedMB:16384},machine:{cpu:'fixture'},uptime:{text:'2小时'}}};return {content:[{type:'text',text:JSON.stringify(details)}],details};}});}`,
	);
	let round = 0,
		finished = false,
		firstSent = 0;
	const model = createServer(async (req, res) => {
		for await (const _ of req) {
		}
		res.writeHead(200, { "Content-Type": "text/event-stream" });
		const send = (delta: unknown, finish: string | null = null) =>
			res.write(
				`data: ${JSON.stringify({ id: "fixture", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`,
			);
		if (++round === 1) {
			send({ role: "assistant", reasoning_content: "检查整机运行情况，确认数据来源。".repeat(2000) });
			send({
				tool_calls: [
					{ index: 0, id: "overview-fixture", type: "function", function: { name: "sys", arguments: "{}" } },
				],
			});
			send({}, "tool_calls");
			res.end("data: [DONE]\n\n");
		} else {
			send({ role: "assistant" });
			for (let i = 0; i < 12; i++) {
				if (i === 0) firstSent = Date.now();
				send({ content: `stream-marker-${i} 逐段正文。\n\n` });
				await new Promise((resolve) => setTimeout(resolve, 180));
			}
			send({}, "stop");
			finished = true;
			res.end("data: [DONE]\n\n");
		}
	});
	const modelPort = await freePort();
	await new Promise<void>((resolve) => model.listen(modelPort, "127.0.0.1", resolve));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "fixture",
			defaultModel: "one",
			defaultTools: ["read", "ls"],
			retry: { enabled: false },
			compaction: { enabled: false },
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				fixture: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					apiKey: "fixture",
					models: [
						{
							id: "one",
							name: "Fixture",
							reasoning: true,
							input: ["text"],
							contextWindow: 100000,
							maxTokens: 20000,
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
			withLoader: (load) => load(),
		}),
		slot = await pool.create(),
		port = await freePort();
	const api = createWebApi(pool, home, port),
		server = createWebServer(
			fileURLToPath(new URL("../static/", import.meta.url)),
			port,
			() => ({ sessions: pool.snapshot(), stage: "test" }),
			api,
		);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`http://127.0.0.1:${port}/s/${slot.id}`);
		await browser.fill(".composer-input", "sys overview");
		await browser.click('[aria-label="发送消息"]');
		await browser.until("document.body.textContent.includes('stream-marker-0')", "首段正文");
		await browser.evaluate(
			`window.__firstParagraph=document.querySelector('.assistant-turn > .markdown p');window.__firstWord=window.__firstParagraph.querySelector('.t-stream-w');window.__streamGaps=[];window.__streamWatch=new MutationObserver(()=>{if(!window.__firstParagraph?.isConnected)window.__streamGaps.push(document.querySelector('.assistant-turn > .markdown')?.textContent??'');});window.__streamWatch.observe(document.querySelector('.conversation'),{childList:true,subtree:true});const original=window.fetch;window.fetch=async(...args)=>{if(String(args[0]).endsWith('/api/sessions/${slot.id}'))await new Promise(r=>setTimeout(r,400));return original(...args);};`,
		);
		assert.equal(
			await browser.evaluate("!!document.querySelector('.assistant-turn[data-live] .turn-process-body')"),
			true,
			"运行中过程立即挂载",
		);
		await browser.evaluate(
			"window.__liveProcess = document.querySelector('.assistant-turn[data-live] .turn-process-body'); void 0",
		);
		const lag = Date.now() - firstSent;
		assert.equal(finished, false, "首段必须在模型完成之前显示");
		assert.ok(lag < 1200, `首段延迟 ${lag}ms，不能等待默认重连`);
		await browser.until("document.body.textContent.includes('stream-marker-11')", "末段正文");
		await browser.until("!document.querySelector('.assistant-turn[data-live]')", "完成态落定");
		assert.equal(
			await browser.evaluate("window.__firstParagraph.isConnected"),
			true,
			"追加段落与定稿保持已读段落节点，不闪回",
		);
		assert.equal(await browser.evaluate("!!window.__firstWord?.isConnected"), true, "流式正文定稿保留已有逐词节点");
		assert.deepEqual(await browser.evaluate("window.__streamGaps"), [], "延迟快照下流式正文不消失");
		assert.equal(await browser.evaluate("window.__liveProcess.isConnected"), true, "完成后折叠过程仍保留运行时节点");
		await writeFile(
			join(root, "timing.json"),
			JSON.stringify({ firstVisibleDelayMs: lag, incremental: true }, null, 2),
		);
		console.log("STREAM_EVIDENCE", root, "lag", lag);
	} finally {
		await browser?.close();
		await pool.close();
		await api.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		model.closeAllConnections();
		await new Promise<void>((resolve) => model.close(() => resolve()));
	}
});
