import {
	type BitLockerOperation,
	type BitLockerResult,
	type BitLockerRunner,
	type BitLockerScope,
	type BitLockerVolume,
	normalizeDrive,
	validateRecoveryPassword,
} from "./bitlocker-data.ts";

export type BitLockerParams = { scope?: BitLockerScope; drive?: string; recoveryPassword?: string };
export type BitLockerInteraction = {
	available: boolean;
	password(volume: BitLockerVolume, signal?: AbortSignal): Promise<string | undefined>;
	confirmParameter(volume: BitLockerVolume, signal?: AbortSignal): Promise<boolean>;
	confirmDecryption(volumes: BitLockerVolume[], signal?: AbortSignal): Promise<boolean>;
};

const NOTICE =
	"状态来自 Win32_EncryptableVolume 本次枚举与实时方法查询；未出现的卷不代表未加密。" +
	"encryptionPercentage 单位为 %，取整后的 0 不保证完全解密；完成以 conversionStatus=0 为准。" +
	"autoUnlockEnabled 仅适用于数据卷，autoUnlockKeysStored 仅适用于当前运行的系统卷。" +
	"字段 null 须结合 collectionErrors 判断；请求结果与当前卷状态分别记录。";
const lockKey = Symbol.for("cst-pilot.bitlocker.volume-locks");
function locks(): Set<string> {
	const globals = globalThis as typeof globalThis & { [lockKey]?: Set<string> };
	globals[lockKey] ??= new Set();
	return globals[lockKey];
}
function operation(status: BitLockerOperation["status"], reason: string): BitLockerOperation {
	return { status, reason, returnCode: null, error: null };
}
function blankVolume(volume: BitLockerVolume, message: string): BitLockerVolume {
	return {
		volumeId: volume.volumeId,
		drive: volume.drive,
		volumeType: volume.volumeType,
		conversionStatus: null,
		encryptionPercentage: null,
		encryptionMethod: null,
		protectionStatus: null,
		lockStatus: null,
		autoUnlockEnabled: null,
		autoUnlockKeysStored: null,
		keyProtectors: [],
		collectionErrors: [{ source: "postOperation", code: null, message }],
	};
}
function decryptReason(volume: BitLockerVolume): BitLockerOperation | undefined {
	if (volume.conversionStatus === 0) return operation("skipped", "卷已完全解密");
	if (volume.conversionStatus === 3) return operation("skipped", "卷正在解密");
	if (volume.lockStatus === 1) return operation("rejected", "卷已锁定");
	if (volume.lockStatus !== 0 || ![1, 2, 4, 5].includes(volume.conversionStatus ?? -1)) {
		return operation("rejected", "执行所需状态缺失或不支持");
	}
	return undefined;
}

/** The runner is the Windows seam; interaction contains only local, non-model dialogs. */
export async function executeBitLocker(
	params: BitLockerParams,
	run: BitLockerRunner,
	interaction: BitLockerInteraction,
	signal?: AbortSignal,
	update?: (stage: string) => void,
): Promise<{ bitlocker: BitLockerResult }> {
	const scope = params.scope ?? "status";
	if (!["status", "unlock", "decrypt"].includes(scope)) throw new Error("scope 必须为 status、unlock 或 decrypt");
	const drive = normalizeDrive(params.drive);
	if (scope === "unlock" && !drive) throw new Error("unlock 必须指定 drive");
	if (scope !== "unlock" && params.recoveryPassword !== undefined) throw new Error("recoveryPassword 仅用于 unlock");
	const provided =
		params.recoveryPassword === undefined ? undefined : validateRecoveryPassword(params.recoveryPassword);
	signal?.throwIfAborted();
	update?.("正在查询 BitLocker 状态");
	const initial = (await run({ scope: "status", drive }, signal)).snapshot;
	if (!initial.volumes.length && initial.collectionErrors.length) {
		throw new Error(`BitLocker 查询失败：${JSON.stringify(initial.collectionErrors)}`);
	}
	if (drive && initial.volumes.length !== 1) throw new Error("指定盘符未找到唯一的可查询 BitLocker 卷");
	const result: BitLockerResult = {
		...initial,
		scope,
		source: "Win32_EncryptableVolume",
		notice: NOTICE,
		degraded: false,
	};
	const finish = () => {
		if (scope === "unlock") {
			for (const volume of result.volumes) {
				if (volume.operation) volume.operation.credentialSource ??= null;
			}
		}
		result.degraded = result.collectionErrors.length > 0 || result.volumes.some((v) => v.collectionErrors.length > 0);
		return { bitlocker: result };
	};
	if (scope === "status") return finish();
	for (const volume of result.volumes) {
		if (scope === "decrypt") volume.operation = decryptReason(volume);
		else if (volume.lockStatus === 0) volume.operation = operation("skipped", "卷已解锁");
		else if (volume.lockStatus !== 1) volume.operation = operation("rejected", "锁定状态采集失败");
	}
	const targets = result.volumes.filter((volume) => !volume.operation);
	if (!targets.length) return finish();
	if (!interaction.available || !initial.admin) {
		for (const volume of targets)
			volume.operation = operation("rejected", !initial.admin ? "需要管理员权限" : "执行操作需要本地交互");
		return finish();
	}
	if (scope === "decrypt") {
		update?.("等待确认解密目标");
		if (!(await interaction.confirmDecryption(targets, signal))) {
			for (const volume of targets) volume.operation = operation("cancelled", "用户取消或确认等待已结束");
			return finish();
		}
	}
	for (let i = 0; i < result.volumes.length; i++) {
		const target = result.volumes[i];
		if (target.operation) continue;
		signal?.throwIfAborted();
		const key = target.volumeId.toUpperCase();
		if (locks().has(key)) {
			target.operation = operation("rejected", "目标卷已有操作进行中");
			continue;
		}
		locks().add(key);
		try {
			let password = provided;
			if (scope === "unlock") {
				if (target.lockStatus === 0) {
					target.operation = operation("skipped", "卷已解锁");
					continue;
				}
				if (target.lockStatus !== 1) {
					target.operation = operation("rejected", "锁定状态采集失败");
					continue;
				}
				update?.("等待本地解锁输入或确认");
				if (password === undefined) password = await interaction.password(target, signal);
				else if (!(await interaction.confirmParameter(target, signal))) password = undefined;
				if (password === undefined) {
					target.operation = operation("cancelled", "用户取消或凭据等待已结束");
					continue;
				}
				password = validateRecoveryPassword(password);
			}
			signal?.throwIfAborted();
			const request = {
				scope,
				drive: target.drive ?? undefined,
				volumeId: target.volumeId,
				recoveryPassword: password,
			};
			update?.(scope === "unlock" ? "正在解锁并复查状态" : "正在提交解密并复查状态");
			try {
				const reply = await run(request, signal);
				const current = reply.snapshot.volumes.find((v) => v.volumeId.toUpperCase() === key);
				result.volumes[i] = current ?? blankVolume(target, "操作后未获取目标卷状态");
				result.volumes[i].operation = reply.operation ?? operation("unknown", "未收到 Windows 操作结果");
				result.collectionErrors.push(...reply.snapshot.collectionErrors);
				result.collectedAt = reply.snapshot.collectedAt;
			} catch {
				const missing = blankVolume(target, "操作调用中断，需重新查询状态");
				missing.operation = operation("unknown", "请求可能已提交，未确认结果；未自动重试");
				result.volumes[i] = missing;
				if (!signal?.aborted) {
					try {
						const reply = await run({ scope: "status", volumeId: target.volumeId }, signal);
						const current = reply.snapshot.volumes.find((v) => v.volumeId.toUpperCase() === key);
						if (current) result.volumes[i] = { ...current, operation: missing.operation };
						result.collectionErrors.push(...reply.snapshot.collectionErrors);
						result.collectedAt = reply.snapshot.collectedAt;
					} catch {
						/* Keep unknown without reusing the pre-operation snapshot. */
					}
				}
			} finally {
				request.recoveryPassword = undefined;
				password = undefined;
			}
			const completed = result.volumes[i].operation;
			if (scope === "unlock" && completed) {
				completed.credentialSource = provided === undefined ? "localInput" : "toolParameter";
			}
			if (signal?.aborted) {
				for (const remaining of result.volumes.slice(i + 1)) {
					remaining.operation ??= operation("cancelled", "提交前已取消");
				}
				break;
			}
		} finally {
			locks().delete(key);
		}
	}
	return finish();
}
