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

test(
	"浏览器 CSTOA APIKEY 登录：账号页退出、侧栏菜单、固定地址与模型目录，桌面浅色和窄屏深色",
	{ timeout: 90_000 },
	async () => {
		const root = join(await testRoot(), "cstoa-key-login");
		await mkdir(root, { recursive: true });
		const home = await mkdtemp(join(root, "home-"));
		await writeFile(join(home, "settings.json"), "{}");
		await cp(fileURLToPath(new URL("../../oauth", import.meta.url)), join(home, "extensions", "oauth"), {
			recursive: true,
		});
		const modelIds = ["deepseek-flash", "MiniMax-M3.1-Flash"];
		const oa = await startMockOa({ keyModels: modelIds });
		const previousHost = process.env.CSTOA_OA_HOST,
			previousHome = process.env.PI_CODING_AGENT_DIR;
		process.env.CSTOA_OA_HOST = oa.host;
		process.env.PI_CODING_AGENT_DIR = home;
		const pool = new WebSessionPool({
			cwd: home,
			agentDir: home,
			sessionDir: join(home, "sessions"),
			withLoader: (load) => load(),
		});
		const port = await freePort();
		const api = createWebApi(pool, home, port);
		const server = createWebServer(
			fileURLToPath(new URL("../static", import.meta.url)),
			port,
			() => ({ sessions: [], stage: "test" }),
			api,
		);
		await listenWebServer(server, port);
		const origin = `http://127.0.0.1:${port}`;
		let browser: BrowserProbe | undefined;
		try {
			browser = await BrowserProbe.launch(root);
			for (const [theme, width, height] of [
				["light", 1440, 1000],
				["dark", 480, 900],
			] as const) {
				await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme }));
				await browser.call("Emulation.setDeviceMetricsOverride", {
					width,
					height,
					deviceScaleFactor: 1,
					mobile: false,
				});
				await browser.navigate(`${origin}/account`);
				if (
					width < 900 &&
					(await browser.evaluate("document.querySelector('.sidebar')?.getAttribute('aria-hidden')==='false'"))
				)
					await browser.click('button[aria-label="收起侧栏"]');
				await browser.until("!!document.querySelector('.account-actions a[href*=api_key]')", "APIKEY 入口");
				await browser.click(".account-actions a[href*=api_key]");
				await browser.until("document.querySelector('.login-card')?.dataset.method==='api_key'", "密钥表单");
				assert.equal(
					await browser.evaluate("document.querySelector('.login-custom-checkbox input')?.disabled"),
					true,
				);
				await browser.fill(".login-key-field input", "sk-test-key");
				await browser.click(".login-submit");
				await browser.until(
					"location.pathname==='/account' && document.querySelector('.account-fields')?.textContent.includes('20230001')",
					"密钥登录后返回本人账号",
				);
				assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
				assert.equal(
					await browser.evaluate(
						"JSON.stringify({...localStorage}).includes('sk-test-key')||JSON.stringify({...sessionStorage}).includes('sk-test-key')",
					),
					false,
				);
				await browser.screenshot(join(root, `key-account-${theme}.png`));
				const models = await (await fetch(`${origin}/api/models`)).json();
				assert.deepEqual(
					models.models
						.filter((model: { provider: string }) => model.provider === "cstoa")
						.map((model: { id: string; name: string }) => ({ id: model.id, name: model.name })),
					modelIds.map((id) => ({ id, name: id })),
				);
				await browser.navigate(`${origin}/settings/models`);
				await browser.until("!!document.querySelector('.settings-search input')", "模型设置");
				await browser.fill(".settings-search input", "cstoa");
				await browser.until("document.querySelectorAll('.settings-model').length===2", "CSTOA 两个可用模型");
				assert.deepEqual(
					await browser.evaluate(
						"[...document.querySelectorAll('.settings-model-name')].map(element=>element.textContent).sort()",
					),
					[...modelIds].sort(),
				);
				await browser.screenshot(join(root, `key-models-${theme}.png`));
				await browser.navigate(`${origin}/account`);
				await browser.until("!!document.querySelector('.account-fields')", "返回账号页");
				if (await browser.evaluate("document.querySelector('.sidebar')?.getAttribute('aria-hidden')==='true'"))
					await browser.click('button[aria-label="展开侧栏"]');
				await browser.click('button[aria-label="账号菜单"]');
				await browser.until("!!document.querySelector('.sidebar-account-menu')", "账号菜单");
				assert.equal(
					await browser.evaluate(
						"document.querySelector('.sidebar-account-menu').textContent.includes('退出 CSTOA')",
					),
					false,
				);
				assert.equal(
					await browser.evaluate(
						"document.querySelector('.sidebar-account-menu').textContent.includes('退出 CST Pilot')",
					),
					true,
				);
				await browser.click('.sidebar-account-menu a[href="/account"]');
				await browser.click(".account-actions button:has(.lucide-log-out)");
				await browser.until(
					"document.querySelector('.account-fields')?.textContent.includes('登录后查看')",
					"退出清空身份",
				);
			}
		} finally {
			await browser?.close();
			await api.close();
			await pool.close();
			await pool.catalog.close();
			server.closeAllConnections();
			await new Promise<void>((resolve) => server.close(() => resolve()));
			await oa.close();
			await rm(home, { recursive: true, force: true });
			if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
			else process.env.CSTOA_OA_HOST = previousHost;
			if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
			else process.env.PI_CODING_AGENT_DIR = previousHome;
		}
	},
);
