import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

const home = await mkdtemp(join(await testRoot(), "web-auth-flows-"));
await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read"] }));
const pool = new WebSessionPool({
	cwd: home,
	agentDir: home,
	sessionDir: join(home, "sessions"),
	withLoader: (load) => load(),
});
const { modelRuntime } = await pool.getServices();
const received: string[] = [];
for (const id of ["web-oauth-probe", "cstoa"]) {
	modelRuntime.registerProvider(id, {
		name: id,
		baseUrl: "https://example.test/v1",
		api: "openai-completions",
		oauth: {
			name: id,
			login: async (callbacks) => {
				callbacks.onAuth({ url: "https://example.test/authorize", instructions: "完成授权后回贴授权码" });
				const code = await callbacks.onManualCodeInput!();
				received.push(code);
				const organization = await callbacks.onSelect!({
					message: "选择组织",
					options: [{ id: "team", label: "维护队" }],
				});
				assert.ok(organization);
				received.push(organization);
				return { access: "probe-access-secret", refresh: "probe-refresh-secret", expires: Date.now() + 3_600_000 };
			},
			refreshToken: async (credentials) => credentials,
			getApiKey: (credentials) => credentials.access,
		},
		models: [],
	});
}
const probe = createNetServer();
await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
const { port } = probe.address() as AddressInfo;
await new Promise<void>((resolve) => probe.close(() => resolve()));
const origin = `http://127.0.0.1:${port}`;
const api = createWebApi(pool, home, port);
const server = createWebServer(
	fileURLToPath(new URL("../static/", import.meta.url)),
	port,
	() => ({ sessions: [], stage: "test" }),
	api,
);
await listenWebServer(server, port);
after(async () => {
	await api.close();
	await pool.close();
	server.closeAllConnections();
	await new Promise<void>((resolve) => server.close(() => resolve()));
	await rm(home, { recursive: true, force: true });
});
const post = (path: string, value: unknown = {}, key = randomUUID()) =>
	fetch(`${origin}${path}`, {
		method: "POST",
		headers: { Origin: origin, "X-CST-Web-Request": "1", "Content-Type": "application/json", "Idempotency-Key": key },
		body: JSON.stringify(value),
	});
type Status = {
	state: string;
	flowId?: string;
	authUrl?: { url: string };
	prompt?: { id: string; type: string; options?: { id: string }[] };
	error?: string;
};
const status = async (provider = "web-oauth-probe"): Promise<Status> =>
	(await fetch(`${origin}/api/auth/${provider}/oauth/status`)).json();
async function until(predicate: (value: Status) => boolean, provider = "web-oauth-probe"): Promise<Status> {
	for (let i = 0; i < 100; i++) {
		const value = await status(provider);
		if (predicate(value)) return value;
		await new Promise((resolve) => setTimeout(resolve, 20));
	}
	throw new Error("OAuth 状态未按预期更新");
}

test("登录能力直接来自 Pi，内置 OAuth 服务无需手动注册即可展示", async () => {
	const auth = await (await fetch(`${origin}/api/auth`)).json();
	for (const id of ["anthropic", "github-copilot", "kimi-coding", "openai-codex"]) {
		assert.equal(
			auth.providers.find((provider: { id: string; supportsOAuth: boolean }) => provider.id === id)?.supportsOAuth,
			true,
			id,
		);
	}
});

test("普通 OAuth：授权链接、回贴码、选择与完成均走真实 Pi login", async () => {
	const started = await post("/api/auth/web-oauth-probe/oauth/start");
	assert.equal(started.status, 202);
	let step = await until((value) => value.prompt?.type === "manual_code");
	assert.equal(step.authUrl?.url, "https://example.test/authorize");
	assert.ok(step.flowId);
	const reply = await post("/api/auth/web-oauth-probe/oauth/respond", {
		flowId: step.flowId,
		promptId: step.prompt?.id,
		value: "authorization-code-secret",
	});
	assert.equal(reply.status, 200);
	assert.equal((await reply.text()).includes("authorization-code-secret"), false);
	step = await until((value) => value.prompt?.type === "select");
	assert.deepEqual(
		step.prompt?.options?.map((option) => option.id),
		["team"],
	);
	assert.equal(
		(
			await post("/api/auth/web-oauth-probe/oauth/respond", {
				flowId: step.flowId,
				promptId: step.prompt?.id,
				value: "unknown",
			})
		).status,
		400,
	);
	assert.equal(
		(
			await post("/api/auth/web-oauth-probe/oauth/respond", {
				flowId: step.flowId,
				promptId: step.prompt?.id,
				value: "team",
			})
		).status,
		200,
	);
	const done = await until((value) => value.state === "succeeded");
	assert.equal(done.prompt, undefined);
	assert.deepEqual(received, ["authorization-code-secret", "team"]);
	const auth = await (await fetch(`${origin}/api/auth`)).text();
	assert.equal(auth.includes("probe-access-secret"), false);
	assert.equal(auth.includes("probe-refresh-secret"), false);
	assert.equal(auth.includes('"type":"oauth"'), true);
});

test("普通 OAuth：取消会清掉待回答步骤，旧步骤不能写入新流程", async () => {
	await post("/api/auth/web-oauth-probe/oauth/start");
	const old = await until((value) => !!value.prompt);
	assert.equal((await post("/api/auth/web-oauth-probe/oauth/cancel")).status, 200);
	const cancelled = await until((value) => value.state === "cancelled");
	assert.equal(cancelled.prompt, undefined);
	await post("/api/auth/web-oauth-probe/oauth/start");
	const next = await until((value) => !!value.prompt && value.flowId !== old.flowId);
	assert.equal(
		(
			await post("/api/auth/web-oauth-probe/oauth/respond", {
				flowId: old.flowId,
				promptId: old.prompt?.id,
				value: "stale",
			})
		).status,
		409,
	);
	await post("/api/auth/web-oauth-probe/oauth/cancel", { flowId: old.flowId });
	assert.equal((await status()).flowId, next.flowId);
	assert.equal((await status()).state, "pending");
	await post("/api/auth/web-oauth-probe/oauth/cancel", { flowId: next.flowId });
});

test("登出会取消进行中的 OAuth，避免授权完成后恢复已退出的凭据", async () => {
	await post("/api/auth/web-oauth-probe/oauth/start");
	await until((value) => !!value.prompt);
	assert.equal((await post("/api/auth/web-oauth-probe/logout")).status, 200);
	assert.equal((await until((value) => value.state === "cancelled")).prompt, undefined);
});

test("账号信息只取 cstoa 状态，资料与额度未接入时明确返回空值", async () => {
	await modelRuntime.login("openai", "api_key", { prompt: async () => "other-provider-key", notify: () => {} });
	const account = await fetch(`${origin}/api/account`);
	assert.equal(account.status, 200);
	const value = await account.json();
	assert.equal(value.providerId, "cstoa");
	assert.equal(value.signedIn, false);
	assert.equal(value.profile.studentId, null);
	assert.equal(value.profile.name, null);
	assert.equal(value.profile.supported, false);
	assert.equal(value.quota.supported, false);
	assert.equal(value.quota.balance, null);
});

test("cstoa OAuth 与其他服务 API Key 共存，退出专属账号不清除其他凭据", async () => {
	await post("/api/auth/cstoa/oauth/start");
	let step = await until((value) => !!value.prompt, "cstoa");
	await post("/api/auth/cstoa/oauth/respond", { flowId: step.flowId, promptId: step.prompt?.id, value: "team-code" });
	step = await until((value) => value.prompt?.type === "select", "cstoa");
	await post("/api/auth/cstoa/oauth/respond", { flowId: step.flowId, promptId: step.prompt?.id, value: "team" });
	await until((value) => value.state === "succeeded", "cstoa");
	assert.equal((await (await fetch(`${origin}/api/account`)).json()).signedIn, true);
	assert.equal((await modelRuntime.listCredentials()).find((item) => item.providerId === "openai")?.type, "api_key");
	await post("/api/auth/cstoa/logout");
	assert.equal((await (await fetch(`${origin}/api/account`)).json()).signedIn, false);
	assert.equal((await modelRuntime.listCredentials()).find((item) => item.providerId === "openai")?.type, "api_key");
});

test("同一模型服务按 Pi 规则只保留最近登录的一种凭据", async () => {
	modelRuntime.registerProvider("anthropic", {
		oauth: {
			name: "测试 OAuth",
			login: async () => ({ access: "replacement-access", refresh: "", expires: Date.now() + 3_600_000 }),
			refreshToken: async (value) => value,
			getApiKey: (value) => value.access,
		},
	});
	await modelRuntime.login("anthropic", "oauth", { prompt: async () => "", notify: () => {} });
	assert.equal((await modelRuntime.listCredentials()).find((item) => item.providerId === "anthropic")?.type, "oauth");
	await modelRuntime.login("anthropic", "api_key", { prompt: async () => "replacement-key", notify: () => {} });
	const credentials = (await modelRuntime.listCredentials()).filter((item) => item.providerId === "anthropic");
	assert.deepEqual(credentials, [{ providerId: "anthropic", type: "api_key" }]);
});
