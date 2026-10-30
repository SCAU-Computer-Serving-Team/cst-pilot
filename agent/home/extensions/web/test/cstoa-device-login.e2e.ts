import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("CSTOA 扫码不显示设备授权码，其他 OAuth 保留必要设备码", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "cstoa-login");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const previous = { home: process.env.PI_CODING_AGENT_DIR, host: process.env.CSTOA_OA_HOST };
	const options = { pendingTimes: 1000 };
	const oa = await startMockOa(options);
	process.env.PI_CODING_AGENT_DIR = home;
	process.env.CSTOA_OA_HOST = oa.host;
	await writeFile(join(home, "settings.json"), "{}");
	await cp(fileURLToPath(new URL("../../oauth/", import.meta.url)), join(home, "extensions", "oauth"), {
		recursive: true,
	});
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const { modelRuntime } = await pool.getServices();
	modelRuntime.registerProvider("device-probe", {
		name: "Device OAuth",
		baseUrl: `${oa.host}/v1`,
		api: "openai-completions",
		models: [],
		oauth: {
			name: "Device OAuth",
			login: async (callbacks) => {
				callbacks.onDeviceCode?.({
					userCode: "ABCD-EFGH",
					verificationUri: `${oa.host}/authorize`,
					expiresInSeconds: 300,
				});
				return new Promise<never>((_resolve, reject) => {
					const abort = () => reject(new Error("Cancelled"));
					if (callbacks.signal?.aborted) abort();
					else callbacks.signal?.addEventListener("abort", abort, { once: true });
				});
			},
			refreshToken: async (credential) => credential,
			getApiKey: (credential) => credential.access,
		},
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "cstoa-login" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const page = browser;
		async function checkScan() {
			await page.until("!!document.querySelector('.login-qr-box svg')", "CSTOA 真实设备授权二维码");
			assert.equal(
				await page.evaluate("document.querySelector('.login-fields').textContent.includes('123456')"),
				false,
			);
			assert.equal(await page.evaluate("document.querySelector('.login-qr-code')===null"), true);
			assert.equal(await page.evaluate("document.querySelector('.oauth-prompt')===null"), true);
			assert.equal(
				await page.evaluate("document.querySelector('.login-qr-hint').textContent"),
				"手机扫码，在 OA 页面完成授权",
			);
			assert.match(
				await page.evaluate<string>("document.querySelector('.login-qr-countdown').textContent"),
				/二维码剩余 \d+:\d{2}/,
			);
		}
		const status = "(await (await fetch('/api/auth/cstoa/oauth/status')).json())";
		await browser.navigate(`${origin}/login?provider=cstoa&method=oauth`);
		await checkScan();
		assert.equal(
			await browser.evaluate(`(async()=>${status}.deviceCode.userCode)()`),
			"123456",
			"设备授权协议字段保留，界面不将它作为登录验证码",
		);
		assert.equal(
			await browser.evaluate(`(async()=>${status}.deviceCode.verificationUri)()`),
			`${oa.host}/#/oauth/device?code=123456`,
		);
		await browser.screenshot(join(root, "desktop-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.screenshot(join(root, "desktop-dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await checkScan();
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		await browser.screenshot(join(root, "narrow-dark.png"));
		const firstFlow = await browser.evaluate<string>(`(async()=>${status}.flowId)()`);
		await browser.click(".login-qr-actions button:first-child");
		await browser.until(
			`(async()=>${status}.flowId!==${JSON.stringify(firstFlow)} && !!document.querySelector('.login-qr-box svg'))()`,
			"刷新二维码产生新授权流程",
		);
		await checkScan();
		await browser.click(".login-qr-actions button:last-child");
		await browser.until("document.querySelector('.login-fields')?.textContent.includes('已取消')", "取消扫码授权");
		await browser.click(".login-fields .login-submit");
		await checkScan();
		await browser.click(".login-close");
		await browser.until("location.pathname==='/'", "关闭登录页");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1600,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings/provider?provider=cstoa&method=oauth`);
		await checkScan();
		options.pendingTimes = 0;
		await browser.until(
			"location.pathname==='/account' && !!document.querySelector('.account-actions button')",
			"模拟手机 OA 批准后进入账号页",
		);
		await browser.navigate(`${origin}/login?provider=device-probe&method=oauth`);
		await browser.until(
			"document.querySelector('.login-qr-code')?.textContent.includes('ABCD-EFGH')",
			"其他服务的必要设备码保留",
		);
		await browser.click(".login-close");
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await oa.close();
		if (previous.home === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous.home;
		if (previous.host === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previous.host;
		await rm(home, { recursive: true, force: true });
	}
});
