import { createHash } from "node:crypto";
import type { Api, Credential, Model, ModelsStoreEntry, RefreshModelsContext } from "@earendil-works/pi-ai";
import { oaHost } from "./oa.ts";

export const PROVIDER_ID = "cstoa";
export const PROVIDER_NAME = "CSTOA API";
const TTL_MS = 5 * 60_000;
const RETRY_MS = 30_000;

/** 凭据只发给固定 OA 地址。模型列表里的 URL、headers 和其他字段不作为连接配置。 */
export function modelBaseUrl(type: Credential["type"]): string {
	return `${oaHost()}${type === "oauth" ? "/api/agent/llm/v1" : "/v1"}`;
}
function identity(credential: Credential): string {
	const key = credential.type === "oauth" ? credential.access : credential.key;
	return createHash("sha256")
		.update(`${modelBaseUrl(credential.type)}\0${credential.type}\0${key ?? ""}`)
		.digest("hex");
}

function parseIds(payload: unknown): string[] {
	if (!payload || typeof payload !== "object" || !("data" in payload) || !Array.isArray(payload.data))
		throw new Error("CSTOA 模型清单格式无效，请稍后重试。");
	if (payload.data.length > 2000) throw new Error("CSTOA 模型清单超过数量上限。");
	const ids: string[] = [];
	for (const row of payload.data) {
		if (
			!row ||
			typeof row !== "object" ||
			typeof row.id !== "string" ||
			!row.id.trim() ||
			row.id.length > 256 ||
			/[\u0000-\u001f\u007f]/.test(row.id)
		)
			throw new Error("CSTOA 模型清单格式无效，请稍后重试。");
		ids.push(row.id);
	}
	return [...new Set(ids)];
}

/** 一个目录只属于一份凭据；成功清单允许为空，失败保留同一凭据的缓存。 */
export class CstoaModels {
	private entry?: ModelsStoreEntry;
	private prepared?: ModelsStoreEntry;
	private retryAt = 0;
	private retryIdentity?: string;
	private readonly metadata: readonly Model<Api>[];
	private readonly fetcher: typeof fetch;
	private readonly now: () => number;
	constructor(metadata: readonly Model<Api>[], fetcher: typeof fetch = fetch, now: () => number = Date.now) {
		this.metadata = metadata;
		this.fetcher = fetcher;
		this.now = now;
	}

	getModels(): readonly Model<Api>[] {
		return this.entry?.models ?? [];
	}
	filter(models: readonly Model<Api>[], credential?: Credential): readonly Model<Api>[] {
		return credential && this.entry?.etag === identity(credential) ? models : [];
	}
	async fetch(credential: Credential, signal: AbortSignal): Promise<ModelsStoreEntry> {
		const baseUrl = modelBaseUrl(credential.type);
		const token = credential.type === "oauth" ? credential.access : credential.key;
		if (!token) throw new Error("CSTOA 缺少登录凭据。");
		let response: Response;
		try {
			response = await this.fetcher(`${baseUrl}/models`, {
				headers: { Authorization: `Bearer ${token}`, Accept: "application/json" },
				signal: AbortSignal.any([signal, AbortSignal.timeout(8_000)]),
				redirect: "error",
			});
		} catch {
			signal.throwIfAborted();
			throw new Error("无法更新 CSTOA 模型清单，请检查网络后重试。");
		}
		if (!response.ok) {
			await response.body?.cancel();
			throw new Error(
				response.status === 401 || response.status === 403
					? "CSTOA 登录已失效，请重新登录。"
					: `CSTOA 模型清单暂不可用（HTTP ${response.status}）。`,
			);
		}
		let ids: string[];
		try {
			ids = parseIds(await response.json());
		} catch {
			throw new Error("CSTOA 模型清单格式无效，请稍后重试。");
		}
		const models = ids.map((id): Model<Api> => {
			const metadataId = ["deepseek-flash", "deepseek-v4.1-flash"].includes(id) ? "deepseek-v4-flash" : id;
			const known = this.metadata.find((model) => model.id === metadataId && model.api === "openai-completions");
			return {
				id,
				name: known?.name ?? id,
				api: "openai-completions",
				provider: PROVIDER_ID,
				baseUrl,
				input: known?.input ?? ["text"],
				reasoning: known?.reasoning ?? false,
				thinkingLevelMap: known?.thinkingLevelMap,
				contextWindow: known?.contextWindow ?? 32_768,
				maxTokens: known?.maxTokens ?? 4096,
				compat: known?.compat ?? { maxTokensField: "max_tokens", supportsReasoningEffort: false },
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			};
		});
		return { models, etag: identity(credential), checkedAt: this.now() };
	}
	/** 登录拿到凭据后预取，Pi 保存凭据时再通过 publish 原子发布到共享目录。 */
	async prepare(credential: Credential, signal: AbortSignal): Promise<void> {
		this.prepared = await this.fetch(credential, signal);
	}
	async refresh(context: RefreshModelsContext): Promise<void> {
		const id = context.credential && identity(context.credential);
		let entry = this.prepared?.etag === id ? this.prepared : this.entry?.etag === id ? this.entry : undefined;
		let fetched = false;
		const stored = context.stored;
		if (id && stored?.etag === id && (!entry || (stored.checkedAt ?? 0) > (entry.checkedAt ?? 0))) entry = stored;
		if (!entry || !id) {
			await context.publish({
				update: () => {
					this.entry = undefined;
				},
			});
		}
		if (
			context.allowNetwork &&
			context.credential &&
			(context.force ||
				((this.retryIdentity !== id || this.now() >= this.retryAt) &&
					(!entry || this.now() - (entry.checkedAt ?? 0) >= TTL_MS)))
		) {
			try {
				entry = await this.fetch(context.credential, context.signal);
				fetched = true;
			} catch (error) {
				await context.publish({
					update: () => {
						this.entry = entry;
						this.retryAt = this.now() + RETRY_MS;
						this.retryIdentity = id;
					},
				});
				throw error;
			}
		}
		if (entry) {
			const next = entry;
			const changed = fetched || next.etag !== stored?.etag || next.checkedAt !== stored?.checkedAt;
			await context.publish({
				...(changed ? { persist: next } : {}),
				update: () => {
					this.entry = next;
					this.prepared = undefined;
				},
			});
		}
	}
}
