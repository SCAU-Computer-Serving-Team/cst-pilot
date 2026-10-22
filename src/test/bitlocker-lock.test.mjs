import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { test } from "node:test";
import { BitLockerVolumeBusy, ownBitLockerVolume } from "../../agent/home/extensions/diagnostics/bitlocker-lock.ts";

async function owner(volume) {
	const module = new URL("../../agent/home/extensions/diagnostics/bitlocker-lock.ts", import.meta.url).href;
	const child = spawn(process.execPath, ["--input-type=module", "-e", `
import {ownBitLockerVolume} from ${JSON.stringify(module)};
const release=await ownBitLockerVolume(${JSON.stringify(volume)});
process.send('ready');
process.on('message',()=>{release();process.exit(0)});
`], { stdio: ["ignore", "ignore", "pipe", "ipc"] });
	await Promise.race([
		once(child, "message"),
		once(child, "exit").then(([code]) => { throw new Error(`卷锁子进程提前退出：${code}`); }),
	]);
	return child;
}

test("独立进程共享卷身份锁，不同卷互不阻塞，进程终止后可再次取得", { timeout: 15_000 }, async () => {
	const id = `\\\\?\\Volume{${randomUUID()}}\\`;
	const child = await owner(id);
	try {
		await assert.rejects(ownBitLockerVolume(id.toLowerCase()), BitLockerVolumeBusy);
		const releaseOther = await ownBitLockerVolume(`\\\\?\\Volume{${randomUUID()}}\\`);
		releaseOther();
		const exit = once(child, "exit");
		child.kill();
		await exit;
		const release = await ownBitLockerVolume(id);
		release();
		release();
		const next = await ownBitLockerVolume(id);
		next();
	} finally {
		if (child.exitCode === null && child.signalCode === null) child.kill();
	}
});

test("同进程重入也拒绝，大小写和尾部斜线统一，身份缺失时拒绝", async () => {
	const id = `volume-${randomUUID()}`;
	const release = await ownBitLockerVolume(`${id}\\`);
	try {
		await assert.rejects(ownBitLockerVolume(id.toUpperCase()), BitLockerVolumeBusy);
		await assert.rejects(ownBitLockerVolume("  "), /身份缺失/);
	} finally {
		release();
	}
});
