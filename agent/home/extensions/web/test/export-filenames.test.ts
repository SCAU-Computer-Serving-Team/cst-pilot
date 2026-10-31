import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { SessionExportStore } from "../server/exports/store.ts";
import type { ExportSnapshot, SessionExport } from "../shared/exports.ts";
import { testRoot } from "./support.ts";

const date = "2026-10-31";
const base = "检查磁盘内存-20261031-123456";
function snapshot(format: "png" | "html"): ExportSnapshot {
	const value = exportSnapshot(
		"fixture",
		"检查:磁盘/内存",
		[{ id: "user", message: { role: "user", content: "fixture", timestamp: 1 } }],
		exportOptions({ format }),
	);
	return {
		...value,
		capturedAt: new Date(2026, 9, 31, 12, 34, 56).toISOString(),
		options: { ...value.options, date },
	};
}
async function completed(store: SessionExportStore, id: string): Promise<SessionExport> {
	for (let i = 0; i < 200; i++) {
		const result = await store.get(id);
		if (result.status === "completed" || result.status === "failed") {
			assert.equal(result.status, "completed", JSON.stringify(result.error));
			return result;
		}
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	throw new Error("导出任务未完成");
}
function storeAt(root: string) {
	return new SessionExportStore(join(root, "artifacts"), join(root, "metadata"), {
		async render(value, emit) {
			for (let page = 1; page <= (value.options.format === "png" ? 2 : 1); page++)
				await emit(value.options.format, Buffer.from(`fixture ${page}`));
		},
		async close() {},
	});
}

test("导出文件按标题、快照本机时间和分页命名，同秒重名追加组号且不覆盖", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-names-"));
	const store = storeAt(root);
	try {
		for (const format of ["png", "html"] as const) {
			const first = await completed(store, (await store.create(snapshot(format))).id);
			const second = await completed(store, (await store.create(snapshot(format))).id);
			const expected = (group: string) =>
				format === "png" ? [`${base}${group}-01.png`, `${base}${group}-02.png`] : [`${base}${group}.html`];
			assert.deepEqual(
				first.files.map((file) => file.name),
				expected(""),
			);
			assert.deepEqual(
				second.files.map((file) => file.name),
				expected("-2"),
			);
			assert.notEqual(first.id, second.id);
			assert.equal((await store.file(first.id, 0)).data.toString(), "fixture 1");
			assert.equal((await store.file(second.id, 0)).data.toString(), "fixture 1");
		}
		assert.equal((await readdir(join(root, "artifacts", date, "sessions"))).length, 6);
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});

test("分页组第二页碰撞时回收自己的第一页，整组改号并保留原文件", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-name-collision-"));
	const directory = join(root, "artifacts", date, "sessions");
	await mkdir(directory, { recursive: true });
	const occupied = `${base}-02.png`;
	await writeFile(join(directory, occupied), "existing page");
	const store = storeAt(root);
	try {
		const result = await completed(store, (await store.create(snapshot("png"))).id);
		assert.deepEqual(
			result.files.map((file) => file.name),
			[`${base}-2-01.png`, `${base}-2-02.png`],
		);
		assert.deepEqual((await readdir(directory)).sort(), [occupied, ...result.files.map((file) => file.name)].sort());
		assert.equal(await readFile(join(directory, occupied), "utf8"), "existing page");
		assert.equal((await store.file(result.id, 1)).data.toString(), "fixture 2");
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});

test("不同存储实例同时发布同名分图仍保持各组完整", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-name-concurrent-"));
	const left = storeAt(root),
		right = new SessionExportStore(join(root, "artifacts"), join(root, "other-metadata"), {
			async render(_value, emit) {
				await emit("png", Buffer.from("other 1"));
				await emit("png", Buffer.from("other 2"));
			},
			async close() {},
		});
	try {
		const [a, b] = await Promise.all([left.create(snapshot("png")), right.create(snapshot("png"))]);
		const [first, second] = await Promise.all([completed(left, a.id), completed(right, b.id)]);
		assert.equal(new Set([...first.files, ...second.files].map((file) => file.name)).size, 4);
		for (const index of [0, 1]) {
			assert.equal((await left.file(first.id, index)).data.toString(), `fixture ${index + 1}`);
			assert.equal((await right.file(second.id, index)).data.toString(), `other ${index + 1}`);
		}
	} finally {
		await Promise.all([left.close(), right.close()]);
		await rm(root, { recursive: true, force: true });
	}
});

test("已登记的 UUID 文件名重启后仍可读取", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-legacy-name-"));
	const store = storeAt(root);
	let restored: SessionExportStore | undefined;
	try {
		const result = await completed(store, (await store.create(snapshot("html"))).id);
		await store.close();
		const legacy = `检查磁盘内存-${result.id}.html`;
		await rename(join(result.directory, result.files[0].name), join(result.directory, legacy));
		const metadata = join(root, "metadata", `${result.id}.json`);
		const record = JSON.parse(await readFile(metadata, "utf8"));
		record.result.files[0].name = legacy;
		record.stored[0].name = legacy;
		await writeFile(metadata, JSON.stringify(record));
		restored = storeAt(root);
		assert.equal((await restored.get(result.id)).files[0].name, legacy);
		assert.equal(await restored.directory(result.id), result.directory);
		assert.equal((await restored.file(result.id, 0)).data.toString(), "fixture 1");
	} finally {
		await store.close();
		await restored?.close();
		await rm(root, { recursive: true, force: true });
	}
});

test("HTML 只接收一个文件，重复输出立即失败并回收暂存文件", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-html-count-"));
	const store = new SessionExportStore(join(root, "artifacts"), join(root, "metadata"), {
		async render(_value, emit) {
			await emit("html", Buffer.from("first"));
			await emit("html", Buffer.from("second"));
		},
		async close() {},
	});
	try {
		let result = await store.create(snapshot("html"));
		for (let i = 0; i < 200 && (result.status === "queued" || result.status === "rendering"); i++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
			result = await store.get(result.id);
		}
		assert.equal(result.status, "failed");
		assert.equal(result.error?.code, "export_too_large");
		assert.deepEqual(result.files, []);
		await store.close();
		assert.deepEqual(await readdir(join(root, "metadata", "work")), []);
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});
