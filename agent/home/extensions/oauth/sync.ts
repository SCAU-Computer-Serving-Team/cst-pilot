import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

const pending = new WeakMap<ModelRuntime, Promise<void>>();

/** 只更新项目的 CSTOA 目录，不开启 Pi 全局启动联网。Provider 自行控制缓存与失败重试。 */
export async function refreshCstoaModels(runtime: ModelRuntime): Promise<void> {
	if (!runtime.getRegisteredNativeProvider("cstoa")) return;
	const existing = pending.get(runtime);
	if (existing) return existing;
	const task = (async () => {
		await runtime.refresh({ providers: ["cstoa"], allowNetwork: true, signal: AbortSignal.timeout(10_000) });
	})();
	pending.set(runtime, task);
	try {
		await task;
	} finally {
		if (pending.get(runtime) === task) pending.delete(runtime);
	}
}
