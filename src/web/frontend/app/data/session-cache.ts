import type { SessionDetail, SessionRow } from "./web-state";

/** 最近七天按更新时间选取十项；预载不改变列表顺序。 */
export function recentSessionIds(rows: readonly SessionRow[], now = Date.now()): string[] {
	const cutoff = now - 7 * 24 * 60 * 60 * 1000;
	return [
		...new Set(
			rows
				.filter((row) => {
					const time = Date.parse(row.updatedAt ?? "");
					return time >= cutoff && time <= now;
				})
				.sort((a, b) => Date.parse(b.updatedAt!) - Date.parse(a.updatedAt!))
				.map((row) => row.id),
		),
	].slice(0, 10);
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
		let next = 0;
		const worker = async () => {
			while (next < ids.length) {
				const id = ids[next++]!;
				if (this.validIds && !this.validIds.has(id)) continue;
				try {
					await this.load(id);
				} catch {
					/* 打开页面时保留正常的重试入口。 */
				}
			}
		};
		await Promise.all([worker(), worker()]);
	}

	reconcile(rows: readonly SessionRow[]): void {
		const ids = new Set(rows.map((row) => row.id));
		this.validIds = ids;
		for (const id of this.revisions.keys()) if (!ids.has(id)) this.remove(id);
	}
}
