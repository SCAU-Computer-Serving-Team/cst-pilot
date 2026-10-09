import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { testRoot } from "../../web/test/support.ts";
import { newState } from "../collect.ts";
import { readCredential } from "../credential.ts";
import { appendRecord, readAll, removeRecords } from "../outbox.ts";
import { buildRecord } from "../record.ts";
import { sendBatch } from "../transport.ts";

const binary = process.env.CST_TELEMETRY_RECEIVER;

test("发送端到 Go 接收端：真实内省协议、入库、去重与队列确认", { skip: !binary }, async () => {
	assert.ok(binary);
	const root = await mkdtemp(join(await testRoot(), "cst-telemetry-receiver-"));
	let introspections = 0;
	const oa = createServer(async (request, response) => {
		assert.equal(request.url, "/api/oauth/introspect");
		assert.equal(request.headers.authorization, "Bearer test-service-token");
		let body = "";
		for await (const chunk of request) body += chunk;
		introspections++;
		const token = JSON.parse(body).token;
		response.setHeader("Content-Type", "application/json");
		response.end(
			JSON.stringify(
				token === "test-agent-token"
					? { active: true, mid: "20230001", device_id: "test-device" }
					: { active: false, reason: "invalid" },
			),
		);
	});
	await new Promise<void>((resolve) => oa.listen(0, "127.0.0.1", resolve));
	const oaAddress = oa.address();
	assert.ok(oaAddress && typeof oaAddress !== "string");
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const address = probe.address();
	assert.ok(address && typeof address !== "string");
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	const origin = `http://127.0.0.1:${address.port}`;
	const child = spawn(binary, ["serve"], {
		cwd: root,
		env: {
			...process.env,
			TELEMETRY_HOST: "127.0.0.1",
			TELEMETRY_PORT: String(address.port),
			TELEMETRY_DB: join(root, "receiver.db"),
			OA_INTROSPECT_URL: `http://127.0.0.1:${oaAddress.port}/api/oauth/introspect`,
			OA_SERVICE_TOKEN: "test-service-token",
		},
		stdio: ["ignore", "pipe", "pipe"],
	});
	let logs = "";
	child.stdout.on("data", (data) => {
		logs += String(data);
	});
	child.stderr.on("data", (data) => {
		logs += String(data);
	});
	const exited = once(child, "exit");
	try {
		let ready = false;
		for (let i = 0; i < 100 && child.exitCode === null; i++) {
			ready = await fetch(`${origin}/healthz`)
				.then((response) => response.ok)
				.catch(() => false);
			if (ready) break;
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.ok(ready, logs);
		const state = newState(randomUUID(), "new", "web");
		state.prompts = 1;
		state.turns = 1;
		const value = buildRecord(state, { endReason: "quit", endedAt: Date.now(), contextEntries: 2 }, "test", false);
		await appendRecord(root, value);
		assert.equal((await sendBatch(`${origin}/v1/sessions`, "invalid-test-token", [value])).kind, "auth");
		assert.equal((await readAll(root)).length, 1);
		await writeFile(
			join(root, "auth.json"),
			JSON.stringify({ cstoa: { access: "test-agent-token", expires: Date.now() + 60_000 } }),
		);
		const credential = await readCredential(root, "cstoa");
		assert.ok(credential);
		const pending = await readAll(root);
		assert.equal((await sendBatch(`${origin}/v1/sessions`, credential.access, pending)).kind, "accepted");
		await removeRecords(root, new Set(pending.map((entry) => entry.recordId)));
		assert.deepEqual(await readAll(root), []);
		assert.equal((await sendBatch(`${origin}/v1/sessions`, credential.access, [value])).kind, "accepted");
		const health = await (await fetch(`${origin}/healthz`)).json();
		assert.equal(health.sessions, 1);
		assert.equal(introspections, 2, "内省结果缓存 60 秒，重发不重复内省");
	} finally {
		child.kill();
		await exited;
		oa.closeAllConnections();
		await new Promise<void>((resolve) => oa.close(() => resolve()));
		await rm(root, { recursive: true, force: true });
	}
});
