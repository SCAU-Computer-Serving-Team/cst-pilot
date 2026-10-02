import assert from "node:assert/strict";
import { test } from "node:test";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { getProviderQuota } from "../server/api/quota.ts";

function runtime(baseUrl: string, key = "secret"): ModelRuntime {
	return {
		getModels: () => [{ provider: "test", baseUrl }],
		getAuth: async () => ({ auth: { apiKey: key } }),
	} as unknown as ModelRuntime;
}

test("Go and Go Plus reuse the API key and parse three usage windows", async () => {
	let calls = 0;
	const fetcher = async (url: URL | RequestInfo, options?: RequestInit) => {
		calls++;
		assert.equal(String(url), "https://opencode.ai/zen/go/v1/usage");
		assert.equal(new Headers(options?.headers).get("Authorization"), "Bearer secret");
		assert.equal(options?.redirect, "error");
		return Response.json({
			usage: Object.fromEntries(
				["rolling", "weekly", "monthly"].map((name) => [
					name,
					{ status: "ok", percent: 12.5, resetsAt: "2026-09-20T12:00:00Z" },
				]),
			),
		});
	};
	const result = await getProviderQuota("opencode-go", runtime("https://opencode.ai/zen/go"), fetcher as typeof fetch);
	assert.equal(result.supported, true);
	assert.equal(result.windows?.rolling.percent, 12.5);
	assert.equal(result.windows?.monthly.status, "ok");
	assert.equal(result.balance, null);
	assert.equal(
		(await getProviderQuota("opencode-go", runtime("https://opencode.ai/zen/go"), fetcher as typeof fetch)).windows
			?.weekly.percent,
		12.5,
	);
	assert.equal(calls, 1);
});

test("DeepSeek balance is read from the official endpoint, preferring CNY", async () => {
	const result = await getProviderQuota("deepseek", runtime("https://api.deepseek.com/v1"), async (url, options) => {
		assert.equal(String(url), "https://api.deepseek.com/user/balance");
		assert.equal(new Headers(options?.headers).get("Authorization"), "Bearer secret");
		return Response.json({
			is_available: true,
			balance_infos: [
				{ currency: "USD", total_balance: "2.50" },
				{ currency: "CNY", total_balance: "0.00" },
			],
		});
	});
	assert.equal(result.supported, true);
	assert.equal(result.balance, 0);
	assert.equal(result.currency, "CNY");
	assert.equal(result.windows, undefined);
});

test("unknown or custom endpoints are not contacted; malformed replies and errors do not fabricate quota", async () => {
	let calls = 0;
	const fetcher = async () => {
		calls++;
		return Response.json({ usage: {} });
	};
	for (const base of [
		"https://opencode.ai.evil.test/zen/go/v1",
		"http://opencode.ai/zen/go/v1",
		"https://example.com/zen/go/v1",
	]) {
		assert.equal((await getProviderQuota("opencode-go", runtime(base), fetcher as typeof fetch)).supported, false);
	}
	assert.equal(
		(await getProviderQuota("opencode", runtime("https://opencode.ai/zen/go/v1"), fetcher as typeof fetch)).supported,
		false,
	);
	assert.equal(calls, 0);
	const bad = await getProviderQuota(
		"opencode-go",
		runtime("https://opencode.ai/zen/go/v1", "different-key"),
		fetcher as typeof fetch,
	);
	assert.equal(bad.supported, false);
	assert.equal(bad.windows, undefined);
	assert.equal(calls, 1);
	assert.equal(
		(
			await getProviderQuota("deepseek", runtime("https://api.deepseek.com", "different-key"), async () =>
				Response.json({ balance_infos: [] }),
			)
		).supported,
		false,
	);
	assert.equal(
		(await getProviderQuota("deepseek", runtime("https://api.deepseek.com", ""), fetcher as typeof fetch)).reason,
		"缺少凭据",
	);
	assert.equal(
		(
			await getProviderQuota(
				"deepseek",
				runtime("https://api.deepseek.com", "bad-key"),
				async () => new Response(null, { status: 401 }),
			)
		).reason,
		"额度接口返回 401",
	);
});
