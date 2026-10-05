import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProviderStatus } from "../data/api";
import {
	accountProvider,
	chooseLoginTab,
	configurationPath,
	initialLogin,
	loginMethods,
	loginPath,
	loginTabs,
	providerGroups,
	returnPath,
	tabProviders,
} from "./providers.ts";

const provider = (id: string, key = true, oauth = false, type: string | null = null): ProviderStatus => ({
	id,
	name: id,
	supportsApiKey: key,
	supportsOAuth: oauth,
	type,
	requiresLogin: false,
});
const providers = [
	provider("deepseek"),
	provider("kimi-coding", true, true),
	provider("cstoa", false, true),
	provider("openai-codex", false, true),
];
test("cstoa、OAuth、APIKEY 固定三入口，切换到另一方式不受当前 Provider 限制", () => {
	assert.deepEqual(
		loginTabs.map((tab) => tab.label),
		["cstoa", "OAuth", "APIKEY"],
	);
	assert.equal(chooseLoginTab(providers, "cstoa", "deepseek")?.id, "cstoa");
	assert.equal(chooseLoginTab(providers, "oauth", "deepseek")?.id, "kimi-coding");
	assert.equal(chooseLoginTab(providers, "api_key", "cstoa")?.id, "deepseek");
	assert.deepEqual(
		tabProviders(providers, "oauth").map((provider) => provider.id),
		["kimi-coding", "openai-codex"],
	);
	assert.equal(chooseLoginTab([provider("deepseek")], "cstoa", "deepseek"), undefined);
});

test("cstoa 默认优先，明确选择的 API Key 和其他 OAuth 不被覆盖", () => {
	assert.deepEqual(initialLogin(providers, null, null), { providerId: "cstoa", method: "oauth" });
	assert.deepEqual(initialLogin(providers, "deepseek", "api_key"), { providerId: "deepseek", method: "api_key" });
	assert.deepEqual(initialLogin(providers, "kimi-coding", "oauth"), { providerId: "kimi-coding", method: "oauth" });
	assert.deepEqual(initialLogin(providers, "openai-codex", "api_key"), {
		providerId: "openai-codex",
		method: "oauth",
	});
});
test("主列表优先 cstoa 并直接显示所有 OAuth 服务，列表不重复", () => {
	const { primary, others } = providerGroups([...providers, provider("opencode-go", true, false, "api_key")]);
	assert.deepEqual(
		primary.map((item) => item.id),
		["cstoa", "kimi-coding", "openai-codex", "opencode-go"],
	);
	assert.equal(
		primary.some((item) => item.id === "kimi-coding"),
		true,
	);
	assert.equal(primary.length + others.length, providers.length + 1);
});
test("登录方式标签保留可用方式，并标明已有凭据的方式", () => {
	assert.deepEqual(loginMethods(provider("cstoa", false, true)), ["oauth"]);
	assert.deepEqual(loginMethods(provider("deepseek")), ["api_key"]);
	assert.deepEqual(loginMethods(provider("kimi-coding", true, true, "oauth")), ["oauth", "api_key"]);
	assert.deepEqual(loginMethods(provider("cstoa", false, true, "api_key")), ["oauth", "api_key"]);
});

test("OAuth 和 API KEY 筛选按登录能力分组，双方式服务进入两个筛选结果", () => {
	const oauth = providerGroups(providers, "oauth");
	assert.deepEqual(
		oauth.primary.map((item) => item.id),
		["cstoa", "kimi-coding", "openai-codex"],
	);
	assert.deepEqual(oauth.others, []);
	const key = providerGroups(providers, "api_key");
	assert.deepEqual(
		key.primary.map((item) => item.id),
		["kimi-coding"],
	);
	assert.deepEqual(
		key.others.map((item) => item.id),
		["deepseek"],
	);
});
test("筛选保留已配置凭据的方式，并支持空结果", () => {
	const legacy = provider("cstoa", false, true, "api_key");
	assert.deepEqual(providerGroups([legacy], "api_key").primary, [legacy]);
	assert.deepEqual(providerGroups([provider("deepseek")], "oauth"), { primary: [], others: [] });
});

test("设置配置使用独立表单页，账号登录保留登录页", () => {
	const url = new URL(configurationPath("kimi-coding", "oauth"), "http://localhost");
	assert.equal(url.pathname, "/settings/provider");
	assert.equal(url.searchParams.get("provider"), "kimi-coding");
	assert.equal(url.searchParams.get("method"), "oauth");
	assert.equal(url.searchParams.get("returnTo"), "/settings#accounts");
	assert.equal(new URL(loginPath("cstoa", "oauth"), "http://localhost").pathname, "/login");
});

test("模型 API Key 不建立专属账号态，返回地址只允许本应用页面", () => {
	assert.equal(accountProvider([provider("deepseek", true, false, "api_key")]), undefined);
	assert.equal(returnPath("https://example.com"), "/");
	assert.equal(returnPath("//example.com"), "/");
	const url = new URL(loginPath("kimi-coding", "oauth"), "http://localhost");
	assert.equal(url.searchParams.get("method"), "oauth");
	assert.equal(url.searchParams.get("returnTo"), "/settings#accounts");
});
