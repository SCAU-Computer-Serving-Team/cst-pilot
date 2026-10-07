import type { AgentSession, AgentSessionEvent } from "@earendil-works/pi-coding-agent";

type AgentMessage = AgentSession["messages"][number];
const TYPE = "cst-web-tool-execution";
export interface ToolExecution {
	startedAt: number;
	endedAt?: number;
	result?: AgentMessage;
}
interface RecordEntry extends ToolExecution {
	messageTimestamp: number;
	toolCallId: string;
}
/** 每次调用按助手消息时间戳+调用ID隔离。SDK并行结果消息晚于独立结束事件，及时提供该调用结果。 */
export class ToolExecutions {
	private readonly records = new Map<string, RecordEntry>();
	private readonly active = new Map<string, string>();
	private readonly unsubscribe: () => void;
	private readonly session: AgentSession;
	private readonly clock: () => number;
	constructor(session: AgentSession, clock = Date.now) {
		this.session = session;
		this.clock = clock;
		for (const entry of session.sessionManager.getEntries())
			if (entry.type === "custom" && entry.customType === TYPE) {
				const record = entry.data as RecordEntry | undefined;
				if (
					record &&
					typeof record.toolCallId === "string" &&
					Number.isFinite(record.messageTimestamp) &&
					Number.isFinite(record.startedAt)
				)
					this.records.set(this.key(record.messageTimestamp, record.toolCallId), record);
			}
		this.unsubscribe = session.subscribe((event) => this.onEvent(event));
	}
	private key(timestamp: number, id: string) {
		return `${timestamp}:${id}`;
	}
	private persist(record: RecordEntry) {
		const { result: _, ...data } = record;
		this.session.sessionManager.appendCustomEntry(TYPE, data);
	}
	private onEvent(event: AgentSessionEvent) {
		if (event.type === "tool_execution_start") {
			const message = [...this.session.messages]
				.reverse()
				.find(
					(message) =>
						message.role === "assistant" &&
						message.content.some((part) => part.type === "toolCall" && part.id === event.toolCallId),
				);
			if (!message) return;
			const record: RecordEntry = {
				messageTimestamp: message.timestamp,
				toolCallId: event.toolCallId,
				startedAt: this.clock(),
			};
			const key = this.key(record.messageTimestamp, event.toolCallId);
			this.records.set(key, record);
			this.active.set(event.toolCallId, key);
			this.persist(record);
		} else if (event.type === "tool_execution_end") {
			const key = this.active.get(event.toolCallId),
				record = key ? this.records.get(key) : undefined;
			if (!record) return;
			record.endedAt = Math.max(record.startedAt, this.clock());
			record.result = {
				role: "toolResult",
				toolCallId: event.toolCallId,
				toolName: event.toolName,
				content: event.result.content,
				details: event.result.details,
				isError: event.isError,
				timestamp: record.endedAt,
			};
			this.active.delete(event.toolCallId);
			this.persist(record);
		} else if (event.type === "message_end" && event.message.role === "toolResult") {
			const toolCallId = event.message.toolCallId;
			const records = [...this.records.values()].reverse();
			const record = records.find((record) => record.toolCallId === toolCallId && record.result !== undefined);
			if (record) delete record.result;
		} else if (event.type === "agent_end") {
			for (const key of this.active.values()) {
				const record = this.records.get(key);
				if (record) {
					record.endedAt = Math.max(record.startedAt, this.clock());
					this.persist(record);
				}
			}
			this.active.clear();
		}
	}
	/** 仅增加HTTP展示字段，不修改Pi的模型上下文或原始消息。未知历史时间不估算。 */
	decorate(message: AgentMessage): AgentMessage {
		if (message.role !== "assistant") return message;
		return {
			...message,
			content: message.content.map((part) => {
				if (part.type !== "toolCall") return part;
				const record = this.records.get(this.key(message.timestamp, part.id));
				if (!record) return part;
				const { startedAt, endedAt, result } = record;
				return { ...part, execution: { startedAt, endedAt, result } };
			}),
		};
	}
	close() {
		this.unsubscribe();
	}
}
