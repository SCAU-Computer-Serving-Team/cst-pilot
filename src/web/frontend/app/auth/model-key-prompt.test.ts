import assert from "node:assert/strict";
import { test } from "node:test";
import type { ModelChoice } from "../data/model-selection";
import {
	anyModelConfigured,
	claimMissingKeyPrompt,
	modelKeyPromptInterval,
	modelKeyPromptStorageKey,
	readModelKeyPromptTime,
	shouldAutoPrompt,
	writeModelKeyPromptTime,
} from "./model-key-prompt.ts";

const model = (provider: string, credentialReady: boolean): ModelChoice => ({
	provider,
	id: `${provider}-model`,
	name: provider,
	reasoning: false,
	credentialReady,
	catalogPresent: true,
});
const memoryStorage = (initial: Record<string, string> = {}) => {
	const data = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => data.get(key) ?? null,
		setItem: (key: string, value: string) => void data.set(key, value),
		removeItem: (key: string) => void data.delete(key),
	};
};
const now = 1_700_000_000_000;

test("任一服务已配置凭据时不再自动提示", () => {
	const catalog = [model("deepseek", false), model("probe", true)];
	assert.equal(anyModelConfigured(catalog), true);
	assert.equal(shouldAutoPrompt({ catalog, lastPromptAt: null, now }), false);
	assert.equal(anyModelConfigured([model("deepseek", false)]), false);
	assert.equal(shouldAutoPrompt({ catalog: [model("deepseek", false)], lastPromptAt: null, now }), true);
});

test("完全没有凭据时 24 小时内只提示一次，跨刷新与新会话共用记录", () => {
	const catalog = [model("deepseek", false)];
	const storage = memoryStorage();
	assert.equal(shouldAutoPrompt({ catalog, lastPromptAt: readModelKeyPromptTime(storage), now }), true);
	writeModelKeyPromptTime(storage, now);
	assert.equal(shouldAutoPrompt({ catalog, lastPromptAt: readModelKeyPromptTime(storage), now: now + 1000 }), false);
	assert.equal(
		shouldAutoPrompt({
			catalog,
			lastPromptAt: readModelKeyPromptTime(storage),
			now: now + modelKeyPromptInterval - 1,
		}),
		false,
	);
	assert.equal(
		shouldAutoPrompt({ catalog, lastPromptAt: readModelKeyPromptTime(storage), now: now + modelKeyPromptInterval }),
		true,
		"满 24 小时后重新提示",
	);
});

test("记录损坏或存储不可用时仍允许提示", () => {
	assert.equal(readModelKeyPromptTime(memoryStorage({ [modelKeyPromptStorageKey]: "not-a-time" })), null);
	assert.equal(readModelKeyPromptTime(null), null);
	writeModelKeyPromptTime(null, now);
	const broken = {
		getItem: () => {
			throw new Error("blocked");
		},
		setItem: () => {
			throw new Error("blocked");
		},
	};
	assert.equal(readModelKeyPromptTime(broken), null);
	assert.doesNotThrow(() => writeModelKeyPromptTime(broken, now));
});

test("claimMissingKeyPrompt 走服务端共享冷却并单向迁移清理旧 localStorage 键", async () => {
	const unconfigured = [model("deepseek", false)];
	const storage = memoryStorage({ [modelKeyPromptStorageKey]: String(now - 60_000) });
	const calls: Array<{ clientLastPromptedAtMs?: number }> = [];

	const first = await claimMissingKeyPrompt({
		catalog: unconfigured,
		storage,
		requestClaim: async (body) => {
			calls.push(body);
			return { claimed: false, missingKeyPromptedAtMs: now - 60_000 };
		},
	});
	assert.equal(first, false, "旧客户端已在 24 小时内提示过，合并后不再重复弹窗");
	assert.equal(calls.length, 1);
	assert.equal(calls[0]?.clientLastPromptedAtMs, now - 60_000);
	assert.equal(readModelKeyPromptTime(storage), null, "单向合并后已清理本地旧键");

	const second = await claimMissingKeyPrompt({
		catalog: unconfigured,
		storage,
		requestClaim: async (body) => {
			calls.push(body);
			return { claimed: true, missingKeyPromptedAtMs: now + modelKeyPromptInterval };
		},
	});
	assert.equal(second, true);
	assert.equal(calls.length, 2);
	assert.equal(calls[1]?.clientLastPromptedAtMs, undefined);

	const configured = [model("deepseek", true)];
	assert.equal(
		await claimMissingKeyPrompt({
			catalog: configured,
			storage,
			requestClaim: async () => {
				throw new Error("should not call claim when configured");
			},
		}),
		false,
	);
});
