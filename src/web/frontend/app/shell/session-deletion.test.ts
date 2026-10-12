import assert from "node:assert/strict";
import { test } from "node:test";
import { deleteSessions, deletionReason, deletionTitle } from "./session-deletion.ts";

const targets = [
	{ id: "first", title: "第一个会话" },
	{ id: "second", title: "第二个会话" },
	{ id: "third", title: "第三个会话" },
];

test("批量删除逐项提交，按成功确认汇总", async () => {
	const calls: string[] = [];
	let active = 0;
	let maximum = 0;
	const result = await deleteSessions(targets, async (id) => {
		calls.push(id);
		maximum = Math.max(maximum, ++active);
		await Promise.resolve();
		active--;
	});
	assert.deepEqual(
		calls,
		targets.map((item) => item.id),
	);
	assert.equal(maximum, 1);
	assert.deepEqual(result, { deleted: targets, failed: [] });
	assert.equal(deletionTitle(result), "已删除 3 个会话");
});

test("一项失败保留具体原因，后续会话仍处理且不重试", async () => {
	const calls: string[] = [];
	const result = await deleteSessions(targets, async (id) => {
		calls.push(id);
		if (id === "second") throw new Error("会话正在执行");
	});
	assert.deepEqual(calls, ["first", "second", "third"]);
	assert.deepEqual(result.deleted, [targets[0], targets[2]]);
	assert.deepEqual(result.failed, [{ ...targets[1], error: "会话正在执行" }]);
	assert.equal(deletionTitle(result), "已删除 2 个，1 个失败");
	assert.equal(deletionReason(result.failed[0].error), "正在运行");
});

test("全部失败仍返回完整结果", async () => {
	const result = await deleteSessions(targets, async () => {
		throw new Error("权限不足");
	});
	assert.deepEqual(result.deleted, []);
	assert.equal(result.failed.length, 3);
	assert.ok(result.failed.every((item) => item.error === "权限不足"));
	assert.equal(deletionTitle(result), "删除失败");
});

test("空选择不提交，同一会话只处理一次", async () => {
	const calls: string[] = [];
	const remove = async (id: string) => {
		calls.push(id);
	};
	assert.deepEqual(await deleteSessions([], remove), { deleted: [], failed: [] });
	assert.deepEqual(calls, []);
	const result = await deleteSessions([targets[0], targets[0]], remove);
	assert.deepEqual(calls, ["first"]);
	assert.deepEqual(result.deleted, [targets[0]]);
});

test("DELETE 使用编码路径，未知确认不算成功", async (context) => {
	const calls: { path: string; method?: string }[] = [];
	context.mock.method(globalThis, "fetch", async (path: string, options?: RequestInit) => {
		calls.push({ path, method: options?.method });
		return new Response(JSON.stringify(path.includes("unknown") ? {} : { deleted: true }), { status: 200 });
	});
	const result = await deleteSessions([
		{ id: "id/with space", title: "正常" },
		{ id: "unknown", title: "未知" },
	]);
	assert.deepEqual(calls, [
		{ path: "/api/sessions/id%2Fwith%20space", method: "DELETE" },
		{ path: "/api/sessions/unknown", method: "DELETE" },
	]);
	assert.equal(result.deleted.length, 1);
	assert.equal(result.failed[0].error, "删除结果未确认");
});

test("响应丢失不会自动重发，也不算成功", async (context) => {
	let calls = 0;
	context.mock.method(globalThis, "fetch", async () => {
		calls++;
		throw new TypeError("连接断开");
	});
	const result = await deleteSessions([targets[0]]);
	assert.equal(calls, 1);
	assert.equal(result.deleted.length, 0);
	assert.equal(result.failed.length, 1);
	assert.match(result.failed[0].error, /无法连接/);
});
