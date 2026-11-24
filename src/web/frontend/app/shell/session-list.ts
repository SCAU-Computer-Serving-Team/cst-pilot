export const RECENT_SESSION_LIMIT = 20;

export function orderedSessions<T extends { updatedAt?: string }>(sessions: readonly T[]): T[] {
	const timestamp = (value?: string) => {
		const time = Date.parse(value ?? "");
		return Number.isFinite(time) ? time : -Infinity;
	};
	return [...sessions].sort((a, b) => timestamp(b.updatedAt) - timestamp(a.updatedAt));
}

/**
 * 按更新时间倒序筛选侧栏最近会话（与桌面端 `filter_recent_sidebar_sessions` 对齐）：
 * 忽略大小写与首尾空白，空标题按「新对话」参与匹配，默认上限 20 条。
 */
export function filterRecentSessions<T extends { title?: string; updatedAt?: string }>(
	sessions: readonly T[],
	query: string,
	limit = RECENT_SESSION_LIMIT,
): T[] {
	const sorted = orderedSessions(sessions);
	const normalized = query.trim().toLowerCase();
	const matched = normalized
		? sorted.filter((session) => {
				const title = session.title?.trim() || "新对话";
				return title.toLowerCase().includes(normalized);
			})
		: sorted;
	return matched.slice(0, limit);
}

export function groupSessions<T extends { updatedAt?: string }>(sessions: readonly T[], now = new Date()) {
	const start = new Date(now);
	start.setHours(0, 0, 0, 0);
	const week = new Date(start);
	week.setDate(week.getDate() - 6);
	const month = new Date(start);
	month.setDate(month.getDate() - 29);
	const groups: { id: string; label: string; sessions: T[] }[] = [
		{ id: "today", label: "今天", sessions: [] },
		{ id: "week", label: "最近一周", sessions: [] },
		{ id: "month", label: "最近一个月", sessions: [] },
		{ id: "earlier", label: "更早", sessions: [] },
	];
	for (const session of orderedSessions(sessions)) {
		const time = Date.parse(session.updatedAt ?? "");
		const index = time >= start.getTime() ? 0 : time >= week.getTime() ? 1 : time >= month.getTime() ? 2 : 3;
		groups[index].sessions.push(session);
	}
	return groups.filter((group) => group.sessions.length > 0);
}
