import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { type MockOptions, startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("CSTOA：身份展示、上下文额度、零值、重试、模型隔离与退出", { timeout: 120_000 }, async () => {
	const root = join(await testRoot(), "account-profile");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	await writeFile(
		join(home, "auth.json"),
		JSON.stringify({
			cstoa: { type: "oauth", access: "access-1", refresh: "refresh-1", expires: Date.now() + 3600_000 },
		}),
	);
	await cp(fileURLToPath(new URL("../../oauth", import.meta.url)), join(home, "extensions", "oauth"), {
		recursive: true,
	});
	const options: MockOptions = { profileStatus: 200 };
	const oa = await startMockOa(options);
	const previousHost = process.env.CSTOA_OA_HOST;
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	process.env.CSTOA_OA_HOST = oa.host;
	process.env.PI_CODING_AGENT_DIR = home;
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const { modelRuntime } = await pool.refreshServices();
	await pool.catalog.refresh({ providers: ["cstoa"], force: true });
	const model = modelRuntime.getModels("cstoa")[0];
	assert.ok(model);
	await modelRuntime.login("openai", "api_key", { prompt: async () => "test-other-key", notify: () => {} });
	const other = modelRuntime.getModels("openai")[0];
	assert.ok(other);
	const slot = await pool.create();
	await slot.session.setModel(model);
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "account-profile" }),
		api,
	);
	await listenWebServer(server, port);
	const browser = await BrowserProbe.launch(root);
	let observer: BrowserProbe | undefined;
	const openContext = async () => {
		await browser.until("!!document.querySelector('.context-meter')", "上下文入口");
		await browser.evaluate("document.querySelector('.context-meter').focus()");
		await browser.until("document.querySelector('.context-meter-wrap')?.dataset.open==='true'", "键盘打开上下文");
	};
	const hideSidebar = async () => {
		if (await browser.evaluate("document.querySelector('.sidebar')?.getAttribute('aria-hidden')==='false'")) {
			await browser.click('button[aria-label="收起侧栏"]');
			await browser.until("document.querySelector('.sidebar').getBoundingClientRect().right<=0", "侧栏收起");
		}
	};
	try {
		for (const [theme, width, height] of [
			["light", 1440, 1000],
			["dark", 480, 900],
		] as const) {
			await browser.call("Emulation.setDeviceMetricsOverride", {
				width,
				height,
				deviceScaleFactor: 1,
				mobile: false,
			});
			await browser.call("Emulation.setEmulatedMedia", {
				features: [{ name: "prefers-color-scheme", value: theme }],
			});
			await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme }));
			await browser.navigate(`${origin}/account`);
			if (width < 900) await hideSidebar();
			await browser.until(
				"document.querySelector('.account-fields')?.innerText.includes('20230001')&&document.querySelector('.account-fields')?.innerText.includes('测试队员')",
				"学号姓名显示",
			);
			await browser.until("document.querySelector('.account-quota')?.innerText==='12,345 额度点'", "账号页额度");
			assert.equal(
				await browser.evaluate("document.querySelector('.account-fields').innerText.includes('access-1')"),
				false,
			);
			await browser.screenshot(join(root, `account-${theme}.png`));
			await browser.navigate(`${origin}/settings`);
			if (width < 900) await hideSidebar();
			await browser.until(
				"document.querySelector('.settings-service-row[data-provider=cstoa]')?.innerText.includes('测试队员')",
				"设置页身份显示",
			);
			assert.equal(
				await browser.evaluate(
					"document.querySelector('.settings-service-row[data-provider=cstoa]').innerText.includes('额度')",
				),
				false,
			);
			await browser.evaluate(
				"document.querySelector('.settings-service-row[data-provider=cstoa]').scrollIntoView({block:'center'})",
			);
			await browser.screenshot(join(root, `settings-${theme}.png`));
			await browser.navigate(`${origin}/s/${slot.id}`);
			if (width < 900) await hideSidebar();
			await openContext();
			await browser.until(
				"document.querySelector('.context-balance')?.innerText==='12,345 额度点'",
				"上下文 CSTOA 额度",
			);
			const geometry = await browser.evaluate<{ left: number; right: number; top: number; width: number }>(
				"(()=>{const r=document.querySelector('.context-panel').getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,width:innerWidth}})()",
			);
			await browser.screenshot(join(root, `context-${theme}.png`));
			assert.ok(
				geometry.left >= 0 && geometry.right <= geometry.width && geometry.top >= 0,
				JSON.stringify(geometry),
			);
			await browser.call("Input.dispatchKeyEvent", {
				type: "keyDown",
				key: "Escape",
				code: "Escape",
				windowsVirtualKeyCode: 27,
			});
			await browser.until("document.querySelector('.context-meter-wrap')?.dataset.open==='false'", "Esc关闭上下文");
		}
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1440,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/account`);
		if (await browser.evaluate("document.querySelector('.sidebar')?.getAttribute('aria-hidden')==='true'"))
			await browser.click('button[aria-label="展开侧栏"]');
		await browser.click('button[aria-label="账号菜单"]');
		await browser.until(
			"document.querySelector('.sidebar-account-identity')?.innerText.includes('测试队员')",
			"菜单身份",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.sidebar-account-menu').innerText.includes('额度')"),
			false,
		);
		await browser.screenshot(join(root, "menu-dark.png"));
		options.profile = { balance: 0 };
		await fetch(`${origin}/api/quota?provider=cstoa&refresh=1`);
		await browser.navigate(`${origin}/account`);
		await browser.until("document.querySelector('.account-quota')?.innerText==='0 额度点'", "账号页零额度");
		await browser.navigate(`${origin}/s/${slot.id}`);
		await openContext();
		await browser.until("document.querySelector('.context-balance')?.innerText==='0 额度点'", "零额度");
		// 更换为其他已登录服务，验证其上下文面板不会沿用 CSTOA 数字。
		await slot.session.setModel(other);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await openContext();
		await browser.until("!document.querySelector('.context-title')", "其他服务不显示 CSTOA 额度");
		await slot.session.setModel(model);
		options.profile = { balance: null, balanceAvailable: false };
		await fetch(`${origin}/api/quota?provider=cstoa&refresh=1`);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await openContext();
		await browser.until(
			"document.querySelector('.context-quota-notice')?.innerText.includes('额度暂不可用')",
			"额度故障",
		);
		await browser.navigate(`${origin}/account`);
		await browser.until(
			"document.querySelector('.account-quota')?.innerText.includes('额度暂不可用')",
			"账号页额度故障",
		);
		options.profile = { balance: 100 };
		await browser.click(".account-quota button");
		await browser.until("document.querySelector('.account-quota')?.innerText==='100 额度点'", "账号页额度重试");
		await browser.navigate(`${origin}/s/${slot.id}`);
		await openContext();
		await browser.until("document.querySelector('.context-balance')?.innerText==='100 额度点'", "上下文额度恢复");
		options.profileStatus = 404;
		await fetch(`${origin}/api/account?refresh=1`);
		await browser.navigate(`${origin}/account`);
		await browser.until("document.querySelector('.account-notice')?.innerText.includes('暂不可用')", "资料故障提示");
		assert.ok(await browser.evaluate("document.querySelector('.account-actions')?.innerText.includes('退出 CSTOA')"));
		options.profileStatus = 200;
		options.profile = { name: "测试队员".repeat(40) };
		await browser.click(".account-notice button");
		await browser.until("document.querySelector('.account-fields')?.innerText.includes('测试队员')", "资料重试");
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		observer = await BrowserProbe.launch(root);
		await observer.navigate(`${origin}/s/${slot.id}`);
		await observer.until("!!document.querySelector('.context-meter')", "另一浏览器上下文入口");
		await observer.evaluate("document.querySelector('.context-meter').focus()");
		await observer.until("!!document.querySelector('.context-balance')", "另一浏览器已有额度");
		await browser.click(".account-actions button");
		await browser.until(
			"document.querySelector('.account-fields')?.innerText.includes('登录后查看')",
			"退出清除资料",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.account-fields').innerText.includes('测试队员')"),
			false,
		);
		assert.equal(await browser.evaluate("document.querySelector('.account-quota').innerText"), "登录后查看");
		assert.ok((await modelRuntime.listCredentials()).some((item) => item.providerId === "openai"));
		await observer.until(
			"document.querySelector('.context-quota-notice')?.innerText.includes('登录后查看')",
			"另一浏览器同步退出",
		);
		assert.equal(await observer.evaluate("!!document.querySelector('.context-balance')"), false);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await openContext();
		await browser.until(
			"document.querySelector('.context-quota-notice')?.innerText.includes('登录后查看')",
			"退出清除额度",
		);
		assert.equal(await browser.evaluate("!!document.querySelector('.context-balance')"), false);
	} finally {
		await observer?.close();
		await browser.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await api.close();
		await pool.close();
		await oa.close();
		if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previousHost;
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		await rm(home, { recursive: true, force: true });
	}
});
