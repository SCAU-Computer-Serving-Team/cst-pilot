import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("停止失败保留实例并允许重试，不提前销毁写入者", async () => {
	const home = await mkdtemp(join(await testRoot(), "stop-failure-"));
	await writeFile(join(home, "settings.json"), "{}");
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (f) => f(),
	});
	const slot = await pool.create();
	const abort = slot.session.abort.bind(slot.session);
	slot.session.abort = async () => {
		throw new Error("测试停止失败");
	};
	try {
		await assert.rejects(pool.close(), /尚未停止/);
		assert.equal(pool.get(slot.id), slot);
		slot.session.abort = abort;
		await pool.close();
		assert.deepEqual(pool.snapshot(), []);
	} finally {
		slot.session.abort = abort;
		await pool.close();
		await rm(home, { recursive: true, force: true });
	}
});

test("退出须可信来源与明确确认；关闭会话后响应，再请求宿主退出", async () => {
	const home = await mkdtemp(join(await testRoot(), "exit-"));
	await writeFile(join(home, "settings.json"), "{}");
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (f) => f(),
	});
	const slot = await pool.create();
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	let stopped = 0;
	const api = createWebApi(pool, home, port, () => stopped++);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ stage: "test", sessions: [] }),
		api,
	);
	await listenWebServer(server, port);
	const post = (confirm: string, source = origin) =>
		fetch(origin + "/api/lifecycle/exit", {
			method: "POST",
			headers: {
				Origin: source,
				"Content-Type": "application/json",
				"X-CST-Web-Request": "1",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify({ confirm }),
		});
	try {
		assert.equal((await post("stop", "https://untrusted.example")).status, 403);
		assert.equal((await post("")).status, 400);
		assert.ok(pool.get(slot.id));
		assert.equal(stopped, 0);
		const state = await (await fetch(origin + "/api/lifecycle")).json();
		assert.ok(state.sessions.some((s: { id: string }) => s.id === slot.id));
		const response = await post("stop");
		assert.equal(response.status, 200);
		assert.deepEqual(await response.json(), { exited: true });
		await assert.rejects(pool.create(), /Web 已退出/);
		await new Promise((r) => setTimeout(r, 180));
		assert.equal(stopped, 1);
		assert.equal((await post("stop")).status, 200);
		await new Promise((r) => setTimeout(r, 150));
		assert.equal(stopped, 1);
		assert.equal((await fetch(origin + "/api/sessions")).status, 409);
	} finally {
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		await rm(home, { recursive: true, force: true });
	}
});
