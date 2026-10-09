import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("登录返回：CSTOA入口切换APIKEY/其他OAuth回模型服务，CSTOA回调到账号", { timeout: 45000 }, async () => {
	const root = join(await testRoot(), "login-navigation");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "settings.json"), "{}");
	const callbacks = new Map<string, (code: string) => void>();
	const completed: string[] = [];
	const external = createServer((req, res) => {
		const url = new URL(req.url ?? "/", "http://localhost");
		if (url.pathname === "/authorize") {
			res.writeHead(302, { Location: `/callback?state=${url.searchParams.get("state")}` });
			res.end();
			return;
		}
		const state = url.searchParams.get("state");
		if (url.pathname === "/callback" && state && callbacks.has(state)) {
			callbacks.get(state)!("test-code");
			res.writeHead(200, { "Content-Type": "text/html" });
			res.end("Authorization completed");
			return;
		}
		res.writeHead(404);
		res.end();
	});
	const externalPort = await freePort();
	await new Promise<void>((r) => external.listen(externalPort, "127.0.0.1", r));
	const externalOrigin = `http://127.0.0.1:${externalPort}`;
	const previousHost = process.env.CSTOA_OA_HOST;
	process.env.CSTOA_OA_HOST = externalOrigin;
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const { modelRuntime } = await pool.getServices();
	for (const id of ["cstoa", "a-external"]) {
		modelRuntime.registerProvider(id, {
			name: id === "cstoa" ? "CSTOA" : "A external OAuth",
			baseUrl: `${externalOrigin}/v1`,
			api: "openai-completions",
			models: [],
			oauth: {
				name: id,
				login: async (input) => {
					const token = randomUUID();
					await new Promise<void>((resolve, reject) => {
						const cancel = () => {
							callbacks.delete(token);
							reject(new Error("Cancelled"));
						};
						callbacks.set(token, () => {
							callbacks.delete(token);
							input.signal?.removeEventListener("abort", cancel);
							resolve();
						});
						input.signal?.addEventListener("abort", cancel, { once: true });
						input.onAuth({
							url: `${externalOrigin}/authorize?state=${token}&redirect_uri=${encodeURIComponent(`${externalOrigin}/callback`)}`,
							instructions: "完成本机模拟授权",
						});
					});
					completed.push(id);
					return { access: `test-${id}`, refresh: "", expires: Date.now() + 3600000 };
				},
				refreshToken: async (value) => value,
				getApiKey: (value) => value.access,
			},
		});
	}
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`,
		api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "login-return" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const entry = `${origin}/login?provider=cstoa&method=oauth&returnTo=%2Faccount`;
		await browser.navigate(entry);
		await browser.until(
			"document.querySelector('.login-card')?.dataset.method==='cstoa'&&!!document.querySelector('.oauth-auth-link')",
			"CSTOA登录入口",
		);
		await browser.click(".login-switch button:nth-child(3)");
		await browser.until("document.querySelector('.login-card')?.dataset.method==='api_key'", "切换APIKEY");
		// 通过浏览器服务选择器选择内置OpenAI，真正由Pi完成密钥保存。
		await browser.click(".login-provider-picker button");
		await browser.until("!!document.querySelector('[data-key=\"openai\"]')", "OpenAI登录选项");
		await browser.click('[data-key="openai"]');
		await browser.fill(".login-key-field input", "test-login-key");
		await browser.click(".login-api-form .login-submit");
		await browser.until("location.pathname==='/settings'&&location.hash==='#accounts'", "APIKEY返回模型服务");
		assert.equal(
			(await modelRuntime.listCredentials()).find((item) => item.providerId === "openai")?.type,
			"api_key",
		);
		await browser.navigate(entry);
		await browser.until("document.querySelector('.login-card')?.dataset.method==='cstoa'", "重新进入CSTOA");
		await browser.click(".login-switch button:nth-child(2)");
		await browser.until(
			"document.querySelector('.login-card')?.dataset.method==='oauth'&&document.querySelector('.login-provider-picker')?.textContent.includes('A external OAuth')&&!!document.querySelector('.oauth-auth-link')",
			"其他OAuth",
		);
		await browser.click(".oauth-auth-link");
		await browser.until("location.pathname==='/settings'&&location.hash==='#accounts'", "其他OAuth回调返回模型服务");
		assert.ok(completed.includes("a-external"));
		await browser.navigate(`${origin}/settings/provider?provider=cstoa&method=oauth&returnTo=%2Fsettings%23accounts`);
		await browser.until(
			"document.querySelector('.provider-config-form')?.dataset.method==='cstoa'&&!!document.querySelector('.oauth-auth-link')",
			"设置中的CSTOA授权",
		);
		await browser.click(".oauth-auth-link");
		await browser.until("location.pathname==='/account'", "CSTOA真实回调返回账号");
		assert.ok(completed.includes("cstoa"));
		assert.equal((await modelRuntime.listCredentials()).find((item) => item.providerId === "cstoa")?.type, "oauth");
		await browser.screenshot(join(root, "cstoa-account.png"));
		await browser.navigate(entry);
		await browser.until("document.querySelector('.login-card')?.dataset.method==='cstoa'", "取消测试就绪");
		await browser.click(".login-switch button:nth-child(3)");
		await browser.click(".login-close");
		await browser.until("location.pathname==='/account'", "取消保留原返回地址");
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{
					apiKey: "/settings#accounts",
					otherOAuth: "/settings#accounts",
					cstoaOAuth: "/account",
					cancelUnchanged: true,
					callbacks: completed,
				},
				null,
				2,
			),
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		external.closeAllConnections();
		await new Promise<void>((r) => external.close(() => r()));
		await rm(home, { recursive: true, force: true });
		if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previousHost;
	}
});
