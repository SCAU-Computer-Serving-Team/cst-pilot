import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("单工具执行：read独立完成并冻结计时，ls继续运行；刷新和重开保留真实耗时", { timeout: 60000 }, async () => {
	const root = join(await testRoot(), "tool-timing");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const gate = join(home, "release-ls"),
		fast = join(home, "fast.txt");
	await writeFile(fast, "FAST_READ_RESULT");
	await mkdir(join(home, "extensions"), { recursive: true });
	// 确定性慢目录工具：真实Pi派发与文件读取，只有等待时间人为控制，避免每次重建整盘WizTree。
	await writeFile(
		join(home, "extensions", "slow-ls.ts"),
		`import {Type} from '@sinclair/typebox';import {access,readdir} from 'node:fs/promises';export default function(pi){pi.registerTool({name:'ls',label:'ls',description:'List test directory after gate',parameters:Type.Object({path:Type.String()}),async execute(id,args,signal){while(true){if(signal?.aborted)throw new Error('aborted');try{await access(${JSON.stringify(gate)});break;}catch{}await new Promise(r=>setTimeout(r,30));}return {content:[{type:'text',text:JSON.stringify(await readdir(args.path))}],details:{}};}});}`,
	);
	let requests = 0;
	const model = createServer(async (request, response) => {
		for await (const _ of request) {
		}
		const first = ++requests % 2 === 1;
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		const delta = first
			? {
					role: "assistant",
					tool_calls: [
						{
							index: 0,
							id: "fast-read",
							type: "function",
							function: { name: "read", arguments: JSON.stringify({ path: fast }) },
						},
						{
							index: 1,
							id: "slow-ls",
							type: "function",
							function: { name: "ls", arguments: JSON.stringify({ path: home }) },
						},
					],
				}
			: { role: "assistant", content: "TIMING_COMPLETE" };
		response.end(
			`data: ${JSON.stringify({ id: "timing", choices: [{ index: 0, delta, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ id: "timing", choices: [{ index: 0, delta: {}, finish_reason: first ? "tool_calls" : "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const mp = await freePort();
	await new Promise<void>((r) => model.listen(mp, "127.0.0.1", r));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", defaultTools: ["read", "ls"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${mp}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "Timing",
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
	let pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (f) => f(),
	});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/`);
		assert.equal(await browser.evaluate("document.documentElement.dataset.theme"), "light", "无主题设置默认浅色");
		await browser.fill(".composer-input", "RUN_TIMING");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"!!document.querySelector('.tool-group-heading') && !document.querySelector('.app-layout')?.dataset.homeTransition",
			"工具分组出现且导航完成",
		);
		await browser.click(".tool-group-heading");
		await browser.until(
			"[...document.querySelectorAll('.tool-card-heading')].some(e=>e.textContent.includes('read · 已完成'))",
			"read在ls返回前独立完成",
		);
		const read = () =>
			browser!.evaluate<string>(
				"[...document.querySelectorAll('.tool-card-heading')].find(e=>e.textContent.includes('read'))?.querySelector('time')?.textContent??''",
			);
		const frozen = await read();
		assert.match(frozen, /^\d+\.\d+s$/);
		await new Promise((r) => setTimeout(r, 1300));
		assert.equal(await read(), frozen, "read的时钟冻结");
		assert.ok(
			await browser.evaluate(
				"[...document.querySelectorAll('.tool-card-heading')].some(e=>e.textContent.includes('正在调用 ls'))",
			),
		);
		await browser.screenshot(join(root, "read-finished-ls-running.png"));
		const id = await browser.evaluate<string>("location.pathname.split('/')[2]");
		await browser.navigate(`${origin}/s/${id}`);
		await browser.until("!!document.querySelector('.tool-group-heading')", "刷新运行态");
		await browser.click(".tool-group-heading");
		await browser.until(
			"[...document.querySelectorAll('.tool-card-heading')].some(e=>e.textContent.includes('read · 已完成'))",
			"刷新不重置read完成态",
		);
		assert.equal(await read(), frozen);
		await writeFile(gate, "go");
		await browser.until("document.body.textContent.includes('TIMING_COMPLETE')", "ls结束及续答");
		await browser.click(".turn-process-head");
		await browser.until(
			"[...document.querySelectorAll('.tool-card-heading')].some(e=>e.textContent.includes('ls · 已完成'))",
			"两个工具完成",
		);
		const times = await browser.evaluate<string[]>(
			"[...document.querySelectorAll('.tool-elapsed')].map(e=>e.textContent)",
		);
		assert.equal(times[0], frozen);
		assert.ok(parseFloat(times[1]!) > parseFloat(times[0]!) + 1, "两工具耗时独立");
		await browser.screenshot(join(root, "both-finished.png"));
		await api.close();
		await pool.close();
		pool = new WebSessionPool({
			cwd: home,
			agentDir: home,
			sessionDir: join(home, "sessions"),
			withLoader: (f) => f(),
		});
		const saved = await pool.openSaved(id);
		const restored = saved.session.messages
			.filter((message) => message.role === "assistant")
			.map((message) => saved.execution.decorate(message));
		const restoredCalls = restored.flatMap((message) =>
			message.role === "assistant" ? message.content.filter((part) => part.type === "toolCall") : [],
		);
		assert.equal(restoredCalls.length, 2);
		const restoredTimes = restoredCalls.map((part) => {
			const execution = (part as typeof part & { execution?: { startedAt: number; endedAt: number } }).execution;
			assert.ok(execution);
			return `${((execution.endedAt - execution.startedAt) / 1000).toFixed(1)}s`;
		});
		assert.deepEqual(restoredTimes, times, "重开恢复单工具真实耗时，无组时间估算");
		await saved.session.prompt("SECOND_TIMING_REUSED_IDS");
		const repeated = saved.session.messages
			.filter((message) => message.role === "assistant")
			.map((message) => saved.execution.decorate(message))
			.flatMap((message) =>
				message.role === "assistant" ? message.content.filter((part) => part.type === "toolCall") : [],
			);
		assert.equal(repeated.length, 4, "两轮允许复用供应商调用ID");
		const firstExecution = (
			repeated[0] as (typeof repeated)[0] & { execution: { startedAt: number; endedAt: number } }
		).execution;
		const laterExecution = (
			repeated[2] as (typeof repeated)[2] & { execution: { startedAt: number; endedAt: number } }
		).execution;
		assert.ok(laterExecution.startedAt > firstExecution.endedAt, "同名同ID工具按轮次隔离");
		assert.equal(
			`${((firstExecution.endedAt - firstExecution.startedAt) / 1000).toFixed(1)}s`,
			frozen,
			"重用ID不覆盖前次耗时",
		);
		await writeFile(
			join(root, "report.json"),
			JSON.stringify({ times, frozen, requests, reopened: true, reusedIdsIsolated: true }, null, 2),
		);
		assert.equal(requests, 4, "两轮各一次工具和续答，刷新不重发工具或模型");
	} finally {
		await writeFile(gate, "go");
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		model.closeAllConnections();
		await new Promise<void>((r) => model.close(() => r()));
		await rm(home, { recursive: true, force: true });
	}
});
