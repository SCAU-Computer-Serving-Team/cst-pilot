import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { SessionExportStore } from "../server/exports/store.ts";
import { testRoot } from "./support.ts";

test("导出队列有界，退出停止渲染并将等待任务标为失败", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-queue-"));
	const store = new SessionExportStore(
		join(root, "artifacts"),
		join(root, "metadata"),
		"http://127.0.0.1:9",
		async (_snapshot, _url, _emit, signal) => {
			signal.throwIfAborted();
			await new Promise<void>((_resolve, reject) =>
				signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true }),
			);
		},
	);
	try {
		const snapshot = exportSnapshot(
			"fixture",
			"排队测试",
			[{ id: "user", message: { role: "user", content: "fixture", timestamp: 1 } }],
			exportOptions({}),
		);
		const jobs = [];
		for (let index = 0; index < 8; index++) jobs.push(await store.create(snapshot));
		await assert.rejects(store.create(snapshot), /任务较多/);
		await store.close();
		for (const job of jobs) assert.equal((await store.get(job.id)).status, "failed");
		await assert.rejects(store.create(snapshot), /退出/);
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});
