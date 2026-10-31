import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import type { SessionExport } from "../shared/exports.ts";
import { freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("打开导出目录：仅已完成登记、同源去重、拒绝路径注入和目录越界，启动失败可重试", { timeout: 20_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-open-"));
	const home = join(root, "home");
	await mkdir(home);
	await configureTestModel(home);
	const pool = new WebSessionPool({
		cwd: root,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	slot.session.sessionManager.appendMessage({ role: "user", content: "打开保存目录", timestamp: Date.now() });
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	let release = () => {};
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const opened: string[] = [];
	let failLaunch = false;
	const options = {
		artifactsRoot: join(root, "artifacts"),
		exportRenderer: {
			async render(_snapshot: unknown, emit: (extension: "html" | "png", bytes: Buffer) => Promise<void>) {
				await gate;
				await emit("html", Buffer.from("<!DOCTYPE html>fixture"));
			},
			async close() {},
		},
		async openExportDirectory(directory: string) {
			if (failLaunch) throw new Error("fixture launch failure");
			opened.push(directory);
		},
	};
	let api = createWebApi(pool, home, port, undefined, options);
	const server = createWebServer(home, port, undefined, (...args) => api(...args));
	await listenWebServer(server, port);
	const post = (path: string, value: unknown = {}, key = crypto.randomUUID(), requestOrigin = origin) =>
		fetch(`${origin}${path}`, {
			method: "POST",
			headers: {
				Origin: requestOrigin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": key,
			},
			body: JSON.stringify(value),
		});
	try {
		const response = await post(`/api/sessions/${slot.id}/exports`, { format: "html", theme: "light" });
		assert.equal(response.status, 202);
		let result = (await response.json()) as SessionExport;
		const path = `/api/exports/${result.id}/open`;
		assert.equal((await post(path)).status, 409, "未完成任务不可打开目录");
		release();
		for (let i = 0; i < 200; i++) {
			result = (await (await fetch(`${origin}/api/exports/${result.id}`)).json()) as SessionExport;
			if (result.status === "completed") break;
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.equal(result.status, "completed");
		assert.equal((await post(path, { directory: "C:/Windows" })).status, 400);
		assert.equal((await post(path, { path: "../outside" })).status, 400);
		assert.equal((await post(path, [], crypto.randomUUID())).status, 400);
		assert.equal((await post(path, {}, crypto.randomUUID(), "https://untrusted.example")).status, 403);
		assert.equal((await fetch(`${origin}${path}`)).status, 404, "GET 无本机启动副作用");
		assert.equal((await post(`/api/exports/${crypto.randomUUID()}/open`)).status, 404);
		assert.equal(opened.length, 0);
		const key = crypto.randomUUID();
		const responses = await Promise.all([post(path, {}, key), post(path, {}, key)]);
		assert.deepEqual(
			responses.map((value) => value.status),
			[200, 200],
		);
		assert.deepEqual(await responses[0].json(), { launched: true });
		assert.deepEqual(opened, [resolve(result.directory)]);
		failLaunch = true;
		const failed = await post(path);
		assert.equal(failed.status, 503);
		assert.equal((await failed.json()).error.code, "export_launch_failed");
		failLaunch = false;
		assert.equal((await post(path)).status, 200);
		assert.equal(opened.length, 2);

		await api.close();
		const recordFile = join(home, "session-exports", `${result.id}.json`);
		const record = JSON.parse(await readFile(recordFile, "utf8"));
		record.result.directory = "C:/Windows";
		await writeFile(recordFile, JSON.stringify(record));
		api = createWebApi(pool, home, port, undefined, options);
		assert.equal((await post(path)).status, 200);
		assert.equal(opened.at(-1), resolve(result.directory), "目录由日期和受控根目录推导，不信任登记中的显示路径");
		assert.equal((await fetch(`${origin}${result.files[0].url}`)).status, 200);
		await rm(result.directory, { recursive: true });
		assert.equal((await post(path)).status, 404);
		const outside = join(root, "outside");
		await mkdir(outside);
		await symlink(outside, result.directory, "junction");
		assert.equal((await post(path)).status, 403);
		assert.equal(opened.length, 3, "缺失和越界目录均不启动文件管理器");
	} finally {
		release();
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(root, { recursive: true, force: true });
	}
});
