// Chrome 端到端对照：抓一个真实的分支会话，把同一份树分别交给 pi TUI 的 TreeList 与
// Web 分支树页面渲染，逐行比较缩进、连接符列、竖线列与行文案。
// 运行：node --test home/extensions/web/test/tree-parity.e2e.ts
// 前置：前端已构建并发布到 ../static（cd src/web/frontend && npm run build）。
// 证据：E:/tmp/<日期>/tree-parity/ 下的截图与对照报告。
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

const PI_DIST = new URL(
	"../../../../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/",
	import.meta.url,
);
const STATIC_DIR = fileURLToPath(new URL("../static/", import.meta.url));
const rootDir = await testRoot();

type TreeEntryShape = {
	id: string;
	parentId: string | null;
	type: string;
	message?: { role?: string; content?: unknown };
};
type TreeNodeShape = { entry: TreeEntryShape; children: TreeNodeShape[] };

/** 一行在“层级/标记”上的形态，TUI 与页面各自算出一份后逐行比对。 */
type RowShape = { indent: number; square: number | null; lines: number[]; blanks: number[] };

async function freePort(): Promise<number> {
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const port = (probe.address() as AddressInfo).port;
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	return port;
}

/** 读 pi TUI 自己算出的排版。TUI 是唯一真源。 */
async function tuiRows(tree: TreeNodeShape[], leafId: string | null) {
	const { TreeSelectorComponent } = await import(new URL("components/tree-selector.js", PI_DIST).href);
	const { initTheme } = await import(new URL("theme/theme.js", PI_DIST).href);
	initTheme("dark");
	const component = new TreeSelectorComponent(
		tree as never,
		leafId as never,
		80,
		() => {},
		() => {},
		async () => {},
	);
	const list = (
		component as {
			treeList: { multipleRoots: boolean; filteredNodes: any[]; isFoldable: (entryId: string) => boolean };
		}
	).treeList;
	const rows: RowShape[] = list.filteredNodes.map((flat) => {
		const indent = list.multipleRoots ? Math.max(0, flat.indent - 1) : flat.indent;
		const shown = flat.showConnector && !flat.isVirtualRootChild;
		return {
			indent,
			square: shown ? Math.max(0, indent - 1) : null,
			lines: flat.gutters
				.filter((gutter: { show: boolean }) => gutter.show)
				.map((gutter: { position: number }) => gutter.position),
			blanks: flat.gutters
				.filter((gutter: { show: boolean }) => !gutter.show)
				.map((gutter: { position: number }) => gutter.position),
		};
	});
	const ids = list.filteredNodes.map((flat) => flat.node.entry.id as string);
	const foldable = ids.map((id) => list.isFoldable(id));
	const text = component.render(150).map((line: string) => line.replace(/\u001b\[[0-9;]*m/g, ""));
	return { rows, ids, foldable, text };
}

/** 极简 CDP 客户端：只用 Target / Runtime / Page 三组命令。 */
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

/** 页面脚本：等分支树渲染出来，读回每行的槽位数与标记位置。 */
const TREE_PROBE = `(async () => {
	const deadline = Date.now() + 20000;
	while (Date.now() < deadline) {
		const rows = [...document.querySelectorAll(".tree-row")];
		if (rows.length) {
			return JSON.stringify({ rows: rows.map((row) => {
				const slots = [...row.querySelectorAll(".tree-slot")];
				return {
				iconLeft: row.querySelector('.tree-icon').getBoundingClientRect().left,
				slots: slots.length,
					square: slots.findIndex((slot) => !!slot.querySelector(".tree-square")),
					lines: slots.map((slot, index) => (slot.querySelector(".tree-line") ? index : -1)).filter((index) => index >= 0),
					fold: !!row.querySelector(".tree-fold"),
					text: (row.textContent || "").replace("当前位置", "").trim(),
				};
			}) });
		}
		await new Promise((resolve) => setTimeout(resolve, 200));
	}
	return JSON.stringify({ rows: [], html: document.body.innerHTML.slice(0, 400) });
})()`;

/** 页面脚本：在聊天工作台输入 /tree，看是否选中即跳转、且不留命令标记。 */
const COMPOSER_PROBE = `(async () => {
	const until = async (test, label) => {
		const deadline = Date.now() + 20000;
		while (Date.now() < deadline) {
			if (test()) return;
			await new Promise((resolve) => setTimeout(resolve, 150));
		}
		throw new Error("等待超时：" + label);
	};
	await until(() => document.querySelector(".composer-input"), "消息编辑器");
	const area = document.querySelector(".composer-input");
	area.textContent = "/tree";
	area.dispatchEvent(new InputEvent("input", { bubbles: true }));
	await until(() => document.querySelectorAll(".command-suggestions button").length > 0, "命令补全");
	const labels = [...document.querySelectorAll(".command-suggestions button")].map((button) => button.textContent || "");
	area.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
	await until(() => location.pathname.endsWith("/tree"), "跳转到分支树");
	await new Promise((resolve) => setTimeout(resolve, 300));
	return JSON.stringify({ path: location.pathname, labels, chip: !!document.querySelector(".composer-command") });
})()`;

/** 打开页面跑一段脚本，返回脚本给出的 JSON 字符串。 */
async function readPage(pageUrl: string, probe: string, shotPath?: string) {
	const cdpPort = await freePort();
	const profile = await mkdtemp(join(rootDir, "cst-web-tree-chrome-"));
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
		const page = await cdp.send("Target.createTarget", { url: "about:blank" });
		const attached = await cdp.send("Target.attachToTarget", { targetId: page.targetId, flatten: true });
		const session = attached.sessionId as string;
		await cdp.send("Page.enable", {}, session);
		await cdp.send("Page.navigate", { url: pageUrl }, session);
		let ready = false;
		for (let attempt = 0; attempt < 150 && !ready; attempt++) {
			try {
				const state = await cdp.send(
					"Runtime.evaluate",
					{
						expression: `location.href === ${JSON.stringify(pageUrl)} && document.readyState === "complete"`,
						returnByValue: true,
					},
					session,
				);
				ready = state.result?.value === true;
			} catch (error) {
				if (!(error instanceof Error) || !/Execution context was destroyed|Cannot find context/.test(error.message))
					throw error;
			}
			if (!ready) await new Promise((resolve) => setTimeout(resolve, 100));
		}
		assert.ok(ready, "页面导航未完成，不在即将销毁的 about:blank 上执行探针");
		const evaluated = await cdp.send(
			"Runtime.evaluate",
			{ expression: probe, awaitPromise: true, returnByValue: true },
			session,
		);
		if (shotPath) {
			const shot = await cdp.send("Page.captureScreenshot", { format: "png" }, session);
			await writeFile(shotPath, Buffer.from(shot.data as string, "base64"));
		}
		return (evaluated as { result: { value: string } }).result.value;
	} finally {
		child.kill();
	}
}

function textOf(entry: TreeEntryShape): string {
	const content = entry.message?.content;
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter((block) => (block as { type?: string }).type === "text")
		.map((block) => (block as { text?: string }).text ?? "")
		.join("");
}

function findEntry(roots: TreeNodeShape[], predicate: (entry: TreeEntryShape) => boolean): TreeEntryShape | undefined {
	for (const node of roots) {
		if (predicate(node.entry)) return node.entry;
		const hit = findEntry(node.children, predicate);
		if (hit) return hit;
	}
	return undefined;
}

function entryText(roots: TreeNodeShape[], id: string): string {
	return textOf(findEntry(roots, (entry) => entry.id === id) ?? { id, parentId: null, type: "unknown" });
}

test("Web 分支树与 TUI 的树逐行一致", async () => {
	const home = await mkdtemp(join(rootDir, "cst-web-tree-e2e-"));
	const evidence = join(rootDir, "tree-parity");
	await mkdir(evidence, { recursive: true });
	let answers = 0;
	const model = createServer(async (request, response) => {
		for await (const _chunk of request) void _chunk;
		const body = `回答 ${++answers}`;
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
		const login = await call("/api/auth/probe/api-key", "PUT", { key: "mock-secret" });
		assert.equal(login.status, 200, "登录失败");
		const session = (await (await call("/api/sessions", "POST", {})).json()) as { id: string };
		const snapshot = async () => (await (await fetch(`${origin}/api/sessions/${session.id}`)).json()) as unknown;
		const replies = async (text: string) => {
			for (let attempt = 0; attempt < 200; attempt++) {
				const state = (await snapshot()) as { running: boolean; messages: { role: string; content?: unknown }[] };
				const user = state.messages.findIndex(
					(message) =>
						message.role === "user" &&
						JSON.stringify(message.content).includes(JSON.stringify(text).slice(1, -1)),
				);
				if (
					!state.running &&
					user >= 0 &&
					state.messages.slice(user + 1).some((message) => message.role === "assistant")
				)
					return;
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
			await replies(text);
		};
		// 先铺一条主干，再回到同一个父条目下分三次岔开，最后在分支 A 里再分一次。
		await send("开场");
		await send("消息 A");
		const first = (await (await fetch(`${origin}/api/sessions/${session.id}/tree`)).json()) as {
			tree: TreeNodeShape[];
		};
		const messageA = findEntry(first.tree, (entry) => textOf(entry) === "消息 A");
		assert.ok(messageA, "没有找到“消息 A”的条目");
		assert.equal(
			(await call(`/api/sessions/${session.id}/tree/navigate`, "POST", { entryId: messageA.id })).status,
			200,
			"导航失败",
		);
		await send("消息 B");
		const second = (await (await fetch(`${origin}/api/sessions/${session.id}/tree`)).json()) as {
			tree: TreeNodeShape[];
		};
		const messageB = findEntry(second.tree, (entry) => textOf(entry) === "消息 B");
		assert.ok(messageB);
		await call(`/api/sessions/${session.id}/tree/navigate`, "POST", { entryId: messageB.id });
		await send("消息 C");
		const third = (await (await fetch(`${origin}/api/sessions/${session.id}/tree`)).json()) as {
			tree: TreeNodeShape[];
		};
		const replyA = findEntry(third.tree, (entry) => textOf(entry) === "回答 2");
		assert.ok(replyA, "没有找到分支 A 的回复");
		await call(`/api/sessions/${session.id}/tree/navigate`, "POST", { entryId: replyA.id });
		await send("分支 A1");
		const final = (await (await fetch(`${origin}/api/sessions/${session.id}/tree`)).json()) as {
			tree: TreeNodeShape[];
			leafId: string;
		};

		const tui = await tuiRows(final.tree, final.leafId);
		const treePage = JSON.parse(
			await readPage(`${origin}/s/${session.id}/tree`, TREE_PROBE, join(evidence, "tree.png")),
		) as {
			rows: { slots: number; square: number; lines: number[]; fold: boolean; text: string; iconLeft: number }[];
		};
		const page = { page: treePage };
		const lines: string[] = [
			`# 分支树对照 · ${session.id}`,
			"",
			`叶子条目：${final.leafId}`,
			`TUI 行数：${tui.rows.length}，页面行数：${page.page.rows.length}`,
			"",
			"## TUI 渲染",
			"",
			"```",
			...tui.text.filter((line: string) => line.trim()),
			"```",
			"",
			"## 逐行比较",
			"",
			"| # | 条目 | TUI 缩进 / 方块 / 竖线 / 可折叠 | 页面 缩进 / 方块 / 竖线 / 可折叠 | 条目正文 | 页面行文案 | 判定 |",
			"|---|---|---|---|---|---|---|",
		];
		let failed = false;
		let alignedBranchSegments = 0;
		assert.equal(
			page.page.rows.length,
			tui.rows.length,
			`行数不一致：TUI ${tui.rows.length}，页面 ${page.page.rows.length}`,
		);
		for (let index = 0; index < tui.rows.length; index++) {
			const expected = tui.rows[index];
			const actual = page.page.rows[index];
			const body = entryText(final.tree, tui.ids[index]);
			const reasons: string[] = [];
			const node = findEntry(final.tree, (entry) => entry.id === tui.ids[index]);
			if (
				expected.square !== null &&
				node &&
				page.page.rows[index + 1] &&
				expected.indent < tui.rows[index + 1].indent &&
				tui.rows[index + 1].square === null
			) {
				alignedBranchSegments++;
				assert.ok(
					Math.abs(actual.iconLeft - page.page.rows[index + 1].iconLeft) < 1,
					"分支起始行与后续单链内容图标对齐",
				);
			}
			if (actual.slots !== expected.indent) reasons.push(`缩进 ${actual.slots}≠${expected.indent}`);
			if ((actual.square === -1 ? null : actual.square) !== expected.square)
				reasons.push(`方块 ${actual.square}≠${expected.square ?? "无"}`);
			if (JSON.stringify(actual.lines) !== JSON.stringify(expected.lines))
				reasons.push(`竖线 ${actual.lines.join(",")}≠${expected.lines.join(",")}`);
			if (actual.fold !== tui.foldable[index]) reasons.push(`可折叠 ${actual.fold}≠${tui.foldable[index]}`);
			if (body && !actual.text.includes(body)) reasons.push(`文案缺正文「${body}」`);
			if (reasons.length) failed = true;
			lines.push(
				`| ${index + 1} | ${tui.ids[index]} | ${expected.indent} / ${expected.square ?? "-"} / ${expected.lines.join(",") || "-"} / ${tui.foldable[index] ? "是" : "否"} | ${actual.slots} / ${actual.square === -1 ? "-" : actual.square} / ${actual.lines.join(",") || "-"} / ${actual.fold ? "是" : "否"} | ${body} | ${actual.text} | ${reasons.length ? `**${reasons.join("；")}**` : "一致"} |`,
			);
		}
		assert.ok(alignedBranchSegments > 0, "已实际覆盖分支起始行与单链内容的对齐");
		lines.push("", `分支内容列对齐：${alignedBranchSegments} 组通过。`);
		// 斜杠命令的入口：在聊天工作台输入 /tree，选中即跳转，不留命令标记。
		const jump = JSON.parse(await readPage(`${origin}/s/${session.id}`, COMPOSER_PROBE)) as {
			path: string;
			labels: string[];
			chip: boolean;
		};
		lines.push(
			"",
			"## 斜杠命令 `/tree`",
			"",
			`补全项：${jump.labels.join(" ")}`,
			`输入 /tree 并按回车后的地址：${jump.path}（期望 /s/${session.id}/tree）`,
			`命令标记残留：${jump.chip ? "有" : "无"}`,
		);
		if (jump.path !== `/s/${session.id}/tree` || jump.chip || !jump.labels.some((label) => label.includes("tree"))) {
			failed = true;
		}
		lines.push("", `截图：${join(evidence, "tree.png")}`, "", failed ? "**结论：不一致**" : "结论：一致");
		await writeFile(join(evidence, "tree-parity.md"), lines.join("\n"));
		assert.ok(!failed, `分支树与 TUI 不一致，报告见 ${join(evidence, "tree-parity.md")}`);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await new Promise<void>((resolve) => model.close(() => resolve()));
	}
});
