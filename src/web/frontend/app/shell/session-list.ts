export const RECENT_SESSION_LIMIT = 20;

export function orderedSessions<T extends { updatedAt?: string }>(sessions: readonly T[]): T[] {
	const timestamp = (value?: string) => {
		const time = Date.parse(value ?? "");
		return Number.isFinite(time) ? time : -Infinity;
	};
	return [...sessions].sort((a, b) => timestamp(b.updatedAt) - timestamp(a.updatedAt));
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
