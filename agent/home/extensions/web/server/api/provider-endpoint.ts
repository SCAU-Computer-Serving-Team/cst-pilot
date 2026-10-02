import { randomUUID } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

function normalizeBaseUrl(value: string): string {
	if (!value.trim() || value.length > 2048) throw new Error("请求的 BaseURL 无效");
	let url: URL;
	try {
		url = new URL(value.trim());
	} catch {
		throw new Error("请求的 BaseURL 无效");
	}
	if (
		!["https:", "http:"].includes(url.protocol) ||
		(url.protocol === "http:" && !["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)) ||
		url.username ||
		url.password ||
		url.search ||
		url.hash
	) {
		throw new Error("请求的 BaseURL 须使用 HTTPS，或本机 HTTP 地址，且不能包含凭据、查询参数或片段");
	}
	return url.href.replace(/\/$/, value.trim().endsWith("/") ? "/" : "");
}

async function atomicWrite(file: string, content: string): Promise<void> {
	const temp = `${file}.${randomUUID()}.tmp`;
	try {
		await writeFile(temp, content, { flag: "wx", mode: 0o600 });
		await rename(temp, file);
	} finally {
		await rm(temp, { force: true });
	}
}

/** Persist Pi's built-in provider endpoint override without storing credentials in models.json. */
export async function withProviderEndpoint<T>(
	agentDir: string,
	providerId: string,
	baseUrl: string,
	modelRuntime: ModelRuntime,
	login: () => Promise<T>,
): Promise<T> {
	const endpoint = normalizeBaseUrl(baseUrl);
	const models = modelRuntime.getModels(providerId);
	if (!models.length) throw new Error("请求的 Provider 没有可用模型，请先在 Pi 中配置模型");
	const file = join(agentDir, "models.json");
	let original: string | undefined;
	try {
		original = await readFile(file, "utf8");
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
	}
	let config: Record<string, unknown>;
	try {
		config = original === undefined ? { providers: {} } : JSON.parse(original);
	} catch {
		throw new Error("请求的 models.json 无法按 JSON 解析，请先检查文件，避免覆盖已有配置");
	}
	if (!config || typeof config !== "object" || Array.isArray(config)) throw new Error("请求的 models.json 格式无效");
	const providers = config.providers;
	if (!providers || typeof providers !== "object" || Array.isArray(providers))
		throw new Error("请求的 models.json 缺少 providers 对象");
	const existing = (providers as Record<string, unknown>)[providerId];
	if (existing !== undefined && (!existing || typeof existing !== "object" || Array.isArray(existing)))
		throw new Error("请求的 Provider 配置无效");
	const provider: Record<string, unknown> = {
		...(existing as Record<string, unknown> | undefined),
		baseUrl: endpoint,
	};
	if (Array.isArray(provider.models)) {
		provider.models = provider.models.map((item: unknown) => {
			if (!item || typeof item !== "object" || Array.isArray(item)) return item;
			return "baseUrl" in item ? { ...item, baseUrl: endpoint } : item;
		});
	}
	const updated = `${JSON.stringify({ ...config, providers: { ...providers, [providerId]: provider } }, null, 2)}\n`;
	await atomicWrite(file, updated);
	try {
		const result = await modelRuntime.refresh({ providers: [providerId], allowNetwork: false });
		if (
			result.errors.get(providerId) ||
			modelRuntime.getModels(providerId).some((model) => model.baseUrl !== endpoint)
		)
			throw new Error("请求的 Provider 无法使用此 BaseURL，请检查模型配置");
		return await login();
	} catch (error) {
		// Restore the original configuration if the endpoint or login is rejected.
		try {
			if ((await readFile(file, "utf8")) === updated) {
				if (original === undefined) await rm(file, { force: true });
				else await atomicWrite(file, original);
			}
			await modelRuntime.refresh({ providers: [providerId], allowNetwork: false });
		} catch {
			throw new Error("Provider 登录未完成，无法恢复原模型配置，请检查 models.json", { cause: error });
		}
		throw error;
	}
}
