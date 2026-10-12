import type { SessionRow } from "./web-state.ts";

/** 页面内删除事务：隐藏待确认项，失败恢复快照，成功项阻止旧列表复活。 */
export class OptimisticSessionDeletion {
	private readonly pending = new Map<string, SessionRow>();
	private readonly confirmed = new Set<string>();

	begin(rows: readonly SessionRow[], ids: readonly string[]): SessionRow[] {
		const selected = new Set(ids);
		for (const row of rows) if (selected.has(row.id)) this.pending.set(row.id, row);
		return this.visible(rows);
	}

	reconcile(rows: readonly SessionRow[]): SessionRow[] {
		const existing = new Set(rows.map((row) => row.id));
		for (const row of rows) if (this.pending.has(row.id)) this.pending.set(row.id, row);
		for (const id of this.confirmed) if (!existing.has(id)) this.confirmed.delete(id);
		return this.visible(rows);
	}

	settle(rows: readonly SessionRow[], deleted: readonly string[], failed: readonly string[]): SessionRow[] {
		const restored: SessionRow[] = [];
		for (const id of deleted) {
			this.pending.delete(id);
			this.confirmed.add(id);
		}
		for (const id of failed) {
			const snapshot = this.pending.get(id);
			if (snapshot) restored.push(snapshot);
			this.pending.delete(id);
		}
		const merged = new Map(this.visible(rows).map((row) => [row.id, row]));
		for (const row of restored) if (!merged.has(row.id)) merged.set(row.id, row);
		return [...merged.values()].sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""));
	}

	private visible(rows: readonly SessionRow[]): SessionRow[] {
		return rows.filter((row) => !this.pending.has(row.id) && !this.confirmed.has(row.id));
	}
}
