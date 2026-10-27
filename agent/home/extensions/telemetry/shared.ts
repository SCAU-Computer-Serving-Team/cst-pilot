/**
 * 进程级共享状态。
 *
 * jiti 以 moduleCache: false 加载扩展，不同会话实例的扩展模块互不共享，
 * 跨实例协调只能挂 globalThis。Web 运行层销毁会话实例时也经同一符号取定稿入口。
 */

import { withinBudget } from "./close-budget.ts";
import type { HeartbeatRun } from "./heartbeat.ts";

export interface TelemetryShared {
	runs: Map<string, HeartbeatRun>;
	shutdowns: Map<string, () => Promise<void>>;
	hints: Map<string, (reason: TelemetryHint) => void>;
	closeBudgets: Map<string, AbortSignal>;
	/** 各端点队列的串行文件操作尾。 */
	fileQueue: Promise<unknown>;
	/** 每个 home 与端点独立发送，互不阻塞。 */
	deliveries: Map<string, { inflight: boolean; stopped: boolean; rerun: boolean }>;
	/** 每个 home 首次初始化时恢复草稿。 */
	recovered: Set<string>;
	/** 定时提交覆盖所有活跃实例，不依赖创建定时器的首个实例。 */
	submissions: Map<string, Map<string, () => Promise<void>>>;
	/** 每个 home 的定时补发；unref 不阻塞正常退出。 */
	retries: Map<string, ReturnType<typeof setInterval>>;
	/** 会话定稿入口，按 sessionId 注册；TUI 由 shutdown 事件触发，Web 由运行层触发。 */
	finalizers: Map<string, (reason: string, signal?: AbortSignal) => Promise<void> | void>;
}

export type TelemetryHint = "pagehide" | "pageshow" | "online";

export const TELEMETRY_SHARED_KEY = Symbol.for("cst-pilot/telemetry");

export function shared(): TelemetryShared {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	holder[TELEMETRY_SHARED_KEY] ??= {
		runs: new Map(),
		shutdowns: new Map(),
		hints: new Map(),
		closeBudgets: new Map(),
		fileQueue: Promise.resolve(),
		deliveries: new Map(),
		recovered: new Set(),
		submissions: new Map(),
		retries: new Map(),
		finalizers: new Map(),
	};
	const state = holder[TELEMETRY_SHARED_KEY]!;
	state.runs ??= new Map();
	state.shutdowns ??= new Map();
	state.hints ??= new Map();
	state.closeBudgets ??= new Map();
	state.retries ??= new Map();
	state.submissions ??= new Map();
	if (!(state.recovered instanceof Set)) state.recovered = new Set();
	return state;
}

/** 真实进程退出使用一次统一预算；会话切换、删除和页面离开不调用。 */
export function beginProcessClose(agentDir: string): AbortSignal {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	const state = holder[TELEMETRY_SHARED_KEY];
	if (!state) return AbortSignal.timeout(5_000);
	state.closeBudgets ??= new Map();
	let signal = state.closeBudgets.get(agentDir);
	if (!signal) {
		signal = AbortSignal.timeout(5_000);
		state.closeBudgets.set(agentDir, signal);
	}
	return signal;
}

/** 宿主取消退出时只清除该次截止时刻，下一次退出重新计时。 */
export function cancelProcessClose(agentDir: string, signal: AbortSignal): void {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	const budgets = holder[TELEMETRY_SHARED_KEY]?.closeBudgets;
	if (budgets?.get(agentDir) === signal) budgets.delete(agentDir);
}

export function announceProcessClose(agentDir: string): void {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	void holder[TELEMETRY_SHARED_KEY]?.shutdowns
		?.get(agentDir)?.()
		.catch(() => undefined);
}

/** 页面通知只触发检查和补发，不创建禁用状态，也不关闭实例。 */
export function hintTelemetry(agentDir: string, reason: TelemetryHint): void {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	holder[TELEMETRY_SHARED_KEY]?.hints?.get(agentDir)?.(reason);
}

export function processCloseBudget(agentDir: string): AbortSignal | undefined {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	return holder[TELEMETRY_SHARED_KEY]?.closeBudgets?.get(agentDir);
}

/** 每个回调在首次 await 前冻结实例统计；单个写盘失败不影响其他实例。 */
export async function submitActive(agentDir: string): Promise<void> {
	await Promise.allSettled([...(shared().submissions.get(agentDir)?.values() ?? [])].map((submit) => submit()));
}

/** 把文件操作挂进串行队列，保证多实例对 pending.jsonl 的读写互斥。 */
export function enqueue<T>(operation: () => Promise<T>): Promise<T> {
	const s = shared();
	const task = s.fileQueue.then(operation, operation);
	s.fileQueue = task.catch(() => undefined);
	return task;
}

/** Web 运行层销毁会话实例时调用：定稿该会话并注销入口。 */
export async function finalizeSession(
	sessionId: string,
	reason: string,
	signal = AbortSignal.timeout(5_000),
): Promise<void> {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	try {
		await withinBudget(
			Promise.resolve(holder[TELEMETRY_SHARED_KEY]?.finalizers.get(sessionId)?.(reason, signal)),
			signal,
		);
	} catch {
		// 定稿失败不影响销毁流程。
	}
}
