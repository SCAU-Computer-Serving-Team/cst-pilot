import type { AgentSession, AgentSessionEvent } from "@earendil-works/pi-coding-agent";

type Message = AgentSession["messages"][number];
const TYPE = "cst-web-work-duration";
export interface WorkDuration {
	startedAt: number;
	endedAt: number;
}
interface WorkRecord extends WorkDuration {
	messageTimestamp: number;
}

/** 每条助手消息完成时记录累计工作区间。自定义条目随 Pi 会话保存，不进入模型上下文。 */
export class WorkDurations {
	private readonly records = new Map<number, WorkRecord>();
	private startedAt: number | undefined;
	private readonly unsubscribe: () => void;
	private readonly session: AgentSession;
	private readonly clock: () => number;
	constructor(session: AgentSession, clock = Date.now) {
		this.session = session;
		this.clock = clock;
		for (const entry of session.sessionManager.getEntries()) {
			if (entry.type !== "custom" || entry.customType !== TYPE) continue;
			const record = entry.data as WorkRecord | undefined;
			if (
				record &&
				Number.isFinite(record.messageTimestamp) &&
				Number.isFinite(record.startedAt) &&
				Number.isFinite(record.endedAt) &&
				record.endedAt >= record.startedAt
			)
				this.records.set(record.messageTimestamp, record);
		}
		this.unsubscribe = session.subscribe((event) => this.onEvent(event));
	}
	private onEvent(event: AgentSessionEvent): void {
		if (event.type === "message_start" && event.message.role === "user") {
			this.startedAt = event.message.timestamp;
		} else if (event.type === "message_end" && event.message.role === "assistant") {
			const record = {
				messageTimestamp: event.message.timestamp,
				startedAt: this.startedAt ?? event.message.timestamp,
				endedAt: Math.max(this.startedAt ?? event.message.timestamp, this.clock()),
			};
			this.records.set(record.messageTimestamp, record);
			this.session.sessionManager.appendCustomEntry(TYPE, record);
		} else if (event.type === "agent_end") {
			this.startedAt = undefined;
		}
	}
	decorate<T extends Message>(message: T): T & { work?: WorkDuration } {
		const record = message.role === "assistant" ? this.records.get(message.timestamp) : undefined;
		if (!record) return message;
		const { startedAt, endedAt } = record;
		return { ...message, work: { startedAt, endedAt } };
	}
	close(): void {
		this.unsubscribe();
	}
}
