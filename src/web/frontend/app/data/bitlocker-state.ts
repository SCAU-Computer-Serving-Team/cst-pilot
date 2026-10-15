export type PendingBitLocker = {
	requestId: string;
	toolCallId: string;
	entryId?: string;
	kind: "password" | "parameter" | "decrypt";
	expiresAt: number;
	targets: {
		volumeId: string;
		drive: string | null;
		conversionStatus: number | null;
		lockStatus: number | null;
		keyProtectors: { id: string; type: number | null }[];
	}[];
};
export type BitLockerSnapshot = { revision: number; requests: PendingBitLocker[]; epoch?: string };
/** 会话快照与事件可能交错；旧版本不能恢复已经消费的请求。 */
export function newestBitLocker(current: BitLockerSnapshot, incoming?: BitLockerSnapshot): BitLockerSnapshot {
	return incoming && (incoming.epoch !== current.epoch || incoming.revision >= current.revision) ? incoming : current;
}
export function requestForCall(
	requests: PendingBitLocker[],
	callId: string,
	entryId?: string,
): PendingBitLocker | undefined {
	return requests.find(
		(request) => request.toolCallId === callId && (!request.entryId || request.entryId === entryId),
	);
}
