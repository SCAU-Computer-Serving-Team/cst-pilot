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
		assert.equal(await browser.evaluate("document.querySelector('.model-popover input')===null"), true);
		await browser.evaluate(
			"Promise.all(document.querySelector('.composer-model .model-label svg').getAnimations().map(a=>a.finished))",
		);
		assert.ok(
			await browser.evaluate(
				"new DOMMatrix(getComputedStyle(document.querySelector('.composer-model .model-label svg')).transform).a < -0.99",
			),
			"模型菜单展开时箭头旋转",
		);
		for (const theme of ["light", "dark"]) {
			await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
			await browser.screenshot(join(root, `model-menu-${theme}.png`));
		}
		await browser.evaluate("document.documentElement.dataset.theme='light'");
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
			"document.querySelector('.error-notification .notification-message')?.textContent.includes('提交待确认') && document.querySelector('.composer-input')?.textContent==='新会话实时记录'",
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
		await browser.evaluate(
			`window.__originalFetch=window.fetch;window.fetch=async(...args)=>{if(String(args[0]).endsWith('/api/sessions/${previous.id}'))await new Promise(r=>setTimeout(r,500));return window.__originalFetch(...args)};window.__sessionTransitionCount=0;const start=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{window.__sessionTransitionCount++;return start(...args);};`,
		);
		await browser.click(`.sidebar-session[data-session-id="${previous.id}"]`);
		await browser.until(
			`location.pathname==='/s/${previous.id}' && document.querySelector('.chat-loaded')?.dataset.sessionId==='${previous.id}'`,
			"预载昨日会话直接切换",
		);
		assert.equal(
			await browser.evaluate("document.body.textContent.includes('正在读取会话')"),
			false,
			"预载切页不出现读取占位",
		);
		assert.equal(await browser.evaluate("window.__sessionTransitionCount"), 0, "侧栏切换不等待页面快照");
		await browser.until("document.querySelector('.conversation')?.textContent.includes('昨日回答')", "昨日会话正文");
		assert.equal(await browser.evaluate("document.querySelector('.chat-loaded').dataset.sessionId"), previous.id);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.chat-loaded')).animationName"),
			"chat-content-reveal",
			"缓存命中保留正文淡入",
		);
		await browser.evaluate("window.fetch=window.__originalFetch");
		await browser.click(`.sidebar-session[data-session-id="${id}"]`);
		await browser.until(`document.querySelector('.chat-loaded')?.dataset.sessionId==='${id}'`, "切回当前会话");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.navigate(`${origin}/s/${id}`);
		await browser.until("!!document.querySelector('.chat-loaded')", "减少动态下加载会话");
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.chat-loaded')).animationName"),
			"none",
		);
		await browser.call("Emulation.setEmulatedMedia", { features: [] });
		await browser.navigate(`${origin}/s/${id}`);
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('测试回答')",
			"刷新恢复真实对话历史",
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.assistant-turn > .markdown')?.textContent.includes('测试回答')",
			),
			true,
			"正文后跟思考内容时，刷新后正文仍显示在过程外",
		);
		await browser.until("!!document.querySelector('.turn-process-head')", "工具过程入口");
		assert.ok(
			await browser.evaluate(
				"(()=>{const i=document.querySelector('.turn-process-head .disclosure__indicator'),m=new DOMMatrix(getComputedStyle(i).transform);return m.b < -.99 && getComputedStyle(i).rotate==='none'})()",
			),
			"折叠指示器朝右",
		);
		await browser.evaluate("document.querySelector('.turn-process-head').click()");
		await browser.until("document.querySelector('.tool-group')?.getBoundingClientRect().width > 0", "工具分组展开");
		assert.ok(
			await browser.evaluate(
				"(()=>{const i=document.querySelector('.tool-group-heading .disclosure__indicator');return new DOMMatrix(getComputedStyle(i).transform).b < -.99})()",
			),
			"父级展开时子级仍朝右",
		);
		await browser.click(".tool-group-heading");
		await browser.click(".tool-card-heading");
		await browser.evaluate(
			"Promise.all([...document.querySelectorAll('.disclosure__content')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
		);
		await browser.click(".tool-raw-toggle");
		await browser.click(".thinking-trigger");
		await browser.evaluate(
			"Promise.all([...document.querySelectorAll('.disclosure__indicator')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
		);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head, .tool-group-heading, .tool-card-heading, .thinking-trigger, .tool-raw-toggle')].every(e=>e.getAttribute('aria-expanded')==='true' && Math.abs(new DOMMatrix(getComputedStyle(e.querySelector('.disclosure__indicator')).transform).a-1)<.01 && getComputedStyle(e.querySelector('.disclosure__indicator')).rotate==='none')",
			),
			true,
			"展开指示器统一朝下",
		);
		await browser.evaluate(
			"Promise.all([...document.querySelectorAll('.disclosure__content')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
		);
		await browser.screenshot(join(root, "details-expanded-light.png"));
		assert.equal(
			await browser.evaluate(
				"(()=>{const e=document.querySelector('.thinking-line pre');return e.scrollHeight>e.clientHeight})()",
			),
			false,
			"思考内容完整展开",
		);
		assert.ok(await browser.evaluate("!!document.querySelector('.tool-card-detail .tool-raw')"), "原始数据可展开");
		await browser.screenshot(join(root, "workspace-light.png"));
		await browser.evaluate(
			"(()=>{const c=document.querySelector('.conversation');c.scrollTop=0;c.dispatchEvent(new Event('scroll'))})()",
		);
		await browser.until("!!document.querySelector('.back-to-bottom')", "上翻显示回到底部");
		assert.equal(
			await browser.evaluate(
				"(()=>{const b=document.querySelector('.back-to-bottom'),s=document.querySelector('.chat-message-surface'),r=b.getBoundingClientRect(),p=s.getBoundingClientRect(),c=getComputedStyle(b);return r.height>=40 && p.bottom-r.bottom<=16 && c.color!==c.backgroundColor})()",
			),
			true,
			"按钮尺寸、位置和对比清晰",
		);
		const scrolling = await browser.evaluate<{ initial: number; middle: number; bottom: number; shown: boolean }>(
			"(async()=>{const c=document.querySelector('.conversation'),initial=c.scrollTop,bottom=c.scrollHeight-c.clientHeight,reduced=matchMedia('(prefers-reduced-motion: reduce)').matches,duration=getComputedStyle(c).getPropertyValue('--duration-fast');document.querySelector('.back-to-bottom').click();const immediate=c.scrollTop;await new Promise(r=>setTimeout(r,80));return {initial,immediate,middle:c.scrollTop,bottom,shown:!!document.querySelector('.back-to-bottom'),reduced,duration};})()",
		);
		assert.ok(
			scrolling.middle > scrolling.initial && scrolling.middle < scrolling.bottom - 1,
			JSON.stringify(scrolling),
		);
		assert.equal(scrolling.shown, true, "滚动过程中保持按钮，避免闪烁");
		await browser.until("!document.querySelector('.back-to-bottom')", "回到底部恢复跟随");
		await browser.evaluate(
			"(()=>{const c=document.querySelector('.conversation');c.scrollTop=0;c.dispatchEvent(new Event('scroll'))})()",
		);
		await browser.until("!!document.querySelector('.back-to-bottom')", "再次上翻");
		const interrupted = await browser.evaluate<{ stopped: number; later: number; visible: boolean }>(
			"(async()=>{const c=document.querySelector('.conversation');document.querySelector('.back-to-bottom').click();await new Promise(r=>setTimeout(r,50));c.dispatchEvent(new WheelEvent('wheel',{deltaY:-100,bubbles:true}));const stopped=c.scrollTop;await new Promise(r=>setTimeout(r,300));return {stopped,later:c.scrollTop,visible:!!document.querySelector('.back-to-bottom')};})()",
		);
		assert.equal(interrupted.later, interrupted.stopped, "用户滚动可打断动画");
		assert.equal(interrupted.visible, true);
		await browser.evaluate("document.querySelector('.back-to-bottom').click()");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.until("!document.querySelector('.back-to-bottom')", "中途启用减少动态时立即到达底部");
		assert.ok(
			await browser.evaluate(
				"(()=>{const c=document.querySelector('.conversation');return c.scrollHeight-c.clientHeight-c.scrollTop<1})()",
			),
		);
		await browser.evaluate(
			"(()=>{const c=document.querySelector('.conversation');c.scrollTop=0;c.dispatchEvent(new Event('scroll'))})()",
		);
		await browser.until("!!document.querySelector('.back-to-bottom')", "减少动态时上翻");
		await browser.click(".back-to-bottom");
		await browser.until("!document.querySelector('.back-to-bottom')", "减少动态时立即到达底部");
		await browser.call("Emulation.setEmulatedMedia", { features: [] });
		await browser.evaluate("document.querySelector('.sidebar-account-trigger').click()");
		await browser.until("!!document.querySelector('.sidebar-account-menu button[aria-label^=主题]')", "账号菜单");
		await browser.evaluate("document.querySelector('.sidebar-account-menu button[aria-label^=主题]').click()");
		await browser.until("document.documentElement.dataset.theme==='dark'", "深色主题");
		assert.equal(await browser.evaluate("!!document.querySelector('.sidebar-account-menu')"), true);
		evidence.themeMenuPersistent = true;
		await browser.evaluate("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
		await browser.until("!document.querySelector('.sidebar-account-menu')", "菜单退出完成");
		await browser.screenshot(join(root, "workspace-dark.png"));
		await browser.evaluate(
			"(()=>{const c=document.querySelector('.conversation');c.scrollTop=0;c.dispatchEvent(new Event('scroll'))})()",
		);
		await browser.until("!!document.querySelector('.back-to-bottom')", "深色主题显示回到底部");
		assert.equal(
			await browser.evaluate(
				"(()=>{const c=getComputedStyle(document.querySelector('.back-to-bottom'));return c.color!==c.backgroundColor && Number(c.backgroundColor.match(/\\d+/)?.[0])>180})()",
			),
			true,
		);
		await browser.screenshot(join(root, "back-to-bottom-dark.png"));
		await browser.click(".back-to-bottom");
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("document.querySelector('.sidebar').inert", "收起侧栏后隐藏交互");
		evidence.sidebarToggle = true;
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
		await browser.evaluate("document.querySelector('[aria-label=收起侧栏]')?.click()");
		await browser.until("document.querySelector('.sidebar').inert", "减少动态下侧栏仍可收起");
		evidence.reducedMotion = true;
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
