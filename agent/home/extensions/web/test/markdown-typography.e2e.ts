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
const metrics = (index: number) => `(() => {
	const root=document.querySelectorAll('.markdown')[${index}];
	const style=e=>{const s=getComputedStyle(e);return {size:parseFloat(s.fontSize),line:parseFloat(s.lineHeight),tracking:parseFloat(s.letterSpacing)||0,weight:s.fontWeight};};
	const gap=(a,b)=>+(b.getBoundingClientRect().top-a.getBoundingClientRect().bottom).toFixed(2);
	const children=Array.from(root.children),heads=children.filter(e=>/^H[1-6]$/.test(e.tagName));
	const list=root.querySelector('ul'),nested=list.querySelector('ul'),loose=root.querySelector('ol li'),quote=root.querySelector('blockquote');
	return {body:style(root),tags:children.map(e=>e.tagName),gaps:children.slice(1).map((e,i)=>gap(children[i],e)),
		heads:heads.map(e=>({tag:e.tagName,style:style(e),before:gap(e.previousElementSibling,e),after:gap(e,e.nextElementSibling)})),
		list:{before:gap(list.previousElementSibling,list),items:gap(list.children[0],list.children[1]),after:gap(list,list.nextElementSibling),nestedBefore:getComputedStyle(nested).marginTop,nestedAfter:getComputedStyle(nested).marginBottom},
		loose:gap(loose.children[0],loose.children[1]),quote:gap(quote.children[0],quote.children[1]),
		code:style(root.querySelector('pre code')),cell:style(root.querySelector('td')),tableWidth:root.querySelector('.markdown-table').getBoundingClientRect().width,
		height:+root.getBoundingClientRect().height.toFixed(2),width:root.getBoundingClientRect().width,
		firstMargin:getComputedStyle(children[0]).marginTop,lastMargin:getComputedStyle(children.at(-1)).marginBottom,
		control:style(document.querySelector('.turn-process-head'))};
})()`;
type Metrics = {
	body: { size: number; line: number; tracking: number };
	tags: string[];
	gaps: number[];
	heads: { tag: string; style: { size: number; line: number; tracking: number }; before: number; after: number }[];
	list: { before: number; items: number; after: number; nestedBefore: string; nestedAfter: string };
	loose: number;
	quote: number;
	code: { line: number; tracking: number };
	cell: { line: number };
	tableWidth: number;
	height: number;
	width: number;
	firstMargin: string;
	lastMargin: string;
	control: { line: number; tracking: number };
};
const near = (actual: number, expected: number) =>
	assert.ok(Math.abs(actual - expected) < 0.06, `${actual} != ${expected}`);
function verify(value: Metrics) {
	assert.equal(value.body.size, 16);
	assert.equal(value.body.line, 28);
	near(value.body.tracking, 0.32);
	assert.equal(value.gaps[0], 16);
	const before = [32, 24, 20, 16, 12, 12],
		after = [16, 12, 8, 8, 8, 4];
	for (let i = 0; i < 6; i++) {
		const head = value.heads[i]!;
		assert.equal(head.before, before[i]);
		assert.equal(head.after, after[i]);
		assert.equal(head.style.size, i < 2 ? 24 : 16);
		near(head.style.line, i < 2 ? 33.6 : 24);
		assert.equal(head.style.tracking, 0);
	}
	assert.equal(value.heads[6]!.after, 12);
	assert.equal(value.heads[7]!.before, 12);
	assert.deepEqual(value.list, { before: 8, items: 6, after: 16, nestedBefore: "4px", nestedAfter: "4px" });
	assert.equal(value.loose, 16);
	assert.equal(value.quote, 16);
	near(value.code.line, 20.8);
	assert.equal(value.code.tracking, 0);
	near(value.cell.line, 25.6);
	assert.deepEqual(value.gaps.slice(-2), [24, 24]);
	assert.equal(value.firstMargin, "0px");
	assert.equal(value.lastMargin, "0px");
	assert.equal(value.control.line, 24);
	assert.equal(value.control.tracking, 0);
}

test("Markdown排版：分级间距、流式一致、浅深主题、窄幅与放大", { timeout: 60000 }, async () => {
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
			"document.querySelector('.markdown')?.textContent.includes('排版尾段标记')",
			"流式正文完整到达",
		);
		await browser.evaluate("document.fonts.ready.then(()=>true)");
		const live = await browser.evaluate<Metrics>(metrics(0));
		verify(live);
		release();
		await browser.until("!document.querySelector('.assistant-turn[data-live]')", "模型完成");
		const finished = await browser.evaluate<Metrics>(metrics(0));
		assert.deepEqual(finished, live, "流式分段边界和高亮不改变排版");
		await browser.evaluate("document.querySelector('.conversation').scrollTop=0");
		await browser.until("document.querySelector('.markdown').getBoundingClientRect().top>100", "正文顶部");
		await browser.screenshot(join(root, "desktop-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		verify(await browser.evaluate<Metrics>(metrics(0)));
		await browser.screenshot(join(root, "desktop-dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until(
			"Math.abs(document.querySelector('.markdown').getBoundingClientRect().left-16)<1",
			"窄幅布局结束",
		);
		await browser.evaluate("document.querySelector('.conversation').scrollTop=0");
		const narrow = await browser.evaluate<Metrics>(metrics(0));
		verify(narrow);
		near(narrow.width, 358);
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
