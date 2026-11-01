import assert from "node:assert/strict";
import { test } from "node:test";
import {
	MessageOutbox,
	type MessageReceipt,
	OUTBOX_KEY,
	type OutboxTransport,
	parseMessageReceipt,
	parseOutbox,
} from "./message-outbox.ts";

class MemoryStorage {
	value: string | null = null;
	fail = false;
	getItem(_key: string) {
		return this.value;
	}
	setItem(_key: string, value: string) {
		if (this.fail) throw new Error("空间不足");
		this.value = value;
	}
}
function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (cause: Error) => void;
	const promise = new Promise<T>((yes, no) => {
		resolve = yes;
		reject = no;
	});
	return { promise, resolve, reject };
}
class Rejected extends Error {}
const receipt = (id: string): MessageReceipt => ({ id, delivery: "direct", status: "pending" });
const input = (id: string, sessionId: string | undefined = "s1") => ({
	id,
	sessionId,
	text: "相同内容",
	serialized: "/skill:sys 相同内容",
	skill: "sys",
	images: ["hash"],
});
const tick = () => new Promise<void>((resolve) => setImmediate(resolve));
function setup(overrides: Partial<OutboxTransport> = {}, storage = new MemoryStorage()) {
	const sent: string[] = [];
	const outbox = new MessageOutbox(storage, {
		create: async () => "created",
		send: async (item) => {
			sent.push(item.id);
			return receipt(item.id);
		},
		lookup: async () => ({ item: null }),
		isRejection: (error) => error instanceof Rejected,
		...overrides,
	});
	return { storage, outbox, sent };
}

test("损坏回执不误判为已接受，宿主中断标记可被识别", () => {
	assert.throws(() => parseMessageReceipt(null));
	assert.throws(() => parseMessageReceipt({ id: "a", delivery: "direct", status: "bad" }));
	assert.equal(parseMessageReceipt({ ...receipt("a"), uncertain: true }).uncertain, true);
});

test("先持久化完整输入与图片引用，再发请求；同文连续提交使用不同身份", async () => {
	const pending = deferred<MessageReceipt>();
	const { outbox, storage } = setup({ send: () => pending.promise });
	outbox.enqueue(input("a"));
	assert.equal(parseOutbox(storage.value)[0].state, "waiting");
	assert.deepEqual(parseOutbox(storage.value)[0].images, ["hash"]);
	assert.equal(outbox.claimEntrance("a"), true);
	assert.equal(outbox.claimEntrance("a"), false);
	assert.throws(() => outbox.enqueue(input("a")), /已经提交/);
	outbox.enqueue(input("b"));
	await tick();
	assert.deepEqual(
		outbox.getSnapshot().map((item) => item.state),
		["sending", "waiting"],
	);
	pending.resolve(receipt("a"));
	await tick();
	assert.equal(outbox.getSnapshot()[0].state, "accepted");
});

test("本地保存失败不发请求；损坏记录保留原文并阻止继续提交", async () => {
	const { storage, outbox, sent } = setup();
	storage.fail = true;
	assert.throws(() => outbox.enqueue(input("a")), /空间不足/);
	await tick();
	assert.deepEqual(sent, []);
	assert.deepEqual(outbox.getSnapshot(), []);
	storage.fail = false;
	storage.value = "{";
	const recovered = setup({}, storage).outbox;
	assert.ok(recovered.recoveryError);
	assert.throws(() => recovered.enqueue(input("b")));
	assert.equal(storage.value, "{");
	assert.throws(() => parseOutbox('[{"id":"broken"}]'));
});

test("明确拒绝保留内容；前条失败阻止后条越序，其他会话继续发送", async () => {
	const sent: string[] = [];
	const { outbox } = setup({
		send: async (item) => {
			sent.push(item.id);
			if (item.id === "a") throw new Rejected("缺少凭据");
			return receipt(item.id);
		},
	});
	outbox.enqueue(input("a"));
	outbox.enqueue(input("b"));
	outbox.enqueue(input("c", "s2"));
	await tick();
	assert.deepEqual(sent, ["a", "c"]);
	assert.equal(outbox.getSnapshot()[0].state, "failed");
	assert.equal(outbox.getSnapshot()[0].serialized, input("a").serialized);
	outbox.remove("a");
	await tick();
	assert.deepEqual(sent, ["a", "c", "b"]);
});

test("编辑恢复期间禁止重试或移除，失败保留原记录，完成后才能转入新草稿", async () => {
	const { outbox } = setup({
		send: async () => {
			throw new Rejected("发送拒绝");
		},
	});
	outbox.enqueue(input("a"));
	await tick();
	assert.equal(outbox.beginEdit("a").serialized, input("a").serialized);
	await assert.rejects(outbox.retry("a"), /恢复/);
	assert.throws(() => outbox.remove("a"));
	outbox.endEdit("a");
	assert.equal(outbox.getSnapshot()[0].state, "failed");
	outbox.beginEdit("a");
	outbox.completeEdit("a");
	assert.deepEqual(outbox.getSnapshot(), []);
});

test("响应丢失先查询回执；后端已接受时不重发", async () => {
	let writes = 0;
	const { outbox } = setup({
		send: async () => {
			writes++;
			throw new Error("响应丢失");
		},
		lookup: async (item) => ({ item: receipt(item.id) }),
	});
	outbox.enqueue(input("a"));
	await tick();
	assert.equal(outbox.getSnapshot()[0].state, "accepted");
	await outbox.retry("a");
	assert.equal(writes, 1);
});

test("查询失败或未找到回执都保留未知态；重连只查询，手动重试沿用原 ID 与内容", async () => {
	const writes: { id: string; text: string; images: string[] }[] = [];
	let lookups = 0;
	const { outbox } = setup({
		send: async (item) => {
			writes.push({ id: item.id, text: item.text, images: item.images });
			if (writes.length === 1) throw new Error("超时");
			return receipt(item.id);
		},
		lookup: async () => {
			lookups++;
			if (lookups <= 2) throw new Error("离线");
			return { item: null };
		},
	});
	outbox.enqueue(input("a"));
	await tick();
	assert.equal(outbox.getSnapshot()[0].state, "unknown");
	await outbox.checkAll();
	assert.equal(writes.length, 1);
	await Promise.all([outbox.retry("a"), outbox.retry("a"), outbox.retry("a")]);
	assert.equal(writes.length, 2);
	assert.deepEqual(writes[0], writes[1]);
});

test("实时回执先到，迟到的 HTTP 拒绝不能降级或重建气泡", async () => {
	const pending = deferred<MessageReceipt>();
	const { outbox } = setup({ send: () => pending.promise });
	outbox.enqueue(input("a"));
	await tick();
	outbox.observe("s1", [receipt("a")], []);
	assert.equal(outbox.getSnapshot()[0].state, "accepted");
	pending.reject(new Rejected("迟到拒绝"));
	await tick();
	assert.equal(outbox.getSnapshot()[0].state, "accepted");
	outbox.observe("s1", [], ["a"]);
	assert.deepEqual(outbox.getSnapshot(), []);
});

test("按消息身份承接历史和队列，相同文本及旧版本快照不影响其他发送", async () => {
	const { outbox } = setup();
	outbox.enqueue(input("a"));
	outbox.enqueue(input("b"));
	await tick();
	outbox.observe("s1", [], ["a"]);
	assert.deepEqual(
		outbox.getSnapshot().map((item) => item.id),
		["b"],
	);
	outbox.observe("s1", [], []);
	assert.equal(outbox.getSnapshot()[0].state, "accepted");
	outbox.observe("s1", [{ ...receipt("b"), delivery: "queue" }], []);
	assert.deepEqual(outbox.getSnapshot(), []);
});

test("刷新恢复发送中与本地等待项，保持图片引用且不自动重放、不重播动效", async () => {
	const pending = deferred<MessageReceipt>();
	const original = setup({ send: () => pending.promise });
	original.outbox.enqueue(input("a"));
	original.outbox.enqueue(input("b"));
	await tick();
	const recovered = setup({}, original.storage);
	assert.deepEqual(
		recovered.outbox.getSnapshot().map((item) => item.state),
		["unknown", "unknown"],
	);
	assert.equal(recovered.outbox.claimEntrance("a"), false);
	await recovered.outbox.checkAll();
	assert.deepEqual(recovered.sent, []);
	assert.deepEqual(parseOutbox(original.storage.getItem(OUTBOX_KEY))[0].images, ["hash"]);
	pending.resolve(receipt("a"));
	await tick();
});

test("存储保留等待态时恢复仍按结果未知处理，后续拒绝不能覆盖原请求", async () => {
	const storage = new MemoryStorage();
	storage.value = JSON.stringify([
		{ ...input("a"), fromHome: false, createdAt: Date.now(), state: "waiting", attempted: false },
	]);
	const { outbox } = setup(
		{
			send: async () => {
				throw new Rejected("拒绝重试");
			},
		},
		storage,
	);
	await outbox.retry("a");
	assert.equal(outbox.getSnapshot()[0].state, "unknown");
});

test("首页创建响应丢失后查询原创建请求，复用会话，只重试尚未接受的首条消息", async () => {
	const storage = new MemoryStorage();
	let creates = 0;
	let sends = 0;
	const { outbox } = setup(
		{
			create: async () => {
				creates++;
				throw new Error("创建响应丢失");
			},
			lookup: async () => ({ sessionId: "same-session", item: null }),
			send: async (item) => {
				sends++;
				assert.equal(item.sessionId, "same-session");
				return receipt(item.id);
			},
		},
		storage,
	);
	outbox.enqueue({ ...input("home-a"), sessionId: undefined });
	await tick();
	assert.equal(outbox.getSnapshot()[0].sessionId, "same-session");
	await outbox.retry("home-a");
	assert.equal(creates, 1);
	assert.equal(sends, 1);
	assert.equal(parseOutbox(storage.value)[0].sessionId, "same-session");
});

test("首页创建成功后发送失败，重试不再次创建；已接受后本地状态写入失败也不重发", async () => {
	let creates = 0;
	let sends = 0;
	const storage = new MemoryStorage();
	const { outbox } = setup(
		{
			create: async () => {
				creates++;
				return "created";
			},
			send: async (item) => {
				sends++;
				if (sends === 1) throw new Rejected("拒绝发送");
				storage.fail = true;
				return receipt(item.id);
			},
		},
		storage,
	);
	outbox.enqueue({ ...input("home-a"), sessionId: undefined });
	await tick();
	await outbox.retry("home-a");
	assert.equal(outbox.getSnapshot()[0].state, "accepted");
	await outbox.retry("home-a");
	assert.equal(creates, 1);
	assert.equal(sends, 2);
});

test("已有不确定请求后，重试的明确拒绝不能证明原请求失败", async () => {
	let sends = 0;
	const { outbox } = setup({
		send: async () => {
			sends++;
			if (sends === 1) throw new Error("响应丢失");
			throw new Rejected("凭据已经失效");
		},
	});
	outbox.enqueue(input("a"));
	await tick();
	await outbox.retry("a");
	assert.equal(outbox.getSnapshot()[0].state, "unknown");
	assert.equal(outbox.getSnapshot()[0].uncertain, true);
});

test("首页创建会话后与会话页共用发送顺序，不让后续消息越过首条请求", async () => {
	const pending = deferred<MessageReceipt>();
	const sent: string[] = [];
	const { outbox } = setup({
		create: async () => "s1",
		send: async (item) => {
			sent.push(item.id);
			return item.id === "first" ? pending.promise : receipt(item.id);
		},
	});
	outbox.enqueue({ ...input("first"), sessionId: undefined });
	await tick();
	outbox.enqueue(input("next", "s1"));
	await tick();
	assert.deepEqual(sent, ["first"]);
	pending.resolve(receipt("first"));
	await tick();
	assert.deepEqual(sent, ["first", "next"]);
});
