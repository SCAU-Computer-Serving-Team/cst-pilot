// Chrome 端到端：分支总结的三条路径（总结、用自定义提示词总结、取消）。
// 运行：node --test home/extensions/web/test/branch-summary.e2e.ts
// 前置：前端已构建并发布到 ../static（cd src/web/frontend && npm run build）。
// 证据：testRoot()/branch-summary/ 下的截图与报告。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

const STATIC_DIR = fileURLToPath(new URL("../static/", import.meta.url));
const rootDir = await testRoot();
/** 总结请求要慢到能被页面看见，否则待总结状态行一闪而过。 */
const SUMMARY_DELAY_MS = 1200;

async function freePort(): Promise<number> {
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const port = (probe.address() as AddressInfo).port;
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	return port;
}

class Cdp {
	private seq = 0;
	private socket: WebSocket;
	private pending = new Map<number, { resolve: (value: any) => void; reject: (error: Error) => void }>();
	constructor(socket: WebSocket) {
		this.socket = socket;
		socket.addEventListener("message", (event) => {
			const message = JSON.parse(String((event as MessageEvent).data)) as {
				id?: number;
				result?: unknown;
				error?: { message: string };
			};
			if (!message.id) return;
			const waiter = this.pending.get(message.id);
			if (!waiter) return;
			this.pending.delete(message.id);
			if (message.error) waiter.reject(new Error(message.error.message));
			else waiter.resolve(message.result);
		});
	}
	send(method: string, params: Record<string, unknown> = {}, sessionId?: string): Promise<any> {
		const id = ++this.seq;
		const payload = JSON.stringify(sessionId ? { id, method, params, sessionId } : { id, method, params });
		return new Promise((resolve, reject) => {
			this.pending.set(id, { resolve, reject });
			this.socket.send(payload);
		});
	}
}

function chromePath(): string {
	const candidates = [
		process.env.CHROME_PATH,
		"C:/Program Files/Google/Chrome/Application/chrome.exe",
		"C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
		join(process.env.LOCALAPPDATA ?? "", "Google/Chrome/Application/chrome.exe"),
	].filter((path): path is string => !!path);
	const found = candidates.find((path) => existsSync(path));
	if (!found) throw new Error("找不到 Chrome，可用 CHROME_PATH 指定");
	return found;
}

async function readPage(pageUrl: string, probe: string, shotPath?: string) {
	const cdpPort = await freePort();
	const profile = await mkdtemp(join(rootDir, "cst-web-summary-chrome-"));
	const child = spawn(
		chromePath(),
		[
			"--headless=new",
			`--remote-debugging-port=${cdpPort}`,
			`--user-data-dir=${profile}`,
			"--no-first-run",
			"--no-default-browser-check",
			"--disable-gpu",
			"--hide-scrollbars",
			"--window-size=1600,1000",
			"about:blank",
		],
		{ stdio: "ignore" },
	);
	try {
		let version: { webSocketDebuggerUrl: string } | undefined;
		for (let attempt = 0; attempt < 100 && !version; attempt++) {
			version = await fetch(`http://127.0.0.1:${cdpPort}/json/version`)
				.then((response) => (response.ok ? response.json() : undefined))
				.catch(() => undefined);
			if (!version) await new Promise((resolve) => setTimeout(resolve, 200));
		}
		assert.ok(version, "Chrome 的调试端口没有就绪");
		const socket = new WebSocket(version.webSocketDebuggerUrl);
		await new Promise<void>((resolve, reject) => {
			socket.addEventListener("open", () => resolve());
			socket.addEventListener("error", () => reject(new Error("无法连接 Chrome")));
		});
		const cdp = new Cdp(socket);
		const page = await cdp.send("Target.createTarget", { url: pageUrl });
		const attached = await cdp.send("Target.attachToTarget", { targetId: page.targetId, flatten: true });
		const session = attached.sessionId as string;
		await cdp.send("Page.enable", {}, session);
		const evaluated = await cdp.send(
			"Runtime.evaluate",
			{ expression: probe, awaitPromise: true, returnByValue: true },
			session,
		);
		if (shotPath) {
			const shot = await cdp.send("Page.captureScreenshot", { format: "png" }, session);
			await writeFile(shotPath, Buffer.from(shot.data as string, "base64"));
		}
		const value = (evaluated as { result?: { value?: unknown }; exceptionDetails?: unknown }).result?.value;
		if (typeof value !== "string")
			throw new Error(
				`页面脚本没有返回字符串：${JSON.stringify((evaluated as { exceptionDetails?: unknown }).exceptionDetails ?? evaluated)}`,
			);
		return value;
	} finally {
		child.kill();
	}
}

/** 页面脚本的公共前半段：等分支树、点某一行、等确认对话框，返回三个选项文案。 */
const dialogPreamble = `
	const until = async (test, label) => {
		const deadline = Date.now() + 30000;
		while (Date.now() < deadline) {
			if (test()) return;
			await new Promise((resolve) => setTimeout(resolve, 80));
		}
		throw new Error("等待超时：" + label);
	};
	const setValue = (area, text) => {
		Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(area, text);
		area.dispatchEvent(new Event("input", { bubbles: true }));
	};
	await until(() => document.querySelectorAll(".tree-row").length > 0, "分支树");
	const rows = [...document.querySelectorAll(".tree-row")];
	const row = rows.find((item) => (item.textContent || "").includes(TARGET));
	if (!row) throw new Error("没有找到目标行：" + TARGET);
	row.click();
	await until(() => document.querySelector(".summary-dialog"), "确认对话框");
	const options = [...document.querySelectorAll(".summary-option")].map((item) => (item.textContent || "").trim());
	const pick = (label) => {
		const hit = [...document.querySelectorAll(".summary-option")].find((item) => (item.textContent || "").trim() === label);
		if (!hit) throw new Error("没有这个选项：" + label);
		hit.click();
	};
	const blocks = () => document.querySelectorAll(".summary-block").length;
`;

/** 场景一：选「总结」→ 回到聊天页 → 待总结状态行 → 总结消息。 */
const summarizeProbe = `(async () => {
	const TARGET = "消息 A";
	${dialogPreamble}
	pick("总结");
	await until(() => location.pathname.indexOf("/tree") < 0, "回到聊天页");
	await until(() => document.querySelector(".summary-pending"), "待总结状态行");
	const pendingText = (document.querySelector(".summary-pending")?.textContent || "").trim();
	await until(() => !document.querySelector(".summary-pending"), "状态行撤掉");
	await until(() => blocks() > 0, "分支总结消息");
	await new Promise((resolve) => setTimeout(resolve, 200));
	const all = [...document.querySelectorAll(".summary-block")];
	const block = all[all.length - 1];
	return JSON.stringify({
		options,
		pendingSeen: true,
		pendingText,
		path: location.pathname,
		search: location.search,
		title: (block?.querySelector(".summary-title")?.textContent || ""),
		body: (block?.textContent || "").replace("分支总结", "").trim().slice(0, 80),
		blocks: blocks(),
		pendingLeft: !!document.querySelector(".summary-pending"),
	});
})()`;

/** 场景二：选「用自定义提示词总结」→ 输入框标记块 → 输入提示词回车 → 待总结状态行 → 总结消息。 */
const customProbe = `(async () => {
	const TARGET = "消息 B";
	${dialogPreamble}
	pick("用自定义提示词总结");
	await until(() => location.pathname.indexOf("/tree") < 0, "回到聊天页");
	await until(() => document.querySelector(".composer-command"), "提示词标记块");
	const tag = (document.querySelector(".composer-command")?.textContent || "").trim();
	const pendingBefore = !!document.querySelector(".summary-pending");
	// 等对话流渲染完再数块：否则基线是 0，后面的增量会多算。
	await until(() => document.querySelector(".turn") || document.querySelector(".assistant-block"), "对话流");
	const onChat = blocks();
	const area = document.querySelector("textarea.composer-input");
	setValue(area, "保留报错原文与文件路径");
	await new Promise((resolve) => setTimeout(resolve, 100));
	area.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
	await until(() => document.querySelector(".summary-pending"), "待总结状态行");
	const pendingSeen = true;
	await until(() => !document.querySelector(".summary-pending"), "状态行撤掉");
	await until(() => !document.querySelector(".composer-command"), "标记块撤掉");
	await until(() => blocks() > onChat, "分支总结消息");
	await new Promise((resolve) => setTimeout(resolve, 200));
	return JSON.stringify({
		tag,
		pendingBefore,
		pendingSeen,
		tagLeft: !!document.querySelector(".composer-command"),
		draft: (document.querySelector("textarea.composer-input")?.value || ""),
		blocks: blocks(),
		grew: blocks() - onChat,
		path: location.pathname,
		search: location.search,
	});
})()`;

/** 场景三：选「总结」后点状态行右端的叉，总结中止，位置不变。 */
const cancelProbe = `(async () => {
	const TARGET = "消息 C";
	${dialogPreamble}
	pick("总结");
	await until(() => location.pathname.indexOf("/tree") < 0, "回到聊天页");
	await until(() => document.querySelector(".summary-pending"), "待总结状态行");
	// 等请求真的发出去：状态行是本地先亮的，立刻点叉可能早于导航请求到达服务器。
	await new Promise((resolve) => setTimeout(resolve, 400));
	const onChat = blocks();
	document.querySelector(".summary-cancel").click();
	await until(() => !document.querySelector(".summary-pending"), "状态行撤掉");
	const note = (document.querySelector(".connection-banner")?.textContent || "").trim();
	await new Promise((resolve) => setTimeout(resolve, 1500));
	return JSON.stringify({
		pendingLeft: !!document.querySelector(".summary-pending"),
		note,
		blocks: blocks(),
		grew: blocks() - onChat,
		path: location.pathname,
		search: location.search,
	});
})()`;

/** 场景三：选「用自定义提示词总结」后停在输入框标记块的样子（截图用，不提交）。 */
const tagProbe = `(async () => {
	const TARGET = "消息 C";
	${dialogPreamble}
	pick("用自定义提示词总结");
	await until(() => location.pathname.indexOf("/tree") < 0, "回到聊天页");
	await until(() => document.querySelector(".composer-command"), "提示词标记块");
	await new Promise((resolve) => setTimeout(resolve, 200));
	return JSON.stringify({
		tag: (document.querySelector(".composer-command")?.textContent || "").trim(),
		icon: !!document.querySelector(".composer-command svg"),
		search: location.search,
	});
})()`;

/** 场景四：点标记块的叉 —— 放弃本次总结，输入框与地址都恢复，不产生总结。 */
const dropTagProbe = `(async () => {
	const TARGET = "消息 C";
	${dialogPreamble}
	pick("用自定义提示词总结");
	await until(() => document.querySelector(".composer-command"), "提示词标记块");
	const onChat = blocks();
	document.querySelector(".composer-command button").click();
	await until(() => !document.querySelector(".composer-command"), "标记块撤掉");
	await new Promise((resolve) => setTimeout(resolve, 800));
	return JSON.stringify({
		tagLeft: !!document.querySelector(".composer-command"),
		grew: blocks() - onChat,
		search: location.search,
	});
})()`;

/** 场景五：只看到生成中的样子就返回，截图抓这一瞬。 */
const pendingProbe = `(async () => {
	const TARGET = "消息 D";
	${dialogPreamble}
	pick("总结");
	await until(() => location.pathname.indexOf("/tree") < 0, "回到聊天页");
	await until(() => document.querySelector(".summary-pending"), "待总结状态行");
	return JSON.stringify({
		seen: true,
		text: (document.querySelector(".summary-pending")?.textContent || "").trim(),
		cancel: !!document.querySelector(".summary-pending .summary-cancel"),
		blocks: blocks(),
	});
})()`;

test("分支总结：总结、自定义提示词与取消", async () => {
	const home = await mkdtemp(join(rootDir, "cst-web-summary-e2e-"));
	const evidence = join(rootDir, "branch-summary");
	await mkdir(evidence, { recursive: true });
	let answers = 0;
	const model = createServer(async (request, response) => {
		let raw = "";
		for await (const chunk of request) raw += String(chunk);
		const body = `回答 ${++answers}`;
		if (raw.includes("<conversation>")) await new Promise((resolve) => setTimeout(resolve, SUMMARY_DELAY_MS));
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.write(
			`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: body }, finish_reason: null }] })}\n\n`,
		);
		response.end(
			`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	const modelPort = (model.address() as AddressInfo).port;
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "mock", defaultTools: ["read"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					models: [
						{ id: "mock", name: "mock", reasoning: false, input: ["text"], contextWindow: 10000, maxTokens: 300 },
					],
				},
			},
		}),
	);
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const api = createWebApi(pool, home, port);
	const server = createWebServer(STATIC_DIR, port, () => ({ sessions: pool.snapshot(), stage: "test" }), api);
	try {
		await listenWebServer(server, port);
		const call = (path: string, method: string, input?: unknown) =>
			fetch(`${origin}${path}`, {
				method,
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": randomUUID(),
				},
				body: input === undefined ? undefined : JSON.stringify(input),
			});
		assert.equal((await call("/api/auth/probe/api-key", "PUT", { key: "mock-secret" })).status, 200, "登录失败");
		const session = (await (await call("/api/sessions", "POST", {})).json()) as { id: string };
		const snapshot = async () =>
			(await (await fetch(`${origin}/api/sessions/${session.id}`)).json()) as {
				messages: { role: string; summary?: string; stopReason?: string }[];
			};
		// 一轮结束的标志是末尾出现带终结原因的助手消息；消息条数会因为分支切换而减少，不能拿它当计数。
		const waitAnswer = async () => {
			for (let attempt = 0; attempt < 300; attempt++) {
				const last = (await snapshot()).messages.at(-1);
				if (last?.role === "assistant" && last.stopReason && last.stopReason !== "toolUse") return;
				await new Promise((resolve) => setTimeout(resolve, 50));
			}
			throw new Error("模型回复没有到达");
		};
		const send = async (text: string) => {
			const response = await call(`/api/sessions/${session.id}/messages`, "POST", {
				id: randomUUID(),
				text,
				delivery: "queue",
			});
			assert.equal(response.status, 202, `提交“${text}”失败`);
			await waitAnswer();
		};
		// 主干：开场 → 回答 1 → 消息 A → 回答 2。
		await send("开场");
		await send("消息 A");
		// 场景一：回到「消息 A」做总结，待总结的是「消息 A + 回答 2」。
		const scenarioOne = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, summarizeProbe, join(evidence, "summarize.png")),
		) as Record<string, unknown>;
		const afterOne = await snapshot();
		// 场景二：新铺一条分支，用自定义提示词总结。
		await send("消息 B");
		const scenarioTwo = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, customProbe, join(evidence, "custom.png")),
		) as Record<string, unknown>;
		const afterTwo = await snapshot();
		// 场景三与四：同一段分上，先看标记块的样子，再点叉放弃。
		await send("消息 C");
		const beforeDrop = await snapshot();
		const scenarioThree = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, tagProbe, join(evidence, "tag.png")),
		) as Record<string, unknown>;
		const scenarioFour = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, dropTagProbe, join(evidence, "drop-tag.png")),
		) as Record<string, unknown>;
		const afterDrop = await snapshot();
		// 场景五：再看一眼生成中的样子，截图后让它跑完。
		await send("消息 D");
		const scenarioFive = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, pendingProbe, join(evidence, "pending.png")),
		) as Record<string, unknown>;
		let finished = false;
		for (let attempt = 0; attempt < 200 && !finished; attempt++) {
			finished = (await snapshot()).messages.filter((message) => message.role === "branchSummary").length >= 3;
			if (!finished) await new Promise((resolve) => setTimeout(resolve, 50));
		}
		const afterFive = await snapshot();
		// 场景六：再铺一条分支，生成中点取消。
		await send("消息 E");
		const beforeCancel = await snapshot();
		const scenarioSix = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, cancelProbe, join(evidence, "cancel.png")),
		) as Record<string, unknown>;
		const afterCancel = await snapshot();

		const summaries = afterFive.messages.filter((message) => message.role === "branchSummary");
		const report = [
			`# 分支总结端到端 · ${session.id}`,
			"",
			"| 场景 | 结果 |",
			"|---|---|",
			`| 确认对话框三选项 | ${JSON.stringify(scenarioOne.options)} |`,
			`| 总结：待总结状态行 | ${JSON.stringify({ seen: scenarioOne.pendingSeen, text: scenarioOne.pendingText })} |`,
			`| 总结：回到聊天页并清掉参数 | ${JSON.stringify({ path: scenarioOne.path, search: scenarioOne.search })} |`,
			`| 总结：消息块 | ${JSON.stringify({ title: scenarioOne.title, body: scenarioOne.body, blocks: scenarioOne.blocks, pendingLeft: scenarioOne.pendingLeft })} |`,
			`| 自定义：输入框标记块 | ${JSON.stringify({ tag: scenarioTwo.tag, pendingBefore: scenarioTwo.pendingBefore, tagLeft: scenarioTwo.tagLeft, draft: scenarioTwo.draft })} |`,
			`| 自定义：待总结与消息块 | ${JSON.stringify({ pendingSeen: scenarioTwo.pendingSeen, blocks: scenarioTwo.blocks })} |`,
			`| 标记块样子 | ${JSON.stringify(scenarioThree)} |`,
			`| 标记块去掉 | ${JSON.stringify(scenarioFour)} |`,
			`| 生成中状态行 | ${JSON.stringify(scenarioFive)} |`,
			`| 取消：状态行撤掉 | ${JSON.stringify({ pendingLeft: scenarioSix.pendingLeft, note: scenarioSix.note })} |`,
			`| 取消：不产生总结 | ${JSON.stringify({ grew: scenarioSix.grew, messagesBefore: beforeCancel.messages.length, messagesAfter: afterCancel.messages.length })} |`,
			`| 总结条数与正文 | ${JSON.stringify(summaries.map((message) => message.summary))} |`,
			"",
		].join("\n");
		await writeFile(join(evidence, "branch-summary.md"), report);

		assert.deepEqual(scenarioOne.options, ["不总结", "总结", "用自定义提示词总结"], "确认对话框缺选项");
		assert.equal(scenarioOne.pendingSeen, true, "没有出现待总结状态行");
		assert.equal(scenarioOne.title, "分支已总结", "总结消息缺少标题");
		assert.equal(scenarioOne.blocks, 1, "总结消息数量不对");
		assert.equal(scenarioOne.pendingLeft, false, "总结结束后状态行没有撤掉");
		assert.equal(scenarioOne.path, `/s/${session.id}`, "没有回到聊天页");
		assert.equal(scenarioOne.search, "", "地址参数没有清掉");
		assert.ok(
			afterOne.messages.some((message) => message.role === "branchSummary"),
			"会话里没有分支总结条目",
		);

		assert.equal(scenarioTwo.tag, "自定义总结提示词", "输入框标记块文案不对");
		assert.equal(scenarioTwo.pendingBefore, false, "输入提示词前就开始了总结");
		assert.equal(scenarioTwo.pendingSeen, true, "回车后没有开始总结");
		assert.equal(scenarioTwo.tagLeft, false, "开始总结后标记块没有撤掉");
		assert.equal(scenarioTwo.draft, "", "开始总结后输入框没有清空");
		assert.equal(scenarioTwo.grew, 1, "第二条总结消息没有出现");
		assert.equal(scenarioTwo.blocks, 2, "正文里的总结消息数量不对");
		assert.equal(
			afterTwo.messages.filter((message) => message.role === "branchSummary").length,
			2,
			"第二条总结没有落到会话里",
		);

		assert.equal(scenarioThree.tag, "自定义总结提示词", "标记块文案不对");
		assert.equal(scenarioThree.icon, true, "标记块缺少图标");
		assert.equal(scenarioFour.tagLeft, false, "点叉后标记块还在");
		assert.equal(scenarioFour.grew, 0, "点叉却产生了总结消息");
		assert.equal(afterDrop.messages.length, beforeDrop.messages.length, "点叉却改动了会话");
		assert.equal(scenarioFive.seen, true, "始终没有看到待总结状态行");
		assert.equal(scenarioFive.cancel, true, "状态行右端缺少取消按钮");
		assert.ok(
			afterFive.messages.filter((message) => message.role === "branchSummary").length >= 3,
			"后台的总结没有落地",
		);

		assert.equal(scenarioSix.pendingLeft, false, "取消后状态行还在");
		assert.equal(scenarioSix.grew, 0, "取消却产生了总结消息");
		assert.equal(afterCancel.messages.length, beforeCancel.messages.length, "取消却改动了会话");
		assert.equal(
			afterCancel.messages.filter((message) => message.role === "branchSummary").length,
			afterFive.messages.filter((message) => message.role === "branchSummary").length,
		);
	} finally {
		server.close();
		model.close();
		await pool.close();
	}
});

test("生成中提交树导航：先停生成再导航，不再 409", async () => {
	const home = await mkdtemp(join(rootDir, "cst-web-tree-abort-"));
	const held = new Set<import("node:http").ServerResponse>();
	const model = createServer(async (request, response) => {
		let raw = "";
		for await (const chunk of request) raw += String(chunk);
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		if (raw.includes("慢速问题")) {
			// 不 end：保持生成中，直到导航把它 abort 掉。
			held.add(response);
			request.on("close", () => held.delete(response));
			response.write(
				`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "……" }, finish_reason: null }] })}\n\n`,
			);
			return;
		}
		response.write(
			`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "回答" }, finish_reason: null }] })}\n\n`,
		);
		response.end(
			`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	const modelPort = (model.address() as AddressInfo).port;
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "mock", defaultTools: ["read"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					models: [
						{ id: "mock", name: "mock", reasoning: false, input: ["text"], contextWindow: 10000, maxTokens: 300 },
					],
				},
			},
		}),
	);
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const api = createWebApi(pool, home, port);
	const server = createWebServer(STATIC_DIR, port, () => ({ sessions: pool.snapshot(), stage: "test" }), api);
	try {
		await listenWebServer(server, port);
		const call = (path: string, method: string, input?: unknown) =>
			fetch(`${origin}${path}`, {
				method,
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": randomUUID(),
				},
				body: input === undefined ? undefined : JSON.stringify(input),
			});
		const session = (await (await call("/api/sessions", "POST", {})).json()) as { id: string };
		const detail = async () =>
			(await (await fetch(`${origin}/api/sessions/${session.id}`)).json()) as {
				running: boolean;
				messages: { role: string; stopReason?: string }[];
			};
		// 等待条件用末尾的终结 assistant，不用 running：投递尚未开始时 running 也是假，会提前退。
		const waitAnswer = async () => {
			for (let attempt = 0; attempt < 300; attempt++) {
				const last = (await detail()).messages.at(-1);
				if (last?.role === "assistant" && last.stopReason && last.stopReason !== "toolUse") return;
				await new Promise((resolve) => setTimeout(resolve, 50));
			}
			throw new Error("模型回复没有落地");
		};
		const send = async (text: string) => {
			const response = await call(`/api/sessions/${session.id}/messages`, "POST", {
				id: randomUUID(),
				text,
				delivery: "queue",
			});
			assert.equal(response.status, 202);
			await waitAnswer();
		};
		await send("开场");
		// 第二条消息挂在慢速流上：会话进入生成中。
		assert.equal(
			(
				await call(`/api/sessions/${session.id}/messages`, "POST", {
					id: randomUUID(),
					text: "慢速问题",
					delivery: "queue",
				})
			).status,
			202,
		);
		let streaming = false;
		for (let attempt = 0; attempt < 100 && !streaming; attempt++) {
			streaming = (await detail()).running;
			if (!streaming) await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.equal(streaming, true, "慢速请求没有进入生成中");
		// 生成中直接导航（旧行为是 409）。
		const tree = (await (await fetch(`${origin}/api/sessions/${session.id}/tree`)).json()) as {
			tree: TreeNode[];
			leafId: string | null;
		};
		type TreeNode = { entry: { id: string; type: string; message?: { role: string } }; children: TreeNode[] };
		const flat: TreeNode[] = [];
		const walk = (nodes: TreeNode[]) => {
			for (const node of nodes) {
				flat.push(node);
				walk(node.children);
			}
		};
		walk(tree.tree);
		const target = flat.find((node) => node.entry.type === "message" && node.entry.message?.role === "assistant")!;
		assert.ok(target, "找不到导航目标");
		const navigated = await call(`/api/sessions/${session.id}/tree/navigate`, "POST", { entryId: target.entry.id });
		assert.equal(navigated.status, 200, "生成中导航被拒绝");
		const result = (await navigated.json()) as { cancelled: boolean };
		assert.equal(result.cancelled, false);
		// 导航后生成停止，叶指针落在目标上。
		await waitAnswer();
		let settled = false;
		for (let attempt = 0; attempt < 100 && !settled; attempt++) {
			settled = !(await detail()).running;
			if (!settled) await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.equal(settled, true, "导航后生成没有停止");
		const after = (await (await fetch(`${origin}/api/sessions/${session.id}/tree`)).json()) as {
			leafId: string | null;
		};
		assert.equal(after.leafId, target.entry.id, "叶指针没有落在导航目标上");
	} finally {
		for (const response of held) response.destroy();
		server.close();
		model.close();
		await pool.close();
	}
});
