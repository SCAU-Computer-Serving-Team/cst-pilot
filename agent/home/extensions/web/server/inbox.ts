import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";

export type Delivery = "queue" | "steer";
export type ItemStatus = "pending" | "delivering" | "delivered" | "failed" | "cancelled";
export interface InboxItem {
	id: string;
	sequence: number;
	text: string;
	delivery: Delivery;
	acceptedText: string;
	acceptedDelivery: Delivery;
	status: ItemStatus;
	acceptedAt: number;
}
export interface InboxSnapshot {
	version: number;
	nextSequence: number;
	paused: boolean;
	items: InboxItem[];
}
export class InboxConflict extends Error {}

export interface InboxExecutor {
	isBusy(): boolean;
	prompt(text: string): Promise<void>;
	steer(text: string): Promise<void>;
}

/** One inbox per session; only the owning server process may write its file. */
export class SessionInbox {
	private data?: InboxSnapshot;
	private serial: Promise<void> = Promise.resolve();
	private dispatching = false;
	private wakeRequested = false;
	private readonly file: string;
	readonly sessionId: string;
	private readonly executor: InboxExecutor;
	private readonly onChange?: (snapshot: InboxSnapshot) => void;

	constructor(root: string, sessionId: string, executor: InboxExecutor, onChange?: (snapshot: InboxSnapshot) => void) {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(sessionId)) throw new Error("会话 ID 无效");
		this.sessionId = sessionId;
		this.executor = executor;
		this.onChange = onChange;
		this.file = join(root, `${sessionId}.json`);
	}

	private async load(): Promise<InboxSnapshot> {
		if (this.data) return this.data;
		try {
			const parsed: unknown = JSON.parse(await readFile(this.file, "utf8"));
			if (!validSnapshot(parsed)) throw new Error("输入收件箱损坏");
			// A crashed process may already have sent this input to the model.
			// Keep it visible, but never automatically replay it.
			this.data = parsed;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
			this.data = { version: 0, nextSequence: 1, paused: false, items: [] };
		}
		return this.data;
	}

	private async save(next: InboxSnapshot): Promise<void> {
		await mkdir(join(this.file, ".."), { recursive: true });
		const temp = `${this.file}.${randomUUID()}.tmp`;
		try {
			await writeFile(temp, JSON.stringify(next), { flag: "wx" });
			await rename(temp, this.file);
		} catch (error) {
			await rm(temp, { force: true }).catch(() => undefined);
			throw error;
		}
		this.data = next;
		try {
			this.onChange?.(structuredClone(next));
		} catch {
			// Notification failure must not turn a successfully persisted input into an HTTP retry.
		}
	}

	private transact<T>(action: (current: InboxSnapshot) => Promise<T>): Promise<T> {
		const task = this.serial.then(async () => action(await this.load()));
		this.serial = task.then(
			() => undefined,
			() => undefined,
		);
		return task;
	}

	async snapshot(): Promise<InboxSnapshot> {
		return this.transact(async (current) => structuredClone(current));
	}

	async accept(id: string, text: string, delivery: Delivery): Promise<InboxItem> {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(id) || !text.trim() || text.length > 100_000) {
			throw new Error("消息格式无效");
		}
		if (delivery !== "queue" && delivery !== "steer") throw new Error("提交方式无效");
		const item = await this.transact(async (current) => {
			const existing = current.items.find((entry) => entry.id === id);
			if (existing) {
				if (existing.acceptedText !== text || existing.acceptedDelivery !== delivery)
					throw new InboxConflict("消息 ID 已用于其他内容");
				return { ...existing };
			}
			const added: InboxItem = {
				id,
				text,
				delivery,
				acceptedText: text,
				acceptedDelivery: delivery,
				status: "pending",
				sequence: current.nextSequence,
				acceptedAt: Date.now(),
			};
			await this.save({
				...current,
				version: current.version + 1,
				nextSequence: current.nextSequence + 1,
				paused: false,
				items: [...current.items, added],
			});
			return { ...added };
		});
		this.schedule();
		return item;
	}

	async pause(): Promise<void> {
		await this.transact(async (current) => {
			await this.save({ ...current, paused: true, version: current.version + 1 });
		});
	}

	async change(
		id: string,
		version: number,
		edit: { text?: string; delivery?: Delivery; remove?: boolean },
	): Promise<InboxSnapshot> {
		const result = await this.transact(async (current) => {
			if (current.version !== version) throw new InboxConflict("队列已更新，请重新读取");
			const target = current.items.find((item) => item.id === id && item.status === "pending");
			if (!target) throw new InboxConflict("条目已投递或不存在");
			if (edit.text !== undefined && (!edit.text.trim() || edit.text.length > 100_000))
				throw new Error("消息格式无效");
			if (edit.delivery !== undefined && edit.delivery !== "queue" && edit.delivery !== "steer")
				throw new Error("提交方式无效");
			const items = current.items.map((item) =>
				item.id === id
					? {
							...item,
							text: edit.text ?? item.text,
							delivery: edit.delivery ?? item.delivery,
							status: edit.remove ? ("cancelled" as const) : item.status,
						}
					: item,
			);
			const next = { ...current, version: version + 1, items };
			await this.save(next);
			return structuredClone(next);
		});
		this.schedule();
		return result;
	}

	async reorder(ids: string[], version: number): Promise<InboxSnapshot> {
		return this.transact(async (current) => {
			if (current.version !== version) throw new InboxConflict("队列已更新，请重新读取");
			const pending = current.items.filter((item) => item.status === "pending" && item.delivery === "queue");
			if (
				ids.length !== pending.length ||
				new Set(ids).size !== ids.length ||
				pending.some((item) => !ids.includes(item.id))
			) {
				throw new InboxConflict("排序条目与当前队列不一致");
			}
			const byId = new Map(pending.map((item) => [item.id, item]));
			const slots = new Set(pending.map((item) => item.id));
			const ordered = ids.map((id) => byId.get(id)!);
			let index = 0;
			const next = {
				...current,
				version: version + 1,
				items: current.items.map((item) => (slots.has(item.id) ? ordered[index++] : item)),
			};
			await this.save(next);
			return structuredClone(next);
		});
	}

	/** No dispatch on load. A new submission or an SDK idle event may wake this process's inbox. */
	wake(): void {
		this.schedule();
	}

	private schedule(): void {
		if (this.dispatching) {
			this.wakeRequested = true;
			return;
		}
		this.dispatching = true;
		void this.drain().finally(() => {
			this.dispatching = false;
			if (this.wakeRequested) {
				this.wakeRequested = false;
				this.schedule();
			}
		});
	}

	private async drain(): Promise<void> {
		try {
			while (true) {
				const next = await this.transact(async (current) => {
					if (current.paused || current.items.some((item) => item.status === "failed")) return undefined;
					const busy = this.executor.isBusy();
					const pending = current.items.find(
						(item) => item.status === "pending" && (item.delivery === "steer" || !busy),
					);
					if (!pending) return undefined;
					const item = { ...pending, status: "delivering" as const };
					await this.save({
						...current,
						version: current.version + 1,
						items: current.items.map((entry) => (entry.id === item.id ? item : entry)),
					});
					return { item, busy };
				});
				if (!next) return;
				try {
					if (next.busy && next.item.delivery === "steer") await this.executor.steer(next.item.text);
					else await this.executor.prompt(next.item.text);
					await this.transact(async (current) => {
						await this.save({
							...current,
							version: current.version + 1,
							items: current.items.map((entry) =>
								entry.id === next.item.id ? { ...entry, status: "delivered" } : entry,
							),
						});
					});
				} catch {
					await this.transact(async (current) => {
						await this.save({
							...current,
							version: current.version + 1,
							items: current.items.map((entry) =>
								entry.id === next.item.id ? { ...entry, status: "failed" } : entry,
							),
						});
					});
					return; // Never skip a failed item.
				}
			}
		} catch {
			// Persistence failed; leave the last durable state untouched and do not submit anything else.
		}
	}
}

function validSnapshot(value: unknown): value is InboxSnapshot {
	if (!value || typeof value !== "object") return false;
	const data = value as Partial<InboxSnapshot>;
	return (
		Number.isSafeInteger(data.version) &&
		Number.isSafeInteger(data.nextSequence) &&
		typeof data.paused === "boolean" &&
		Array.isArray(data.items) &&
		data.items.every(
			(item: InboxItem) =>
				typeof item.id === "string" &&
				typeof item.text === "string" &&
				typeof item.acceptedText === "string" &&
				(item.acceptedDelivery === "queue" || item.acceptedDelivery === "steer") &&
				Number.isSafeInteger(item.sequence) &&
				(item.delivery === "queue" || item.delivery === "steer") &&
				["pending", "delivering", "delivered", "failed", "cancelled"].includes(item.status),
		)
	);
}
