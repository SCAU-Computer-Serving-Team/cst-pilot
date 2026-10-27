import { randomUUID } from "node:crypto";
import { createConnection, createServer } from "node:net";

export interface DraftOwner {
	token: string;
	/** Windows 命名管道；其他平台仅监听随机本机端口。 */
	address: string | number;
}

const key = Symbol.for("cst-pilot/telemetry-draft-owner");

/** 内核随进程退出释放；所有者证明仅写本地草稿，不进入上传记录。 */
export function draftOwner(): Promise<DraftOwner> {
	const holder = globalThis as { [key]?: Promise<DraftOwner> };
	holder[key] ??= new Promise<DraftOwner>((resolve, reject) => {
		const token = randomUUID();
		const pipe = `\\\\.\\pipe\\cst-pilot-telemetry-${token}`;
		const server = createServer((socket) => {
			socket.on("error", () => undefined);
			socket.end(token);
		});
		server.once("error", reject);
		const ready = () => {
			server.unref();
			const address = server.address();
			resolve({ token, address: typeof address === "string" ? address : address!.port });
		};
		if (process.platform === "win32") server.listen(pipe, ready);
		else server.listen({ host: "127.0.0.1", port: 0, exclusive: true }, ready);
	}).catch((error: unknown) => {
		delete holder[key];
		throw error;
	});
	return holder[key]!;
}

/** 只核对本机内核所有者；拷贝到另一台机器的 PID 不参与存活判定。 */
export async function ownerAlive(owner?: DraftOwner): Promise<boolean> {
	if (!owner || !/^[\da-f-]{36}$/.test(owner.token)) return false;
	const valid =
		typeof owner.address === "number"
			? Number.isInteger(owner.address) && owner.address > 0 && owner.address <= 65535
			: owner.address === `\\\\.\\pipe\\cst-pilot-telemetry-${owner.token}`;
	if (!valid) return false;
	return new Promise<boolean>((resolve) => {
		const socket =
			typeof owner.address === "string"
				? createConnection(owner.address)
				: createConnection({ host: "127.0.0.1", port: owner.address });
		let body = "";
		const finish = (alive: boolean) => {
			socket.destroy();
			resolve(alive);
		};
		socket.setTimeout(250, () => finish(true));
		socket.on("data", (chunk) => {
			body += chunk.toString();
			if (body.length >= owner.token.length) finish(body === owner.token);
		});
		socket.once("end", () => finish(body === owner.token));
		socket.once("error", (error: NodeJS.ErrnoException) =>
			finish(!["ENOENT", "ECONNREFUSED"].includes(error.code ?? "")),
		);
	});
}
