import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { promisify } from "node:util";
import { testRoot } from "../../web/test/support.ts";

const root = await mkdtemp(join(await testRoot(), "cst-telemetry-file-lock-"));
after(() => rm(root, { recursive: true, force: true }));
const run = promisify(execFile);

async function probe(mode: string): Promise<void> {
	const dir = join(root, mode);
	await mkdir(dir);
	const script = join(dir, "probe.mts");
	await writeFile(
		script,
		`
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { syncBuiltinESMExports } from "node:module";
import { withFileLock } from ${JSON.stringify(new URL("../file-lock.ts", import.meta.url).href)};
const target = ${JSON.stringify(join(dir, "pending.jsonl"))};
const mode = ${JSON.stringify(mode)};
const original = fs.mkdir;
let attempts = 0;
let operations = 0;
fs.mkdir = async (...args) => {
  if (args[0] === target + ".lock") {
    attempts++;
    if (mode !== "transient" || attempts <= 2) {
      throw Object.assign(new Error("injected lock directory failure"), { code: mode === "denied" ? "EACCES" : "EPERM" });
    }
  }
  return original(...args);
};
syncBuiltinESMExports();
const operation = async () => { operations++; await fs.writeFile(target, "committed"); return 42; };
if (mode === "transient") {
  assert.equal(await withFileLock(target, operation), 42);
  assert.equal(attempts, 3);
  assert.equal(operations, 1);
  assert.equal(await fs.readFile(target, "utf8"), "committed");
  await assert.rejects(fs.stat(target + ".lock"), { code: "ENOENT" });
} else {
  await assert.rejects(withFileLock(target, operation), { code: mode === "denied" ? "EACCES" : "EPERM" });
  assert.equal(operations, 0);
  assert.equal(attempts, mode === "denied" ? 1 : 101);
}
console.log("lock-probe-ok");
`,
	);
	const result = await run(process.execPath, [script], { timeout: 15000 });
	assert.match(result.stdout, /lock-probe-ok/);
}

test("Windows 锁目录短暂 EPERM 后重试，事务只执行一次且释放锁", { skip: process.platform !== "win32" }, async () => {
	await probe("transient");
});

test("真实 EACCES 权限错误立即返回，不执行事务", async () => {
	await probe("denied");
});

test("Windows 持续 EPERM 达到等待上限后保留原错误", { skip: process.platform !== "win32" }, async () => {
	await probe("permanent");
});
