import type { IncomingMessage, ServerResponse } from "node:http";
import { resolveModelScopeWithDiagnostics } from "@earendil-works/pi-coding-agent";
import type { SessionEvents } from "../session/events.ts";
import type { WebSessionPool } from "../session/sessions.ts";
import type { WebSettings } from "../session/web-settings.ts";
import { body, send } from "./http.ts";

interface ModelRoutesOptions {
	pool: WebSessionPool;
	webSettings: WebSettings;
	globalEvents: SessionEvents;
	writeOnce<T>(request: IncomingMessage, pathname: string, input: unknown, action: () => Promise<T>): Promise<T>;
	serialize<T>(id: string, action: () => Promise<T>): Promise<T>;
}

export function createModelRoutes({ pool, webSettings, globalEvents, writeOnce, serialize }: ModelRoutesOptions) {
	return async (
		request: IncomingMessage,
		response: ServerResponse,
		pathname: string,
		method: string,
	): Promise<boolean> => {
		if (pathname === "/api/models/refresh" && method === "POST") {
			const input = await body(request);
			if (Object.keys(input).length) throw new Error("请求的模型刷新参数无效");
			const result = await writeOnce(request, pathname, input, async () => {
				await pool.getServices();
				const refresh = await pool.catalog.refresh({ force: true });
				return { aborted: refresh.aborted, failedProviders: [...refresh.errors.keys()] };
			});
			send(response, 200, result);
			return true;
		}
		if (pathname === "/api/models" && method === "GET") {
			const { modelRuntime, settingsManager } = await pool.refreshServices();
			const sessionId = new URL(request.url ?? "/", "http://localhost").searchParams.get("sessionId");
			const slot = sessionId ? await pool.openSaved(sessionId) : undefined;
			const stats = slot?.session.getSessionStats();
			// 未指定会话（首页）或会话未选模型时，回退到全局默认模型：新会话实际会使用它。
			const fallbackProvider = settingsManager.getDefaultProvider();
			const fallbackModel = settingsManager.getDefaultModel();
			const selectedModel =
				slot?.session.model ??
				(fallbackProvider && fallbackModel && modelRuntime.getModel(fallbackProvider, fallbackModel)
					? { provider: fallbackProvider, id: fallbackModel }
					: undefined);
			// 与 /scoped-models 同口径：清单只含可用模型；存储的启用模式展开为具体 id，未匹配的单独返回
			// 模型可用性沿用 Pi，包含持久凭据、配置与环境变量；不增加 Web 专属过滤。
			const available = await modelRuntime.getAvailable();
			const patterns = settingsManager.getEnabledModels();
			let enabled: string[] | null = null;
			let unavailableEnabled: string[] = [];
			if (!patterns?.length && (await webSettings.read()).emptyModelScope) enabled = [];
			if (patterns?.length) {
				const scope = await resolveModelScopeWithDiagnostics(patterns, modelRuntime);
				const listed = new Set(available.map((model) => `${model.provider}/${model.id}`));
				enabled = [];
				unavailableEnabled = [];
				for (const scoped of scope.scopedModels) {
					const id = `${scoped.model.provider}/${scoped.model.id}`;
					(listed.has(id) ? enabled : unavailableEnabled).push(id);
				}
				for (const diagnostic of scope.diagnostics)
					if (diagnostic.code === "no-match") unavailableEnabled.push(diagnostic.pattern);
			}
			send(response, 200, {
				models: available.map((model) => ({
					id: model.id,
					provider: model.provider,
					name: model.name,
					input: model.input,
					reasoning: model.reasoning,
					contextWindow: model.contextWindow,
					maxTokens: model.maxTokens,
				})),
				enabled,
				unavailableEnabled,
				thinkingLevels: slot?.session.getAvailableThinkingLevels() ?? [],
				contextUsage: stats?.contextUsage ?? null,
				usage: stats
					? {
							tokens: stats.tokens,
							cacheHitRate:
								stats.tokens.input + stats.tokens.cacheRead > 0
									? stats.tokens.cacheRead / (stats.tokens.input + stats.tokens.cacheRead)
									: null,
						}
					: null,
				selected: selectedModel
					? {
							provider: selectedModel.provider,
							id: selectedModel.id,
							thinkingLevel:
								slot?.session.thinkingLevel ??
								settingsManager.getModelThinkingLevel(selectedModel.provider, selectedModel.id) ??
								undefined,
						}
					: null,
			});
			return true;
		}
		if (pathname === "/api/models/select" && method === "POST") {
			const input = await body(request);
			if (
				(input.sessionId !== undefined && typeof input.sessionId !== "string") ||
				typeof input.provider !== "string" ||
				typeof input.modelId !== "string"
			)
				throw new Error("请求的模型选择无效");
			const { modelRuntime, settingsManager } = await pool.getServices();
			const slot = typeof input.sessionId === "string" ? await pool.openSaved(input.sessionId) : undefined;
			const model = modelRuntime.getModel(input.provider, input.modelId);
			if (!model) throw new Error("请求的模型不存在");
			const result = await writeOnce(request, pathname, input, () =>
				serialize("model-default", async () => {
					if (slot) await slot.session.setModel(model, { persist: true });
					else {
						if (!(await modelRuntime.checkAuth(model.provider))) throw new Error("请求的模型服务尚未配置凭据");
						const patterns = settingsManager.getEnabledModels();
						const scope = patterns?.length
							? await resolveModelScopeWithDiagnostics(patterns, modelRuntime)
							: undefined;
						settingsManager.setDefaultModelAndProvider(model.provider, model.id);
						await webSettings.saveEmptyModelScope(false);
						if (
							patterns?.length &&
							scope &&
							!scope.scopedModels.some(
								(item) => item.model.provider === model.provider && item.model.id === model.id,
							)
						) {
							settingsManager.setEnabledModels([...patterns, `${model.provider}/${model.id}`]);
							await webSettings.saveEmptyModelScope(false);
							pool.setScopedModels([...scope.scopedModels, { model }]);
						}
					}
					globalEvents.publish("state", { type: "settings_changed" });
					return {
						provider: model.provider,
						id: model.id,
						thinkingLevel:
							slot?.session.thinkingLevel ?? settingsManager.getModelThinkingLevel(model.provider, model.id),
						thinkingLevels: slot?.session.getAvailableThinkingLevels() ?? [],
					};
				}),
			);
			send(response, 200, result);
			return true;
		}
		if (pathname === "/api/models/thinking" && method === "POST") {
			const input = await body(request);
			const { sessionId, level } = input;
			if (typeof sessionId !== "string" || typeof level !== "string") throw new Error("请求的思考强度无效");
			const slot = await pool.openSaved(sessionId);
			const selected = slot.session.getAvailableThinkingLevels().find((candidate) => candidate === level);
			if (!selected) throw new Error("请求的思考强度无效");
			const result = await writeOnce(request, pathname, input, async () => {
				slot.session.setThinkingLevel(selected);
				return { level: slot.session.thinkingLevel };
			});
			send(response, 200, result);
			return true;
		}
		if (pathname === "/api/models/scoped" && method === "POST") {
			const input = await body(request);
			const { patterns, empty } = input;
			if (
				!Array.isArray(patterns) ||
				patterns.length > 100 ||
				(empty !== undefined && typeof empty !== "boolean") ||
				(empty === true && patterns.length !== 0) ||
				!patterns.every((pattern): pattern is string => typeof pattern === "string" && pattern.length <= 200)
			)
				throw new Error("请求的模型范围无效");
			const { settingsManager, modelRuntime } = await pool.getServices();
			const result = await writeOnce(request, pathname, input, async () => {
				const scope = await resolveModelScopeWithDiagnostics(patterns, modelRuntime);
				if (scope.diagnostics.length) throw new Error("请求的模型范围没有匹配项");
				// 与 /scoped-models 保存一致：全部可用模型使用默认范围，顺序由用户保存。
				const availableIds = (await modelRuntime.getAvailable()).map((model) => `${model.provider}/${model.id}`);
				const coversAll =
					patterns.length > 0 &&
					patterns.length === availableIds.length &&
					availableIds.every((id) => patterns.includes(id));
				settingsManager.setEnabledModels(coversAll ? undefined : patterns);
				pool.setScopedModels(coversAll ? [] : scope.scopedModels);
				await webSettings.saveEmptyModelScope(empty === true);
				globalEvents.publish("state", { type: "settings_changed" });
				return { enabled: empty === true ? [] : (settingsManager.getEnabledModels() ?? null) };
			});
			send(response, 200, result);
			return true;
		}
		return false;
	};
}
