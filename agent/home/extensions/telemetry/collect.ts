/**
 * 采集器：会话事件 → 内存累计状态。
 *
 * 三条纪律：只改内存；不做任何同步 I/O；整体 try/catch，任何一步出错跳过本次累计。
 * 纯数据结构，可单测。轮次级草稿写盘不在本模块，由入口层在每个 turn_end 后触发。
 */

import { randomUUID } from "node:crypto";
import { type CurrencyMap, currencyOf } from "./config.ts";

export interface ModelAgg {
	provider: string;
	model: string;
	thinkingLevel: string;
	turns: number;
	input: number;
	output: number;
	cacheRead: number;
	cacheWrite: number;
	totalTokens: number;
	cost: number;
	currency: string;
}

export interface ToolAgg {
	name: string;
	scope?: string;
	calls: number;
	failures: number;
	degraded: number;
	totalMs: number;
	maxMs: number;
	resultBytes: number;
	truncated: number;
	entriesTotal?: number;
	providers?: Record<string, number>;
	modes?: Record<string, number>;
}

export interface ContextUsageLike {
	tokens: number | null;
	contextWindow: number;
}

export interface SessionState {
	// 会话
	sessionId: string;
	reason: string;
	channel: "tui" | "web";
	recordId: string;
	// 时间与用量
	startedAt: number;
	prompts: number;
	turns: number;
	activeMs: number;
	// 模型
	models: Map<string, ModelAgg>;
	// 上下文
	compactions: number;
	compactionTokens: number;
	compactionOverflows: number;
	compactionFailures: number;
	contextPeak: number;
	contextWindow: number;
	// 工具
	tools: Map<string, ToolAgg>;
	// 失败
	providerErrors: Map<string, number>;
	networkErrors: number;
	aborted: number;
	toolFailures: number;
	errors: Map<string, number>;
	// 草稿恢复用：最近一个 turn_end 时刻
	lastTurnEndedAt?: number;
	// 运行期辅助（不进记录）
	turnStartAt?: number;
	pendingThinkingLevel?: string;
	sawProviderResponse: boolean;
	toolStarts: Map<string, { name: string; startedAt: number }>;
	toolScopes: Map<string, string | undefined>;
}

export function newState(sessionId: string, reason: string, channel: "tui" | "web"): SessionState {
	return {
		sessionId,
		reason,
		channel,
		recordId: randomUUID(),
		startedAt: Date.now(),
		prompts: 0,
		turns: 0,
		activeMs: 0,
		models: new Map(),
		compactions: 0,
		compactionTokens: 0,
		compactionOverflows: 0,
		compactionFailures: 0,
		contextPeak: 0,
		contextWindow: 0,
		tools: new Map(),
		providerErrors: new Map(),
		networkErrors: 0,
		aborted: 0,
		toolFailures: 0,
		errors: new Map(),
		sawProviderResponse: false,
		toolStarts: new Map(),
		toolScopes: new Map(),
	};
}

/** 无 scope 工具省略；有 scope 的工具省略入参时按各自默认值记录。 */
const DEFAULT_SCOPES: Record<string, string> = { sys: "overview", driver: "problem", eventlog: "recent" };

function scopeOf(toolName: string, input: unknown): string | undefined {
	if (toolName in DEFAULT_SCOPES || toolName === "disk") {
		const value = (input as { scope?: unknown } | undefined)?.scope;
		return typeof value === "string" && value ? value : DEFAULT_SCOPES[toolName];
	}
	return undefined;
}

function toolKey(name: string, scope: string | undefined): string {
	return scope ? `${name}(${scope})` : name;
}

function contentText(result: unknown): string {
	const content = (result as { content?: unknown } | undefined)?.content;
	if (typeof content === "string") return content;
	if (Array.isArray(content)) {
		return content
			.map((block) => (typeof block === "object" && block !== null && "text" in block ? String(block.text) : ""))
			.join("");
	}
	return "";
}

function readTruncated(result: unknown): boolean {
	if (contentText(result).includes("outputTruncated")) return true;
	const truncation = (result as { details?: { truncation?: { truncated?: unknown } } } | undefined)?.details
		?.truncation;
	return truncation?.truncated === true;
}

function readDegraded(toolName: string, scope: string | undefined, result: unknown): boolean {
	const details = (result as { details?: Record<string, unknown> } | undefined)?.details;
	if (details === undefined || typeof details !== "object") return false;
	if (toolName === "startup") {
		return (details.startup as { degraded?: unknown } | undefined)?.degraded === true;
	}
	if (toolName === "disk") {
		if ((details as { degraded?: unknown }).degraded === true) return true;
		const usage = details.usage as { method?: string; degradedFrom?: string } | undefined;
		return usage?.method === "node-walk" || (usage?.degradedFrom ?? "") !== "";
	}
	if (toolName === "sys" || toolName === "driver" || toolName === "eventlog") {
		const byScope = details[scope ?? ""] as { degraded?: unknown } | undefined;
		return byScope?.degraded === true;
	}
	return false;
}

function bump(record: Record<string, number> | undefined, key: string): Record<string, number> {
	const next = record ?? {};
	next[key] = (next[key] ?? 0) + 1;
	return next;
}

export function onInput(state: SessionState, source: string): void {
	if (source === "interactive" || source === "rpc") state.prompts++;
}

export function onBeforeProviderRequest(state: SessionState, thinkingLevel: string | undefined): void {
	state.pendingThinkingLevel = thinkingLevel;
	state.sawProviderResponse = false;
}

export function onTurnStart(state: SessionState): void {
	state.turnStartAt = Date.now();
}

export function onAfterProviderResponse(state: SessionState, status: number): void {
	state.sawProviderResponse = true;
	if (!(status >= 200 && status < 300)) {
		state.providerErrors.set(String(status), (state.providerErrors.get(String(status)) ?? 0) + 1);
	}
}

export interface TurnEndMessage {
	role: string;
	provider?: string;
	model?: string;
	usage?: {
		input?: number;
		output?: number;
		cacheRead?: number;
		cacheWrite?: number;
		totalTokens?: number;
		cost?: { total?: number };
	};
	stopReason?: string;
	errorMessage?: string;
}

export function onTurnEnd(
	state: SessionState,
	message: TurnEndMessage,
	contextUsage: ContextUsageLike | undefined,
	currency: CurrencyMap | undefined,
): void {
	if (state.turnStartAt !== undefined) {
		state.activeMs += Date.now() - state.turnStartAt;
		state.turnStartAt = undefined;
	}
	state.turns++;
	state.lastTurnEndedAt = Date.now();

	if (message.role === "assistant") {
		const key = `${message.provider}\u0000${message.model}\u0000${state.pendingThinkingLevel ?? ""}`;
		let agg = state.models.get(key);
		if (!agg) {
			agg = {
				provider: message.provider ?? "",
				model: message.model ?? "",
				thinkingLevel: state.pendingThinkingLevel ?? "",
				turns: 0,
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: 0,
				currency: currencyOf(currency, message.provider ?? ""),
			};
			state.models.set(key, agg);
		}
		agg.turns++;
		const usage = message.usage;
		if (usage) {
			agg.input += usage.input ?? 0;
			agg.output += usage.output ?? 0;
			agg.cacheRead += usage.cacheRead ?? 0;
			agg.cacheWrite += usage.cacheWrite ?? 0;
			agg.totalTokens += usage.totalTokens ?? 0;
			agg.cost += usage.cost?.total ?? 0;
		}
	}

	if (contextUsage !== undefined && contextUsage.tokens !== null) {
		if (contextUsage.tokens > state.contextPeak) {
			state.contextPeak = contextUsage.tokens;
			state.contextWindow = contextUsage.contextWindow;
		}
	}

	if (message.stopReason === "aborted") state.aborted++;
	if (message.stopReason === "error") {
		if (!state.sawProviderResponse) state.networkErrors++;
		const text = message.errorMessage;
		if (text) state.errors.set(text, (state.errors.get(text) ?? 0) + 1);
	}
	state.pendingThinkingLevel = undefined;
}

export function onToolCall(state: SessionState, toolCallId: string, toolName: string, input: unknown): void {
	state.toolScopes.set(toolCallId, scopeOf(toolName, input));
}

export function onToolExecutionStart(state: SessionState, toolCallId: string, toolName: string): void {
	state.toolStarts.set(toolCallId, { name: toolName, startedAt: Date.now() });
}

export function onToolExecutionEnd(state: SessionState, toolCallId: string, result: unknown, isError: boolean): void {
	const start = state.toolStarts.get(toolCallId);
	state.toolStarts.delete(toolCallId);
	const scope = state.toolScopes.get(toolCallId);
	state.toolScopes.delete(toolCallId);
	const name = start?.name;
	if (!name) return;

	const key = toolKey(name, scope);
	let agg = state.tools.get(key);
	if (!agg) {
		agg = { name, scope, calls: 0, failures: 0, degraded: 0, totalMs: 0, maxMs: 0, resultBytes: 0, truncated: 0 };
		state.tools.set(key, agg);
	}
	agg.calls++;
	if (isError) {
		agg.failures++;
		state.toolFailures++;
	}
	const elapsed = start ? Date.now() - start.startedAt : 0;
	agg.totalMs += elapsed;
	agg.maxMs = Math.max(agg.maxMs, elapsed);
	agg.resultBytes += Buffer.byteLength(contentText(result), "utf8");
	if (readTruncated(result)) agg.truncated++;
	if (readDegraded(name, scope, result)) agg.degraded++;

	const details = (result as { details?: Record<string, unknown> } | undefined)?.details;
	if (name === "runbook") {
		const items = (details?.items ?? (details?.runbook as { items?: unknown[] } | undefined)?.items) as
			| unknown[]
			| undefined;
		if (Array.isArray(items)) agg.entriesTotal = (agg.entriesTotal ?? 0) + items.length;
	}
	if (name === "web_search") {
		const provider = details?.provider;
		if (typeof provider === "string" && provider) agg.providers = bump(agg.providers, provider);
	}
	if (name === "fetch_content") {
		const mode = details?.mode;
		if (typeof mode === "string" && mode) agg.modes = bump(agg.modes, mode);
	}
}

export function onCompact(state: SessionState, tokensBefore: number | undefined, reason: string): void {
	state.compactions++;
	state.compactionTokens += tokensBefore ?? 0;
	if (reason === "overflow") state.compactionOverflows++;
}

export function onCompactFailed(state: SessionState): void {
	state.compactionFailures++;
}
