import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { MutationReceipts } from "../server/receipts.ts";
import { testRoot } from "./support.ts";

const rootDir = await testRoot();

test("concurrent retries and a process restart return one result", async () => {
	const root = await mkdtemp(join(rootDir, "cst-receipts-"));
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
	const root = await mkdtemp(join(rootDir, "cst-receipts-failure-"));
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
