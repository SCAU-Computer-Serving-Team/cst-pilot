import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("设置用户链路：分类导航、自动保存、失败回滚、凭据、多步骤授权、半屏与通知定位", { timeout: 120_000 }, async () => {
	const root = join(await testRoot(), "settings-ui");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "probe",
			defaultModel: "one",
			enabledModels: ["probe/two", "probe/one"],
			theme: "light",
			defaultTools: ["read"],
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: "http://127.0.0.1:9/v1",
					api: "openai-completions",
					apiKey: "test-only",
					models: Array.from({ length: 8 }, (_, i) => ({
						id: ["one", "two"][i] ?? `model-${i}`,
						name: `设置测试模型 ${i + 1}`,
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
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "settings-ui" }),
		api,
	);
	await listenWebServer(server, port);
	const send = (path: string, method: string, body: unknown) =>
		fetch(`${origin}${path}`, {
			method,
			headers: {
				Origin: origin,
				"Content-Type": "application/json",
				"X-CST-Web-Request": "1",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify(body),
		});
	let browser: BrowserProbe | undefined;
	try {
		assert.equal((await send("/api/auth/openai/api-key", "PUT", { key: "settings-initial-secret" })).status, 200);
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1920,
			height: 1080,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings`);
		await browser.until(
			"document.querySelector('.settings-services')?.textContent.includes('OpenAI')",
			"已配置服务列表",
		);
		await browser.screenshot(join(root, "services-light.png"));
		await browser.click(".sidebar-account-trigger");
		await browser.until("!!document.querySelector('.sidebar-account-popover')", "账号菜单");
		const menuGeometry = await browser.evaluate<{ left: number; right: number; placement: string }>(
			"(()=>{const sidebar=document.querySelector('.sidebar').getBoundingClientRect(),node=document.querySelector('.sidebar-account-popover'),menu=node.getBoundingClientRect();return{left:menu.left,right:sidebar.right,placement:node.getAttribute('data-placement')||''}})()",
		);
		assert.ok(menuGeometry.left >= menuGeometry.right - 2, `账号菜单在侧栏右侧：${JSON.stringify(menuGeometry)}`);
		await browser.call("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape" });
		await browser.click('a[href="/settings/add"]');
		await browser.until("!!document.querySelector('.settings-search input')", "添加服务");
		assert.equal(
			await browser.evaluate("document.querySelectorAll('.settings-service-card .settings-service-row').length"),
			6,
			"可添加服务先展示六项",
		);
		await browser.click(".settings-service-card .settings-models-toggle");
		await browser.until(
			"document.querySelectorAll('.settings-service-card .settings-service-row').length>6",
			"展开服务列表",
		);
		await browser.click(".settings-service-card .settings-models-toggle");
		await browser.until(
			"document.querySelectorAll('.settings-service-card .settings-service-row').length===6",
			"折叠服务列表",
		);
		await browser.click('a[href="/settings/custom"]');
		await browser.until("!!document.querySelector('.settings-custom-form')", "自定义服务表单");
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.settings-custom-model-grid .settings-custom-field:nth-child(3) input').value",
			),
			"262144",
			"默认上下文为 256K",
		);
		const catalog = (await (await fetch(`${origin}/api/models`)).json()).models as { maxTokens: number }[];
		const outputCounts = new Map<number, number>();
		for (const { maxTokens } of catalog) outputCounts.set(maxTokens, (outputCounts.get(maxTokens) ?? 0) + 1);
		const outputMode = [...outputCounts.entries()].sort((a, b) => b[1] - a[1] || a[0] - b[0])[0][0];
		await browser.until(
			`document.querySelector('.settings-custom-model-grid .settings-custom-field:nth-child(4) input').value==='${outputMode}'`,
			"默认最大输出采用模型列表众数",
		);
		assert.equal(
			await browser.evaluate(
				"getComputedStyle(document.querySelector('.settings-custom-actions button')).borderRadius",
			),
			"4px",
			"主按钮圆角沿用控件规范",
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.settings-custom-capabilities input[type=checkbox]').length",
			),
			0,
			"能力项使用独立按钮",
		);
		assert.equal(
			await browser.evaluate(
				"(()=>{const rows=[...document.querySelectorAll('.settings-capability-row')].map(n=>n.getBoundingClientRect());return rows.length===2&&Math.abs(rows[1].top-rows[0].bottom-12)<1&&[...document.querySelectorAll('.settings-capability-row')].every(n=>{const label=n.querySelector('span').getBoundingClientRect(),button=n.querySelector('button').getBoundingClientRect();return button.width<=80&&button.left>label.right+24&&Math.abs(label.top+label.height/2-button.top-button.height/2)<1})})()",
			),
			true,
			"两条能力设置间距 12px，文字与右侧按钮垂直居中",
		);
		await browser.click(".settings-capability-button");
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-capability-button').getAttribute('aria-checked')"),
			"true",
			"能力按钮可切换",
		);
		assert.equal(
			await browser.evaluate(
				"(()=>{const field=document.querySelector('.settings-custom-protocol').getBoundingClientRect(),key=document.querySelector('.settings-custom-key').getBoundingClientRect(),button=document.querySelector('.settings-custom-select .select__trigger'),r=button.getBoundingClientRect(),icon=button.querySelector('svg').getBoundingClientRect();return Math.abs(field.width-key.width)<2&&Math.abs(r.right-icon.right-12)<2&&Math.abs(icon.top+icon.height/2-r.top-r.height/2)<1&&Math.round(r.height)===32})()",
			),
			true,
			"协议独占整行，下拉图标居中并保留 12px 右间距",
		);
		await browser.click(".settings-custom-select .select__trigger");
		await browser.until("!!document.querySelector('.settings-protocol-popover')", "协议下拉");
		await browser.click("[role=option][data-key=openai-responses]");
		await browser.until(
			"document.querySelector('.settings-custom-select').textContent.includes('OpenAI Responses')",
			"协议可选",
		);
		await browser.until("!document.querySelector('.settings-protocol-popover')", "协议下拉退场结束");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1920,
			height: 1440,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.evaluate("document.querySelector('.settings-custom-form').scrollIntoView({block:'start'})");
		await browser.screenshot(join(root, "custom-service-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.evaluate("new Promise(resolve=>setTimeout(resolve,300))");
		await browser.screenshot(join(root, "custom-service-dark.png"));
		await browser.evaluate("document.documentElement.dataset.theme='light'");
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("document.querySelector('.sidebar').getBoundingClientRect().right<=1", "侧栏收起完成");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.evaluate("document.querySelector('.settings-custom-form').scrollIntoView({block:'start'})");
		assert.equal(
			await browser.evaluate(
				"document.documentElement.scrollWidth<=innerWidth&&[...document.querySelectorAll('.settings-custom-field')].every(n=>n.getBoundingClientRect().right<=innerWidth)",
			),
			true,
			"窄屏表单不溢出",
		);
		await browser.screenshot(join(root, "custom-service-mobile.png"));
		await browser.evaluate(
			"document.querySelector('.settings-custom-capabilities').scrollIntoView({block:'center'})",
		);
		await browser.screenshot(join(root, "custom-service-mobile-model.png"));
		await browser.click(".settings-custom-models > button");
		await browser.until("document.querySelectorAll('.settings-custom-model').length===2", "窄屏添加模型");
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.settings-custom-model')[1].querySelector('.settings-custom-field:nth-child(4) input').value",
			),
			String(outputMode),
			"新增模型沿用目录众数",
		);
		await browser.click(".settings-custom-model:last-of-type .settings-custom-model-title button");
		await browser.until("document.querySelectorAll('.settings-custom-model').length===1", "移除新增模型");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1920,
			height: 1080,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="展开侧栏"]');
		await browser.until("document.querySelector('.sidebar').getBoundingClientRect().left>=-1", "恢复桌面侧栏");
		await browser.fill(".settings-custom-fields .settings-custom-field:nth-child(1) input", "浏览器测试服务");
		await browser.fill(".settings-custom-fields .settings-custom-field:nth-child(2) input", "browser-probe");
		await browser.fill(
			".settings-custom-fields .settings-custom-field:nth-child(4) input",
			"https://api.example.test/v1",
		);
		await browser.fill(".settings-custom-fields .settings-custom-field:nth-child(5) input", "browser-test-secret");
		await browser.fill(".settings-custom-model-grid .settings-custom-field:nth-child(1) input", "alpha");
		await browser.fill(".settings-custom-model-grid .settings-custom-field:nth-child(2) input", "Alpha");
		await browser.click(".settings-custom-actions button");
		await browser.until("location.pathname==='/settings'", "自定义服务已保存");
		assert.equal(
			(await (await fetch(`${origin}/api/models`)).json()).models.some(
				(model: { provider: string; id: string }) => model.provider === "browser-probe" && model.id === "alpha",
			),
			true,
			"自定义服务进入 Pi 可用模型",
		);
		await browser.click('a[href="/settings/add"]');
		await browser.until("!!document.querySelector('.settings-search input')", "返回添加服务");
		await browser.fill(".settings-search input", "anthropic");
		await browser.until("document.querySelectorAll('.settings-service-row').length===1", "服务搜索");
		await browser.screenshot(join(root, "add-service-light.png"));
		assert.equal(
			await browser.evaluate(
				"(()=>{const t=document.querySelector('.settings-service-row .settings-action'),b=t.getBoundingClientRect(),r=document.createRange();r.selectNodeContents(t);const text=r.getBoundingClientRect();return Math.abs(b.left+b.width/2-text.left-text.width/2)<1})()",
			),
			true,
		);
		await browser.fill(".settings-search input", "this-provider-does-not-exist");
		await browser.until("document.body.innerText.includes('没有匹配的模型服务')", "服务搜索空状态");
		await browser.call("Page.reload");
		await browser.until(
			"document.querySelector('.settings-page-heading h2')?.textContent==='添加服务'",
			"添加服务深链接刷新",
		);
		await browser.click('a[href="/settings/models"]');
		await browser.until("document.querySelectorAll('.settings-model').length===6", "模型预览六项");
		await browser.click(".settings-provider-select button");
		await browser.until("!!document.querySelector('[role=option][data-key=probe]')", "服务筛选选项");
		assert.equal(
			await browser.evaluate(
				"(()=>{const trigger=document.querySelector('.settings-provider-select .select__trigger').getBoundingClientRect(),menu=document.querySelector('.settings-provider-popover').getBoundingClientRect();return Math.abs(trigger.right-menu.right)<2&&menu.width<=242})()",
			),
			true,
			"服务筛选面板右边缘对齐按钮，不横向撑开",
		);
		await browser.click("[role=option][data-key=probe]");
		await browser.until(
			"[...document.querySelectorAll('.settings-model-provider')].every(e=>e.textContent==='probe')",
			"按服务筛选模型",
		);
		await browser.screenshot(join(root, "model-preferences-light.png"));
		await browser.click(".settings-models-toggle");
		await browser.fill(".settings-search input", "probe");
		await browser.until("document.querySelectorAll('.settings-model').length===8", "展开全部模型");
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.settings-models').scrollHeight <= document.querySelector('.settings-models').clientHeight",
			),
			true,
			"展开后列表不产生内部滚动",
		);
		await browser.evaluate(
			"document.querySelector('[aria-label=\"调整 设置测试模型 1 的顺序\"]').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowUp',altKey:true,bubbles:true}))",
		);
		await browser.until(
			"document.querySelector('.settings-notification')?.textContent.includes('已自动保存')",
			"键盘排序自动保存",
		);
		assert.deepEqual(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).enabledModels, [
			"probe/one",
			"probe/two",
		]);
		await browser.fill(".settings-search input", "设置测试模型 3");
		await browser.until("document.querySelectorAll('.settings-model').length===1", "模型搜索");
		await browser.click(".settings-check-box");
		await browser.until(
			"document.querySelector('.settings-check-box input')?.checked && !document.querySelector('.settings-model-card')?.getAttribute('aria-busy')?.includes('true')",
			"模型启用保存",
		);
		assert.deepEqual(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).enabledModels, [
			"probe/one",
			"probe/two",
			"probe/model-2",
		]);
		// 只在 HTTP 边界注入一次失败，产品状态与持久化继续走真实接口。
		await browser.evaluate(
			"window.__settingsFetch=window.fetch;window.__settingsFailed=false;window.fetch=async(...args)=>{if(!window.__settingsFailed&&String(args[0]).endsWith('/api/models/scoped')&&args[1]?.method==='POST'){window.__settingsFailed=true;return new Response(JSON.stringify({error:{message:'测试写入失败'}}),{status:503,headers:{'Content-Type':'application/json'}})}return window.__settingsFetch(...args)}",
		);
		await browser.click(".settings-check-box");
		await browser.until(
			"document.querySelector('.settings-load-error')?.textContent.includes('测试写入失败') && document.querySelector('.settings-check-box input')?.checked",
			"保存失败恢复勾选",
		);
		await browser.evaluate("window.fetch=window.__settingsFetch");
		await browser.fill(".settings-search input", "");
		await browser.click(".settings-model-bulk button:nth-child(2)");
		await browser.until(
			"document.querySelector('.settings-section header span')?.textContent==='0 个已启用'",
			"清空模型范围",
		);
		await browser.until(
			"document.querySelector('.settings-model-card')?.getAttribute('aria-busy')==='false'",
			"清空已保存",
		);
		assert.deepEqual((await (await fetch(`${origin}/api/models`)).json()).enabled, []);
		assert.equal(
			await browser.evaluate("document.querySelectorAll('.settings-model').length"),
			8,
			"取消启用的模型留在展开列表",
		);
		await browser.click(".settings-model-bulk button:nth-child(3)");
		await browser.until(
			"document.querySelector('.settings-section header span')?.textContent!=='0 个已启用'",
			"恢复默认模型范围",
		);
		await browser.navigate(`${origin}/`);
		await browser.until("!!document.querySelector('.composer-model')", "首页模型选择器");
		assert.equal((await send("/api/models/scoped", "POST", { patterns: ["probe/one"] })).status, 200);
		await browser.click(".composer-model");
		await browser.until(
			"document.querySelectorAll('.model-popover [role=option]').length===1",
			"后台范围变化实时更新选择器",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.model-popover')?.textContent.includes('设置测试模型 1')"),
			true,
		);
		await browser.navigate(`${origin}/settings/models`);
		await browser.until("!!document.querySelector('a[href=\"/settings/general\"]')", "返回设置分类");
		await browser.until(
			"document.querySelector('.settings-section header span')?.textContent==='1 个已启用'",
			"重进设置页读取最新模型范围",
		);
		await browser.click('a[href="/settings/general"]');
		await browser.until("!!document.querySelector('[aria-label=深色]')", "主题图标选择器");
		assert.equal(
			await browser.evaluate(
				"getComputedStyle(document.querySelector('.settings-route')).viewTransitionName==='settings-page'",
			),
			true,
			"设置分类使用内容快照衔接",
		);
		await browser.click('[aria-label="深色"]');
		await browser.until(
			"document.documentElement.dataset.theme==='dark' && !!document.querySelector('.settings-notification')",
			"主题自动保存",
		);
		assert.equal(JSON.parse(await readFile(join(home, "web-settings.json"), "utf8")).theme, "dark");
		assert.equal(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).theme, "light", "Web主题不修改TUI");
		assert.equal(
			await browser.evaluate(
				"(()=>{const main=document.querySelector('.workspace-content').getBoundingClientRect(),toast=document.querySelector('.settings-notification').getBoundingClientRect();return toast.left>=main.left&&toast.right<=main.right})()",
			),
			true,
			"保存通知不遮挡侧栏或超出主区",
		);
		await browser.evaluate("Promise.allSettled(document.getAnimations().map(a=>a.finished))");
		assert.equal(
			await browser.evaluate("document.querySelectorAll('.settings-notification').length"),
			1,
			"连续保存复用同一条通知",
		);
		await browser.screenshot(join(root, "general-dark.png"));
		await browser.evaluate(
			"window.__themeFetch=window.fetch;window.fetch=async(...args)=>String(args[0]).endsWith('/api/settings')&&args[1]?.method==='PATCH'?new Response(JSON.stringify({error:{message:'测试主题保存失败'}}),{status:503,headers:{'Content-Type':'application/json'}}):window.__themeFetch(...args)",
		);
		await browser.click('[aria-label="浅色"]');
		await browser.until(
			"document.documentElement.dataset.theme==='dark' && document.querySelector('.settings-load-error')?.textContent.includes('测试主题保存失败')",
			"主题失败恢复已保存主题",
		);
		await browser.evaluate("window.fetch=window.__themeFetch");
		await browser.navigate(`${origin}/settings/provider?provider=openai&method=api_key`);
		await browser.until("!!document.querySelector('.settings-key-input input')", "密钥管理页");
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-key-input input').value"),
			"",
			"不读取已有密钥",
		);
		await browser.screenshot(join(root, "provider-dark.png"));
		await browser.fill(".settings-key-input input", "settings-updated-secret");
		await browser.click(".settings-page-heading");
		await browser.until(
			"!!document.querySelector('.settings-notification') && !!document.querySelector('.settings-key-input input')",
			"密钥失焦保存",
		);
		assert.equal(await browser.evaluate("document.querySelector('.settings-key-input input').value"), "");
		const auth = await readFile(join(home, "auth.json"), "utf8");
		assert.ok(auth.includes("settings-updated-secret"));
		assert.equal(
			await browser.evaluate(
				"JSON.stringify({...localStorage,...sessionStorage}).includes('settings-updated-secret')",
			),
			false,
		);
		await browser.click(".settings-danger-zone button");
		await browser.until("!!document.querySelector('dialog[open]')", "移除凭据确认");
		assert.equal(await browser.evaluate("document.activeElement.textContent"), "取消");
		await browser.click("dialog .exit-confirm");
		await browser.until(
			"!document.querySelector('dialog[open]') && !document.querySelector('.settings-danger-zone')",
			"移除凭据完成",
		);
		assert.ok(!(await readFile(join(home, "auth.json"), "utf8")).includes("settings-updated-secret"));
		await browser.navigate(`${origin}/settings/provider?provider=cloudflare-workers-ai&method=api_key`);
		await browser.until("!!document.querySelector('.settings-key-input input')", "多步骤密钥服务");
		await browser.fill(".settings-key-input input", "settings-cloudflare-secret");
		await browser.click(".settings-page-heading");
		await browser.until(
			"document.querySelector('.oauth-prompt')?.textContent.includes('account ID')",
			"Pi额外请求账号ID",
		);
		await browser.fill(".oauth-prompt input", "settings-test-account");
		await browser.click('.oauth-prompt button[type="submit"]');
		await browser.until(
			"!!document.querySelector('.settings-key-input input') && !!document.querySelector('.settings-danger-zone')",
			"多步骤授权完成",
		);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 960,
			height: 1080,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings`);
		await browser.until("!!document.querySelector('.settings-services')", "半屏服务列表");
		await browser.evaluate("document.querySelector('[aria-label=展开侧栏]')?.click()");
		await browser.until(
			"(()=>{const sidebar=document.querySelector('.sidebar').getBoundingClientRect(),main=document.querySelector('.workspace-content').getBoundingClientRect();return sidebar.width>0&&main.left>=sidebar.right-1&&!document.querySelector('.sidebar-backdrop')})()",
			"半屏侧栏保留占位且不遮挡主区",
		);
		assert.equal(
			await browser.evaluate("Math.max(document.documentElement.scrollWidth,document.body.scrollWidth)<=innerWidth"),
			true,
		);
		await browser.screenshot(join(root, "services-narrow-dark.png"));
		await browser.click('a[href="/settings/general"]');
		await browser.until("!!document.querySelector('[aria-label=浅色]')", "半屏主题页");
		await browser.click('[aria-label="浅色"]');
		await browser.until("!!document.querySelector('.settings-notification')", "半屏自动保存通知");
		assert.equal(
			await browser.evaluate(
				"(()=>{const r=document.querySelector('.settings-notification').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth})()",
			),
			true,
			"半屏保存通知不越界",
		);
		await browser.screenshot(join(root, "general-narrow-light.png"));
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("document.querySelector('.sidebar').inert", "侧栏收起后隐藏交互");
		await browser.click('a[href="/settings/shortcuts"]');
		await browser.until("document.querySelectorAll('.settings-shortcuts kbd').length>0", "快捷键导航");
		await browser.evaluate("history.back()");
		await browser.until("location.pathname==='/settings/general'", "浏览器返回");
		await browser.screenshot(join(root, "general-narrow-collapsed.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 480,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="展开侧栏"]');
		await browser.until("!!document.querySelector('.sidebar-backdrop')", "手机宽度设置侧栏为浮层");
		assert.equal(await browser.evaluate("document.querySelector('.workspace-content').getBoundingClientRect().x"), 0);
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.workspace-content').getBoundingClientRect().width>=innerWidth-1",
			),
			true,
			"浮层不挤占主区宽度",
		);
		assert.equal(
			await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"),
			true,
			"手机设置页不横向溢出",
		);
		await browser.screenshot(join(root, "general-mobile-overlay.png"));
		await browser.evaluate("document.querySelector('.sidebar-backdrop').click()");
		await browser.until(
			"!!document.querySelector('[aria-label=展开侧栏]') && !document.querySelector('.sidebar-backdrop')",
			"点击遮罩收起侧栏",
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
