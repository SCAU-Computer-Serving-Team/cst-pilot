import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { SessionExportStore } from "../server/exports/store.ts";
import { testRoot } from "./support.ts";

const fixture = () =>
	exportSnapshot(
		"fixture",
		"队列状态",
		[{ id: "u", message: { role: "user", content: "fixture", timestamp: 1 } }],
		exportOptions({ format: "html" }),
	);

test("并行轮询保持任务状态单向推进，返回副本隔离，完成状态对应已落盘文件", { timeout: 15_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-poll-"));
	let release = () => {},
		closed = 0;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const metadata = join(root, "metadata");
	const store = new SessionExportStore(join(root, "artifacts"), metadata, {
		async render(snapshot, emit) {
			await gate;
			assert.equal(snapshot.title, "队列状态");
			await emit("html", Buffer.from("<!DOCTYPE html>完整文件"));
		},
		async close() {
			closed++;
		},
	});
	try {
		const jobs = await Promise.all(Array.from({ length: 8 }, () => store.create(fixture())));
		const copy = await store.get(jobs[0].id);
		copy.title = "修改副本";
		copy.options.format = "png";
		release();
		await Promise.all(
			jobs.map(async (job) => {
				let rendering = false;
				const deadline = Date.now() + 10_000;
				while (Date.now() < deadline) {
					const value = await store.get(job.id);
					assert.notEqual(value.status, "failed", JSON.stringify(value.error));
					if (rendering) assert.notEqual(value.status, "queued");
					rendering ||= value.status === "rendering";
					if (value.status === "completed") {
						assert.equal((await store.file(job.id, 0)).data.toString(), "<!DOCTYPE html>完整文件");
						assert.equal(
							JSON.parse(await readFile(join(metadata, `${job.id}.json`), "utf8")).result.status,
							"completed",
						);
						return;
					}
					await new Promise((resolve) => setTimeout(resolve, 1));
				}
				throw new Error("任务未完成");
			}),
		);
		await store.close();
		await store.close();
		assert.equal(closed, 1);
		const interrupted = jobs[0];
		const record = JSON.parse(await readFile(join(metadata, `${interrupted.id}.json`), "utf8"));
		record.result.status = "rendering";
		record.result.files = [];
		record.stored = [];
		await writeFile(join(metadata, `${interrupted.id}.json`), JSON.stringify(record));
		const restored = new SessionExportStore(join(root, "artifacts"), metadata, {
			async render() {},
			async close() {},
		});
		assert.equal((await restored.get(interrupted.id)).error?.code, "export_interrupted");
		await restored.close();
	} finally {
		release();
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});

test("创建与退出并发时不留下排队登记，渲染器只关闭一次", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-create-close-"));
	const metadata = join(root, "metadata");
	let closed = 0;
	const store = new SessionExportStore(join(root, "artifacts"), metadata, {
		async render() {
			assert.fail("退出后不应渲染");
		},
		async close() {
			closed++;
		},
	});
	try {
		const pending = store.create(fixture());
		const closing = store.close();
		await assert.rejects(pending, { code: "export_stopping" });
		await closing;
		assert.deepEqual(await readdir(metadata), []);
		assert.equal(closed, 1);
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});
