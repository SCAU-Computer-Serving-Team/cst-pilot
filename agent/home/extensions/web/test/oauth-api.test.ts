/**
 * Web 扫码登录路由测试：隔离 agentDir 里加载真实 oauth 扩展，
 * mock OA 走 start → device_code 推送/查询 → 自动批准 → succeeded。
 */

import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

const base = await testRoot();
const home = await mkdtemp(join(base, "cst-web-oauth-"));
await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
// 把真实 oauth 扩展放进隔离 agentDir；pool 加载后 modelRuntime 即注册 cstoa provider。
await cp(fileURLToPath(new URL("../../oauth", import.meta.url)), join(home, "extensions", "oauth"), {
	recursive: true,
});

const mock = await startMockOa();
process.env.CSTOA_OA_HOST = mock.host;
// 发行版由 pi.cmd 设置该变量；测试显式指向隔离目录，保证 device.json 不落到真实配置。
const previousAgentDir = process.env.PI_CODING_AGENT_DIR;
process.env.PI_CODING_AGENT_DIR = home;

const pool = new WebSessionPool({
	cwd: home,
	agentDir: home,
	sessionDir: join(home, "sessions"),
	withLoader: (load) => load(),
});
await pool.create();
const probe = createNetServer();
await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
const { port } = probe.address() as AddressInfo;
await new Promise<void>((resolve) => probe.close(() => resolve()));
const origin = `http://127.0.0.1:${port}`;
const api = createWebApi(pool, home, port);
const server = createWebServer(
	fileURLToPath(new URL("../static/", import.meta.url)),
	port,
	() => ({
		sessions: [],
		stage: "test",
	}),
	api,
);
await listenWebServer(server, port);

after(async () => {
	server.closeAllConnections();
	await new Promise<void>((resolve) => server.close(() => resolve()));
	await pool.close();
	await api.close();
	await mock.close();
	delete process.env.CSTOA_OA_HOST;
	if (previousAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
	else process.env.PI_CODING_AGENT_DIR = previousAgentDir;
	await rm(home, { recursive: true, force: true });
});

const post = (path: string, value: unknown) =>
	fetch(`${origin}${path}`, {
		method: "POST",
		headers: {
			Origin: origin,
			"X-CST-Web-Request": "1",
			"Content-Type": "application/json",
			"Idempotency-Key": randomUUID(),
		},
		body: JSON.stringify(value),
	});
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const oauthStatus = async () => {
	const response = await fetch(`${origin}/api/auth/cstoa/oauth/status`);
	return (await response.json()) as {
		state: string;
		deviceCode?: { userCode: string; verificationUri: string; expiresInSeconds: number };
		error?: string;
	};
};

test("扫码登录：start 后 device_code 可查，批准后凭据写入 auth.json", async () => {
	const started = await post("/api/auth/cstoa/oauth/start", {});
	assert.equal(started.status, 202);
	const startedBody = (await started.json()) as {
		started: boolean;
		deviceCode?: { userCode: string; verificationUri: string; expiresInSeconds: number };
	};
	assert.equal(startedBody.started, true);
	assert.equal(startedBody.deviceCode?.userCode, "123456");
	assert.equal(startedBody.deviceCode?.verificationUri, `${mock.host}/#/oauth/device?code=123456`);
	assert.ok((startedBody.deviceCode?.expiresInSeconds ?? 0) > 0);
	assert.equal(JSON.stringify(startedBody).includes("device-code-1"), false);

	let status = await oauthStatus();
	for (let i = 0; i < 100 && !(status.state === "pending" && status.deviceCode); i++) {
		await sleep(50);
		status = await oauthStatus();
	}
	assert.equal(status.state, "pending");
	assert.equal(status.deviceCode?.userCode, "123456");
	assert.equal(status.deviceCode?.verificationUri, `${mock.host}/#/oauth/device?code=123456`);
	// device_code 是换令牌的秘密，任何响应都不能回传。
	assert.equal(JSON.stringify(status).includes("device-code-1"), false);

	for (let i = 0; i < 100 && status.state !== "succeeded"; i++) {
		await sleep(100);
		status = await oauthStatus();
	}
	assert.equal(status.state, "succeeded");

	const auth = (await (await fetch(`${origin}/api/auth`)).json()) as {
		providers: { id: string; type: string | null; supportsOAuth: boolean }[];
	};
	const cstoa = auth.providers.find((provider) => provider.id === "cstoa");
	assert.equal(cstoa?.supportsOAuth, true);
	assert.equal(cstoa?.type, "oauth");

	const stored = JSON.parse(await readFile(join(home, "auth.json"), "utf8")) as {
		cstoa?: { access?: string; refresh?: string };
	};
	assert.equal(stored.cstoa?.access, "access-1");
	assert.equal(stored.cstoa?.refresh, "refresh-1");
});

test("扫码登录：device_code 事件经 /api/events 推送", async () => {
	const controller = new AbortController();
	const stream = await fetch(`${origin}/api/events`, { signal: controller.signal });
	const reader = stream.body!.getReader();
	const decoder = new TextDecoder();
	let buffer = "";
	try {
		// 先读到 `: connected`，确保订阅建立后再触发登录。
		buffer += decoder.decode((await reader.read()).value, { stream: true });
		await post("/api/auth/cstoa/oauth/start", {});
		const deadline = Date.now() + 10_000;
		while (Date.now() < deadline && !buffer.includes("oauth_device_code")) {
			const { value, done } = await reader.read();
			if (done) break;
			buffer += decoder.decode(value, { stream: true });
		}
		assert.ok(buffer.includes("oauth_device_code"), "应推送设备码事件");
		assert.ok(buffer.includes("123456"), "事件应包含 user_code");
	} finally {
		controller.abort();
		await reader.cancel().catch(() => undefined);
		await post("/api/auth/cstoa/oauth/cancel", {});
	}
});

test("扫码登录：status 返回真实剩余秒数而不是固定重置", async () => {
	// 倒计时期间保持待批准，避免自动批准先清除设备码。
	const pending = await startMockOa({ pendingTimes: 1000 });
	const previousHost = process.env.CSTOA_OA_HOST;
	process.env.CSTOA_OA_HOST = pending.host;
	try {
		await post("/api/auth/cstoa/oauth/start", {});
		const first = await oauthStatus();
		await sleep(1_200);
		const second = await oauthStatus();
		assert.equal(first.state, "pending");
		assert.equal(second.state, "pending");
		assert.ok(first.deviceCode?.expiresInSeconds);
		assert.ok(second.deviceCode?.expiresInSeconds);
		assert.ok(second.deviceCode.expiresInSeconds < first.deviceCode.expiresInSeconds);
	} finally {
		await post("/api/auth/cstoa/oauth/cancel", {});
		if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previousHost;
		await pending.close();
	}
});

test("扫码登录：取消后状态为 cancelled，可重新发起", async () => {
	await post("/api/auth/cstoa/oauth/start", {});
	await sleep(100);
	const cancelled = await post("/api/auth/cstoa/oauth/cancel", {});
	assert.equal(cancelled.status, 200);
	assert.equal(((await cancelled.json()) as { cancelled: boolean }).cancelled, true);

	let status = await oauthStatus();
	for (let i = 0; i < 50 && status.state !== "cancelled"; i++) {
		await sleep(50);
		status = await oauthStatus();
	}
	assert.equal(status.state, "cancelled");

	const restarted = await post("/api/auth/cstoa/oauth/start", {});
	assert.equal(restarted.status, 202);
	status = await oauthStatus();
	for (let i = 0; i < 50 && status.state !== "pending"; i++) {
		await sleep(50);
		status = await oauthStatus();
	}
	assert.equal(status.state, "pending");
	await post("/api/auth/cstoa/oauth/cancel", {});
});

test("扫码登录：不支持 OAuth 的服务返回 400", async () => {
	const response = await post("/api/auth/not-a-provider/oauth/start", {});
	assert.equal(response.status, 400);
});
