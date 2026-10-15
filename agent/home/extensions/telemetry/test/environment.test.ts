import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { testRoot } from "../../web/test/support.ts";
import { captureEnvironment } from "../environment.ts";
import { loadInstallation } from "../installation.ts";

const root = await mkdtemp(join(await testRoot(), "telemetry-environment-"));
after(() => rm(root, { recursive: true, force: true }));

test("U 盘换机保持安装标识，系统环境独立，遥测不含原始标识", async () => {
	const home = join(root, "portable");
	await mkdir(home);
	const probe = (machineGuid: string) => async () => ({
		machineGuid,
		name: "Windows 11 Pro",
		displayVersion: "24H2",
		arch: "x64",
		isPE: false,
		admin: true,
	});
	const first = await captureEnvironment(home, probe(" MACHINE-A "));
	const again = await captureEnvironment(home, probe("machine-a"));
	const second = await captureEnvironment(home, probe("machine-b"));
	assert.ok(first.identity.installationId);
	assert.equal(first.identity.installationId, second.identity.installationId);
	assert.equal(first.identity.environmentId, again.identity.environmentId);
	assert.notEqual(first.identity.environmentId, second.identity.environmentId);
	const stored = JSON.parse(await readFile(join(home, "telemetry", "identity.json"), "utf8"));
	assert.equal(
		first.identity.environmentId,
		createHmac("sha256", Buffer.from(stored.identityKey, "base64"))
			.update(JSON.stringify(["environment", "machine-a"]))
			.digest("hex"),
	);
	assert.equal(first.os.name, "Windows 11 Pro");
	assert.equal(first.runtime.arch, process.arch);
	assert.equal(first.admin, true);
	assert.equal(JSON.stringify(first).toLowerCase().includes("machine-a"), false);
	assert.equal(JSON.stringify(first).includes(stored.identityKey), false);
});

test("并发首次运行共用持久安装身份，PE 和探测失败保持未知", async () => {
	const home = join(root, "concurrent");
	const installations = await Promise.all(Array.from({ length: 8 }, () => loadInstallation(home)));
	assert.ok(installations[0]);
	assert.equal(new Set(installations.map((value) => value?.installationId)).size, 1);
	const pe = await captureEnvironment(home, async () => ({ machineGuid: "cloned-pe-guid", isPE: true }));
	assert.equal(pe.identity.environmentId, null);
	assert.equal(pe.identity.environmentSource, "unavailable");
	const failed = await captureEnvironment(home, async () => {
		throw new Error("denied");
	});
	assert.equal(failed.os.name, null);
	assert.equal(failed.os.arch, null);
	assert.equal(failed.admin, null);
	assert.equal(failed.identity.installationId, installations[0].installationId);
});

test("损坏或无法持久化的安装身份不重建、不生成临时编号", async () => {
	const broken = join(root, "broken");
	await mkdir(join(broken, "telemetry"), { recursive: true });
	await writeFile(join(broken, "telemetry", "identity.json"), "broken");
	assert.equal(await loadInstallation(broken), undefined);
	assert.equal(await readFile(join(broken, "telemetry", "identity.json"), "utf8"), "broken");
	const blocked = join(root, "blocked");
	await mkdir(blocked);
	await writeFile(join(blocked, "telemetry"), "not a directory");
	const value = await captureEnvironment(blocked, async () => ({ machineGuid: "test-guid", isPE: false }));
	assert.equal(value.identity.installationId, null);
	assert.equal(value.identity.environmentId, null);
});
