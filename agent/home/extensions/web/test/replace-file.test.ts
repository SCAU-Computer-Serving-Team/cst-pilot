import assert from "node:assert/strict";
import { mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { replaceFile } from "../server/session/replace-file.ts";
import { testRoot } from "./support.ts";

test("短暂文件占用只重试替换，不删除已保存文件", async () => {
	const dir = await mkdtemp(join(await testRoot(), "replace-")),
		source = join(dir, "next"),
		target = join(dir, "current");
	try {
		await writeFile(target, "old");
		await writeFile(source, "new");
		let calls = 0;
		await replaceFile(source, target, async (a, b) => {
			calls++;
			if (calls < 3) {
				assert.equal(await readFile(target, "utf8"), "old");
				throw Object.assign(new Error("busy"), { code: "EPERM" });
			}
			await rename(a, b);
		});
		assert.equal(calls, 3);
		assert.equal(await readFile(target, "utf8"), "new");
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
test("永久权限失败与空间不足仍失败；只读目标保持原数据", async () => {
	const dir = await mkdtemp(join(await testRoot(), "replace-fail-")),
		source = join(dir, "next"),
		target = join(dir, "current");
	try {
		await writeFile(target, "old");
		await writeFile(source, "new");
		let calls = 0;
		const error = Object.assign(new Error("permission"), { code: "EPERM" });
		await assert.rejects(
			replaceFile(source, target, async () => {
				calls++;
				throw error;
			}),
			(e) => e === error,
		);
		assert.equal(calls, 5);
		assert.equal(await readFile(target, "utf8"), "old");
		calls = 0;
		await assert.rejects(
			replaceFile(source, target, async () => {
				calls++;
				throw Object.assign(new Error("full"), { code: "ENOSPC" });
			}),
		);
		assert.equal(calls, 1);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
