/** 短时本地文件事务锁；兼容 U 盘 FAT/exFAT，不依赖硬链接。 */
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { setTimeout } from "node:timers/promises";
import { draftOwner, ownerAlive } from "./draft-owner.ts";

async function abandonedLock(lock: string): Promise<boolean> {
	const info = await stat(lock).catch(() => undefined);
	if (!info) return false;
	try {
		const owner = JSON.parse(await readFile(`${lock}/owner.json`, "utf8"));
		if (owner.proof) return !(await ownerAlive(owner.proof));
		if (Number.isInteger(owner.pid) && owner.pid > 0) {
			try {
				process.kill(owner.pid, 0);
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code === "ESRCH") return true;
			}
		}
	} catch {
		/* owner 尚未写完时，等待短事务完成。 */
	}
	return Date.now() - info.mtimeMs > 120_000;
}

export async function withFileLock<T>(target: string, operation: () => Promise<T>): Promise<T> {
	await mkdir(dirname(target), { recursive: true });
	const lock = `${target}.lock`;
	for (let attempt = 0; ; attempt++) {
		try {
			await mkdir(lock);
			try {
				await writeFile(`${lock}/owner.json`, JSON.stringify({ pid: process.pid, proof: await draftOwner() }));
			} catch (error) {
				await rm(lock, { recursive: true, force: true });
				throw error;
			}
			break;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
			if (await abandonedLock(lock)) {
				// 仅一个进程清理失效锁，取得清理权后重新核对 owner。
				const recovery = `${lock}.recovery`;
				let held = false;
				try {
					await mkdir(recovery);
					held = true;
					if (await abandonedLock(lock)) {
						const stale = `${lock}.stale-${randomUUID()}`;
						await rename(lock, stale);
						await rm(stale, { recursive: true, force: true });
					}
				} catch {
					/* 其他进程已处理或 owner 尚未写完，后续重试。 */
				} finally {
					if (held) await rm(recovery, { recursive: true, force: true });
				}
			}
			if (attempt >= 100) throw new Error("遥测文件锁等待超时");
			await setTimeout(20 + Math.random() * 30);
		}
	}
	try {
		return await operation();
	} finally {
		await rm(lock, { recursive: true, force: true });
	}
}
