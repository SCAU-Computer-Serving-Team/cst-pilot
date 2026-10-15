/** 短时本地文件事务锁；兼容 U 盘 FAT/exFAT，不依赖硬链接。 */
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { setTimeout } from "node:timers/promises";

export async function withFileLock<T>(target: string, operation: () => Promise<T>): Promise<T> {
	await mkdir(dirname(target), { recursive: true });
	const lock = `${target}.lock`;
	for (let attempt = 0; ; attempt++) {
		try {
			await mkdir(lock);
			try {
				await writeFile(`${lock}/owner.json`, JSON.stringify({ pid: process.pid }));
			} catch (error) {
				await rm(lock, { recursive: true, force: true });
				throw error;
			}
			break;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
			const info = await stat(lock).catch(() => undefined);
			let abandoned = false;
			try {
				const owner = JSON.parse(await readFile(`${lock}/owner.json`, "utf8"));
				if (Number.isInteger(owner.pid) && owner.pid > 0) {
					try {
						process.kill(owner.pid, 0);
					} catch (error) {
						abandoned = (error as NodeJS.ErrnoException).code === "ESRCH";
					}
				}
			} catch {
				abandoned = !!info && Date.now() - info.mtimeMs > 120_000;
			}
			if (abandoned) {
				// 仅一个进程清理失效锁，取得清理权后重新核对 owner。
				const recovery = `${lock}.recovery`;
				let held = false;
				try {
					await mkdir(recovery);
					held = true;
					const owner = await readFile(`${lock}/owner.json`, "utf8")
						.then((text) => JSON.parse(text))
						.catch(() => undefined);
					let dead = false;
					if (Number.isInteger(owner?.pid) && owner.pid > 0) {
						try {
							process.kill(owner.pid, 0);
						} catch (error) {
							dead = (error as NodeJS.ErrnoException).code === "ESRCH";
						}
					} else {
						const latest = await stat(lock);
						dead = Date.now() - latest.mtimeMs > 120_000;
					}
					if (dead) {
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
