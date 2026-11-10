export const MAX_SESSIONS_PER_SHARD = 32;
const EVENT_SOURCE_CLOSED = 2;

export type WorkerIncomingMessage =
	| { type: "subscribe"; global: boolean; sessions: string[] }
	| { type: "disconnect" }
	| { type: "resume" };

export type WorkerOutgoingMessage =
	| { type: "mode"; mode: "multiplex" | "legacy" | "app_active" }
	| { type: "global"; name: "open" | "state" | "reset" | "error"; data?: string }
	| {
			type: "session";
			sessionId: string;
			name: "open" | "heartbeat" | "session" | "queue" | "reset" | "error";
			data?: string;
	  }
	| { type: "session_not_found"; sessionId: string; message: string };

export interface EventSourceLike {
	readonly readyState: number;
	onopen: ((event: Event) => void) | null;
	onerror: ((event: Event) => void) | null;
	addEventListener(type: string, listener: (event: Event) => void): void;
	close(): void;
}

export interface PortLike {
	postMessage(message: WorkerOutgoingMessage): void;
}

export interface CoordinatorOptions {
	fetchImpl?: typeof fetch;
	createEventSource?: (url: string) => EventSourceLike;
	debounceMs?: number;
	initialBackoffMs?: number;
	maxBackoffMs?: number;
	createId?: () => string;
}

function setsEqual(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
	if (a.size !== b.size) return false;
	for (const item of a) if (!b.has(item)) return false;
	return true;
}

export function parseVectorCursor(lastEventId: string): Map<string, string> {
	const map = new Map<string, string>();
	if (!lastEventId) return map;
	for (const part of lastEventId.split(",")) {
		const trimmed = part.trim();
		if (!trimmed) continue;
		const firstColon = trimmed.indexOf(":");
		if (firstColon <= 0) continue;
		const key = trimmed.slice(0, firstColon);
		const rest = trimmed.slice(firstColon + 1);
		if (rest.includes(":")) map.set(key, rest);
	}
	return map;
}

class MultiplexShard {
	readonly clientId: string;
	readonly isPrimary: boolean;
	readonly assignedSessions = new Set<string>();
	readonly desiredSessions = new Set<string>();
	confirmedSessions = new Set<string>();
	readonly lastEventIds = new Map<string, string>();
	source: EventSourceLike | null = null;
	streamReady = false;
	inFlight = false;
	stopped = false;
	retryAttempt = 0;
	reconnectAttempt = 0;
	debounceTimer: ReturnType<typeof setTimeout> | undefined;
	retryTimer: ReturnType<typeof setTimeout> | undefined;
	reconnectTimer: ReturnType<typeof setTimeout> | undefined;

	private readonly coordinator: EventStreamCoordinator;

	constructor(coordinator: EventStreamCoordinator, clientId: string, isPrimary: boolean) {
		this.coordinator = coordinator;
		this.clientId = clientId;
		this.isPrimary = isPrimary;
	}

	ensureConnected(): void {
		if (this.stopped || this.source || this.reconnectTimer !== undefined) return;
		const source = this.coordinator.createEventSource(
			`/api/events/stream?clientId=${encodeURIComponent(this.clientId)}`,
		);
		this.source = source;
		source.onopen = () => {
			if (this.stopped || this.source !== source) return;
			if (this.isPrimary) this.coordinator.broadcastGlobal("open");
			for (const sid of this.assignedSessions) this.coordinator.broadcastSession(sid, "open");
		};
		source.onerror = () => {
			if (this.stopped || this.source !== source) return;
			this.streamReady = false;
			if (this.isPrimary) this.coordinator.broadcastGlobal("error");
			for (const sid of this.assignedSessions) this.coordinator.broadcastSession(sid, "error");
			if (source.readyState === EVENT_SOURCE_CLOSED) {
				source.close();
				this.source = null;
				this.clearRetryTimer();
				this.scheduleReconnect();
			}
		};
		source.addEventListener("heartbeat", (event) => {
			if (this.stopped || this.source !== source) return;
			this.recordVectorCursor(event);
			for (const sid of this.assignedSessions) this.coordinator.broadcastSession(sid, "heartbeat");
		});
		source.addEventListener("state", (event) => {
			if (this.stopped || this.source !== source) return;
			this.recordVectorCursor(event);
			const data = (event as MessageEvent).data as string | undefined;
			if (data) {
				try {
					const parsed = JSON.parse(data) as {
						type?: string;
						mode?: string;
						subscribedSessions?: string[];
					};
					if (parsed?.type === "stream_ready") {
						this.streamReady = true;
						this.reconnectAttempt = 0;
						this.confirmedSessions = new Set(
							Array.isArray(parsed.subscribedSessions) ? parsed.subscribedSessions : [],
						);
						this.clearRetryTimer();
						this.retryAttempt = 0;
						this.scheduleSync(0);
						return;
					}
					if (parsed?.type === "channel_mode_changed") {
						if (parsed.mode === "app_active") {
							this.coordinator.enterAppActive();
							return;
						}
					}
				} catch {
					/* 非结构化状态帧按普通全局事件分发。 */
				}
			}
			if (this.isPrimary) this.coordinator.broadcastGlobal("state", data);
		});
		source.addEventListener("session", (event) => {
			if (this.stopped || this.source !== source) return;
			this.recordVectorCursor(event);
			try {
				const payload = JSON.parse((event as MessageEvent).data) as {
					sessionId?: string;
					event?: unknown;
				};
				if (typeof payload?.sessionId === "string" && payload.event !== undefined) {
					this.coordinator.broadcastSession(payload.sessionId, "session", JSON.stringify(payload.event));
				}
			} catch {
				/* 忽略损坏帧，等待后续重放或刷新。 */
			}
		});
		source.addEventListener("queue", (event) => {
			if (this.stopped || this.source !== source) return;
			this.recordVectorCursor(event);
			try {
				const payload = JSON.parse((event as MessageEvent).data) as {
					sessionId?: string;
					queue?: unknown;
				};
				if (typeof payload?.sessionId === "string" && payload.queue !== undefined) {
					this.coordinator.broadcastSession(payload.sessionId, "queue", JSON.stringify(payload.queue));
				}
			} catch {
				/* 忽略损坏帧。 */
			}
		});
		source.addEventListener("reset", (event) => {
			if (this.stopped || this.source !== source) return;
			this.recordVectorCursor(event);
			try {
				const payload = JSON.parse((event as MessageEvent).data ?? "{}") as {
					scope?: string;
					sessionId?: string;
				};
				if (payload.scope === "session" && typeof payload.sessionId === "string") {
					this.coordinator.broadcastSession(payload.sessionId, "reset");
					return;
				}
			} catch {
				/* 默认按全局与分片内会话重置。 */
			}
			if (this.isPrimary) this.coordinator.broadcastGlobal("reset");
			for (const sid of this.assignedSessions) this.coordinator.broadcastSession(sid, "reset");
		});
	}

	private scheduleReconnect(): void {
		if (this.stopped || this.reconnectTimer !== undefined) return;
		const delay = Math.min(
			this.coordinator.maxBackoffMs,
			this.coordinator.initialBackoffMs * 2 ** this.reconnectAttempt,
		);
		this.reconnectAttempt++;
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = undefined;
			this.coordinator.recheckCapabilities();
		}, delay);
	}

	private recordVectorCursor(event: Event): void {
		const rawId = (event as MessageEvent).lastEventId;
		if (typeof rawId !== "string" || !rawId) return;
		for (const [key, cursor] of parseVectorCursor(rawId)) {
			this.lastEventIds.set(key, cursor);
		}
	}

	clearDebounceTimer(): void {
		if (this.debounceTimer !== undefined) {
			clearTimeout(this.debounceTimer);
			this.debounceTimer = undefined;
		}
	}

	clearRetryTimer(): void {
		if (this.retryTimer !== undefined) {
			clearTimeout(this.retryTimer);
			this.retryTimer = undefined;
		}
	}

	scheduleSync(delayMs = this.coordinator.debounceMs): void {
		if (this.stopped) return;
		this.clearDebounceTimer();
		if (delayMs <= 0) {
			void this.syncSubscriptions();
			return;
		}
		this.debounceTimer = setTimeout(() => {
			this.debounceTimer = undefined;
			void this.syncSubscriptions();
		}, delayMs);
	}

	async syncSubscriptions(): Promise<void> {
		if (this.stopped || !this.streamReady || this.inFlight) return;
		if (setsEqual(this.desiredSessions, this.confirmedSessions)) return;

		this.inFlight = true;
		this.clearRetryTimer();
		const target = [...this.desiredSessions];
		const lastEventIds: Record<string, string> = {};
		for (const sid of target) {
			if (!this.confirmedSessions.has(sid)) {
				const cursor = this.lastEventIds.get(sid);
				if (cursor) lastEventIds[sid] = cursor;
			}
		}

		type SyncOutcome =
			| { kind: "ok"; subscribed: string[] }
			| { kind: "not_found"; sessionId?: string; message: string }
			| { kind: "app_active" }
			| { kind: "transient" };

		let outcome: SyncOutcome = { kind: "transient" };
		try {
			const response = await this.coordinator.fetchImpl(
				`/api/events/stream/${encodeURIComponent(this.clientId)}/subscriptions`,
				{
					method: "PUT",
					cache: "no-store",
					credentials: "same-origin",
					headers: {
						"Content-Type": "application/json",
						"X-CST-Web-Request": "1",
						"Idempotency-Key": this.coordinator.createId(),
					},
					body: JSON.stringify({ sessions: target, lastEventIds }),
				},
			);
			const data = (await response.json().catch(() => null)) as {
				subscribedSessions?: string[];
				error?: { code?: string; message?: string; sessionId?: string };
			} | null;

			if (response.ok) {
				outcome = {
					kind: "ok",
					subscribed: Array.isArray(data?.subscribedSessions) ? data.subscribedSessions : target,
				};
			} else if (response.status === 404) {
				outcome = {
					kind: "not_found",
					sessionId: data?.error?.sessionId,
					message: data?.error?.message ?? "订阅的会话不存在",
				};
			} else if (response.status === 403 && data?.error?.code === "web_mode_inactive") {
				outcome = { kind: "app_active" };
			} else {
				outcome = { kind: "transient" };
			}
		} catch {
			outcome = { kind: "transient" };
		} finally {
			this.inFlight = false;
		}

		if (this.stopped) return;

		if (outcome.kind === "ok") {
			const previousConfirmed = this.confirmedSessions;
			this.confirmedSessions = new Set(outcome.subscribed);
			this.retryAttempt = 0;
			for (const sid of this.confirmedSessions) {
				if (!previousConfirmed.has(sid)) {
					this.coordinator.broadcastSession(sid, "open");
				}
			}
			if (!setsEqual(this.desiredSessions, this.confirmedSessions)) {
				void this.syncSubscriptions();
			}
			return;
		}

		if (outcome.kind === "not_found") {
			const missingId = outcome.sessionId ?? target.find((sid) => !this.confirmedSessions.has(sid));
			if (missingId) {
				this.coordinator.rejectSession(missingId, outcome.message);
			}
			if (!setsEqual(this.desiredSessions, this.confirmedSessions)) {
				void this.syncSubscriptions();
			}
			return;
		}

		if (outcome.kind === "app_active") {
			this.coordinator.enterAppActive();
			return;
		}

		// Transient failure (network error, 5xx, 429, 409): schedule bounded exponential backoff
		const delay = Math.min(this.coordinator.maxBackoffMs, this.coordinator.initialBackoffMs * 2 ** this.retryAttempt);
		this.retryAttempt++;
		this.clearRetryTimer();
		this.retryTimer = setTimeout(() => {
			this.retryTimer = undefined;
			void this.syncSubscriptions();
		}, delay);
	}

	stop(): void {
		this.stopped = true;
		this.streamReady = false;
		this.clearDebounceTimer();
		this.clearRetryTimer();
		if (this.reconnectTimer !== undefined) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = undefined;
		}
		if (this.source) {
			this.source.close();
			this.source = null;
		}
	}
}

type PortSubscription = {
	global: boolean;
	sessions: Set<string>;
};

export class EventStreamCoordinator {
	readonly fetchImpl: typeof fetch;
	readonly createEventSource: (url: string) => EventSourceLike;
	readonly debounceMs: number;
	readonly initialBackoffMs: number;
	readonly maxBackoffMs: number;
	readonly createId: () => string;

	private mode: "unprobed" | "probing" | "multiplex" | "legacy" | "app_active" = "unprobed";
	private probeAttempt = 0;
	private probeTimer: ReturnType<typeof setTimeout> | undefined;
	private readonly ports = new Map<PortLike, PortSubscription>();
	private readonly shards: MultiplexShard[] = [];
	private readonly rejectedSessions = new Set<string>();

	constructor(options: CoordinatorOptions = {}) {
		this.fetchImpl = options.fetchImpl ?? ((...args) => fetch(...args));
		this.createEventSource = options.createEventSource ?? ((url) => new EventSource(url));
		this.debounceMs = options.debounceMs ?? 40;
		this.initialBackoffMs = options.initialBackoffMs ?? 250;
		this.maxBackoffMs = options.maxBackoffMs ?? 5000;
		this.createId = options.createId ?? (() => crypto.randomUUID());
	}

	get currentMode(): string {
		return this.mode;
	}

	get activeShardCount(): number {
		return this.shards.length;
	}

	updatePort(port: PortLike, global: boolean, sessions: readonly string[]): void {
		const previousSessions = this.ports.get(port)?.sessions;
		const validSessions = new Set(sessions.filter((s) => typeof s === "string" && s.length > 0));
		this.ports.set(port, { global, sessions: validSessions });

		// Clear rejected status for any session that is no longer subscribed by any port
		for (const rejected of [...this.rejectedSessions]) {
			if (!this.isSessionRequestedByAnyPort(rejected)) {
				this.rejectedSessions.delete(rejected);
			}
		}

		if (!this.hasAnySubscriptions()) {
			this.stopAllShards();
			return;
		}

		if (this.mode === "legacy" || this.mode === "app_active") {
			port.postMessage({ type: "mode", mode: this.mode });
			return;
		}

		if (this.mode === "unprobed") {
			void this.probeCapabilities();
			return;
		}

		if (this.mode === "multiplex") {
			port.postMessage({ type: "mode", mode: "multiplex" });
			this.reconcileShards();
			for (const sid of validSessions) {
				const wasSubscribed = previousSessions?.has(sid) ?? false;
				if (!wasSubscribed && this.shards.some((s) => s.confirmedSessions.has(sid))) {
					port.postMessage({ type: "session", sessionId: sid, name: "open" });
				}
			}
		}
	}

	removePort(port: PortLike): void {
		this.ports.delete(port);
		for (const rejected of [...this.rejectedSessions]) {
			if (!this.isSessionRequestedByAnyPort(rejected)) {
				this.rejectedSessions.delete(rejected);
			}
		}
		if (!this.hasAnySubscriptions()) {
			this.stopAllShards();
			return;
		}
		if (this.mode === "multiplex") {
			this.reconcileShards();
		}
	}

	resumeFromStandby(): void {
		if (this.mode === "app_active" || this.mode === "unprobed") {
			this.mode = "unprobed";
			if (this.hasAnySubscriptions()) {
				void this.probeCapabilities();
			}
		}
	}

	recheckCapabilities(): void {
		if (this.mode === "multiplex") this.mode = "unprobed";
		if (this.mode === "unprobed" && this.hasAnySubscriptions()) {
			void this.probeCapabilities();
		}
	}

	private hasAnySubscriptions(): boolean {
		for (const sub of this.ports.values()) {
			if (sub.global || sub.sessions.size > 0) return true;
		}
		return false;
	}

	private isSessionRequestedByAnyPort(sessionId: string): boolean {
		for (const sub of this.ports.values()) {
			if (sub.sessions.has(sessionId)) return true;
		}
		return false;
	}

	private async probeCapabilities(): Promise<void> {
		if (this.mode !== "unprobed") return;
		this.mode = "probing";
		if (this.probeTimer !== undefined) {
			clearTimeout(this.probeTimer);
			this.probeTimer = undefined;
		}

		try {
			const response = await this.fetchImpl("/api/state", {
				method: "GET",
				cache: "no-store",
				credentials: "same-origin",
			});
			const data = (await response.json().catch(() => null)) as {
				stage?: string;
				channelMode?: string;
				multiplexStream?: boolean;
				error?: { code?: string };
			} | null;

			if (this.mode !== "probing") return;
			if (
				(response.status === 403 && data?.error?.code === "web_mode_inactive") ||
				data?.stage === "app_active" ||
				data?.channelMode === "app_active"
			) {
				this.probeAttempt = 0;
				this.enterAppActive();
				return;
			}

			if (response.ok || response.status === 404) {
				this.probeAttempt = 0;
				if (response.ok && data?.multiplexStream === true) {
					this.mode = "multiplex";
					this.broadcastMode("multiplex");
					if (this.hasAnySubscriptions()) {
						this.reconcileShards();
					}
				} else {
					this.mode = "legacy";
					this.stopAllShards();
					this.broadcastMode("legacy");
				}
				return;
			}
		} catch {
			/* 网络错误按退避重试能力协商，不切换为单流模式。 */
		}

		if (this.mode !== "probing") return;
		// Transient failure on GET /api/state: notify error & schedule retry backoff
		this.mode = "unprobed";
		this.broadcastGlobal("error");
		for (const sub of this.ports.values()) {
			for (const sid of sub.sessions) {
				this.broadcastSession(sid, "error");
			}
		}
		const delay = Math.min(this.maxBackoffMs, this.initialBackoffMs * 2 ** this.probeAttempt);
		this.probeAttempt++;
		this.probeTimer = setTimeout(() => {
			this.probeTimer = undefined;
			if (this.mode === "unprobed" && this.hasAnySubscriptions()) {
				void this.probeCapabilities();
			}
		}, delay);
	}

	enterAppActive(): void {
		this.mode = "app_active";
		if (this.probeTimer !== undefined) {
			clearTimeout(this.probeTimer);
			this.probeTimer = undefined;
		}
		this.stopAllShards();
		this.broadcastMode("app_active");
	}

	rejectSession(sessionId: string, message: string): void {
		this.rejectedSessions.add(sessionId);
		for (const [port, sub] of this.ports) {
			if (sub.sessions.has(sessionId)) {
				port.postMessage({ type: "session_not_found", sessionId, message });
			}
		}
		for (const shard of this.shards) {
			shard.assignedSessions.delete(sessionId);
			shard.desiredSessions.delete(sessionId);
		}
	}

	private reconcileShards(): void {
		let wantGlobal = false;
		const wantedSessions = new Set<string>();
		for (const sub of this.ports.values()) {
			if (sub.global) wantGlobal = true;
			for (const sid of sub.sessions) {
				if (!this.rejectedSessions.has(sid)) {
					wantedSessions.add(sid);
				}
			}
		}

		if (!wantGlobal && wantedSessions.size === 0) {
			this.stopAllShards();
			return;
		}

		if (this.shards.length === 0) {
			this.shards.push(new MultiplexShard(this, this.createId(), true));
		}

		// 1. Remove unassigned sessions from existing shards
		for (const shard of this.shards) {
			for (const sid of [...shard.assignedSessions]) {
				if (!wantedSessions.has(sid)) {
					shard.assignedSessions.delete(sid);
					shard.desiredSessions.delete(sid);
				}
			}
		}

		// 2. Assign newly requested sessions to shards with capacity (< 32)
		for (const sid of wantedSessions) {
			const alreadyAssigned = this.shards.some((s) => s.assignedSessions.has(sid));
			if (alreadyAssigned) continue;
			let targetShard = this.shards.find((s) => s.assignedSessions.size < MAX_SESSIONS_PER_SHARD);
			if (!targetShard) {
				targetShard = new MultiplexShard(this, this.createId(), false);
				this.shards.push(targetShard);
			}
			targetShard.assignedSessions.add(sid);
			targetShard.desiredSessions.add(sid);
		}

		// 3. Reclaim empty secondary shards, connect active shards, and schedule subscription sync
		for (let i = this.shards.length - 1; i >= 0; i--) {
			const shard = this.shards[i]!;
			if (!shard.isPrimary && shard.assignedSessions.size === 0) {
				shard.stop();
				this.shards.splice(i, 1);
				continue;
			}
			shard.ensureConnected();
			if (shard.streamReady) {
				shard.scheduleSync();
			}
		}
	}

	private stopAllShards(): void {
		for (const shard of this.shards) {
			shard.stop();
		}
		this.shards.length = 0;
	}

	private broadcastMode(mode: "multiplex" | "legacy" | "app_active"): void {
		for (const port of this.ports.keys()) {
			port.postMessage({ type: "mode", mode });
		}
	}

	broadcastGlobal(name: "open" | "state" | "reset" | "error", data?: string): void {
		for (const [port, sub] of this.ports) {
			if (sub.global) {
				port.postMessage({ type: "global", name, data });
			}
		}
	}

	broadcastSession(
		sessionId: string,
		name: "open" | "heartbeat" | "session" | "queue" | "reset" | "error",
		data?: string,
	): void {
		for (const [port, sub] of this.ports) {
			if (sub.sessions.has(sessionId)) {
				port.postMessage({ type: "session", sessionId, name, data });
			}
		}
	}
}

// SharedWorker entry point when loaded as a browser SharedWorker
declare const self: unknown;
if (
	typeof self === "object" &&
	self !== null &&
	"onconnect" in self &&
	typeof (globalThis as { window?: unknown }).window === "undefined"
) {
	const coordinator = new EventStreamCoordinator();
	(self as { onconnect: ((event: MessageEvent) => void) | null }).onconnect = (event: MessageEvent) => {
		const port = event.ports[0];
		if (!port) return;
		const portAdapter: PortLike = {
			postMessage: (msg) => port.postMessage(msg),
		};
		port.onmessage = (msgEvent: MessageEvent<WorkerIncomingMessage>) => {
			const msg = msgEvent.data;
			if (!msg || typeof msg !== "object") return;
			if (msg.type === "subscribe") {
				coordinator.updatePort(portAdapter, !!msg.global, Array.isArray(msg.sessions) ? msg.sessions : []);
			} else if (msg.type === "disconnect") {
				coordinator.removePort(portAdapter);
			} else if (msg.type === "resume") {
				coordinator.resumeFromStandby();
			}
		};
		port.start();
	};
}
