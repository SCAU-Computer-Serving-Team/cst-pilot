import type { SessionDetail, SessionRow } from "./web-state";

/** 按更新时间选取最新十项；不限制日期跨度，不改变列表顺序。 */
export function recentSessionIds(rows: readonly SessionRow[]): string[] {
	const timestamp = (row: SessionRow) => {
		const time = Date.parse(row.updatedAt ?? "");
		return Number.isFinite(time) ? time : -Infinity;
	};
	return [...new Set([...rows].sort((a, b) => timestamp(b) - timestamp(a)).map((row) => row.id))].slice(0, 10);
}

/** 标签页内存缓存；预载与点击共享请求，旧响应不得覆盖更新或复活已删除会话。 */
export class SessionDetailCache {
	private readonly entries = new Map<string, SessionDetail>();
	private readonly pending = new Map<string, { task: Promise<SessionDetail>; revision: object }>();
	private readonly revisions = new Map<string, object>();
	private readonly fetchDetail: (id: string) => Promise<SessionDetail>;
	private readonly capacity: number;
	private validIds?: Set<string>;

	constructor(fetchDetail: (id: string) => Promise<SessionDetail>, capacity = 20) {
		this.fetchDetail = fetchDetail;
		this.capacity = capacity;
	}

	peek(id: string | undefined): SessionDetail | undefined {
		if (!id) return;
		const value = this.entries.get(id);
		if (value) {
			this.entries.delete(id);
			this.entries.set(id, value);
		}
		return value;
	}

	put(value: SessionDetail): void {
		this.revisions.set(value.id, {});
		this.entries.delete(value.id);
		this.entries.set(value.id, value);
		while (this.entries.size > this.capacity) {
			const oldest = this.entries.keys().next().value!;
			this.entries.delete(oldest);
			if (!this.pending.has(oldest)) this.revisions.delete(oldest);
		}
	}

	remove(id: string): void {
		this.entries.delete(id);
		this.revisions.delete(id);
		this.pending.delete(id);
	}

	load(id: string, fresh = false): Promise<SessionDetail> {
		const pending = this.pending.get(id);
		if (pending) {
			if (fresh && this.revisions.get(id) !== pending.revision) return pending.task.then(() => this.load(id, true));
			return pending.task;
		}
		const cached = this.peek(id);
		if (cached && !fresh) return Promise.resolve(cached);
		const revision = {};
		this.revisions.set(id, revision);
		const request = this.fetchDetail(id)
			.then((value) => {
				if (value.id !== id) throw new Error("会话 ID 与请求不一致");
				if (this.revisions.get(id) === revision) this.put(value);
				return this.peek(id) ?? value;
			})
			.finally(() => {
				if (this.pending.get(id)?.task === request) this.pending.delete(id);
				if (!this.entries.has(id) && this.revisions.get(id) === revision) this.revisions.delete(id);
			});
		this.pending.set(id, { task: request, revision });
		return request;
	}

	async preload(ids: readonly string[]): Promise<void> {
		for (let start = 0; start < ids.length; start += 5) {
			await Promise.allSettled(
				ids
					.slice(start, start + 5)
					.filter((id) => !this.validIds || this.validIds.has(id))
					.map((id) => this.load(id)),
			);
		}
	}

	reconcile(rows: readonly SessionRow[]): void {
		const ids = new Set(rows.map((row) => row.id));
		this.validIds = ids;
		for (const id of this.revisions.keys()) if (!ids.has(id)) this.remove(id);
	}
}
