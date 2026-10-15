import { createHash, createHmac, createPrivateKey, createPublicKey, randomBytes, randomUUID, sign } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { withFileLock } from "./file-lock.ts";

export interface Installation {
	installationId: string;
	identityKey: string;
}

/** 原子初始化，已有文件损坏或不可写时保持未知，不替换已有身份。 */
export async function loadInstallation(agentDir: string): Promise<Installation | undefined> {
	const path = join(agentDir, "telemetry", "identity.json");
	const read = async (): Promise<Installation> => {
		const value = JSON.parse(await readFile(path, "utf8"));
		if (
			typeof value.installationId !== "string" ||
			!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value.installationId) ||
			typeof value.identityKey !== "string" ||
			Buffer.from(value.identityKey, "base64").length !== 32
		)
			throw new Error("安装标识无效");
		return { installationId: value.installationId, identityKey: value.identityKey };
	};
	try {
		return await read();
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") return undefined;
	}
	try {
		return await withFileLock(path, async () => {
			try {
				return await read();
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
			}
			const value = { installationId: randomUUID(), identityKey: randomBytes(32).toString("base64") };
			const temp = `${path}.${randomUUID()}.tmp`;
			try {
				await writeFile(temp, JSON.stringify(value), { mode: 0o600 });
				await rename(temp, path);
				return value;
			} finally {
				await rm(temp, { force: true }).catch(() => undefined);
			}
		});
	} catch {
		return undefined;
	}
}

/** 用独立用途的派生密钥签名；identityKey 与私钥均不发送。 */
export function installationHeaders(installation: Installation, body: string): Record<string, string> {
	const seed = createHmac("sha256", Buffer.from(installation.identityKey, "base64")).update("upload-signing").digest();
	const privateKey = createPrivateKey({
		key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]),
		format: "der",
		type: "pkcs8",
	});
	const publicKey = createPublicKey(privateKey).export({ format: "der", type: "spki" }).toString("base64url");
	const timestamp = String(Date.now());
	const message = [
		"telemetry-upload",
		installation.installationId,
		timestamp,
		createHash("sha256").update(body).digest("hex"),
	].join("\n");
	return {
		Authorization: `Installation ${installation.installationId}`,
		"X-Telemetry-Key": publicKey,
		"X-Telemetry-Time": timestamp,
		"X-Telemetry-Signature": sign(null, Buffer.from(message), privateKey).toString("base64url"),
	};
}
