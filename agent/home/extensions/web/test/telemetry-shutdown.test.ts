import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("Web 批量关闭共用预算，单个未完成定稿不阻塞其他实例销毁", async () => {
	const home = await mkdtemp(join(await testRoot(), "cst-web-close-budget-"));
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const holder = globalThis as Record<symbol, unknown>;
	const key = Symbol.for("cst-pilot/telemetry");
	const previous = holder[key];
	let release!: () => void;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	try {
		const one = await pool.create();
		const two = await pool.create();
		const controller = new AbortController();
		const received: (AbortSignal | undefined)[] = [];
		holder[key] = {
			closeBudgets: new Map([[home, controller.signal]]),
			finalizers: new Map([
				[
					one.id,
					async (_reason: string, signal?: AbortSignal) => {
						received.push(signal);
						await blocked;
					},
				],
				[
					two.id,
					async (_reason: string, signal?: AbortSignal) => {
						received.push(signal);
					},
				],
			]),
		};
		const timer = setTimeout(() => controller.abort(), 30);
		const start = Date.now();
		await pool.close();
		clearTimeout(timer);
		assert.ok(Date.now() - start < 500);
		assert.deepEqual(received, [controller.signal, controller.signal]);
		assert.deepEqual(pool.snapshot(), []);
	} finally {
		release();
		if (previous === undefined) delete holder[key];
		else holder[key] = previous;
		await pool.close();
		await rm(home, { recursive: true, force: true });
	}
});

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
	const started: string[] = [];
	let release!: () => void;
	const closing = new Promise<void>((resolve) => {
		release = resolve;
	});
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
						started.push(two.id);
						await closing;
						completed.push(two.id);
					},
				],
				[
					three.id,
					async () => {
						started.push(three.id);
						await closing;
						throw new Error("测试定稿失败");
					},
				],
			]),
		};
		await pool.deleteSaved(one.id);
		assert.deepEqual(completed, [one.id]);
		assert.equal(pool.get(one.id), undefined);
		const closeTask = pool.close();
		await new Promise((resolve) => setTimeout(resolve, 30));
		try {
			assert.deepEqual(started.sort(), [two.id, three.id].sort(), "全部实例在等待前进入关闭边界");
		} finally {
			release();
			await closeTask;
		}
		assert.deepEqual(completed, [one.id, two.id]);
		assert.deepEqual(pool.snapshot(), []);
	} finally {
		release();
		if (previous === undefined) delete holder[key];
		else holder[key] = previous;
		await pool.close();
		await rm(home, { recursive: true, force: true });
	}
});
