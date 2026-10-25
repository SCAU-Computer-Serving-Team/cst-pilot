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

const sample = [
	"中文正文保持既有字体，检查中文、English words、数字123以及长句换行。".repeat(8),
	"第二段正文用于检查段落之间的实际距离。",
	...[1, 2, 3, 4, 5, 6].flatMap((level) => [`${"#".repeat(level)} ${level}级标题`, `${level}级标题后面的正文。`]),
	"## 连续标题",
	"### 连续下级标题",
	"连续标题后面的正文。",
	"列表说明：",
	"- 第一项包含 `tool_name` 与中文说明。\n- 第二项包含嵌套列表。\n  - 内层第一项\n  - 内层第二项",
	"列表结束后接正文。",
	"1. 列表内第一段。\n\n   列表内第二段。\n\n2. 下一条列表项。",
	"> 引用第一段。\n>\n> 引用第二段。",
	"引用结束后接正文。",
	"```typescript\nconst path = 'C:/very/long/path/' + '字段'.repeat(60);\nconsole.log(path);\n```",
	"代码结束后接正文。",
	"| 字段 | 当前结果 | 说明 | 状态 |\n| --- | --- | --- | --- |\n| 名称 | 中文 English 123 | 宽表格窄幅折行 | 正常 |",
	"表格结束后接正文。",
	"---",
	"排版尾段标记，流式和完成态共用相邻规则。",
].join("\n\n");
const metrics = () => `(() => {
 const root=document.querySelector('.markdown'),rect=root.getBoundingClientRect();
 return {text:root.textContent,headings:[...root.querySelectorAll('h1,h2,h3,h4,h5,h6')].map(e=>e.tagName),
 blocks:['ul','ol','blockquote','pre code','table'].map(selector=>!!root.querySelector(selector)),
 width:rect.width,tableWidth:root.querySelector('.markdown-table').getBoundingClientRect().width,
 withinViewport:rect.left>=0&&rect.right<=innerWidth+1,pageOverflow:document.documentElement.scrollWidth>innerWidth};
})()`;
interface Metrics {
	text: string;
	headings: string[];
	blocks: boolean[];
	width: number;
	tableWidth: number;
	withinViewport: boolean;
	pageOverflow: boolean;
}
function verify(value: Metrics) {
	for (const tag of ["H1", "H2", "H3", "H4", "H5", "H6"]) assert.ok(value.headings.includes(tag), tag);
	assert.ok(value.blocks.every(Boolean), "列表、引用、代码与表格正常渲染");
	assert.ok(value.width > 0 && value.withinViewport, "正文在可用视口内");
	assert.ok(value.tableWidth <= value.width + 1, "宽表格在正文内滚动");
	assert.equal(value.pageOverflow, false, "页面不产生横向滚动");
}

test("Markdown结构渲染、流式完成、浅深主题、窄幅与放大", { timeout: 60000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "markdown-typeset-")),
		home = join(root, "home");
	await mkdir(home, { recursive: true });
	let release = () => {};
	const model = createServer(async (req, res) => {
		for await (const _ of req) {
		}
		res.writeHead(200, { "Content-Type": "text/event-stream" });
		const send = (delta: unknown, finish: string | null = null) =>
			res.write(
				`data: ${JSON.stringify({ id: "fixture", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`,
			);
		send({ role: "assistant", content: sample });
		await new Promise<void>((resolve) => {
			release = resolve;
		});
		send({}, "stop");
		res.end("data: [DONE]\n\n");
	});
	const modelPort = await freePort();
	await new Promise<void>((r) => model.listen(modelPort, "127.0.0.1", r));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "fixture",
			defaultModel: "one",
			defaultTools: [],
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
							reasoning: false,
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
	});
	const slot = await pool.create(),
		port = await freePort(),
		api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "typography" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		// 排版快照固定为减少动态，独立于导航和侧栏过渡动画。
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.navigate(`http://127.0.0.1:${port}/s/${slot.id}`);
		await browser.fill(".composer-input", "排版测试");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			`document.querySelector('.markdown')?.textContent.includes(${JSON.stringify(sample.split("\n\n").at(-1))})`,
			"流式正文完整到达",
		);
		await browser.evaluate("document.fonts.ready.then(()=>true)");
		const live = await browser.evaluate<Metrics>(metrics());
		verify(live);
		release();
		await browser.until("!document.querySelector('.assistant-turn[data-live]')", "模型完成");
		const finished = await browser.evaluate<Metrics>(metrics());
		verify(finished);
		assert.equal(
			finished.text.replace(/\s+/g, " ").trim(),
			live.text.replace(/\s+/g, " ").trim(),
			"流式完成不丢失或重复正文",
		);
		await browser.evaluate("document.querySelector('.conversation').scrollTop=0");
		await browser.until("document.querySelector('.markdown').getBoundingClientRect().top>100", "正文顶部");
		await browser.screenshot(join(root, "desktop-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		verify(await browser.evaluate<Metrics>(metrics()));
		await browser.screenshot(join(root, "desktop-dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until(
			"(()=>{const r=document.querySelector('.markdown').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth+1})()",
			"窄幅布局结束",
		);
		await browser.evaluate("document.querySelector('.conversation').scrollTop=0");
		const narrow = await browser.evaluate<Metrics>(metrics());
		verify(narrow);
		assert.ok(narrow.tableWidth <= narrow.width);
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=390"), true);
		await browser.screenshot(join(root, "narrow-dark.png"));
		await browser.evaluate("document.documentElement.dataset.theme='light'");
		await browser.screenshot(join(root, "narrow-light.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1600,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.evaluate("document.documentElement.style.zoom='2'");
		assert.equal(
			await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"),
			true,
			"桌面200%放大不产生页面横向滚动",
		);
		await writeFile(join(root, "metrics.json"), JSON.stringify({ live, finished, narrow }, null, 2));
		console.log("TYPOGRAPHY_EVIDENCE", root);
	} finally {
		release();
		await browser?.close();
		await pool.close();
		await api.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		model.closeAllConnections();
		await new Promise<void>((r) => model.close(() => r()));
	}
});
