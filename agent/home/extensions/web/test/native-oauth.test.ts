import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { OAuthFlows } from "../server/api/oauth-flow.ts";
import { testRoot } from "./support.ts";

test("Pi 原生回环 OAuth：回调、state 校验、凭据保存与取消，无需手动回应", async () => {
	const home = await mkdtemp(join(await testRoot(), "native-oauth-"));
	const runtime = await ModelRuntime.create({
		authPath: join(home, "auth.json"),
		modelsPath: null,
		allowModelNetwork: false,
	});
	const flows = new OAuthFlows(() => {});
	const realFetch = globalThis.fetch;
	const exchanges: string[] = [];
	globalThis.fetch = async (input, init) => {
		const url = new URL(input instanceof Request ? input.url : String(input));
		if (
			url.href === "https://platform.claude.com/v1/oauth/token" ||
			url.href === "https://auth.openai.com/oauth/token"
		) {
			exchanges.push(url.hostname);
			const payload = Buffer.from(
				JSON.stringify({ "https://api.openai.com/auth": { chatgpt_account_id: "test-account" } }),
			).toString("base64url");
			return Response.json({
				access_token: `test.${payload}.test`,
				refresh_token: "test-refresh",
				expires_in: 3600,
			});
		}
		if (url.href === "https://openrouter.ai/api/v1/auth/keys") {
			exchanges.push(url.hostname);
			return Response.json({ key: "test-openrouter-key" });
		}
		if (!["127.0.0.1", "localhost"].includes(url.hostname)) throw new Error("测试禁止外网请求");
		return realFetch(input, init);
	};
	const until = async (provider: string, ready: (view: ReturnType<OAuthFlows["status"]>) => boolean) => {
		for (let attempt = 0; attempt < 200; attempt++) {
			const value = flows.status(provider);
			if (ready(value)) return value;
			if (value.state === "failed") throw new Error(value.error);
			await new Promise((resolve) => setTimeout(resolve, 20));
		}
		throw new Error(`等待 ${provider} 授权超时`);
	};
	try {
		for (const provider of ["anthropic", "openai-codex", "openrouter"]) {
			const flow = flows.start(runtime, provider);
			let view = await flows.ready(provider, flow);
			if (provider === "openai-codex") {
				assert.ok(view.state !== "idle" && view.prompt?.type === "select");
				flows.respond(provider, view.flowId, view.prompt.id, "browser");
			}
			view = await until(provider, (value) => value.state === "pending" && !!value.authUrl);
			assert.ok(view.state === "pending" && view.authUrl);
			const authorization = new URL(view.authUrl.url);
			const callback = new URL(
				authorization.searchParams.get("redirect_uri") ?? authorization.searchParams.get("callback_url")!,
			);
			// 避免 Node 的 localhost 解析成 ::1，原请求 URI 与 state 保持不变。
			callback.hostname = "127.0.0.1";
			callback.searchParams.set("code", "test-code");
			const state = authorization.searchParams.get("state");
			if (state) {
				callback.searchParams.set("state", "wrong-state");
				assert.equal((await fetch(callback)).status, 400);
				assert.equal(flows.status(provider).state, "pending");
				callback.searchParams.set("state", state);
			}
			assert.equal((await fetch(callback)).status, 200);
			await until(provider, (value) => value.state === "succeeded");
			assert.equal((await runtime.listCredentials()).find((item) => item.providerId === provider)?.type, "oauth");
			assert.equal(flows.status(provider).state, "succeeded");
			await flow.task;
		}
		assert.deepEqual(exchanges, ["platform.claude.com", "auth.openai.com", "openrouter.ai"]);
		const pending = flows.start(runtime, "anthropic");
		await flows.ready("anthropic", pending);
		flows.cancel("anthropic");
		await pending.task;
		assert.equal(flows.status("anthropic").state, "cancelled");
		assert.equal((await runtime.listCredentials()).find((item) => item.providerId === "anthropic")?.type, "oauth");
	} finally {
		await flows.close();
		globalThis.fetch = realFetch;
		await rm(home, { recursive: true, force: true });
	}
});
