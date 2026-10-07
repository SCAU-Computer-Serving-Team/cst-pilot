import { createHash } from "node:crypto";
import { realpath } from "node:fs/promises";
import { createServer, type Server } from "node:net";

export class SessionOwnerConflict extends Error {
	readonly code = "CST_SESSION_OWNED";
	constructor() {
		super("该会话正由另一端使用，请先关闭持有该会话的 TUI 或 Web 服务，再重新打开。");
	}
}
/** Windows使用会话身份命名管道，不占TCP端口；其他平台使用本机监听锁。
 * 内核在进程退出后释放，不进行超时抢占。监听仅拒绝连接，不提供读写接口。
 */
export async function ownSession(agentDir: string, id: string): Promise<() => void> {
	let root = await realpath(agentDir);
	if (process.platform === "win32") root = root.toLowerCase();
	const key = createHash("sha256").update(`${root}\0${id}`).digest();
	const port = 12000 + (key.readUInt32BE(0) % 20000);
	const address = process.platform === "win32" ? `\\\\.\\pipe\\cst-pilot-session-${key.toString("hex")}` : undefined;
	const server: Server = createServer((socket) => socket.destroy());
	await new Promise<void>((resolve, reject) => {
		server.once("error", (error) => {
			const code = (error as NodeJS.ErrnoException).code;
			// Bun1.3.14命名管道占用报ERR_INVALID_ARG_TYPE/Failed to listen。
			const conflict =
				code === "EADDRINUSE" ||
				(!!address && code === "ERR_INVALID_ARG_TYPE" && error.message === `Failed to listen at ${address}`);
			reject(conflict ? new SessionOwnerConflict() : error);
		});
		if (address) server.listen(address, resolve);
		else server.listen({ host: "127.0.0.1", port, exclusive: true }, resolve);
	});
	server.unref();
	let closed = false;
	return () => {
		if (!closed) {
			closed = true;
			server.close();
		}
	};
}
