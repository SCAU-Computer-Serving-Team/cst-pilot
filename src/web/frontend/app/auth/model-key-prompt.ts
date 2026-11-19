import { apiJson } from "../data/api.ts";
import type { ModelChoice } from "../data/model-selection";

/** 旧版 Web 本地冷却键，首次接入服务端共享冷却后单向合并并清理。 */
export const modelKeyPromptStorageKey = "cst-model-key-prompt-at";
export const modelKeyPromptInterval = 24 * 60 * 60 * 1000;

export type MissingKeyPromptClaimResponse = {
	claimed: boolean;
	missingKeyPromptedAtMs: number | null;
};

type PromptStorage = Pick<Storage, "getItem" | "setItem"> & Partial<Pick<Storage, "removeItem">>;

function defaultStorage(): PromptStorage | null {
	try {
		return typeof window === "undefined" ? null : window.localStorage;
	} catch {
		// 隐私模式下 localStorage 不可用。
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

export function clearModelKeyPromptTime(storage: PromptStorage | null = defaultStorage()): void {
	try {
		storage?.removeItem?.(modelKeyPromptStorageKey);
	} catch {
		// 清理失败不影响主流程。
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

/**
 * 向服务端原子申领三端共享的 24 小时缺密钥提示窗口。
 * 若本地存有旧时间戳，随首次请求单向合并上报并清理本地键。
 */
export async function claimMissingKeyPrompt(options: {
	catalog: ModelChoice[];
	storage?: PromptStorage | null;
	signal?: AbortSignal;
	now?: number;
	requestClaim?: (
		body: { clientLastPromptedAtMs?: number },
		signal?: AbortSignal,
	) => Promise<MissingKeyPromptClaimResponse>;
}): Promise<boolean> {
	if (anyModelConfigured(options.catalog)) return false;
	const storage = options.storage === undefined ? defaultStorage() : options.storage;
	const legacyPromptAt = readModelKeyPromptTime(storage);
	const body =
		legacyPromptAt !== null && legacyPromptAt > 0 ? { clientLastPromptedAtMs: Math.floor(legacyPromptAt) } : {};
	const requestClaim =
		options.requestClaim ??
		((payload, signal) =>
			apiJson<MissingKeyPromptClaimResponse>("/api/ui-hints/missing-key-prompt/claim", {
				method: "POST",
				body: payload,
				signal,
			}));
	try {
		const response = await requestClaim(body, options.signal);
		if (legacyPromptAt !== null) {
			clearModelKeyPromptTime(storage);
		}
		return Boolean(response.claimed);
	} catch {
		if (options.signal?.aborted) return false;
		const now = options.now ?? Date.now();
		if (!shouldAutoPrompt({ catalog: options.catalog, lastPromptAt: legacyPromptAt, now })) {
			return false;
		}
		writeModelKeyPromptTime(storage, now);
		return true;
	}
}
