import { type Static, Type } from "typebox";
import { Value } from "typebox/value";

export { redactRecoveryPasswords, validateRecoveryPassword } from "./bitlocker-password.ts";

const number = Type.Union([Type.Integer({ minimum: 0 }), Type.Null()]);
const boolean = Type.Union([Type.Boolean(), Type.Null()]);
const error = Type.Object({ source: Type.String(), code: number, message: Type.String() });
const volume = Type.Object({
	volumeId: Type.String({ minLength: 1 }),
	drive: Type.Union([Type.String(), Type.Null()]),
	volumeType: number,
	conversionStatus: number,
	encryptionPercentage: number,
	encryptionMethod: number,
	protectionStatus: number,
	lockStatus: number,
	autoUnlockEnabled: boolean,
	autoUnlockKeysStored: boolean,
	keyProtectors: Type.Array(Type.Object({ id: Type.String(), type: number })),
	collectionErrors: Type.Array(error),
});
const snapshot = Type.Object({
	collectedAt: Type.String(),
	admin: Type.Boolean(),
	volumes: Type.Array(volume),
	collectionErrors: Type.Array(error),
});
const action = Type.Object({
	status: Type.Union([
		Type.Literal("accepted"),
		Type.Literal("rejected"),
		Type.Literal("skipped"),
		Type.Literal("unknown"),
	]),
	reason: Type.String(),
	returnCode: number,
	error: Type.Union([error, Type.Null()]),
});

export type BitLockerError = Static<typeof error>;
export type BitLockerVolume = Static<typeof volume> & { operation?: BitLockerOperation };
export type BitLockerSnapshot = Static<typeof snapshot>;
export type BitLockerScope = "status" | "unlock" | "decrypt";
export type BitLockerOperation = {
	status: Static<typeof action>["status"] | "cancelled";
	reason: string;
	returnCode: number | null;
	error: BitLockerError | null;
	credentialSource?: "localInput" | "toolParameter" | null;
};
export type BitLockerRequest = {
	scope: BitLockerScope;
	drive?: string;
	volumeId?: string;
	recoveryPassword?: string;
};
export type BitLockerReply = { snapshot: BitLockerSnapshot; operation?: BitLockerOperation };
export type BitLockerResult = Omit<BitLockerSnapshot, "volumes"> & {
	scope: BitLockerScope;
	source: "Win32_EncryptableVolume";
	volumes: BitLockerVolume[];
	notice: string;
	degraded: boolean;
};
export type BitLockerRunner = (request: BitLockerRequest, signal?: AbortSignal) => Promise<BitLockerReply>;

export function parseBitLockerReply(value: unknown): BitLockerReply {
	const schema = Type.Object({ snapshot, operation: Type.Optional(action) });
	if (!Value.Check(schema, value)) throw new Error("BitLocker 接口返回的数据结构无效");
	return value;
}

export function normalizeDrive(value: string | undefined): string | undefined {
	if (value === undefined) return undefined;
	if (!/^[a-z]:?$/i.test(value)) throw new Error("drive 必须为盘符，如 C 或 C:");
	return `${value[0].toUpperCase()}:`;
}
