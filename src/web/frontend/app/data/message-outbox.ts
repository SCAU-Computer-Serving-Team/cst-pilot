export const OUTBOX_KEY = "cst-message-outbox:v1";
export type OutgoingState = "waiting" | "sending" | "unknown" | "failed" | "accepted";
export type OutgoingMessage = {
	id: string;
	sessionId?: string;
	fromHome: boolean;
	createdAt: number;
	text: string;
	serialized: string;
	images: string[];
	provider?: string;
	modelId?: string;
	thinkingLevel?: string;
	skill?: string;
	state: OutgoingState;
	attempted: boolean;
	uncertain?: boolean;
	error?: string;
};
export type MessageReceipt = {
	id: string;
	status: "pending" | "delivering" | "delivered" | "failed" | "cancelled";
	delivery: "direct" | "queue" | "steer";
	uncertain?: boolean;
};
export function parseMessageReceipt(value: unknown): MessageReceipt {
	if (
		!value ||
		typeof value !== "object" ||
		!("id" in value) ||
		typeof value.id !== "string" ||
		!("status" in value) ||
		!("delivery" in value)
	)
		throw new Error("消息回执无效，请核对发送结果。");
	const { status, delivery } = value;
	if (
		(status !== "pending" &&
			status !== "delivering" &&
			status !== "delivered" &&
			status !== "failed" &&
			status !== "cancelled") ||
		(delivery !== "direct" && delivery !== "queue" && delivery !== "steer")
	)
		throw new Error("消息回执无效，请核对发送结果。");
	return {
		id: value.id,
		status,
		delivery,
		...("uncertain" in value && value.uncertain === true ? { uncertain: true } : {}),
	};
}
export type OutboxLookup = { sessionId?: string; item?: MessageReceipt | null; creationPending?: boolean };
export type OutboxTransport = {
	create(message: OutgoingMessage): Promise<string>;
	send(message: OutgoingMessage): Promise<MessageReceipt>;
	lookup(message: OutgoingMessage): Promise<OutboxLookup>;
	isRejection(error: unknown): boolean;
};

export function parseOutbox(value: string | null): OutgoingMessage[] {
	if (value === null) return [];
	const parsed: unknown = JSON.parse(value);
	if (
		!Array.isArray(parsed) ||
		!parsed.every(
			(item: unknown) =>
				!!item &&
				typeof item === "object" &&
				"id" in item &&
				typeof item.id === "string" &&
				/^[\w-]{1,128}$/.test(item.id) &&
				"text" in item &&
				typeof item.text === "string" &&
				"serialized" in item &&
				typeof item.serialized === "string" &&
				"images" in item &&
				Array.isArray(item.images) &&
				item.images.every((hash: unknown) => typeof hash === "string") &&
				"createdAt" in item &&
				typeof item.createdAt === "number" &&
				Number.isFinite(item.createdAt) &&
				"fromHome" in item &&
				typeof item.fromHome === "boolean" &&
				"attempted" in item &&
				typeof item.attempted === "boolean" &&
				(!("uncertain" in item) || typeof item.uncertain === "boolean") &&
				"state" in item &&
				["waiting", "sending", "unknown", "failed", "accepted"].includes(String(item.state)) &&
				["sessionId", "provider", "modelId", "thinkingLevel", "skill", "error"].every(
					(key) => !(key in item) || typeof Reflect.get(item, key) === "string",
				),
		) ||
		new Set(parsed.map((item) => item.id)).size !== parsed.length
	)
		throw new Error("待发送记录损坏，已保留原记录，请勿清除浏览器数据。");
	return parsed;
}

/** 发送记录先保存再展示；写请求仅由提交或显式重试启动，重连只核对回执。 */
export class MessageOutbox {
	private records: readonly OutgoingMessage[];
	private readonly listeners = new Set<() => void>();
	private readonly active = new Map<string, Promise<void>>();
	private readonly fresh = new Set<string>();
	private readonly checks = new Map<string, Promise<void>>();
	private readonly editing = new Set<string>();
	private readonly storage: Pick<Storage, "getItem" | "setItem">;
	private readonly transport: OutboxTransport;
	private readonly changed?: () => void;
	readonly recoveryError: string | undefined;
	constructor(storage: Pick<Storage, "getItem" | "setItem">, transport: OutboxTransport, changed?: () => void) {
		this.storage = storage;
		this.transport = transport;
		this.changed = changed;
		let saved: OutgoingMessage[] = [];
		try {
			saved = parseOutbox(storage.getItem(OUTBOX_KEY));
		} catch (error) {
			this.recoveryError = error instanceof Error ? error.message : "无法读取待发送记录";
		}
		this.records = saved.map((item) =>
			item.state === "sending" ||
			item.state === "waiting" ||
			item.state === "unknown" ||
			(item.state === "failed" && item.uncertain)
				? {
						...item,
						state: "unknown",
						uncertain: true,
						error: "页面已重新打开，请先核对发送结果。",
					}
				: item,
		);
	}
	subscribe = (listener: () => void) => {
		this.listeners.add(listener);
		return () => {
			this.listeners.delete(listener);
		};
	};
	getSnapshot = () => this.records;
	private save(records: readonly OutgoingMessage[], strict = true) {
		try {
			this.storage.setItem(OUTBOX_KEY, JSON.stringify(records));
		} catch (error) {
			if (strict) throw error;
		}
		this.records = records;
		for (const listener of this.listeners) listener();
		this.changed?.();
	}
	private update(id: string, patch: Partial<OutgoingMessage>) {
		this.save(
			this.records.map((item) => (item.id === id ? { ...item, ...patch } : item)),
			false,
		);
	}
	private find(id: string) {
		return this.records.find((item) => item.id === id);
	}
	claimEntrance(id: string): boolean {
		return this.fresh.delete(id);
	}
	enqueue(input: Omit<OutgoingMessage, "state" | "attempted" | "createdAt" | "fromHome">) {
		if (this.recoveryError) throw new Error(this.recoveryError);
		if (this.find(input.id)) throw new Error("这条消息已经提交，请在消息下方核对状态。");
		const item: OutgoingMessage = {
			...input,
			images: [...input.images],
			fromHome: !input.sessionId,
			createdAt: Date.now(),
			state: "waiting",
			attempted: false,
		};
		this.fresh.add(item.id);
		try {
			this.save([...this.records, item]);
		} catch (error) {
			this.fresh.delete(item.id);
			throw error;
		}
		queueMicrotask(() => this.pump());
	}
	private lane(item: OutgoingMessage) {
		return item.sessionId ?? `home:${item.id}`;
	}
	private pump() {
		for (const item of this.records) {
			if (item.state !== "waiting" || this.editing.has(item.id)) continue;
			const lane = this.lane(item);
			if (
				this.records
					.slice(0, this.records.indexOf(item))
					.some((older) => this.lane(older) === lane && older.state !== "accepted")
			)
				continue;
			void this.attempt(item.id).catch(() => {});
		}
	}
	private attempt(id: string): Promise<void> {
		const running = this.active.get(id);
		if (running) return running;
		const task = this.deliver(id).finally(() => {
			this.active.delete(id);
			this.pump();
		});
		this.active.set(id, task);
		return task;
	}
	private async deliver(id: string) {
		let item = this.find(id);
		if (!item || item.state === "accepted") return;
		try {
			this.update(id, { state: "sending", attempted: true, error: undefined });
			if (!item.sessionId) {
				const sessionId = await this.transport.create(item);
				if (!this.find(id)) return;
				this.update(id, { sessionId });
			}
			item = this.find(id);
			if (!item || item.state === "accepted") return;
			const receipt = await this.transport.send(item);
			this.accept(id, receipt);
		} catch (error) {
			const current = this.find(id);
			if (!current || current.state === "accepted") return;
			const rejected = !current.uncertain && this.transport.isRejection(error);
			this.update(id, {
				state: rejected ? "failed" : "unknown",
				uncertain: current.uncertain || !rejected,
				error: error instanceof Error ? error.message : "发送结果待确认，请核对后重试。",
			});
			if (!rejected) await this.check(id);
		}
	}
	private accept(id: string, receipt: MessageReceipt) {
		if (receipt.id !== id) throw new Error("发送回执与消息不一致，请核对发送结果。");
		const item = this.find(id);
		if (item && item.state !== "accepted") this.update(id, { state: "accepted", error: undefined });
	}
	check(id: string): Promise<void> {
		const running = this.checks.get(id);
		if (running) return running;
		const task = this.lookup(id).finally(() => {
			this.checks.delete(id);
		});
		this.checks.set(id, task);
		return task;
	}
	private async lookup(id: string) {
		const item = this.find(id);
		if (!item) return;
		try {
			const result = await this.transport.lookup(item);
			if (!this.find(id)) return;
			if (result.sessionId && !result.creationPending) this.update(id, { sessionId: result.sessionId });
			if (result.item) this.accept(id, result.item);
		} catch {
			// 查询失败保留原状态；查询错误不能证明写请求未被接受。
		}
	}
	async retry(id: string) {
		if (this.editing.has(id)) throw new Error("正在恢复这条消息，请等待编辑完成。");
		const running = this.active.get(id);
		if (running) await running;
		await this.check(id);
		const item = this.find(id);
		if (!item || item.state === "accepted") return;
		if (this.editing.has(id)) throw new Error("正在恢复这条消息，请等待编辑完成。");
		const blocked = this.records
			.slice(0, this.records.indexOf(item))
			.some((older) => this.lane(older) === this.lane(item) && older.state !== "accepted");
		if (blocked) throw new Error("请先处理前一条未确认的消息。");
		await this.attempt(id);
	}
	async checkAll() {
		await Promise.allSettled(
			this.records
				.filter((item) => item.state === "unknown" || item.state === "accepted")
				.map((item) => this.check(item.id)),
		);
	}
	/** 正式气泡或可见队列已经承接该条记录后才移除本地占位。 */
	observe(sessionId: string, receipts: readonly MessageReceipt[], messageIds: readonly string[]) {
		const known = new Set(messageIds);
		const byId = new Map(receipts.map((item) => [item.id, item]));
		let changed = false;
		const next = this.records.flatMap((item) => {
			if (item.sessionId !== sessionId) return [item];
			const receipt = byId.get(item.id);
			if (known.has(item.id)) {
				changed = true;
				return [];
			}
			if (receipt?.uncertain) {
				if (item.state !== "unknown" || !item.uncertain) changed = true;
				return [{ ...item, state: "unknown" as const, uncertain: true }];
			}
			if (
				receipt &&
				(receipt.delivery !== "direct" || ["failed", "cancelled", "delivered"].includes(receipt.status))
			) {
				changed = true;
				return [];
			}
			if (receipt && item.state !== "accepted") {
				changed = true;
				return [{ ...item, state: "accepted" as const, error: undefined }];
			}
			return [item];
		});
		if (changed) {
			this.save(next);
			this.pump();
		}
	}
	beginEdit(id: string): OutgoingMessage {
		const item = this.find(id);
		if (
			!item ||
			this.active.has(id) ||
			this.checks.has(id) ||
			this.editing.has(id) ||
			(item.state !== "failed" && !(item.state === "waiting" && !item.attempted))
		)
			throw new Error("消息状态已变化，请核对后再编辑。");
		this.editing.add(id);
		return item;
	}
	endEdit(id: string) {
		this.editing.delete(id);
		this.pump();
	}
	completeEdit(id: string) {
		if (!this.editing.has(id)) throw new Error("编辑状态已变化，请重新核对。");
		this.editing.delete(id);
		this.remove(id);
	}
	remove(id: string) {
		const item = this.find(id);
		if (!item) return;
		if (this.editing.has(id) || this.active.has(id) || item.state === "accepted")
			throw new Error("消息已在提交或处理，请先核对状态。");
		this.save(this.records.filter((entry) => entry.id !== id));
		this.fresh.delete(id);
		this.pump();
	}
}
