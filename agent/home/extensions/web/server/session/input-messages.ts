import type { AgentSession, AgentSessionEvent } from "@earendil-works/pi-coding-agent";

type Message = AgentSession["messages"][number];
const TYPE = "cst-web-input-message";

/** 请求 ID 随会话保存；仅向浏览器投影关联字段，不改动模型消息。 */
export class InputMessages {
	private readonly ids = new Map<number, string>();
	private readonly pending: string[] = [];
	private readonly session: AgentSession;
	private readonly unsubscribe: () => void;
	constructor(session: AgentSession) {
		this.session = session;
		for (const entry of session.sessionManager.getEntries()) {
			if (entry.type !== "custom" || entry.customType !== TYPE) continue;
			const data: unknown = entry.data;
			if (
				data &&
				typeof data === "object" &&
				"id" in data &&
				typeof data.id === "string" &&
				"timestamp" in data &&
				typeof data.timestamp === "number" &&
				Number.isFinite(data.timestamp)
			)
				this.ids.set(data.timestamp, data.id);
		}
		this.unsubscribe = session.subscribe((event) => this.onEvent(event));
	}
	async track(id: string, action: () => Promise<void>, deferred = false) {
		this.pending.push(id);
		try {
			await action();
		} catch (error) {
			this.forget(id);
			throw error;
		} finally {
			if (!deferred) this.forget(id);
		}
	}
	private forget(id: string) {
		const index = this.pending.indexOf(id);
		if (index >= 0) this.pending.splice(index, 1);
	}
	private onEvent(event: AgentSessionEvent) {
		if (event.type === "message_start" && event.message.role === "user") {
			const id = this.pending.shift();
			if (id) this.ids.set(event.message.timestamp, id);
		}
		if (event.type === "message_end" && event.message.role === "user") {
			const id = this.ids.get(event.message.timestamp);
			if (id) this.session.sessionManager.appendCustomEntry(TYPE, { id, timestamp: event.message.timestamp });
		}
		if (event.type === "agent_end") this.pending.length = 0;
	}
	decorate<T extends Message>(message: T): T & { clientMessageId?: string } {
		const id = message.role === "user" ? this.ids.get(message.timestamp) : undefined;
		return id ? { ...message, clientMessageId: id } : message;
	}
	close() {
		this.unsubscribe();
	}
	has(id: string): boolean {
		return [...this.ids.values()].includes(id);
	}
}
