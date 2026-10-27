import assert from "node:assert/strict";
import { test } from "node:test";
import { recentSessionIds, SessionDetailCache } from "./session-cache.ts";
import type { SessionDetail, SessionRow } from "./web-state.ts";

const detail = (id: string): SessionDetail => ({
	id,
	entries: [],
	messages: [],
	running: false,
	queue: { version: 0, paused: false, items: [] },
	ui: [],
});
const deferred = <T>() => {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
};

test("预载按更新时间取七天内最新十项，排除未来、无效日期和重复 ID", () => {
	const now = Date.UTC(2026, 9, 27, 12);
	const rows: SessionRow[] = Array.from({ length: 14 }, (_, i) => ({
		id: String(i),
		title: String(i),
		running: false,
		updatedAt: new Date(now - i * 3600000).toISOString(),
	})).reverse();
	rows.push(
		{ id: "old", title: "", running: false, updatedAt: new Date(now - 8 * 86400000).toISOString() },
		{ id: "future", title: "", running: false, updatedAt: new Date(now + 1).toISOString() },
		{ id: "invalid", title: "", running: false, updatedAt: "bad" },
		rows[0],
	);
	assert.deepEqual(
		recentSessionIds(rows, now),
		Array.from({ length: 10 }, (_, i) => String(i)),
	);
	assert.deepEqual(
		recentSessionIds(
			[{ id: "edge", title: "", running: false, updatedAt: new Date(now - 7 * 86400000).toISOString() }],
			now,
		),
		["edge"],
	);
});

test("预载最多两个请求并行，切页复用在途请求与内存结果，单项失败不阻断其他项", async () => {
	const requests = new Map<string, ReturnType<typeof deferred<SessionDetail>>>();
	let active = 0,
		peak = 0;
	const cache = new SessionDetailCache(async (id) => {
		active++;
		peak = Math.max(peak, active);
		const task = deferred<SessionDetail>();
		requests.set(id, task);
		try {
			return await task.promise;
		} finally {
			active--;
		}
	});
	const work = cache.preload(["a", "b", "c"]);
	assert.equal(requests.size, 2);
	const foreground = cache.load("a");
	assert.equal(requests.size, 2);
	requests.get("a")!.resolve(detail("a"));
	assert.equal((await foreground).id, "a");
	await Promise.resolve();
	requests.get("b")!.resolve(detail("b"));
	await Promise.resolve();
	await Promise.resolve();
	requests.get("c")!.resolve(detail("c"));
	await work;
	assert.equal(peak, 2);
	assert.equal(cache.peek("a")?.id, "a");
	assert.equal(await cache.load("a"), cache.peek("a"));
	const failing = new SessionDetailCache(async (id) => {
		if (id === "bad") throw new Error("broken");
		return detail(id);
	});
	await failing.preload(["bad", "ok"]);
	assert.equal(failing.peek("ok")?.id, "ok");
});

test("删除淘汰缓存，过期在途结果不得复活；较新的事件更新优先于旧 GET", async () => {
	const task = deferred<SessionDetail>();
	const cache = new SessionDetailCache(() => task.promise);
	const pending = cache.load("a");
	cache.remove("a");
	task.resolve(detail("a"));
	await pending;
	assert.equal(cache.peek("a"), undefined);
	const task2 = deferred<SessionDetail>();
	const latest = new SessionDetailCache(() => task2.promise);
	const old = latest.load("a");
	const fresh = { ...detail("a"), running: true };
	latest.put(fresh);
	task2.resolve(detail("a"));
	assert.equal(await old, fresh);
	assert.equal(latest.peek("a"), fresh);
});

test("事件更新使在途快照过期时，后续强制刷新重新读取完成态", async () => {
	const first = deferred<SessionDetail>();
	let reads = 0;
	const cache = new SessionDetailCache(async () => (++reads === 1 ? first.promise : detail("a")));
	const old = cache.load("a");
	cache.put({ ...detail("a"), running: true });
	const refresh = cache.load("a", true);
	first.resolve(detail("a"));
	await old;
	assert.equal((await refresh).running, false);
	assert.equal(reads, 2);
});

test("缓存有容量上限，按最近读取淘汰，不保存在浏览器持久存储", () => {
	const cache = new SessionDetailCache(async (id) => detail(id), 2);
	cache.put(detail("a"));
	cache.put(detail("b"));
	cache.peek("a");
	cache.put(detail("c"));
	assert.equal(cache.peek("b"), undefined);
	assert.equal(cache.peek("a")?.id, "a");
});
