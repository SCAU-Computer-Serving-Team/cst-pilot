import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProviderStatus } from "../data/api";
import { providerDescription, settingsProviders } from "./provider-list.ts";

const provider = (id: string, changes: Partial<ProviderStatus> = {}): ProviderStatus => ({
	id,
	name: id,
	supportsApiKey: true,
	supportsOAuth: false,
	type: null,
	requiresLogin: false,
	...changes,
});
test("已配置与可添加互斥，CSTOA置顶，失效凭据留在已配置列表", () => {
	const all = [
		provider("z-key", { type: "api_key" }),
		provider("a-oauth", { supportsOAuth: true }),
		provider("cstoa", { type: "oauth" }),
		provider("expired", { requiresLogin: true }),
		provider("unavailable", { supportsApiKey: false }),
	];
	const result = settingsProviders(all);
	assert.deepEqual(
		result.configured.map((item) => item.id),
		["cstoa", "expired", "z-key"],
	);
	assert.deepEqual(
		result.available.map((item) => item.id),
		["a-oauth"],
	);
	assert.deepEqual(settingsProviders(all, "  A-OAuth ").available, result.available);
	assert.deepEqual(settingsProviders(all, "no-such-provider").available, []);
});
test("服务说明区分配置、授权、失效和能力，不把有密钥表述为已验证可调用", () => {
	assert.equal(providerDescription(provider("key", { type: "api_key" })), "API key · 已配置");
	assert.equal(providerDescription(provider("oauth", { type: "oauth" })), "账号授权 · 已登录");
	assert.equal(providerDescription(provider("expired", { requiresLogin: true })), "凭据已失效 · 需要重新登录");
	assert.equal(providerDescription(provider("both", { supportsOAuth: true })), "OAuth · API key");
});
