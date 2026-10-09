import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("Web 删除与退出等待遥测定稿，失败不阻止会话销毁", async () => {
	const home = await mkdtemp(join(await testRoot(), "cst-web-telemetry-"));
	await mkdir(home, { recursive: true });
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const key = Symbol.for("cst-pilot/telemetry");
	const holder = globalThis as Record<symbol, unknown>;
	const previous = holder[key];
	const completed: string[] = [];
	try {
		const one = await pool.create();
		const two = await pool.create();
		const three = await pool.create();
		holder[key] = {
			finalizers: new Map([
				[
					one.id,
					async (reason: string) => {
						assert.equal(reason, "delete");
						await new Promise((resolve) => setTimeout(resolve, 20));
						completed.push(one.id);
					},
				],
				[
					two.id,
					async (reason: string) => {
						assert.equal(reason, "quit");
						await new Promise((resolve) => setTimeout(resolve, 20));
						completed.push(two.id);
					},
				],
				[
					three.id,
					async () => {
						throw new Error("测试定稿失败");
					},
				],
			]),
		};
		await pool.deleteSaved(one.id);
		assert.deepEqual(completed, [one.id]);
		assert.equal(pool.get(one.id), undefined);
		await pool.close();
		assert.deepEqual(completed, [one.id, two.id]);
		assert.deepEqual(pool.snapshot(), []);
	} finally {
		if (previous === undefined) delete holder[key];
		else holder[key] = previous;
		await pool.close();
		await rm(home, { recursive: true, force: true });
	}
});
