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

test("首轮工作时长：主页发送、纯正文回复完成、后续对话与刷新均保留完成状态", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "first-turn-status");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	let requests = 0;
	const model = createServer(async (request, response) => {
		for await (const _chunk of request) {
			/* 外部模型边界，其余使用真实产品实现。 */
		}
		const turn = ++requests;
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.flushHeaders();
		await new Promise((resolve) => setTimeout(resolve, 1800));
		response.write(
			`data: ${JSON.stringify({ id: `turn-${turn}`, choices: [{ index: 0, delta: { role: "assistant", content: `第${turn}轮纯正文回答。` }, finish_reason: null }] })}\n\n`,
		);
		await new Promise((resolve) => setTimeout(resolve, 300));
		response.end(
			`data: ${JSON.stringify({ id: `turn-${turn}`, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const modelPort = await freePort();
	await new Promise<void>((resolve) => model.listen(modelPort, "127.0.0.1", resolve));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", enabledModels: ["probe/one"], theme: "light" }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "首轮验收模型",
							reasoning: false,
							input: ["text"],
							contextWindow: 10000,
							maxTokens: 100,
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
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/`);
		await browser.until(
			"document.querySelector('.model-label')?.textContent.includes('首轮验收模型')",
			"首页模型就绪",
		);
		await browser.fill(".composer-input", "你好");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"location.pathname.startsWith('/s/') && document.querySelector('.turn-process-head')?.textContent.includes('工作中')",
			"首轮运行时长出现",
		);
		const id = await browser.evaluate<string>("location.pathname.split('/')[2]");
		await browser.evaluate(
			"window.__processTransitions=[];window.__observedProcess=document.querySelector('.turn-process-head');window.__processObserver=new MutationObserver(()=>window.__processTransitions.push({connected:window.__observedProcess.isConnected,labels:[...document.querySelectorAll('.turn-process-head')].map(e=>e.textContent)}));window.__processObserver.observe(document.querySelector('.conversation'),{childList:true,subtree:true});",
		);
		await browser.screenshot(join(root, "first-running.png"));
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('第1轮纯正文回答。') && !!document.querySelector('[aria-label=发送消息]')",
			"首轮回复结束",
		);
		const completed = await browser.evaluate<{
			label: string;
			kept: boolean;
			history: { connected: boolean; labels: string[] }[];
		}>(
			"(()=>{window.__processObserver.disconnect();return {label:document.querySelector('.turn-process-head')?.textContent??'',kept:window.__observedProcess.isConnected,history:window.__processTransitions};})()",
		);
		await writeFile(join(root, "first-observation.json"), JSON.stringify(completed, null, 2));
		await browser.screenshot(join(root, "first-completed.png"));
		assert.match(completed.label, /已工作 \d+ 秒/, "首轮纯正文完成后保留工作时长");
		assert.equal(completed.kept, true, "完成状态更新不卸载已显示的过程头");
		assert.ok(
			completed.history.every((frame) => frame.connected),
			"流式结束与快照交接期间过程头连续存在",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.turn-process-head').getAttribute('aria-expanded')"),
			"false",
			"首轮完成后自动折叠",
		);
		await browser.click(".turn-process-head");
		await browser.until(
			"document.querySelector('.turn-process-head').getAttribute('aria-expanded')==='true'",
			"手动展开首轮",
		);
		await browser.fill(".composer-input", "第二句");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"[...document.querySelectorAll('.turn-process-head')].some(e=>e.textContent.includes('工作中'))",
			"第二轮运行",
		);
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('第2轮纯正文回答。') && !!document.querySelector('[aria-label=发送消息]')",
			"第二轮回复完成",
		);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head')].filter(e=>/已工作/.test(e.textContent)).length",
			),
			2,
			"后续完成不删除首轮时长",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.turn-process-head').textContent"),
			completed.label,
			"首轮计时冻结，不随第二轮继续增加",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.turn-process-head').getAttribute('aria-expanded')"),
			"true",
			"后续轮次不覆盖用户的展开选择",
		);
		await browser.navigate(`${origin}/s/${id}`);
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('第2轮纯正文回答。')",
			"刷新恢复两轮历史",
		);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head')].filter(e=>/已工作/.test(e.textContent)).length",
			),
			2,
			"刷新后仍保留两轮完成时长",
		);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head')].every(e=>e.getAttribute('aria-expanded')==='false')",
			),
			true,
			"历史轮次默认折叠",
		);
		await browser.screenshot(join(root, "after-refresh.png"));
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{
					firstCompleted: completed.label,
					stableHeader: completed.kept,
					secondCompleted: true,
					firstDurationFrozen: true,
					manualExpansionPreserved: true,
					refreshRestored: true,
					modelRequests: requests,
				},
				null,
				2,
			),
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		model.closeAllConnections();
		await new Promise<void>((resolve) => model.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
