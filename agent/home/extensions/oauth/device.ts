/**
 * OA 登录扩展 · 设备标识。
 *
 * 维护 agent/home/device.json：首次运行生成 dev_<uuid hex> 并持久化，
 * 之后登录复用同一个标识（设备码申请与令牌载荷的 device_id 都用它）。
 * 它只作标识，不证明请求来自原 U 盘（规格「安全边界」）。
 */

import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

/** 与 OA 侧 login_devices._valid_device_id 相同的允许集。 */
const DEVICE_ID_PATTERN = /^[A-Za-z0-9_.:-]{1,64}$/;
const FILE_NAME = "device.json";

interface DeviceFile {
	device_id?: unknown;
	created_at?: unknown;
}

function newDeviceId(): string {
	return `dev_${randomUUID().replace(/-/g, "")}`;
}

function validDeviceId(value: unknown): value is string {
	return typeof value === "string" && DEVICE_ID_PATTERN.test(value);
}

async function readDeviceId(path: string): Promise<string | undefined> {
	try {
		const parsed = JSON.parse(await readFile(path, "utf8")) as DeviceFile;
		return validDeviceId(parsed.device_id) ? parsed.device_id : undefined;
	} catch {
		return undefined;
	}
}

async function loadOrCreate(agentDir: string): Promise<string> {
	const path = join(agentDir, FILE_NAME);
	const existing = await readDeviceId(path);
	if (existing) return existing;

	const deviceId = newDeviceId();
	await mkdir(dirname(path), { recursive: true });
	const temp = `${path}.${process.pid}.${Date.now()}.tmp`;
	try {
		const payload = `${JSON.stringify({ device_id: deviceId, created_at: Date.now() }, null, "\t")}\n`;
		await writeFile(temp, payload, { flag: "wx" });
		await rename(temp, path);
	} catch (error) {
		await rm(temp, { force: true }).catch(() => undefined);
		// 并发写入时以先落盘的为准。
		const raced = await readDeviceId(path);
		if (raced) return raced;
		throw error;
	}
	return deviceId;
}

/** 同一进程内只解析一次，避免并发登录各写各的标识。 */
let cached: Promise<string> | undefined;

export function getDeviceId(agentDir: string): Promise<string> {
	cached ??= loadOrCreate(agentDir);
	return cached;
}

/** 测试用：清掉进程内缓存。 */
export function resetDeviceIdCache(): void {
	cached = undefined;
}
