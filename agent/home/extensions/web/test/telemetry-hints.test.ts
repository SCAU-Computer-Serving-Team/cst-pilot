import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { processCloseBudget } from "../../telemetry/shared.ts";
import { createWebApi } from "../server/api.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("页面离开与恢复提示只请求遥测检查，不关闭任何会话；禁用时不创建状态", async () => {
	const home = await mkdtemp(join(await testRoot(), "telemetry-hints-"));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const key = Symbol.for("cst-pilot/telemetry");
	const holder = globalThis as Record<symbol, unknown>;
	const previous = holder[key];
	let api: ReturnType<typeof createWebApi>;
	const server = createServer((request, response) => {
		void api(request, response, request.url!);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const origin = `http://127.0.0.1:${address.port}`;
	api = createWebApi(pool, home, address.port, () => undefined);
	const post = (reason: string, trusted = true) =>
		fetch(`${origin}/api/lifecycle/telemetry`, {
			method: "POST",
			headers: {
				Origin: trusted ? origin : "https://invalid.test",
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
			},
			body: JSON.stringify({ reason }),
		});
	try {
		delete holder[key];
		assert.equal((await post("pagehide")).status, 202);
		assert.equal(holder[key], undefined);
		const one = await pool.create();
		const two = await pool.create();
		const hints: string[] = [];
		holder[key] = {
			hints: new Map([
				[
					home,
					(reason: string) => {
						hints.push(reason);
					},
				],
			]),
		};
		for (const reason of ["pagehide", "pageshow", "online"]) assert.equal((await post(reason)).status, 202);
		assert.deepEqual(hints, ["pagehide", "pageshow", "online"]);
		assert.equal((await post("quit")).status, 400);
		assert.equal((await post("online", false)).status, 403);
		assert.equal(pool.get(one.id), one);
		assert.equal(pool.get(two.id), two);
		const close = pool.close.bind(pool);
		pool.close = async () => {
			throw new Error("测试停止失败");
		};
		const exit = () =>
			fetch(`${origin}/api/lifecycle/exit`, {
				method: "POST",
				headers: { Origin: origin, "X-CST-Web-Request": "1", "Content-Type": "application/json" },
				body: JSON.stringify({ confirm: "stop" }),
			});
		try {
			assert.equal((await exit()).status, 500);
			assert.equal(processCloseBudget(home), undefined, "退出失败后清除本次预算，重试重新计时");
			assert.equal(pool.get(one.id), one);
		} finally {
			pool.close = close;
		}
		assert.equal((await exit()).status, 200);
		assert.deepEqual(pool.snapshot(), []);
	} finally {
		if (previous === undefined) delete holder[key];
		else holder[key] = previous;
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await pool.close();
		await api.close();
		await rm(home, { recursive: true, force: true });
	}
});
