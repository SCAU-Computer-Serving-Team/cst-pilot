/**
 * 定稿序列化：累计状态 → 一条会话记录（形状见 doc/telemetry/schema.md）。
 * endedAt / endReason / contextEntries 在定稿时补齐；crash 恢复路径见 draft.ts。
 */

import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { BillingSummary } from "./billing.ts";
import type { ModelAgg, SessionState, ToolAgg } from "./collect.ts";

import { type EnvironmentSnapshot, unknownEnvironment } from "./environment.ts";

export const CONTRACT_VERSION = "0.1";
export const ERRORS_MAX_GROUPS = 10;

/** 发行包根：扩展目录在 <包根>/agent/home/extensions/telemetry，上四级是包根。 */
export function packageRoot(extensionDir: string): string {
	return resolve(extensionDir, "..", "..", "..", "..");
}

export interface SessionRecord extends EnvironmentSnapshot {
	v: string;
	type: "session";
	recordId: string;
	kitVersion: string;
	sessionId: string;
	channel: "tui" | "web";
	reason: string;
	endReason: string;
	startedAt: string;
	endedAt: string;
	durationMs: number;
	activeMs: number;
	prompts: number;
	turns: number;
	contextEntries?: number;
	models: ReturnType<typeof modelEntries>;
	cstoaBilling?: BillingSummary;
	compactions: number;
	compactionTokens: number;
	compactionOverflows: number;
	compactionFailures: number;
	contextPeak: number;
	contextWindow: number;
	tools: ReturnType<typeof toolEntries>;
	providerErrors: Record<string, number>;
	networkErrors: number;
	aborted: number;
	toolFailures: number;
	errors: { message: string; count: number }[];
}

/** 带本地时区偏移的 ISO 8601；toISOString() 是 UTC，不符合契约。 */
export function localIso(epoch: number): string {
	const date = new Date(epoch);
	const offsetMinutes = -date.getTimezoneOffset();
	const sign = offsetMinutes >= 0 ? "+" : "-";
	const pad = (value: number) => String(Math.abs(value)).padStart(2, "0");
	const hh = pad(Math.floor(Math.abs(offsetMinutes) / 60));
	const mm = pad(Math.abs(offsetMinutes) % 60);
	return (
		`${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
		`T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}${sign}${hh}:${mm}`
	);
}

function modelEntries(models: Map<string, ModelAgg>): Omit<ModelAgg, never>[] {
	return [...models.values()].map((model) => ({ ...model }));
}

function toolEntries(tools: Map<string, ToolAgg>): object[] {
	return [...tools.values()].map((agg) => {
		const entry: Record<string, unknown> = { ...agg };
		if (entry.scope === undefined) delete entry.scope;
		if (agg.entriesTotal === undefined) delete entry.entriesTotal;
		if (agg.providers === undefined) delete entry.providers;
		if (agg.modes === undefined) delete entry.modes;
		return entry;
	});
}

export interface FinalizeInput {
	endReason: string;
	endedAt: number;
	contextEntries: number | undefined;
}

export function buildRecord(
	state: SessionState,
	input: FinalizeInput,
	kitVersion: string,
	admin: boolean | null,
	environment: EnvironmentSnapshot = unknownEnvironment(),
): SessionRecord {
	const errorEntries = [...state.errors.entries()]
		.sort((a, b) => b[1] - a[1])
		.slice(0, ERRORS_MAX_GROUPS)
		.map(([message, count]) => ({ message, count }));
	const record: SessionRecord = {
		v: CONTRACT_VERSION,
		type: "session",
		recordId: state.recordId,
		kitVersion,
		sessionId: state.sessionId,
		channel: state.channel,
		reason: state.reason,
		endReason: input.endReason,
		startedAt: localIso(state.startedAt),
		endedAt: localIso(input.endedAt),
		durationMs: input.endedAt - state.startedAt,
		activeMs: state.activeMs,
		prompts: state.prompts,
		turns: state.turns,
		models: modelEntries(state.models),
		compactions: state.compactions,
		compactionTokens: state.compactionTokens,
		compactionOverflows: state.compactionOverflows,
		compactionFailures: state.compactionFailures,
		contextPeak: state.contextPeak,
		contextWindow: state.contextWindow,
		tools: toolEntries(state.tools),
		providerErrors: Object.fromEntries(state.providerErrors),
		networkErrors: state.networkErrors,
		aborted: state.aborted,
		toolFailures: state.toolFailures,
		errors: errorEntries,
		...environment,
		admin,
	};
	if (state.cstoaBilling?.summary()) record.cstoaBilling = state.cstoaBilling.summary();
	if (input.contextEntries !== undefined) record.contextEntries = input.contextEntries;
	return record;
}

/** crash 恢复：从草稿记录直接改写，不重新累计。 */
export function crashRecord(draft: SessionRecord, lastTurnEndedAt: number): SessionRecord {
	const endedAt = lastTurnEndedAt || Date.parse(draft.startedAt);
	return {
		...draft,
		recordId: draft.recordId || randomUUID(),
		endReason: "crash",
		endedAt: localIso(endedAt),
		durationMs: endedAt - Date.parse(draft.startedAt),
	};
}

let kitVersionCache: string | undefined;

/** 发行版根的 VERSION。启动目录不定，优先按扩展目录定位包根，回退启动目录；读不到留空且不缓存，下次事件重试（文件读毫秒级）。 */
export async function kitVersion(extensionDir: string, cwd: string): Promise<string> {
	if (kitVersionCache !== undefined) return kitVersionCache;
	for (const file of [join(packageRoot(extensionDir), "VERSION"), join(cwd, "VERSION")]) {
		try {
			const value = (await readFile(file, "utf8")).trim();
			if (value) {
				kitVersionCache = value;
				return kitVersionCache;
			}
		} catch {
			// 读不到就试下一个候选。
		}
	}
	return "";
}
