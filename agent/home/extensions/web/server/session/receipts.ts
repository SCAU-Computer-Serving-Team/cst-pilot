import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { InboxConflict } from "./inbox.ts";

type Receipt<T> = { signature: string; status: "started" | "done"; result?: T };

/** Reserve a mutation before side effects; uncertain work is never replayed on restart. */
export class MutationReceipts {
	private readonly work = new Map<string, Promise<unknown>>();
	private readonly root: string;
	constructor(root: string) {
		this.root = root;
	}

	async once<T>(key: string, signature: string, action: () => Promise<T>): Promise<T> {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(key)) throw new Error("缺少有效的去重键");
		const file = join(this.root, `${createHash("sha256").update(key).digest("hex")}.json`);
		const running = this.work.get(key);
		if (running) {
			const result = await running;
			const saved = await this.read<T>(file);
			if (saved?.signature !== signature) throw new InboxConflict("去重键已用于其他操作");
			return result as T;
		}
		const job = (async () => {
			const saved = await this.read<T>(file);
			if (saved) {
				if (saved.signature !== signature) throw new InboxConflict("去重键已用于其他操作");
				if (saved.status !== "done") throw new InboxConflict("前次操作状态不确定，请核查后重试");
				return saved.result as T;
			}
			await this.save(file, { signature, status: "started" });
			const result = await action();
			await this.save(file, { signature, status: "done", result });
			return result;
		})();
		this.work.set(key, job);
		try {
			return await job;
		} finally {
			this.work.delete(key);
		}
	}

	private async read<T>(file: string): Promise<Receipt<T> | undefined> {
		try {
			const data: unknown = JSON.parse(await readFile(file, "utf8"));
			if (
				!data ||
				typeof data !== "object" ||
				!("status" in data) ||
				!("signature" in data) ||
				(data.status !== "started" && data.status !== "done") ||
				typeof data.signature !== "string"
			)
				throw new Error("操作记录损坏");
			return data as Receipt<T>;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
			throw error;
		}
	}

	private async save<T>(file: string, entry: Receipt<T>): Promise<void> {
		await mkdir(this.root, { recursive: true });
		const temp = `${file}.${randomUUID()}.tmp`;
		try {
			await writeFile(temp, JSON.stringify(entry), { flag: "wx" });
			await rename(temp, file);
		} catch (error) {
			await rm(temp, { force: true }).catch(() => undefined);
			throw error;
		}
	}
}
