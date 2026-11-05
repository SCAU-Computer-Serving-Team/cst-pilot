import assert from "node:assert/strict";
import { test } from "node:test";
import type { Credential, ModelsStoreEntry, RefreshModelsContext } from "@earendil-works/pi-ai";
import { CstoaModels } from "../models.ts";

test("模型目录随凭据隔离，自动刷新增删、允许空目录，失败保留同凭据缓存并退避", async () => {
	let now = 1000;
	let calls = 0;
	let status = 200;
	let ids = ["first", "second"];
	const a: Credential = { type: "api_key", key: "test-a" };
	const b: Credential = { type: "api_key", key: "test-b" };
	const catalog = new CstoaModels(
		[],
		async () => {
			calls++;
			return Response.json({ data: ids.map((id) => ({ id })) }, { status });
		},
		() => now,
	);
	let stored: ModelsStoreEntry | undefined;
	let writes = 0;
	const refresh = async (credential?: Credential, force = false) => {
		const context: RefreshModelsContext = {
			credential,
			stored,
			allowNetwork: true,
			force,
			signal: new AbortController().signal,
			publish: async ({ persist, update }) => {
				if (persist) {
					stored = structuredClone(persist);
					writes++;
				}
				update?.();
				return true;
			},
		};
		await catalog.refresh(context);
	};
	await refresh(a);
	assert.deepEqual(
		catalog.getModels().map((model) => model.id),
		ids,
	);
	assert.equal(catalog.filter(catalog.getModels(), b).length, 0);
	assert.equal(JSON.stringify(stored).includes("test-a"), false);
	assert.ok(catalog.getModels().every((model) => model.baseUrl === "https://www.cstoa.top/v1"));
	await refresh(a);
	assert.equal(calls, 1, "五分钟缓存不重复联网");
	assert.equal(writes, 1, "读取缓存不重写目录，避免跨进程初始化互相覆盖");
	const restarted = new CstoaModels([]);
	await restarted.refresh({
		credential: a,
		stored,
		allowNetwork: false,
		signal: new AbortController().signal,
		publish: async ({ persist, update }) => {
			assert.equal(persist, undefined, "新进程恢复目录不改动共享文件");
			update?.();
			return true;
		},
	});
	assert.deepEqual(
		restarted.getModels().map((model) => model.id),
		ids,
	);
	ids = ["added"];
	now += 300_001;
	await refresh(a);
	assert.deepEqual(
		catalog.getModels().map((model) => model.id),
		["added"],
	);
	status = 503;
	now += 300_001;
	await assert.rejects(refresh(a), /暂不可用/);
	assert.deepEqual(
		catalog.getModels().map((model) => model.id),
		["added"],
	);
	const failedCalls = calls;
	await refresh(a);
	assert.equal(calls, failedCalls, "失败后不连续查询");
	await assert.rejects(refresh(b), /暂不可用/);
	assert.equal(catalog.getModels().length, 0, "换凭据失败不能恢复其他账号目录");
	status = 200;
	ids = [];
	await refresh(b, true);
	assert.equal(catalog.getModels().length, 0, "成功空目录移除旧模型");
	await refresh();
	assert.equal(catalog.getModels().length, 0);
});

test("CSTOA 展示与请求均保留网关原始 ID，元数据别名不改名、不丢模型，缓存恢复同口径", async () => {
	const ids = ["deepseek-flash", "deepseek-v4-pro", "MiniMax-M3.1-Flash"];
	const credential: Credential = { type: "api_key", key: "test-key" };
	const catalog = new CstoaModels(
		[
			{
				id: "deepseek-v4-flash",
				name: "DeepSeek V4 Flash",
				api: "openai-completions",
				provider: "deepseek",
				baseUrl: "https://api.deepseek.com",
				input: ["text"],
				reasoning: true,
				contextWindow: 1_000_000,
				maxTokens: 384_000,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			},
		],
		async () => Response.json({ data: ids.map((id) => ({ id })) }),
	);
	const stored = await catalog.fetch(credential, new AbortController().signal);
	assert.deepEqual(
		stored.models.map((model) => model.id),
		ids,
	);
	assert.deepEqual(
		stored.models.map((model) => model.name),
		ids,
	);
	assert.equal(stored.models[0]?.contextWindow, 1_000_000, "能力参数仍可复用 Pi 目录");
	const legacy = {
		...stored,
		models: stored.models.map((model, index) => (index === 0 ? { ...model, name: "DeepSeek V4 Flash" } : model)),
	};
	const restarted = new CstoaModels([]);
	await restarted.refresh({
		credential,
		stored: legacy,
		allowNetwork: false,
		signal: new AbortController().signal,
		publish: async ({ persist, update }) => {
			assert.equal(persist, undefined, "恢复缓存不重写共享目录");
			update?.();
			return true;
		},
	});
	assert.deepEqual(
		restarted.getModels().map((model) => model.name),
		ids,
	);
});

test("CSTOA 两个常用模型按官方规格取上下文与输出上限，不携带其他适配器参数", async () => {
	const ids = ["deepseek-flash", "MiniMax-M3.1-Flash"];
	const catalog = new CstoaModels(
		[
			{
				id: "deepseek-v4-flash",
				name: "DeepSeek V4 Flash",
				api: "openai-completions",
				provider: "deepseek",
				baseUrl: "https://api.deepseek.com",
				input: ["text"],
				reasoning: true,
				contextWindow: 128_000,
				maxTokens: 8_192,
				compat: { maxTokensField: "max_tokens" },
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			},
			{
				id: "MiniMax-M3.1-Flash",
				name: "MiniMax-M3.1-Flash",
				api: "anthropic-messages",
				provider: "minimax",
				baseUrl: "https://api.minimax.io/anthropic",
				input: ["text", "image"],
				reasoning: true,
				contextWindow: 1_000_000,
				maxTokens: 512_000,
				thinkingLevelMap: { low: "low" },
				compat: { forceAdaptiveThinking: true },
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			},
		],
		async () => Response.json({ data: ids.map((id) => ({ id })) }),
	);
	const entry = await catalog.fetch({ type: "api_key", key: "test-key" }, new AbortController().signal);
	const [deepseek, minimax] = entry.models;
	assert.deepEqual(
		entry.models.map((model) => [model.contextWindow, model.maxTokens]),
		[
			[1_000_000, 384_000],
			[1_000_000, 524_288],
		],
		"官方规格覆盖目录口径",
	);
	assert.deepEqual(minimax?.compat, { maxTokensField: "max_tokens", supportsReasoningEffort: false });
	assert.equal(minimax?.thinkingLevelMap, undefined, "不借用 Anthropic 请求参数");
	assert.deepEqual(minimax?.input, ["text", "image"], "官方能力字段优先于目录缺口");
	assert.equal(minimax?.reasoning, true);
	assert.deepEqual(deepseek?.input, ["text", "image"]);
	assert.equal(deepseek?.reasoning, true);
	assert.deepEqual(deepseek?.compat, { maxTokensField: "max_tokens" });
});

test("模型清单只接收 ID，不接受上游提供的请求地址或密钥", async () => {
	const catalog = new CstoaModels([], async () =>
		Response.json({
			data: [
				{
					id: "safe",
					baseUrl: "https://untrusted.invalid",
					headers: { Authorization: "private-value" },
				},
			],
		}),
	);
	const entry = await catalog.fetch(
		{ type: "oauth", access: "test-access", refresh: "", expires: Date.now() + 60_000 },
		new AbortController().signal,
	);
	assert.equal(entry.models[0]?.baseUrl, "https://www.cstoa.top/api/agent/llm/v1");
	assert.equal(JSON.stringify(entry).includes("private-value"), false);
	const invalid = new CstoaModels([], async () => Response.json({ data: [{ id: {} }] }));
	await assert.rejects(invalid.fetch({ type: "api_key", key: "test" }, new AbortController().signal), /格式/);
});
