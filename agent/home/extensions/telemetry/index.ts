/**
 * 遥测事件入口：请求和轮次保存检查点，回答结束与定时检查提交累计快照。
 * submission.ts 冻结统计、串行持久化与入队；网络发送和回答后的查账在后台执行。
 * TUI 经 session_shutdown 关闭，Web 经共享定稿入口关闭；启动时恢复残留草稿。
 */

import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { observeRequests } from "../oauth/request-observer.ts";
import { billingLookup, CstoaBilling } from "./billing.ts";
import { withinBudget } from "./close-budget.ts";
import * as collect from "./collect.ts";
import { type CurrencyMap, loadConfig, loadCurrency, type TelemetryConfig } from "./config.ts";
import { readCredential } from "./credential.ts";
import { flushEndpoints, migratePending, queueRecord } from "./delivery.ts";
import { recoverDrafts } from "./draft.ts";
import { captureEnvironment } from "./environment.ts";
import { HeartbeatRun } from "./heartbeat.ts";
import { loadInstallation } from "./installation.ts";
import { kitVersion } from "./record.ts";
import { announceProcessClose, beginProcessClose, processCloseBudget, shared, submitActive } from "./shared.ts";
import { SessionSubmission } from "./submission.ts";

export default function telemetry(pi: ExtensionAPI): void {
	const agentDir = getAgentDir();
	const extensionDir = dirname(fileURLToPath(import.meta.url));
	const s = shared();

	let config: TelemetryConfig | undefined;
	let currency: CurrencyMap | undefined;
	let version = "";
	let state: collect.SessionState | undefined;
	let lifecycle: { closed: boolean; finalize: (reason: string, signal?: AbortSignal) => Promise<void> } | undefined;
	let ctx: ExtensionContext | undefined;
	let ready: Promise<void> | undefined;
	let submission: SessionSubmission | undefined;
	let recovery: Promise<void> | undefined;

	/** 首个事件到达时加载一次；此后 config / currency / version 为缓存值。
	 * 管理员探测耗时秒级，不进这里：session_start 是会话关键路径，只发探测不等待，定稿时取结果。 */
	function ensureReady(): Promise<void> {
		ready ??= (async () => {
			[config, currency, version] = await Promise.all([
				loadConfig(agentDir),
				loadCurrency(extensionDir),
				kitVersion(extensionDir, process.cwd()),
			]);
		})();
		return ready;
	}

	const active = () => config?.enabled === true && config.endpoints.length > 0;

	function recoverAndFlush(): Promise<void> {
		recovery ??= recover().finally(() => {
			recovery = undefined;
		});
		return recovery;
	}

	async function recover(): Promise<void> {
		try {
			await migratePending(agentDir, config!.endpoints);
			await recoverDrafts(
				agentDir,
				(record) => queueRecord(agentDir, record, config!.endpoints),
				async (record, draft) => {
					if (!draft.cstoaRequests?.length) return record;
					const billing = new CstoaBilling(draft.cstoaRequests);
					await billing.settle(billingLookup(agentDir, fetch, AbortSignal.timeout(4_500)));
					return { ...record, cstoaBilling: billing.summary() };
				},
			);
		} catch {
			// 恢复失败不阻塞上报。
		}
		await flush();
	}

	function queueDraft(): void {
		void submission?.checkpoint().catch(() => undefined);
	}

	async function finalize(reason: string): Promise<void> {
		const signal = processCloseBudget(agentDir) ?? AbortSignal.timeout(5_000);
		await withinBudget(lifecycle?.finalize(reason, signal) ?? Promise.resolve(), signal);
	}

	function safeContextEntries(context: ExtensionContext): number | undefined {
		try {
			return context.sessionManager.buildContextEntries().length;
		} catch {
			return undefined;
		}
	}

	function safeContextUsage(): collect.ContextUsageLike | undefined {
		try {
			return ctx?.getContextUsage();
		} catch {
			return undefined;
		}
	}

	function safeThinkingLevel(): string | undefined {
		try {
			return pi.getThinkingLevel();
		} catch {
			return undefined;
		}
	}

	async function closeHeartbeat(): Promise<void> {
		const credential = await readCredential(agentDir, config!.authProvider);
		const installation = await loadInstallation(agentDir);
		if (installation)
			await s.runs
				.get(agentDir)
				?.pulse(credential ? { installation, access: credential.access } : installation, config!.endpoints, true);
	}

	async function flush(forceHeartbeat = false): Promise<void> {
		if (!active()) return;
		const credential = await readCredential(agentDir, config!.authProvider);
		const installation = await loadInstallation(agentDir);
		const auth = installation && credential ? { installation, access: credential.access } : installation;
		if (auth) {
			// 心跳独立发送，不等待统计积压或当前实例的持久化。
			void s.runs
				.get(agentDir)
				?.pulse(auth, config!.endpoints, false, forceHeartbeat)
				.catch(() => undefined);
			await flushEndpoints(agentDir, auth, config!.endpoints);
		}
		// OA 无效时使用同一安装签名补发，版本身份不变。
		if (credential && installation) await flushEndpoints(agentDir, installation, config!.endpoints);
	}

	pi.on("session_start", async (event, context) => {
		await ensureReady();
		if (!active()) return;
		if (!s.shutdowns.has(agentDir)) s.shutdowns.set(agentDir, closeHeartbeat);
		if (!s.hints.has(agentDir)) {
			let lastHint = -Infinity;
			s.hints.set(agentDir, () => {
				if (Date.now() - lastHint < 2_000) return;
				lastHint = Date.now();
				void flush(true).catch(() => undefined);
				void submitActive(agentDir)
					.then(() => flush())
					.catch(() => undefined);
			});
		}
		if (!s.retries.has(agentDir)) {
			const timer = setInterval(() => {
				void flush().catch(() => undefined);
				void recoverAndFlush().catch(() => undefined);
				void submitActive(agentDir)
					.then(() => flush())
					.catch(() => undefined);
			}, 30_000);
			timer.unref();
			s.retries.set(agentDir, timer);
		}
		// 同一实例被顶替（上一会话未走正常销毁路径）：先把旧累计按 crash 定稿，再开新会话。
		if (state && !lifecycle?.closed) await finalize("crash");
		ctx = context;
		const environmentProbe = captureEnvironment(agentDir);
		if (!s.runs.has(agentDir)) s.runs.set(agentDir, new HeartbeatRun({ environment: environmentProbe, agentDir }));
		state = collect.newState(
			context.sessionManager.getSessionId(),
			event.reason,
			context.mode === "tui" ? "tui" : "web",
		);
		const current = state;
		current.cstoaBilling = new CstoaBilling();
		submission = new SessionSubmission({
			agentDir,
			state: current,
			version,
			environment: environmentProbe,
			endpoints: config!.endpoints,
			flush,
		});
		const writer = submission;
		const stopObserving = observeRequests(current.sessionId, () => {
			const finish = current.cstoaBilling!.start();
			void writer.checkpoint().catch(() => undefined);
			return (id) => {
				finish(id);
				void writer.checkpoint().catch(() => undefined);
			};
		});
		const bound = {
			closed: false,
			async finalize(reason: string, signal?: AbortSignal): Promise<void> {
				if (bound.closed) return;
				stopObserving();
				try {
					await writer.finalize(reason, safeContextEntries(context), signal);
					bound.closed = true;
					if (s.finalizers.get(current.sessionId) === bound.finalize) s.finalizers.delete(current.sessionId);
					s.submissions.get(agentDir)?.delete(current.recordId);
					s.runs.get(agentDir)?.remove(current.recordId);
				} catch {
					// 定时检查继续重试该实例；同一 sessionId 恢复后也保留旧实例的入口。
					s.submissions.get(agentDir)?.set(current.recordId, () => bound.finalize(reason));
				}
			},
		};
		lifecycle = bound;
		s.finalizers.set(current.sessionId, bound.finalize);
		s.runs.get(agentDir)!.register(current.recordId, () => writer.snapshotRevision);
		let submissions = s.submissions.get(agentDir);
		if (!submissions) {
			submissions = new Map();
			s.submissions.set(agentDir, submissions);
		}
		submissions.set(current.recordId, () => writer.submit());
		void flush().catch(() => undefined);
		if (!s.recovered.has(agentDir)) {
			s.recovered.add(agentDir);
			void recoverAndFlush();
		}
	});

	pi.on("session_shutdown", async (event) => {
		if (state && event.reason === "quit" && ctx?.mode === "tui") beginProcessClose(agentDir);
		if (state) await finalize(event.reason);
		if (state && event.reason === "quit" && ctx?.mode === "tui") announceProcessClose(agentDir);
	});

	pi.on("input", (event) => {
		if (state) {
			collect.onInput(state, event.source);
			s.runs.get(agentDir)?.activity(state.recordId, true);
		}
	});

	pi.on("before_provider_request", () => {
		if (state) collect.onBeforeProviderRequest(state, safeThinkingLevel());
	});

	pi.on("agent_start", () => {
		if (state) s.runs.get(agentDir)?.activity(state.recordId, true);
	});

	pi.on("turn_start", () => {
		if (state) collect.onTurnStart(state);
	});

	pi.on("turn_end", (event) => {
		if (!state) return;
		collect.onTurnEnd(state, event.message, safeContextUsage(), currency);
		queueDraft();
	});

	pi.on("agent_settled", () => {
		if (!state || lifecycle?.closed) return;
		s.runs.get(agentDir)?.activity(state.recordId, false);
		const writer = submission;
		void writer
			?.submit()
			.then(() => writer.settleBilling())
			.catch(() => undefined);
	});

	pi.on("after_provider_response", (event) => {
		if (state) {
			collect.onAfterProviderResponse(state, event.status);
			s.runs.get(agentDir)?.activity(state.recordId);
		}
	});

	pi.on("tool_call", (event) => {
		if (state) collect.onToolCall(state, event.toolCallId, event.toolName, event.input);
	});

	pi.on("tool_execution_start", (event) => {
		if (state) collect.onToolExecutionStart(state, event.toolCallId, event.toolName);
	});

	pi.on("tool_execution_end", (event) => {
		if (state) collect.onToolExecutionEnd(state, event.toolCallId, event.result, event.isError === true);
	});

	pi.on("session_compact", (event) => {
		if (state) collect.onCompact(state, event.compactionEntry?.tokensBefore, event.reason);
	});

	pi.on("session_compact_failed", () => {
		if (state) collect.onCompactFailed(state);
	});
}
