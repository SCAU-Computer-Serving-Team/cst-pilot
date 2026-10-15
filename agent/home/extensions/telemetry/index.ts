/**
 * 遥测发送端入口。
 *
 * 采集订阅会话事件、轮次级草稿、会话结束定稿、异步上报：
 * - turn_end：把累计状态重写为该会话草稿（两端一致）。
 * - shutdown：定稿。TUI 走 pi 的 session_shutdown 事件；Web 由运行层销毁实例时经
 *   globalThis 入口调用（见 WebSessionPool 的 deleteSaved 与 close）。
 * - 进程启动：首个实例执行草稿恢复，补 endReason = crash 的记录。
 * 正常退出等待各端点队列落盘；网络异步，不抛错、不向队员输出。
 */

import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import * as collect from "./collect.ts";
import { type CurrencyMap, loadConfig, loadCurrency, type TelemetryConfig } from "./config.ts";
import { readCredential } from "./credential.ts";
import { flushEndpoints, migratePending, queueRecord } from "./delivery.ts";
import { recoverDrafts, removeDraft, writeDraft } from "./draft.ts";
import { captureEnvironment, type EnvironmentSnapshot, unknownEnvironment } from "./environment.ts";
import { loadInstallation } from "./installation.ts";
import { buildRecord, kitVersion } from "./record.ts";
import { shared } from "./shared.ts";

export default function telemetry(pi: ExtensionAPI): void {
	const agentDir = getAgentDir();
	const extensionDir = dirname(fileURLToPath(import.meta.url));
	const s = shared();

	let config: TelemetryConfig | undefined;
	let currency: CurrencyMap | undefined;
	let version = "";
	let state: collect.SessionState | undefined;
	let finalized = false;
	let ctx: ExtensionContext | undefined;
	let ready: Promise<void> | undefined;
	let environment = unknownEnvironment();
	let environmentProbe: Promise<EnvironmentSnapshot> | undefined;

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

	async function recoverAndFlush(): Promise<void> {
		try {
			await migratePending(agentDir, config!.endpoints);
			await recoverDrafts(agentDir, (record) => queueRecord(agentDir, record, config!.endpoints));
		} catch {
			// 恢复失败不阻塞上报。
		}
		await flush();
	}

	async function writeCurrentDraft(): Promise<void> {
		if (!state || finalized) return;
		environment = (await environmentProbe) ?? environment;
		const record = buildRecord(
			state,
			{ endReason: "", endedAt: Date.now(), contextEntries: undefined },
			version,
			environment.admin,
			environment,
		);
		await writeDraft(agentDir, state.sessionId, {
			record,
			lastTurnEndedAt: state.lastTurnEndedAt ?? Date.now(),
		});
	}

	/** 同一会话的草稿写串行化：连续 turn_end 的重写并发会让临时文件 rename 乱序，旧内容覆盖新内容。 */
	let draftChain: Promise<void> = Promise.resolve();
	function queueDraft(): void {
		draftChain = draftChain.then(() => writeCurrentDraft()).catch(() => undefined);
	}

	async function finalize(reason: string): Promise<void> {
		const current = state;
		if (!current || finalized) return;
		finalized = true;
		s.finalizers.delete(current.sessionId);
		try {
			if (current.turns === 0 && current.prompts === 0) {
				await removeDraft(agentDir, current.sessionId);
				return;
			}
			environment = (await environmentProbe) ?? environment;
			await draftChain;
			const record = buildRecord(
				current,
				{ endReason: reason, endedAt: Date.now(), contextEntries: safeContextEntries() },
				version,
				environment.admin,
				environment,
			);
			await writeDraft(agentDir, current.sessionId, {
				record,
				lastTurnEndedAt: current.lastTurnEndedAt ?? Date.now(),
			});
			await queueRecord(agentDir, record, config!.endpoints);
			await removeDraft(agentDir, current.sessionId);
		} catch {
			// 入队失败保留草稿，不影响退出流程。
		}
		void flush();
	}

	function safeContextEntries(): number | undefined {
		try {
			return ctx?.sessionManager.buildContextEntries().length;
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

	async function flush(): Promise<void> {
		if (!active()) return;
		const credential = await readCredential(agentDir, config!.authProvider);
		const auth = credential?.access ?? (await loadInstallation(agentDir));
		if (auth) await flushEndpoints(agentDir, auth, config!.endpoints);
		// OA 无效时仍可使用安装凭据补发；两次发送使用相同 recordId。
		if (credential) {
			const installation = await loadInstallation(agentDir);
			if (installation) await flushEndpoints(agentDir, installation, config!.endpoints);
		}
	}

	pi.on("session_start", async (event, context) => {
		await ensureReady();
		if (!active()) return;
		if (!s.retries.has(agentDir)) {
			const timer = setInterval(() => {
				void flush().catch(() => undefined);
			}, 30_000);
			timer.unref();
			s.retries.set(agentDir, timer);
		}
		// 同一实例被顶替（上一会话未走正常销毁路径）：先把旧累计按 crash 定稿，再开新会话。
		if (state && !finalized) await finalize("crash");
		ctx = context;
		environment = unknownEnvironment();
		environmentProbe = captureEnvironment(agentDir).then((value) => {
			environment = value;
			return value;
		});
		state = collect.newState(
			context.sessionManager.getSessionId(),
			event.reason,
			context.mode === "tui" ? "tui" : "web",
		);
		finalized = false;
		s.finalizers.set(state.sessionId, (reason: string) => finalize(reason));
		if (!s.recovered) {
			s.recovered = true;
			void recoverAndFlush();
		} else {
			void flush();
		}
	});

	pi.on("session_shutdown", async (event) => {
		if (state) await finalize(event.reason);
	});

	pi.on("input", (event) => {
		if (state) collect.onInput(state, event.source);
	});

	pi.on("before_provider_request", () => {
		if (state) collect.onBeforeProviderRequest(state, safeThinkingLevel());
	});

	pi.on("turn_start", () => {
		if (state) collect.onTurnStart(state);
	});

	pi.on("turn_end", (event) => {
		if (!state) return;
		collect.onTurnEnd(state, event.message, safeContextUsage(), currency);
		queueDraft();
	});

	pi.on("after_provider_response", (event) => {
		if (state) collect.onAfterProviderResponse(state, event.status);
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
