/**
 * 进程级共享状态。
 *
 * jiti 以 moduleCache: false 加载扩展，不同会话实例的扩展模块互不共享，
 * 跨实例协调只能挂 globalThis。Web 运行层销毁会话实例时也经同一符号取定稿入口。
 */

export interface TelemetryShared {
	/** pending.jsonl 的串行队列尾。 */
	fileQueue: Promise<unknown>;
	/** 发送单飞：进程内同时只有一个在途批次。 */
	inflight: boolean;
	/** 恢复单次：进程内首个初始化的实例执行草稿恢复。 */
	recovered: boolean;
	/** 收到 404 / 410 后置位，视为停采指令。 */
	stopped: boolean;
	/** 管理员探测单飞：进程内只发一次，定稿时取结果。 */
	adminProbe?: Promise<boolean>;
	/** 会话定稿入口，按 sessionId 注册；TUI 由 shutdown 事件触发，Web 由运行层触发。 */
	finalizers: Map<string, (reason: string) => void>;
}

export const TELEMETRY_SHARED_KEY = Symbol.for("cst-pilot/telemetry");

export function shared(): TelemetryShared {
	const holder = globalThis as { [TELEMETRY_SHARED_KEY]?: TelemetryShared };
	holder[TELEMETRY_SHARED_KEY] ??= {
		fileQueue: Promise.resolve(),
		inflight: false,
		recovered: false,
		stopped: false,
		finalizers: new Map(),
	};
	return holder[TELEMETRY_SHARED_KEY]!;
}

/** 把文件操作挂进串行队列，保证多实例对 pending.jsonl 的读写互斥。 */
export function enqueue<T>(operation: () => Promise<T>): Promise<T> {
	const s = shared();
	const task = s.fileQueue.then(operation, operation);
	s.fileQueue = task.catch(() => undefined);
	return task;
}

/** Web 运行层销毁会话实例时调用：定稿该会话并注销入口。 */
export function finalizeSession(sessionId: string, reason: string): void {
	try {
		shared().finalizers.get(sessionId)?.(reason);
	} catch {
		// 定稿失败不影响销毁流程。
	}
}
