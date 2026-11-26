import type { InboxSnapshot } from "../data/web-state";

export function queuedIdsOf(queue: InboxSnapshot): string[] {
	return queue.items
		.filter((item) => item.status === "pending" && item.delivery === "queue")
		.map((item) => item.id);
}

/**
 * Reconciles a pending user reorder intent against the latest confirmed queue snapshot.
 * Returns null if the intent no longer changes the confirmed order.
 */
export function reconcilePendingOrder(confirmed: InboxSnapshot, desiredIds: string[]): string[] | null {
	const currentIds = queuedIdsOf(confirmed);
	if (currentIds.length < 2) return null;
	const currentSet = new Set(currentIds);
	const merged: string[] = [];
	for (const id of desiredIds) {
		if (currentSet.has(id) && !merged.includes(id)) merged.push(id);
	}
	for (const id of currentIds) {
		if (!merged.includes(id)) merged.push(id);
	}
	if (merged.length !== currentIds.length || merged.every((id, index) => id === currentIds[index])) {
		return null;
	}
	return merged;
}

/**
 * Accepts an incoming queue snapshot only when its version is at least the confirmed version.
 */
export function acceptNewerQueue(current: InboxSnapshot, incoming?: InboxSnapshot | null): InboxSnapshot {
	if (!incoming || typeof incoming.version !== "number") return current;
	return incoming.version >= current.version ? incoming : current;
}
