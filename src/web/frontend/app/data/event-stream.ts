import {
	EventStreamCoordinator,
	type PortLike,
	type WorkerIncomingMessage,
	type WorkerOutgoingMessage,
} from "./event-stream-worker";

export type GlobalEventName = "open" | "state" | "reset" | "error";
export type GlobalEventHandler = (name: GlobalEventName, event: MessageEvent | undefined) => void;

export type SessionStreamEventName = "open" | "heartbeat" | "session" | "queue" | "reset" | "error" | "not_found";
export type SessionStreamHandler = (name: SessionStreamEventName, data?: string) => void;

type ChannelModeListener = (mode: "multiplex" | "legacy" | "app_active") => void;

class PageEventStreamClient {
	private mode: "pending" | "multiplex" | "legacy" | "app_active" = "pending";
	private readonly globalListeners = new Set<GlobalEventHandler>();
	private readonly sessionListeners = new Map<string, Set<SessionStreamHandler>>();
	private readonly modeListeners = new Set<ChannelModeListener>();

	private transportSend: ((msg: WorkerIncomingMessage) => void) | null = null;
	private legacyGlobalSource: EventSource | null = null;
	private readonly legacySessionSources = new Map<string, EventSource>();
	private lifecycleBound = false;

	private ensureTransport(): void {
		if (this.transportSend) return;
		this.bindPageLifecycle();

		if (typeof SharedWorker === "function") {
			try {
				const worker = new SharedWorker(new URL("./event-stream-worker.ts", import.meta.url), {
					type: "module",
					name: "cst-pilot-event-stream",
				});
				const port = worker.port;
				port.onmessage = (event: MessageEvent<WorkerOutgoingMessage>) => {
					this.onWorkerMessage(event.data);
				};
				port.start();
				this.transportSend = (msg) => port.postMessage(msg);
				return;
			} catch {
				/* 回退到当前标签页内的聚合协调器。 */
			}
		}

		const coordinator = new EventStreamCoordinator();
		const localPort: PortLike = {
			postMessage: (msg) => this.onWorkerMessage(msg),
		};
		this.transportSend = (msg) => {
			if (msg.type === "subscribe") {
				coordinator.updatePort(localPort, msg.global, msg.sessions);
			} else if (msg.type === "disconnect") {
				coordinator.removePort(localPort);
			} else if (msg.type === "resume") {
				coordinator.resumeFromStandby();
			}
		};
	}

	private bindPageLifecycle(): void {
		if (this.lifecycleBound || typeof window === "undefined") return;
		this.lifecycleBound = true;
		window.addEventListener("pagehide", () => {
			this.transportSend?.({ type: "disconnect" });
			this.closeLegacySources();
		});
		window.addEventListener("pageshow", () => {
			if (this.globalListeners.size > 0 || this.sessionListeners.size > 0) {
				this.pushSubscriptionState();
				if (this.mode === "legacy") {
					this.syncLegacySources();
				}
			}
		});
	}

	private pushSubscriptionState(): void {
		this.ensureTransport();
		const global = this.globalListeners.size > 0;
		const sessions = [...this.sessionListeners.keys()];
		if (!global && sessions.length === 0) {
			this.transportSend?.({ type: "disconnect" });
			return;
		}
		this.transportSend?.({
			type: "subscribe",
			global,
			sessions,
		});
	}

	private onWorkerMessage(msg: WorkerOutgoingMessage): void {
		if (!msg || typeof msg !== "object") return;
		if (msg.type === "mode") {
			this.mode = msg.mode;
			for (const listener of this.modeListeners) listener(msg.mode);
			if (msg.mode === "legacy") {
				this.syncLegacySources();
			} else {
				this.closeLegacySources();
			}
			return;
		}
		if (this.mode === "legacy") return;

		if (msg.type === "global") {
			const evt = msg.data !== undefined ? new MessageEvent(msg.name, { data: msg.data }) : undefined;
			for (const handler of this.globalListeners) {
				handler(msg.name, evt);
			}
			return;
		}

		if (msg.type === "session") {
			const listeners = this.sessionListeners.get(msg.sessionId);
			if (!listeners) return;
			for (const handler of listeners) {
				handler(msg.name, msg.data);
			}
			return;
		}

		if (msg.type === "session_not_found") {
			const listeners = this.sessionListeners.get(msg.sessionId);
			if (!listeners) return;
			for (const handler of listeners) {
				handler("not_found", msg.message);
			}
		}
	}

	private syncLegacySources(): void {
		if (this.mode !== "legacy") return;

		if (this.globalListeners.size > 0) {
			if (!this.legacyGlobalSource) {
				const source = new EventSource("/api/events");
				this.legacyGlobalSource = source;
				for (const name of ["open", "state", "reset", "error"] as const) {
					source.addEventListener(name, (event) => {
						for (const handler of this.globalListeners) {
							handler(name, event as MessageEvent);
						}
					});
				}
			}
		} else if (this.legacyGlobalSource) {
			this.legacyGlobalSource.close();
			this.legacyGlobalSource = null;
		}

		for (const [sid, source] of [...this.legacySessionSources]) {
			if (!this.sessionListeners.has(sid)) {
				source.close();
				this.legacySessionSources.delete(sid);
			}
		}

		for (const sid of this.sessionListeners.keys()) {
			if (!this.legacySessionSources.has(sid)) {
				this.openLegacySessionSource(sid);
			}
		}
	}

	private openLegacySessionSource(sessionId: string): void {
		const existing = this.legacySessionSources.get(sessionId);
		existing?.close();
		const source = new EventSource(`/api/sessions/${encodeURIComponent(sessionId)}/events`);
		this.legacySessionSources.set(sessionId, source);
		const dispatch = (name: SessionStreamEventName, data?: string) => {
			if (this.legacySessionSources.get(sessionId) !== source) return;
			const handlers = this.sessionListeners.get(sessionId);
			if (!handlers) return;
			for (const handler of handlers) handler(name, data);
		};
		source.onopen = () => dispatch("open");
		source.onerror = () => dispatch("error");
		source.addEventListener("heartbeat", () => dispatch("heartbeat"));
		source.addEventListener("session", (event) => dispatch("session", (event as MessageEvent).data));
		source.addEventListener("queue", (event) => dispatch("queue", (event as MessageEvent).data));
		source.addEventListener("reset", () => dispatch("reset"));
	}

	private closeLegacySources(): void {
		if (this.legacyGlobalSource) {
			this.legacyGlobalSource.close();
			this.legacyGlobalSource = null;
		}
		for (const source of this.legacySessionSources.values()) {
			source.close();
		}
		this.legacySessionSources.clear();
	}

	subscribeGlobal(handler: GlobalEventHandler): () => void {
		this.globalListeners.add(handler);
		this.pushSubscriptionState();
		if (this.mode === "legacy") {
			this.syncLegacySources();
		}
		return () => {
			this.globalListeners.delete(handler);
			this.pushSubscriptionState();
			if (this.mode === "legacy") {
				this.syncLegacySources();
			}
		};
	}

	subscribeSession(sessionId: string, handler: SessionStreamHandler): { close: () => void; reconnect: () => void } {
		let set = this.sessionListeners.get(sessionId);
		if (!set) {
			set = new Set();
			this.sessionListeners.set(sessionId, set);
		}
		set.add(handler);
		this.pushSubscriptionState();
		if (this.mode === "legacy") {
			this.syncLegacySources();
		}
		return {
			close: () => {
				const current = this.sessionListeners.get(sessionId);
				if (current) {
					current.delete(handler);
					if (current.size === 0) {
						this.sessionListeners.delete(sessionId);
					}
				}
				this.pushSubscriptionState();
				if (this.mode === "legacy") {
					this.syncLegacySources();
				}
			},
			reconnect: () => {
				if (this.mode === "legacy") {
					if (this.sessionListeners.has(sessionId)) {
						this.openLegacySessionSource(sessionId);
					}
				} else {
					this.pushSubscriptionState();
				}
			},
		};
	}

	subscribeMode(listener: ChannelModeListener): () => void {
		this.modeListeners.add(listener);
		return () => {
			this.modeListeners.delete(listener);
		};
	}

	resumeFromStandby(): void {
		if (this.mode === "app_active") {
			this.mode = "pending";
		}
		this.transportSend?.({ type: "resume" });
		this.pushSubscriptionState();
	}
}

let pageClient: PageEventStreamClient | undefined;
function client(): PageEventStreamClient {
	pageClient ??= new PageEventStreamClient();
	return pageClient;
}

export function subscribeGlobalEvents(handler: GlobalEventHandler): () => void {
	return client().subscribeGlobal(handler);
}

export function subscribeSessionEvents(
	sessionId: string,
	handler: SessionStreamHandler,
): { close: () => void; reconnect: () => void } {
	return client().subscribeSession(sessionId, handler);
}

export function subscribeChannelMode(listener: ChannelModeListener): () => void {
	return client().subscribeMode(listener);
}

export function resumeEventStreamFromStandby(): void {
	client().resumeFromStandby();
}
