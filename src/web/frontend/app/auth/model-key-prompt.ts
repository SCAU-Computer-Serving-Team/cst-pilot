import type { ModelChoice } from "../data/model-selection";

/** 自动提示记录写在 localStorage，跨标签页、刷新与新会话共用。 */
export const modelKeyPromptStorageKey = "cst-model-key-prompt-at";
export const modelKeyPromptInterval = 24 * 60 * 60 * 1000;

type PromptStorage = Pick<Storage, "getItem" | "setItem">;

function defaultStorage(): PromptStorage | null {
	try {
		return typeof window === "undefined" ? null : window.localStorage;
	} catch {
		// 隐私模式下 localStorage 不可用，退化为本次页面内提示。
		return null;
	}
}

export function readModelKeyPromptTime(storage: PromptStorage | null = defaultStorage()): number | null {
	try {
		const raw = storage?.getItem(modelKeyPromptStorageKey);
		if (raw === null || raw === undefined) return null;
		const value = Number(raw);
		return Number.isFinite(value) ? value : null;
	} catch {
		return null;
	}
}

export function writeModelKeyPromptTime(storage: PromptStorage | null = defaultStorage(), now = Date.now()): void {
	try {
		storage?.setItem(modelKeyPromptStorageKey, String(now));
	} catch {
		// 写入失败不阻断提示。
	}
}

/** 任一服务已配置凭据即视为已配置模型，不再自动提示。 */
export function anyModelConfigured(catalog: ModelChoice[]): boolean {
	return catalog.some((model) => model.credentialReady);
}

export function shouldAutoPrompt(options: {
	catalog: ModelChoice[];
	lastPromptAt: number | null;
	now: number;
}): boolean {
	if (anyModelConfigured(options.catalog)) return false;
	const elapsed = options.lastPromptAt === null ? Number.NaN : options.now - options.lastPromptAt;
	return !(elapsed >= 0 && elapsed < modelKeyPromptInterval);
}
