import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { MutationReceipts } from "./receipts.ts";

const now = new Date();
const date = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
await mkdir(join("E:/tmp", date), { recursive: true });

test("concurrent retries and a process restart return one result", async () => {
	const root = await mkdtemp(join("E:/tmp", date, "cst-receipts-"));
	try {
		const store = new MutationReceipts(root);
		let count = 0;
		const [a, b] = await Promise.all([
			store.once("key", "rename:one", async () => ++count),
			store.once("key", "rename:one", async () => ++count),
		]);
		assert.equal(a, 1);
		assert.equal(b, 1);
		assert.equal(await new MutationReceipts(root).once("key", "rename:one", async () => ++count), 1);
		assert.equal(count, 1);
		await assert.rejects(store.once("key", "rename:two", async () => 2));
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});

test("a failed mutation remains uncertain and is never rerun automatically", async () => {
	const root = await mkdtemp(join("E:/tmp", date, "cst-receipts-failure-"));
	try {
		const store = new MutationReceipts(root);
		await assert.rejects(
			store.once("key", "delete:id", async () => {
				throw new Error("disk failure");
			}),
		);
		await assert.rejects(
			new MutationReceipts(root).once("key", "delete:id", async () => {
				throw new Error("unexpected retry");
			}),
			/状态不确定/,
		);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
