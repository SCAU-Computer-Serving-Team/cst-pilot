import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { InboxConflict, SessionInbox } from "../server/session/inbox.ts";
import { testRoot } from "./support.ts";

const tempRoot = await testRoot();
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

test("idle first message goes directly to the session; only follow-ups are queued", async () => {
	let release!: () => void;
	let started!: () => void;
	const running = new Promise<void>((resolve) => {
		started = resolve;
	});
	const finish = new Promise<void>((resolve) => {
		release = resolve;
	});
	const sent: string[] = [];
	const inbox = new SessionInbox(root, "direct", {
		isBusy: () => false,
		prompt: async (text) => {
			sent.push(text);
			if (text === "first") {
				started();
				await finish;
			}
		},
		steer: async () => {},
	});
	try {
		const first = await inbox.accept("first", "first", "queue");
		assert.equal(first.delivery, "direct");
		await running;
		const followUp = await inbox.accept("second", "second", "queue");
		assert.equal(followUp.delivery, "queue");
		assert.equal((await inbox.snapshot()).items.find((item) => item.id === "second")?.status, "pending");
		assert.deepEqual(sent, ["first"]);
		release();
		await waitFor(async () => (await inbox.snapshot()).items.every((item) => item.status === "delivered"));
		assert.deepEqual(sent, ["first", "second"]);
	} finally {
		release();
		await inbox.close();
	}
});

test("a direct input that loses the idle race becomes a visible follow-up", async () => {
	let busy = false;
	const sent: string[] = [];
	const inbox = new SessionInbox(
		root,
		"race",
		{
			isBusy: () => busy,
			prompt: async (text) => {
				sent.push(text);
			},
			steer: async () => {},
		},
		(snapshot) => {
			if (snapshot.items[0]?.delivery === "direct" && snapshot.items[0].status === "pending") busy = true;
		},
	);
	await inbox.accept("first", "first", "queue");
	await waitFor(async () => (await inbox.snapshot()).items[0]?.delivery === "queue");
	assert.equal((await inbox.snapshot()).items[0].status, "pending");
	assert.deepEqual(sent, []);
	busy = false;
	inbox.wake();
	await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "delivered");
	assert.deepEqual(sent, ["first"]);
	await inbox.close();
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

test("an in-flight input is visible but never resent after a simulated process restart", async () => {
	let release: (() => void) | undefined;
	let calls = 0;
	const inbox = new SessionInbox(root, "uncertain", {
		isBusy: () => false,
		prompt: async () => {
			calls++;
			await new Promise<void>((resolve) => {
				release = resolve;
			});
		},
		steer: async () => {},
	});
	await inbox.accept("request", "diagnose", "queue");
	await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "delivering");
	const restarted = new SessionInbox(root, "uncertain", {
		isBusy: () => false,
		prompt: async () => {
			calls++;
		},
		steer: async () => {},
	});
	const uncertain = await restarted.snapshot();
	assert.equal(uncertain.items[0].status, "delivering");
	for (const edit of [{ retry: true }, { text: "changed" }, { remove: true }])
		await assert.rejects(restarted.change("request", uncertain.version, edit), InboxConflict);
	restarted.wake();
	await new Promise((resolve) => setTimeout(resolve, 30));
	assert.equal(calls, 1);
	release?.();
	await inbox.close();
	await restarted.close();
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
	assert.equal((await restored.snapshot()).items[0].error, "model unavailable");
	assert.deepEqual(calls, []);
	await assert.rejects(restored.accept("a", "changed", "queue"), InboxConflict);
});

test("model failure reports each new idle message without trapping it behind the failed item", async () => {
	const calls: string[] = [];
	const inbox = new SessionInbox(root, "failed-then-send", {
		isBusy: () => false,
		prompt: async (text) => {
			calls.push(text);
			throw new Error("model unavailable");
		},
		steer: async () => {},
	});
	await inbox.accept("first", "one", "queue");
	await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "failed");
	const second = await inbox.accept("second", "two", "queue");
	assert.equal(second.delivery, "direct");
	await waitFor(async () => (await inbox.snapshot()).items[1]?.status === "failed");
	assert.deepEqual(calls, ["one", "two"]);
	await inbox.close();
});

test("failed items can be edited, retried once with attachments and skill, or deleted", async () => {
	let fail = true;
	let busy = false;
	const calls: { text: string; images: unknown; skill?: string }[] = [];
	const images = [{ id: "image", mimeType: "image/png", bytes: 12 }];
	const inbox = new SessionInbox(root, "recover", {
		isBusy: () => busy,
		prompt: async (text, images, skill) => {
			calls.push({ text, images, skill });
			if (fail) throw new Error("service unavailable");
		},
		steer: async () => {
			throw new Error("must not steer");
		},
	});
	try {
		await inbox.accept("a", "original", "queue", images, "sys");
		await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "failed");
		let state = await inbox.snapshot();
		await assert.rejects(inbox.change("a", state.version - 1, { retry: true }), InboxConflict);
		state = await inbox.change("a", state.version, { text: "edited" });
		assert.equal(state.items[0].status, "failed", "editing must not resubmit");
		assert.equal(calls.length, 1);
		fail = false;
		busy = true;
		const version = state.version;
		state = await inbox.change("a", version, { retry: true });
		assert.equal(state.items[0].status, "pending");
		assert.equal(state.items[0].delivery, "queue");
		assert.equal(state.items[0].error, undefined);
		await assert.rejects(inbox.change("a", version, { retry: true }), InboxConflict);
		busy = false;
		inbox.wake();
		await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "delivered");
		assert.deepEqual(calls, [
			{ text: "original", images, skill: "sys" },
			{ text: "edited", images, skill: "sys" },
		]);
		assert.equal((await inbox.accept("a", "original", "queue", images, "sys")).status, "delivered");
		assert.equal(calls.length, 2);
		fail = true;
		await inbox.accept("b", "delete me", "queue");
		await waitFor(async () => (await inbox.snapshot()).items[1]?.status === "failed");
		state = await inbox.snapshot();
		state = await inbox.change("b", state.version, { remove: true });
		assert.equal(state.items[1].status, "cancelled");
		await assert.rejects(inbox.change("a", state.version, { retry: true }), InboxConflict);
	} finally {
		await inbox.close();
	}
});

test("deleting a failed head releases queued work; editing alone never skips it", async () => {
	let busy = true;
	const calls: string[] = [];
	const inbox = new SessionInbox(root, "recover-order", {
		isBusy: () => busy,
		prompt: async (text) => {
			calls.push(text);
			if (text === "first") throw new Error("unavailable");
		},
		steer: async () => {},
	});
	try {
		await inbox.accept("first", "first", "queue");
		await inbox.accept("next", "next", "queue");
		busy = false;
		inbox.wake();
		await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "failed");
		let state = await inbox.snapshot();
		state = await inbox.change("first", state.version, { text: "changed" });
		await new Promise((resolve) => setTimeout(resolve, 30));
		assert.deepEqual(calls, ["first"]);
		await inbox.change("first", state.version, { remove: true });
		await waitFor(async () => (await inbox.snapshot()).items[1]?.status === "delivered");
		assert.deepEqual(calls, ["first", "next"]);
	} finally {
		await inbox.close();
	}
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

test("BitLocker skill is accepted explicitly while unknown skills are rejected", async () => {
	const received: { text: string; skill?: string }[] = [];
	const inbox = new SessionInbox(root, "bitlocker-skill", {
		isBusy: () => false,
		prompt: async (text, _images, skill) => {
			received.push({ text, skill });
		},
		steer: async () => {},
	});
	try {
		await assert.rejects(inbox.accept("unknown", "", "queue", [], "untrusted"), /技能名称无效/);
		await inbox.accept("status", "查询状态", "queue", [], "bitlocker");
		await waitFor(async () => (await inbox.snapshot()).items[0]?.status === "delivered");
		assert.deepEqual(received, [{ text: "查询状态", skill: "bitlocker" }]);
	} finally {
		await inbox.close();
	}
});

test("rejects path traversal in session ID", () => {
	assert.throws(
		() => new SessionInbox(root, "../auth", { isBusy: () => false, prompt: async () => {}, steer: async () => {} }),
	);
});
