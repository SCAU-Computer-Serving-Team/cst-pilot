import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, utimes, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const usage = {
	input: 1,
	output: 1,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 2,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

test("用户链路：响应丢失重试、真实工具执行、刷新恢复、主题、登录与页面动效", { timeout: 90_000 }, async () => {
	const root = join(await testRoot(), "ui-regressions");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const fixture = join(home, "ui-read-fixture.txt");
	await writeFile(fixture, "READ_E2E_OK：由 Pi 内置 read 工具真实读取的文件。", "utf8");
	const model = createServer(async (request, response) => {
		const chunks: Buffer[] = [];
		for await (const chunk of request) chunks.push(chunk);
		const input = JSON.parse(Buffer.concat(chunks).toString("utf8")) as { messages: { role: string }[] };
		const hasToolResult = input.messages.some((message) => message.role === "tool");
		await new Promise((resolve) => setTimeout(resolve, 1800));
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		const delta = hasToolResult
			? {
					role: "assistant",
					reasoning_content: `${"这是用于检查思考正文排版的文本。\n".repeat(45)}`,
					content: `测试回答：检查后的正文。\n\n${"长摘要与当前位置标记应当互不遮挡。".repeat(20)}`,
				}
			: {
					role: "assistant",
					content: "检查前的正文。",
					tool_calls: [
						{
							index: 0,
							id: "call-e2e-read",
							type: "function",
							function: { name: "read", arguments: JSON.stringify({ path: fixture }) },
						},
					],
				};
		response.write(`data: ${JSON.stringify({ id: "e2e", choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`);
		response.end(
			`data: ${JSON.stringify({ id: "e2e", choices: [{ index: 0, delta: {}, finish_reason: hasToolResult ? "stop" : "tool_calls" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	const modelPort = (model.address() as AddressInfo).port;
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "probe",
			defaultModel: "one",
			enabledModels: ["probe/two"],
			theme: "light",
			defaultTools: ["read"],
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: ["one", "two"].map((id) => ({
						id,
						name: `测试模型 ${id}`,
						reasoning: false,
						input: ["text"],
						contextWindow: 10000,
						maxTokens: 100,
					})),
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
	const { modelRuntime } = await pool.getServices();
	for (const id of ["cstoa", "aaa-test-oauth"])
		modelRuntime.registerProvider(id, {
			name: id === "cstoa" ? "CSTOA" : "000 Test OAuth",
			baseUrl: `http://127.0.0.1:${modelPort}/v1`,
			api: "openai-completions",
			models: [],
			oauth: {
				name: id,
				login: async (callbacks) => {
					if (id === "cstoa")
						callbacks.onDeviceCode?.({
							userCode: "123456",
							verificationUri: `http://127.0.0.1:${modelPort}/authorize`,
							intervalSeconds: 1,
							expiresInSeconds: 600,
						});
					else callbacks.onAuth({ url: `http://127.0.0.1:${modelPort}/authorize`, instructions: "测试授权" });
					await callbacks.onManualCodeInput!();
					return { access: "test-access", refresh: "test-refresh", expires: Date.now() + 3600000 };
				},
				refreshToken: async (credentials) => credentials,
				getApiKey: (credentials) => credentials.access,
			},
		});
	const previous = await pool.create();
	const yesterday = Date.now() - 86400000;
	previous.session.sessionManager.appendMessage({
		role: "user",
		content: [{ type: "text", text: "昨日会话" }],
		timestamp: yesterday,
	});
	previous.session.sessionManager.appendMessage({
		role: "assistant",
		content: [{ type: "text", text: "昨日回答" }],
		api: "openai-completions",
		provider: "probe",
		model: "one",
		stopReason: "stop",
		usage,
		timestamp: yesterday,
	});
	assert.ok(previous.file);
	await utimes(previous.file, new Date(yesterday), new Date(yesterday));
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
	const evidence: Record<string, unknown> = {};
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/`);
		await browser.until(
			"document.querySelector('.sidebar-session')?.textContent === '昨日会话' && document.querySelector('.model-label')?.textContent.includes('two')",
			"首页配置与昨日记录",
		);
		assert.equal(
			await browser.evaluate("[...document.querySelectorAll('.sidebar-group h2')].some(e=>e.textContent==='今天')"),
			false,
		);
		await browser.screenshot(join(root, "home-desktop-light.png"));
		await browser.evaluate("document.querySelector('.composer-model').click()");
		await browser.until("!!document.querySelector('.model-popover [role=option]')", "模型菜单");
		const models = await browser.evaluate<string[]>(
			"[...document.querySelectorAll('.model-popover [role=option]')].map(e=>e.textContent.trim())",
		);
		assert.deepEqual(models, ["测试模型 two"]);
		assert.equal(await browser.evaluate("document.querySelector('.model-search').textContent.includes('/')"), false);
		await browser.evaluate("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
		await browser.evaluate(
			"window.__transitions=[]; if(document.startViewTransition){const start=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=start(...args);window.__transitions.push(t);return t;};}",
		);
		await browser.fill(".composer-input", "新会话实时记录");
		// 只在 HTTP 传输边界丢掉已接受消息的响应，验证用户重试整条链路。
		await browser.evaluate(
			"window.__fetchBeforeFault=window.fetch;window.__lostResponse=false;window.fetch=async(...args)=>{const response=await window.__fetchBeforeFault(...args);if(!window.__lostResponse&&String(args[0]).endsWith('/messages')&&args[1]?.method==='POST'){window.__lostResponse=true;await response.text();throw new TypeError('测试：响应丢失');}return response;};",
		);
		await browser.until("!document.querySelector('[aria-label=发送消息]').disabled", "发送按钮可用");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"document.querySelector('.composer-error')?.textContent.includes('无法连接') && document.querySelector('.composer-input')?.textContent==='新会话实时记录'",
			"响应丢失后保留草稿",
		);
		await browser.evaluate("window.fetch=window.__fetchBeforeFault");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"location.pathname.startsWith('/s/') && [...document.querySelectorAll('.sidebar-group h2')].some(e=>e.textContent==='今天') && [...document.querySelectorAll('.sidebar-session')].some(e=>e.textContent.includes('新会话实时记录'))",
			"新会话在今天分类中实时出现",
		);
		evidence.realtimeSidebar = true;
		await browser.until("window.__transitions.length>0", "路由触发原生页面过渡");
		evidence.nativeTransition = await browser.evaluate(
			"(async()=>{if(!window.__transitions.length)return {supported:!!document.startViewTransition,count:0};await window.__transitions[0].ready;await window.__transitions[0].finished;return {supported:true,ready:true,count:window.__transitions.length};})()",
		);
		assert.ok((evidence.nativeTransition as { count: number }).count > 0);
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('测试回答')",
			"隔离模型回答",
			25000,
		);
		const id = await browser.evaluate<string>("location.pathname.split('/')[2]");
		const outcome = await browser.evaluate<{ users: number; tools: number; result: string }>(
			`(async()=>{const data=await (await fetch('/api/sessions/${id}')).json();return {users:data.messages.filter(m=>m.role==='user').length,tools:data.messages.filter(m=>m.role==='toolResult').length,result:data.messages.filter(m=>m.role==='toolResult').flatMap(m=>m.content).map(p=>p.text??'').join(' ')};})()`,
		);
		assert.equal(outcome.users, 1, "响应丢失重试只产生一条用户消息");
		assert.equal(outcome.tools, 1, "重试未重复执行已接受的工具调用");
		assert.ok(outcome.result.includes("READ_E2E_OK"));
		evidence.acceptedMessageRetry = { userMessages: outcome.users, toolResults: outcome.tools };
		await browser.navigate(`${origin}/s/${id}`);
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('测试回答')",
			"刷新恢复真实对话历史",
		);
		await browser.until("!!document.querySelector('.turn-process-head')", "工具过程入口");
		await browser.evaluate("document.querySelector('.turn-process-head').click()");
		await browser.until("document.querySelector('.tool-group')?.getBoundingClientRect().width > 0", "工具分组展开");
		const alignment = await browser.evaluate<{ prose: number; tool: number; width: number; proseWidth: number }>(
			"(()=>{const tool=document.querySelector('.tool-group').getBoundingClientRect();const prose=document.querySelector('.assistant-block').getBoundingClientRect();return {prose:prose.x,tool:tool.x,width:tool.width,proseWidth:prose.width};})()",
		);
		assert.ok(Math.abs(alignment.tool - alignment.prose) < 1, JSON.stringify(alignment));
		assert.equal(alignment.width, alignment.proseWidth);
		evidence.toolAlignment = alignment;
		await browser.click(".tool-group-heading");
		await browser.click(".tool-card-heading");
		await browser.evaluate(
			"Promise.all([...document.querySelectorAll('.disclosure__content')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
		);
		await browser.click(".tool-raw-toggle");
		await browser.click(".thinking-trigger");
		await browser.evaluate(
			"Promise.all([...document.querySelectorAll('.disclosure__content')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
		);
		const detailGeometry = await browser.evaluate<{
			rawOffset: number;
			toggleOffset: number;
			guideGap: number;
			thinkingFont: string;
			toolFont: string;
			thinkingOverflow: boolean;
		}>(`(()=>{
			const card=document.querySelector('.tool-card-detail'),raw=card.querySelector('.disclosure .tool-raw'),invocation=card.querySelector('.tool-invocation'),toggle=card.querySelector('.tool-raw-toggle');
			const outer=getComputedStyle(document.querySelector('.tool-group > .disclosure__content'),'::before');
			const thinking=document.querySelector('.thinking-line pre'),style=getComputedStyle(thinking);
			return {rawOffset:raw.getBoundingClientRect().left-invocation.getBoundingClientRect().left,toggleOffset:toggle.getBoundingClientRect().left-invocation.getBoundingClientRect().left,guideGap:parseFloat(outer.bottom),thinkingFont:style.fontSize,toolFont:getComputedStyle(raw).fontSize,thinkingOverflow:thinking.scrollHeight>thinking.clientHeight};
		})()`);
		await browser.screenshot(join(root, "details-expanded-light.png"));
		assert.ok(Math.abs(detailGeometry.rawOffset) < 1, "原始数据与字段左缘对齐");
		assert.ok(Math.abs(detailGeometry.toggleOffset) < 1, "原始数据入口与字段左缘对齐");
		assert.equal(detailGeometry.guideGap, 0, "外层引导线延伸到内容底部");
		assert.equal(detailGeometry.thinkingFont, detailGeometry.toolFont, "思考与工具文本使用相同字号");
		assert.equal(detailGeometry.thinkingOverflow, false, "思考内容完整展开，不设内部高度限制");
		evidence.detailGeometry = detailGeometry;
		await browser.screenshot(join(root, "workspace-light.png"));
		await browser.evaluate("document.querySelector('.sidebar-account-trigger').click()");
		await browser.until("!!document.querySelector('.sidebar-account-menu button[aria-label^=主题]')", "账号菜单");
		await browser.evaluate("document.querySelector('.sidebar-account-menu button[aria-label^=主题]').click()");
		await browser.until("document.documentElement.dataset.theme==='dark'", "深色主题");
		assert.equal(await browser.evaluate("!!document.querySelector('.sidebar-account-menu')"), true);
		evidence.themeMenuPersistent = true;
		await browser.evaluate("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
		await browser.until("!document.querySelector('.sidebar-account-menu')", "菜单退出完成");
		await browser.screenshot(join(root, "workspace-dark.png"));
		const sizes = await browser.evaluate<{
			newWidth: number;
			sessionWidth: number;
			newHeight: number;
			sessionHeight: number;
		}>(
			"(()=>{const a=document.querySelector('.sidebar-new').getBoundingClientRect(),b=document.querySelector('.sidebar-session').getBoundingClientRect();return {newWidth:a.width,sessionWidth:b.width,newHeight:a.height,sessionHeight:b.height};})()",
		);
		assert.equal(sizes.newWidth, sizes.sessionWidth);
		assert.equal(sizes.newHeight, sizes.sessionHeight);
		evidence.sidebarSelection = sizes;
		const startRight = await browser.evaluate<number>(
			"document.querySelector('.sidebar').getBoundingClientRect().right",
		);
		await browser.holdAnimations(".sidebar");
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("window.__heldAnimations.length>0", "侧栏开始退场");
		const motion = await browser.evaluate<{ middleRight: number; width: number }>(
			"(()=>{const side=document.querySelector('.sidebar');window.__heldAnimations.forEach(a=>a.currentTime=175);const rect=side.getBoundingClientRect();return {middleRight:rect.right,width:rect.width};})()",
		);
		assert.equal(motion.width, 264, "过渡期间侧栏保持原宽度");
		assert.ok(motion.middleRight > 0 && motion.middleRight < startRight, "侧栏经过可见的中间位置");
		await browser.screenshot(join(root, "sidebar-transition-midpoint.png"));
		await browser.releaseAnimations();
		await browser.until("document.querySelector('.sidebar').getBoundingClientRect().right<=0.5", "侧栏完整退场");
		evidence.sidebarMotion = {
			startRight,
			...motion,
			endRight: await browser.evaluate("document.querySelector('.sidebar').getBoundingClientRect().right"),
		};
		await browser.evaluate("document.querySelector('[aria-label=展开侧栏]').click()");
		await browser.until("!document.querySelector('.sidebar').inert", "展开侧栏");
		await browser.navigate(`${origin}/login?provider=probe&method=api_key`);
		await browser.until(
			"document.querySelectorAll('.login-switch button').length===3 && !document.querySelector('.login-switch button').disabled",
			"三个登录入口",
		);
		assert.deepEqual(
			await browser.evaluate("[...document.querySelectorAll('.login-switch button')].map(e=>e.textContent)"),
			["cstoa", "OAuth", "APIKEY"],
		);
		await browser.evaluate("document.querySelector('.login-switch button').click()");
		await browser.until(
			"document.querySelector('.login-card').dataset.method==='cstoa' && !!document.querySelector('.login-card svg')",
			"从密钥切换专属扫码",
		);
		await browser.evaluate("document.querySelectorAll('.login-switch button')[1].click()");
		await browser.until("document.querySelector('.login-card').dataset.method==='oauth'", "普通 OAuth");
		await browser.evaluate("document.querySelectorAll('.login-switch button')[2].click()");
		await browser.until(
			"document.querySelector('.login-card').dataset.method==='api_key' && !!document.querySelector('.login-key-field')",
			"APIKEY 登录",
		);
		evidence.loginTabs = true;
		await browser.screenshot(join(root, "login-dark.png"));
		await browser.navigate(`${origin}/login?provider=cloudflare-workers-ai&method=api_key`);
		await browser.until("!!document.querySelector('.login-api-form .login-submit')", "原生 Cloudflare APIKEY 表单");
		await browser.click(".login-api-form .login-submit");
		await browser.until("!!document.querySelector('.oauth-prompt input[type=password]')", "Pi 请求 APIKEY");
		await browser.fill(".oauth-prompt input", "ui-test-key");
		await browser.click('.oauth-prompt button[type="submit"]');
		await browser.until(
			"document.querySelector('.oauth-prompt')?.textContent.includes('account ID')",
			"Pi 请求账号 ID",
		);
		await browser.fill(".oauth-prompt input", "ui-test-account");
		await browser.click('.oauth-prompt button[type="submit"]');
		await browser.until(
			"location.pathname==='/' && !!document.querySelector('.home-main')",
			"APIKEY 多步完成返回主页",
		);
		evidence.nativeApiKey = true;
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 800,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/s/${id}/tree`);
		await browser.until("!!document.querySelector('.tree-row')", "分支树");
		await browser.evaluate("document.querySelector('[aria-label=收起侧栏]')?.click()");
		await browser.until(
			"document.querySelector('.sidebar').getBoundingClientRect().right <= 0.5",
			"半屏侧栏退场完成",
		);
		const treeGeometry = await browser.evaluate<{ rolesOnOneLine: boolean; markerClear: boolean }>(
			"(()=>{const rows=[...document.querySelectorAll('.tree-row')];return {rolesOnOneLine:rows.every(row=>{const role=row.querySelector('.tree-role'),icon=row.querySelector('.tree-icon');return !role||(role.getBoundingClientRect().height<=row.getBoundingClientRect().height&&role.getBoundingClientRect().left>=icon.getBoundingClientRect().right)}),markerClear:rows.every(row=>{const text=row.querySelector('.tree-text'),marker=row.querySelector('.tree-current');return !marker||text.getBoundingClientRect().right<=marker.getBoundingClientRect().left;})};})()",
		);
		assert.ok(treeGeometry.rolesOnOneLine, "长摘要不挤压角色列");
		assert.ok(treeGeometry.markerClear, "长摘要不遮挡当前位置");
		evidence.treeGeometry = treeGeometry;
		assert.equal(
			await browser.evaluate(
				"(()=>{const toolbar=document.querySelector('.tree-toolbar').getBoundingClientRect(),counter=document.querySelector('.tree-counter').getBoundingClientRect();return counter.top>=toolbar.top && counter.bottom<=toolbar.bottom;})()",
			),
			true,
		);
		await browser.screenshot(join(root, "tree-narrow-dark.png"));
		await browser.navigate(`${origin}/`);
		await browser.until(
			"document.querySelector('.model-label')?.textContent.includes('two')",
			"半屏 scoped 模型加载完成",
		);
		assert.equal(
			await browser.evaluate("Math.max(document.documentElement.scrollWidth,document.body.scrollWidth)<=innerWidth"),
			true,
		);
		await browser.screenshot(join(root, "home-narrow-dark.png"));
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		evidence.reducedMotion = await browser.evaluate(
			"(()=>{document.querySelector('[aria-label=收起侧栏]')?.click();return getComputedStyle(document.querySelector('.sidebar')).transitionDuration;})()",
		);
		assert.equal(evidence.reducedMotion, "0s");
		await writeFile(join(root, "report.json"), JSON.stringify(evidence, null, 2));
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await new Promise<void>((resolve) => model.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
