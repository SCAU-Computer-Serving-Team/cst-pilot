import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { RpcClient } from "@earendil-works/pi-coding-agent";
import { startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("跨端用户链路：网页保存范围和模型、原生扫码、服务配置、新 Pi 进程与 Web 重启", { timeout: 120_000 }, async () => {
	const root = join(await testRoot(), "shared-configuration-e2e");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const previous = {
		agentDir: process.env.PI_CODING_AGENT_DIR,
		oaHost: process.env.CSTOA_OA_HOST,
		offline: process.env.PI_OFFLINE,
	};
	const oa = await startMockOa({ pendingTimes: 2 });
	process.env.PI_CODING_AGENT_DIR = home;
	process.env.CSTOA_OA_HOST = oa.host;
	process.env.PI_OFFLINE = "1";
	await cp(fileURLToPath(new URL("../../oauth/", import.meta.url)), join(home, "extensions", "oauth"), {
		recursive: true,
	});
	const modelPaths: string[] = [];
	const model = createServer(async (request, response) => {
		for await (const _chunk of request) {
			/* 只模拟外部模型服务，不替换产品模块。 */
		}
		modelPaths.push(request.url ?? "");
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.write(
			`data: ${JSON.stringify({ id: "shared", choices: [{ index: 0, delta: { role: "assistant", content: "共享配置链路回答" }, finish_reason: null }] })}\n\n`,
		);
		response.end(
			`data: ${JSON.stringify({ id: "shared", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const modelPort = await freePort();
	await new Promise<void>((resolve) => model.listen(modelPort, "127.0.0.1", resolve));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "probe",
			defaultModel: "two",
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
					models: ["one", "two", "three"].map((id) => ({
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
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	let live: { pool: WebSessionPool; api: ReturnType<typeof createWebApi>; server: Server } | undefined;
	async function startWeb() {
		const pool = new WebSessionPool({
			cwd: home,
			agentDir: home,
			sessionDir: join(home, "sessions"),
			withLoader: (load) => load(),
		});
		const api = createWebApi(pool, home, port);
		const server = createWebServer(
			fileURLToPath(new URL("../static/", import.meta.url)),
			port,
			async () => ({ sessions: await pool.list(), stage: "test" }),
			api,
		);
		await listenWebServer(server, port);
		live = { pool, api, server };
	}
	async function stopWeb() {
		const current = live;
		live = undefined;
		if (!current) return;
		await current.api.close();
		await current.pool.close();
		current.server.closeAllConnections();
		await new Promise<void>((resolve) => current.server.close(() => resolve()));
	}
	function piClient() {
		return new RpcClient({
			cliPath: fileURLToPath(
				new URL("../../../../node_modules/@earendil-works/pi-coding-agent/dist/cli.js", import.meta.url),
			),
			cwd: home,
			env: { PI_CODING_AGENT_DIR: home, PI_OFFLINE: "1", CSTOA_OA_HOST: oa.host },
			args: ["--no-session", "--no-context-files"],
		});
	}
	let browser: BrowserProbe | undefined;
	let pi: RpcClient | undefined;
	const evidence: Record<string, unknown> = {};
	try {
		await startWeb();
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/settings`);
		await browser.until("!!document.querySelector('[aria-label=\"启用 测试模型 one\"]')", "模型范围设置加载");
		await browser.fill(".settings-search input", "测试模型");
		await browser.click('.settings-model:has([aria-label="启用 测试模型 one"]) .settings-check-box');
		await browser.click(".settings-save");
		await browser.until(
			"document.querySelector('.settings-save')?.textContent.includes('已保存')",
			"网页保存模型范围",
		);
		await browser.navigate(`${origin}/`);
		await browser.until("document.querySelector('.model-label')?.textContent.includes('two')", "默认模型 two");
		await browser.click(".composer-model");
		await browser.until("document.querySelectorAll('.model-popover [role=option]').length===2", "网页可选范围");
		assert.deepEqual(
			await browser.evaluate(
				"[...document.querySelectorAll('.model-popover [role=option]')].map(e=>e.textContent.trim())",
			),
			["测试模型 two", "测试模型 one"],
		);
		await browser.click('.model-popover [role="option"][data-key="probe/one"]');
		await browser.until("document.querySelector('.model-label')?.textContent.includes('one')", "主页选择 one");
		pi = piClient();
		await pi.start();
		const selected = await pi.getState();
		assert.equal(selected.model?.id, "one", "新 Pi 进程读取网页保存的默认模型");
		assert.equal((await pi.cycleModel())?.model.id, "two");
		assert.equal((await pi.cycleModel())?.model.id, "one");
		evidence.sharedModelScope = { selected: selected.model?.id, cycle: ["two", "one"] };
		await pi.stop();
		pi = undefined;

		await browser.navigate(`${origin}/account`);
		await browser.until("!!document.querySelector('.account-actions a')", "专属账号页");
		await browser.click(".account-actions a");
		await browser.until(
			"document.querySelector('.login-qr-code')?.textContent.includes('123456')",
			"真实 cstoa 扩展请求设备码",
		);
		await browser.click(".login-close");
		await browser.until("location.pathname==='/account'", "取消返回账号页");
		await browser.until(
			"(async()=> (await (await fetch('/api/auth/cstoa/oauth/status')).json()).state==='cancelled')()",
			"取消设备授权",
		);
		assert.equal(
			await browser.evaluate("(async()=> (await (await fetch('/api/account')).json()).signedIn)()"),
			false,
		);
		await browser.click(".account-actions a");
		await browser.until(
			"location.pathname==='/account' && document.querySelector('.account-actions button')?.textContent.includes('退出 CSTOA')",
			"OA 设备授权后返回账号页",
		);
		evidence.cstoaDeviceFlow = { cancelledWithoutLogin: true, approved: true };

		await browser.navigate(`${origin}/settings/provider?provider=probe&method=api_key`);
		await browser.until(
			"!!document.querySelector('.login-api-form') && !!document.querySelector('.login-custom-checkbox')",
			"服务配置表单",
		);
		await browser.click(".login-custom-checkbox");
		const endpoint = `http://127.0.0.1:${modelPort}/custom/v1`;
		await browser.fill('input[type="url"]', endpoint);
		await browser.fill(".login-key-field input", "shared-e2e-key");
		await browser.click(".login-api-form .login-submit");
		await browser.until("location.pathname==='/settings'", "保存服务返回设置");
		await stopWeb();
		await startWeb();
		await browser.navigate(`${origin}/account`);
		await browser.until(
			"document.querySelector('.account-actions button')?.textContent.includes('退出 CSTOA')",
			"Web 重启恢复账号登录态",
		);
		pi = piClient();
		await pi.start();
		const restarted = await pi.getState();
		assert.equal(restarted.model?.id, "one");
		assert.equal(restarted.model?.baseUrl, endpoint, "Pi 新进程使用网页保存的 Provider 地址");
		assert.ok(
			(await pi.getAvailableModels()).some((item) => item.provider === "cstoa"),
			"Pi 新进程读取扫码凭据",
		);
		await pi.promptAndWait("Pi 进程验证共享配置", undefined, 15_000);
		assert.equal(await pi.getLastAssistantText(), "共享配置链路回答");
		await pi.stop();
		pi = undefined;
		await browser.navigate(`${origin}/`);
		await browser.until("document.querySelector('.model-label')?.textContent.includes('one')", "Web 再进入恢复模型");
		await browser.fill(".composer-input", "网页验证共享配置");
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"location.pathname.startsWith('/s/') && document.querySelector('.conversation')?.textContent.includes('共享配置链路回答')",
			"网页使用同一 Provider 发消息",
		);
		assert.ok(modelPaths.length >= 2 && modelPaths.every((path) => path === "/custom/v1/chat/completions"));
		evidence.restartAndProvider = {
			defaultModel: "one",
			bothChannelsReachedConfiguredEndpoint: true,
			cstoaCredentialRestored: true,
		};

		await browser.navigate(`${origin}/account`);
		await browser.until("!!document.querySelector('.account-actions button')", "账号退出入口");
		await browser.click(".account-actions button");
		await browser.until(
			"!document.querySelector('.account-actions button') && document.querySelector('.account-actions a')?.textContent.includes('扫码登录')",
			"账号退出完成",
		);
		pi = piClient();
		await pi.start();
		assert.equal(
			(await pi.getAvailableModels()).some((item) => item.provider === "cstoa"),
			false,
			"退出后新 Pi 进程不保留旧扫码凭据",
		);
		assert.ok(
			(await pi.getAvailableModels()).some((item) => item.provider === "probe"),
			"退出 CSTOA 保留其他 Provider",
		);
		evidence.logoutIsolation = true;

		assert.equal(
			await browser.evaluate(
				"(async()=> (await (await fetch('/api/auth')).json()).providers.find(p=>p.id==='probe')?.type)()",
			),
			"api_key",
			"退出 CSTOA 不清除其他已登录服务凭据",
		);
		await pi.stop();
		pi = undefined;

		// models.json/settings.json 是支持的外部配置输入；修改后只通过页面和新 Pi 进程核查结果。
		const config = JSON.parse(await readFile(join(home, "models.json"), "utf8"));
		const settings = JSON.parse(await readFile(join(home, "settings.json"), "utf8"));
		await writeFile(
			join(home, "models.json"),
			JSON.stringify({
				providers: {
					changed: {
						...config.providers.probe,
						models: [{ ...config.providers.probe.models[0], id: "four", name: "改名后的可选模型" }],
					},
				},
			}),
		);
		await writeFile(
			join(home, "settings.json"),
			JSON.stringify({
				...settings,
				defaultProvider: "changed",
				defaultModel: "four",
				enabledModels: ["changed/four"],
			}),
		);
		pi = piClient();
		await pi.start();
		const changed = await pi.getState();
		assert.equal(changed.model?.provider, "changed");
		assert.equal(changed.model?.id, "four");
		assert.equal(
			(await pi.getAvailableModels()).some((item) => item.provider === "probe"),
			false,
		);
		await pi.stop();
		pi = undefined;
		await browser.navigate(`${origin}/`);
		await browser.until(
			"document.querySelector('.model-label')?.textContent.includes('改名后的可选模型')",
			"再次进入读取 Provider 与模型增删改",
		);
		await browser.click(".composer-model");
		await browser.until("document.querySelectorAll('.model-popover [role=option]').length===1", "新的 scoped 清单");
		assert.deepEqual(
			await browser.evaluate(
				"[...document.querySelectorAll('.model-popover [role=option]')].map(e=>e.textContent.trim())",
			),
			["改名后的可选模型"],
		);
		evidence.providerAndModelChanges = { removedProvider: "probe", newProvider: "changed", selectedModel: "four" };
		await browser.navigate(`${origin}/account`);
		await browser.until(
			"!!document.querySelector('.account-actions a') && !document.querySelector('.account-actions button')",
			"退出态仍保持",
		);
		await browser.screenshot(join(root, "account-after-logout.png"));
		await writeFile(join(root, "report.json"), JSON.stringify(evidence, null, 2));
	} finally {
		await pi?.stop();
		await browser?.close();
		await stopWeb();
		await oa.close();
		model.closeAllConnections();
		await new Promise<void>((resolve) => model.close(() => resolve()));
		if (previous.agentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous.agentDir;
		if (previous.oaHost === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previous.oaHost;
		if (previous.offline === undefined) delete process.env.PI_OFFLINE;
		else process.env.PI_OFFLINE = previous.offline;
		await rm(home, { recursive: true, force: true });
	}
});
