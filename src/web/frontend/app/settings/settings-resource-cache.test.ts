import assert from "node:assert/strict";
import { test } from "node:test";
import {
	cachedSetting,
	cacheSetting,
	invalidateSetting,
	loadSetting,
	settingGeneration,
	settingReady,
} from "./settings-resource-cache.ts";

test("保存期间目录失效后，局部保存结果不能覆盖新目录读取", async () => {
	const previous = globalThis.fetch;
	let finish!: (response: Response) => void;
	globalThis.fetch = () =>
		new Promise<Response>((resolve) => {
			finish = resolve;
		});
	const path = "/cache-test/save-race";
	try {
		cacheSetting(path, { models: ["old"], enabled: [] });
		const generation = settingGeneration(path);
		invalidateSetting(path);
		const pending = loadSetting(path);
		assert.equal(cacheSetting(path, { models: ["old"], enabled: ["old"] }, generation), false);
		assert.equal(settingReady(path), false);
		finish(json({ models: ["new"], enabled: ["old"] }));
		assert.deepEqual(await pending, { models: ["new"], enabled: ["old"] });
		assert.deepEqual(cachedSetting(path), { models: ["new"], enabled: ["old"] });
	} finally {
		globalThis.fetch = previous;
	}
});

const json = (value: unknown) =>
	new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });

test("设置预加载复用进行中的读取与已确认值", async () => {
	const previous = globalThis.fetch;
	let calls = 0;
	globalThis.fetch = async () => {
		calls++;
		return json({ theme: "dark" });
	};
	try {
		const [first, second] = await Promise.all([loadSetting("/cache-test/dedupe"), loadSetting("/cache-test/dedupe")]);
		assert.deepEqual(first, { theme: "dark" });
		assert.deepEqual(second, first);
		assert.deepEqual(await loadSetting("/cache-test/dedupe"), first);
		assert.equal(calls, 1);
		assert.equal(settingReady("/cache-test/dedupe"), true);
	} finally {
		globalThis.fetch = previous;
	}
});

test("共享配置变化后失效旧请求，迟到响应不能盖掉新目录", async () => {
	const previous = globalThis.fetch;
	let finishOld!: (response: Response) => void;
	const old = new Promise<Response>((resolve) => {
		finishOld = resolve;
	});
	let calls = 0;
	globalThis.fetch = async () => (++calls === 1 ? old : json({ models: ["new"] }));
	try {
		const pending = loadSetting("/cache-test/invalidate");
		invalidateSetting("/cache-test/invalidate");
		assert.deepEqual(await loadSetting("/cache-test/invalidate"), { models: ["new"] });
		finishOld(json({ models: ["old"] }));
		assert.deepEqual(await pending, { models: ["new"] });
		assert.deepEqual(cachedSetting("/cache-test/invalidate"), { models: ["new"] });
		cacheSetting("/cache-test/invalidate", { models: ["saved"] });
		assert.deepEqual(await loadSetting("/cache-test/invalidate"), { models: ["saved"] });
	} finally {
		globalThis.fetch = previous;
	}
});
