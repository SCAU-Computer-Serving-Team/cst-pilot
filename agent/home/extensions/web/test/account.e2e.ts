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

test("账号页面：学号姓名、接口未上线提示、重试与退出", { timeout: 60_000 }, async () => {
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
			if (width < 900) {
				await browser.click('button[aria-label="收起侧栏"]');
				await browser.until('document.querySelector(".sidebar").getBoundingClientRect().right <= 0', "侧栏关闭");
			}
			await browser.until(
				`document.querySelector(".account-fields")?.innerText.includes("20230001") && document.querySelector(".account-fields")?.innerText.includes("测试队员")`,
				"学号姓名显示",
			);
			assert.equal(
				await browser.evaluate(`document.querySelector(".account-fields").innerText.includes("access-1")`),
				false,
			);
			assert.equal(
				await browser.evaluate(
					`document.querySelector(".account-fields").scrollWidth <= document.querySelector(".account-fields").clientWidth`,
				),
				true,
			);
			await browser.screenshot(join(root, `account-${theme}.png`));
		}
		options.profileStatus = 404;
		await browser.navigate(`${origin}/account`);
		if (await browser.evaluate(`document.querySelector(".sidebar")?.getAttribute("aria-hidden") === "false"`))
			await browser.click('button[aria-label="收起侧栏"]');
		await browser.until(`document.querySelector(".account-notice")?.innerText.includes("尚未上线")`, "未上线提示");
		assert.ok(await browser.evaluate(`document.querySelector(".account-actions")?.innerText.includes("退出 CSTOA")`));
		options.profileStatus = 200;
		await browser.click(".account-notice button");
		await browser.until(`document.querySelector(".account-fields")?.innerText.includes("测试队员")`, "资料重试成功");
		await browser.click(".account-actions button");
		await browser.until(
			`document.querySelector(".account-fields")?.innerText.includes("登录后查看")`,
			"退出清除资料",
		);
		assert.equal(
			await browser.evaluate(`document.querySelector(".account-fields").innerText.includes("测试队员")`),
			false,
		);
	} finally {
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
