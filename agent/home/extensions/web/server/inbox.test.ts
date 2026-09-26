import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { InboxConflict, SessionInbox } from "./inbox.ts";

const today = new Date().toISOString().slice(0, 10);
const tempRoot = join("E:/tmp", today);
await mkdir(tempRoot, { recursive: true });
const root = await mkdtemp(join(tempRoot, "cst-web-inbox-"));
after(() => rm(root, { recursive: true, force: true }));
const waitFor = async (condition: () => Promise<boolean>) => {
	for (let i = 0; i < 100; i++) {
		if (await condition()) return;
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	throw new Error("condition not reached");
};

test("accept persists before delivery, deduplicates simultaneous writers and survives restart without replay", async () => {
	let busy = true;
	const sent: string[] = [];
	const executor = {
		isBusy: () => busy,
		prompt: async (text: string) => {
			sent.push(text);
		},
		steer: async (text: string) => {
			sent.push(text);
		},
	};
	const inbox = new SessionInbox(root, "one", executor);
	const [a, b] = await Promise.all([inbox.accept("msg", "hello", "queue"), inbox.accept("msg", "hello", "queue")]);
	assert.deepEqual(a, b);
	assert.equal((await inbox.snapshot()).items.length, 1);
	await assert.rejects(inbox.accept("msg", "different", "queue"), InboxConflict);
	const resumed = new SessionInbox(root, "one", executor);
	assert.equal((await resumed.snapshot()).items[0].status, "pending");
	busy = false;
	assert.deepEqual(sent, []);
	await resumed.accept("another", "next", "queue");
	await waitFor(async () => (await resumed.snapshot()).items.every((item) => item.status === "delivered"));
	assert.deepEqual(sent, ["hello", "next"]);
});

test("versions, reorder and pause are enforced by the server", async () => {
	let busy = true;
	const sent: string[] = [];
	const executor = {
		isBusy: () => busy,
		prompt: async (text: string) => {
			sent.push(text);
		},
		steer: async (text: string) => {
			sent.push(text);
		},
	};
	const inbox = new SessionInbox(root, "two", executor);
	await inbox.accept("a", "first", "queue");
	await inbox.accept("b", "second", "queue");
	let snapshot = await inbox.snapshot();
	await assert.rejects(inbox.change("a", snapshot.version - 1, { text: "changed" }), InboxConflict);
	snapshot = await inbox.reorder(["b", "a"], snapshot.version);
	assert.deepEqual(
		snapshot.items.map((item) => item.id),
		["b", "a"],
	);
	await inbox.pause();
	busy = false;
	assert.deepEqual(sent, []);
	await inbox.accept("c", "third", "queue");
	await waitFor(async () => (await inbox.snapshot()).items.every((item) => item.status === "delivered"));
	assert.deepEqual(sent, ["second", "first", "third"]);
});

test("editing or cancelling keeps the original idempotency key reserved", async () => {
	const inbox = new SessionInbox(root, "edits", {
		isBusy: () => true,
		prompt: async () => {},
		steer: async () => {},
	});
	await inbox.accept("a", "original", "queue");
	let snapshot = await inbox.snapshot();
	snapshot = await inbox.change("a", snapshot.version, { text: "edited" });
	assert.equal((await inbox.accept("a", "original", "queue")).sequence, 1);
	await assert.rejects(inbox.accept("a", "edited", "queue"), InboxConflict);
	snapshot = await inbox.change("a", snapshot.version, { remove: true });
	assert.equal(snapshot.items[0].status, "cancelled");
	assert.equal((await inbox.accept("a", "original", "queue")).status, "cancelled");
});

test("failed delivery is not replayed after restart", async () => {
	const inbox = new SessionInbox(root, "three", {
		isBusy: () => false,
		prompt: async () => {
			throw new Error("model unavailable");
		},
		steer: async () => {},
	});
	await inbox.accept("a", "attempt", "queue");
	await waitFor(async () => (await inbox.snapshot()).items[0].status === "failed");
	const calls: string[] = [];
	const restored = new SessionInbox(root, "three", {
		isBusy: () => false,
		prompt: async (text) => {
			calls.push(text);
		},
		steer: async () => {},
	});
	assert.equal((await restored.snapshot()).items[0].status, "failed");
	assert.deepEqual(calls, []);
	await assert.rejects(restored.accept("a", "changed", "queue"), InboxConflict);
});

test("failed persistence never calls the model", async () => {
	const invalidRoot = join(root, "file-instead-of-directory");
	await writeFile(invalidRoot, "occupied");
	let calls = 0;
	const inbox = new SessionInbox(invalidRoot, "a", {
		isBusy: () => false,
		prompt: async () => {
			calls++;
		},
		steer: async () => {
			calls++;
		},
	});
	await assert.rejects(inbox.accept("one", "message", "queue"));
	assert.equal(calls, 0);
});

test("rejects path traversal in session ID", () => {
	assert.throws(
		() => new SessionInbox(root, "../auth", { isBusy: () => false, prompt: async () => {}, steer: async () => {} }),
	);
});
