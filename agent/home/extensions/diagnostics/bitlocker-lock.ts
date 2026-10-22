import { createHash } from "node:crypto";
import { createServer } from "node:net";

export class BitLockerVolumeBusy extends Error {
	constructor() {
		super("目标卷已有操作进行中");
	}
}

/** Machine-wide identity excludes kit home, session and drive letters. The OS releases it on process exit. */
export async function ownBitLockerVolume(volumeId: string): Promise<() => void> {
	const identity = volumeId
		.trim()
		.replace(/[\\/]+$/, "")
		.toUpperCase();
	if (!identity) throw new Error("目标卷身份缺失");
	const hash = createHash("sha256").update(identity).digest();
	const address =
		process.platform === "win32" ? `\\\\.\\pipe\\cst-pilot-bitlocker-${hash.toString("hex")}` : undefined;
	const server = createServer((socket) => socket.destroy());
	try {
		await new Promise<void>((resolve, reject) => {
			server.once("error", reject);
			if (address) server.listen(address, resolve);
			else
				server.listen(
					{ host: "127.0.0.1", port: 32000 + (hash.readUInt32BE(0) % 20000), exclusive: true },
					resolve,
				);
		});
	} catch (error) {
		const code = (error as NodeJS.ErrnoException).code;
		if (
			code === "EADDRINUSE" ||
			(address &&
				code === "ERR_INVALID_ARG_TYPE" &&
				error instanceof Error &&
				error.message === `Failed to listen at ${address}`)
		)
			throw new BitLockerVolumeBusy();
		throw new Error("无法取得目标卷操作锁，未提交操作", { cause: error });
	}
	server.unref();
	let released = false;
	return () => {
		if (released) return;
		released = true;
		server.close();
	};
}
