export const RECENT_SESSION_LIMIT = 10;

export function orderedSessions<T extends { updatedAt?: string }>(sessions: readonly T[]): T[] {
	const timestamp = (value?: string) => {
		const time = Date.parse(value ?? "");
		return Number.isFinite(time) ? time : -Infinity;
	};
	return [...sessions].sort((a, b) => timestamp(b.updatedAt) - timestamp(a.updatedAt));
}
