import { createHash, randomUUID } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { InboxConflict } from "../session/inbox.ts";

const APIs = ["openai-completions", "openai-responses", "anthropic-messages", "google-generative-ai"] as const;
type Api = (typeof APIs)[number];
type Config = { providers: Record<string, Record<string, unknown>>; [key: string]: unknown };
type ModelInput = {
	id: string;
	name: string;
	contextWindow: number;
	maxTokens: number;
	reasoning: boolean;
	image: boolean;
};
export type CustomProviderInput = {
	id: string;
	name: string;
	baseUrl: string;
	api: Api;
	apiKey?: string;
	models: ModelInput[];
	revision?: string;
};

async function readConfig(agentDir: string): Promise<{ config: Config; source: string | null }> {
	let source: string | null = null;
	try {
		source = await readFile(join(agentDir, "models.json"), "utf8");
	} catch (cause) {
		if ((cause as NodeJS.ErrnoException).code !== "ENOENT") throw cause;
	}
	let config: Config;
	try {
		config = source === null ? { providers: {} } : JSON.parse(source);
	} catch {
		throw new Error("models.json 无法解析，请先在 Pi 中修复文件");
	}
	if (
		!config ||
		typeof config !== "object" ||
		Array.isArray(config) ||
		!config.providers ||
		typeof config.providers !== "object" ||
		Array.isArray(config.providers)
	)
		throw new Error("models.json 的 providers 格式无效");
	return { config, source };
}
function revision(value: unknown) {
	return createHash("sha256")
		.update(JSON.stringify(value ?? null))
		.digest("hex");
}
function custom(value: unknown): value is Record<string, unknown> {
	return (
		!!value &&
		typeof value === "object" &&
		!Array.isArray(value) &&
		typeof (value as Record<string, unknown>).baseUrl === "string" &&
		Array.isArray((value as Record<string, unknown>).models) &&
		typeof (value as Record<string, unknown>).api === "string"
	);
}
export async function readCustomProviders(agentDir: string) {
	const { config } = await readConfig(agentDir);
	return Object.entries(config.providers)
		.filter(([, value]) => custom(value))
		.map(([id]) => id);
}
export async function readCustomProvider(agentDir: string, id: string) {
	const { config } = await readConfig(agentDir);
	const provider = config.providers[id];
	if (!custom(provider)) return null;
	return {
		id,
		name: provider.name ?? id,
		baseUrl: provider.baseUrl,
		api: provider.api,
		apiKeySet: !!provider.apiKey,
		models: (provider.models as Record<string, unknown>[]).map((model) => ({
			id: model.id,
			name: model.name ?? model.id,
			contextWindow: model.contextWindow ?? 128000,
			maxTokens: model.maxTokens ?? 16384,
			reasoning: model.reasoning === true,
			image: Array.isArray(model.input) && model.input.includes("image"),
		})),
		revision: revision(provider),
	};
}
function validate(input: Record<string, unknown>): CustomProviderInput {
	if (
		Object.keys(input).some((key) => !["id", "name", "baseUrl", "api", "apiKey", "models", "revision"].includes(key))
	)
		throw new Error("自定义服务包含不支持的参数");
	const { id, name, baseUrl, api, apiKey, models, revision: expected } = input;
	if (
		typeof id !== "string" ||
		!/^[a-z][a-z0-9_-]{1,63}$/.test(id) ||
		["__proto__", "constructor", "prototype"].includes(id)
	)
		throw new Error("服务 ID 须为 2 至 64 位小写字母、数字、下划线或连字符");
	if (typeof name !== "string" || !name.trim() || name.length > 100) throw new Error("请输入服务名称");
	if (typeof baseUrl !== "string" || baseUrl.length > 2048) throw new Error("服务地址无效");
	let url: URL;
	try {
		url = new URL(baseUrl.trim());
	} catch {
		throw new Error("服务地址无效");
	}
	if (
		!(
			["https:", "http:"].includes(url.protocol) &&
			(url.protocol === "https:" || ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))
		) ||
		url.username ||
		url.password ||
		url.hash ||
		url.search
	)
		throw new Error("服务地址须为 HTTPS 或本机 HTTP，且不包含凭据、查询或片段");
	if (typeof api !== "string" || !APIs.includes(api as Api)) throw new Error("请选择支持的接口协议");
	if (apiKey !== undefined && (typeof apiKey !== "string" || !apiKey.trim() || apiKey.length > 8192))
		throw new Error("API key 无效");
	if (expected !== undefined && (typeof expected !== "string" || !/^[a-f0-9]{64}$/.test(expected)))
		throw new Error("配置版本无效");
	if (!Array.isArray(models) || !models.length || models.length > 32) throw new Error("请配置 1 至 32 个模型");
	const seen = new Set<string>();
	for (const model of models) {
		if (
			!model ||
			typeof model !== "object" ||
			Array.isArray(model) ||
			Object.keys(model).some(
				(key) => !["id", "name", "contextWindow", "maxTokens", "reasoning", "image"].includes(key),
			)
		)
			throw new Error("模型参数无效");
		const m = model as ModelInput;
		if (
			typeof m.id !== "string" ||
			!/^[a-zA-Z0-9][\w.\-/:+]{0,127}$/.test(m.id) ||
			seen.has(m.id) ||
			typeof m.name !== "string" ||
			!m.name.trim() ||
			m.name.length > 128 ||
			!Number.isSafeInteger(m.contextWindow) ||
			m.contextWindow < 1024 ||
			m.contextWindow > 100_000_000 ||
			!Number.isSafeInteger(m.maxTokens) ||
			m.maxTokens < 1 ||
			m.maxTokens > m.contextWindow ||
			typeof m.reasoning !== "boolean" ||
			typeof m.image !== "boolean"
		)
			throw new Error("模型 ID、名称或上下文参数无效，且模型 ID 不得重复");
		seen.add(m.id);
	}
	return {
		id,
		name: name.trim(),
		baseUrl: url.href.replace(/\/$/, baseUrl.trim().endsWith("/") ? "/" : ""),
		api: api as Api,
		apiKey: apiKey as string | undefined,
		models: models as ModelInput[],
		revision: expected as string | undefined,
	};
}
async function atomicWrite(file: string, content: string) {
	const temporary = `${file}.${randomUUID()}.tmp`;
	try {
		await writeFile(temporary, content, { flag: "wx", mode: 0o600 });
		await rename(temporary, file);
	} finally {
		await rm(temporary, { force: true });
	}
}
export async function saveCustomProvider(agentDir: string, raw: Record<string, unknown>, runtime: ModelRuntime) {
	const input = validate(raw);
	const file = join(agentDir, "models.json");
	const { config, source } = await readConfig(agentDir);
	const previous = config.providers[input.id];
	if (previous && !custom(previous)) throw new InboxConflict("服务 ID 已由 Pi 内置服务占用");
	if (!previous && runtime.getProvider(input.id)) throw new InboxConflict("服务 ID 已存在，请更换");
	if (input.revision !== undefined && revision(previous) !== input.revision)
		throw new InboxConflict("服务配置已在其他窗口修改，请刷新后重试");
	if (previous && input.revision === undefined) throw new InboxConflict("服务已存在，请刷新后编辑");
	if (
		!input.apiKey &&
		!previous?.apiKey &&
		!(await runtime.getProviderAuthStatus(input.id)).configured &&
		!new URL(input.baseUrl).hostname.match(/^(localhost|127\.0\.0\.1|\[::1\])$/)
	)
		throw new Error("请输入 API key");
	const updatedProvider = {
		...(previous ?? {}),
		name: input.name,
		baseUrl: input.baseUrl,
		api: input.api,
		...(input.apiKey
			? { apiKey: input.apiKey }
			: !previous?.apiKey && new URL(input.baseUrl).protocol === "http:"
				? { apiKey: "local" }
				: {}),
		models: input.models.map((model) => {
			const earlier = Array.isArray(previous?.models)
				? previous.models.find((item: { id?: string }) => item.id === model.id)
				: undefined;
			return {
				...(earlier ?? {}),
				id: model.id,
				name: model.name.trim(),
				contextWindow: model.contextWindow,
				maxTokens: model.maxTokens,
				reasoning: model.reasoning,
				input: model.image ? ["text", "image"] : ["text"],
			};
		}),
	};
	const next = `${JSON.stringify({ ...config, providers: { ...config.providers, [input.id]: updatedProvider } }, null, 2)}\n`;
	await atomicWrite(file, next);
	try {
		const result = await runtime.refresh({ providers: [input.id], allowNetwork: false });
		if (result.errors.has(input.id) || !runtime.getModels(input.id).length)
			throw new Error("Pi 无法加载此服务，请检查接口类型和模型参数");
	} catch (cause) {
		if ((await readFile(file, "utf8")) === next) {
			if (source === null) await rm(file, { force: true });
			else await atomicWrite(file, source);
		}
		await runtime.refresh({ providers: [input.id], allowNetwork: false });
		throw cause;
	}
	return { id: input.id, revision: revision(updatedProvider) };
}
export async function deleteCustomProvider(agentDir: string, id: string, expected: string, runtime: ModelRuntime) {
	const file = join(agentDir, "models.json");
	const { config } = await readConfig(agentDir);
	if (!custom(config.providers[id])) throw new Error("自定义服务不存在");
	if (revision(config.providers[id]) !== expected) throw new InboxConflict("服务配置已在其他窗口修改，请刷新后重试");
	const providers = { ...config.providers };
	delete providers[id];
	await atomicWrite(file, `${JSON.stringify({ ...config, providers }, null, 2)}\n`);
	await runtime.refresh({ providers: [id], allowNetwork: false });
	return { id };
}
