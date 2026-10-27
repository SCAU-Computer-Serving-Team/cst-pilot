import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { testRoot } from "../../web/test/support.ts";
import { newState } from "../collect.ts";
import { flushEndpoints, queueRecord } from "../delivery.ts";
import { unknownEnvironment } from "../environment.ts";
import { type Heartbeat, HeartbeatRun } from "../heartbeat.ts";
import { loadInstallation } from "../installation.ts";
import { readAll } from "../outbox.ts";
import { buildRecord } from "../record.ts";
import { sendBatch } from "../transport.ts";

async function waitFor(condition: () => boolean): Promise<void> {
	const until = Date.now() + 3000;
	while (!condition() && Date.now() < until) await new Promise((resolve) => setTimeout(resolve, 5));
	assert.ok(condition(), "等待本地请求");
}

test("统计积压不阻塞心跳，慢端点只补发最新心跳且不影响另一端", async () => {
	const dir = await mkdtemp(join(await testRoot(), "telemetry-heartbeat-backlog-"));
	const installation = await loadInstallation(dir);
	assert.ok(installation);
	const environment = unknownEnvironment();
	environment.identity.installationId = installation.installationId;
	let now = Date.now();
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const seen: Record<string, number[]> = { "/slow": [], "/fast": [] };
	let statsStarted = false;
	const server = createServer(async (request, response) => {
		if (request.method === "GET") {
			response.end(
				JSON.stringify({ protocols: ["0.2"], capabilities: ["session-snapshots", "revision-acks", "heartbeat"] }),
			);
			return;
		}
		let raw = "";
		for await (const chunk of request) raw += chunk;
		const body = JSON.parse(raw);
		if (body.heartbeat) {
			seen[request.url!].push(body.heartbeat.sequence);
			if (request.url === "/slow" && body.heartbeat.sequence === 1) await gate;
		} else {
			statsStarted = true;
			await gate;
		}
		response.writeHead(202);
		response.end(
			JSON.stringify({
				v: "0.2",
				acks: [],
				...(body.heartbeat
					? { heartbeatAck: { runId: body.heartbeat.runId, sequence: body.heartbeat.sequence, status: "stored" } }
					: {}),
			}),
		);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const endpoints = ["/slow", "/fast"].map((path) => ({ url: `http://127.0.0.1:${address.port}${path}` }));
	let flushing: Promise<void> | undefined;
	const pulses: Promise<void>[] = [];
	try {
		const state = newState("backlog", "new", "web");
		state.prompts = 1;
		await queueRecord(
			dir,
			buildRecord(state, { endReason: "quit", endedAt: now, contextEntries: undefined }, "test", null),
			endpoints,
		);
		flushing = flushEndpoints(dir, installation, endpoints);
		await waitFor(() => statsStarted);
		const run = new HeartbeatRun({ environment: Promise.resolve(environment), now: () => now });
		pulses.push(run.pulse(installation, endpoints));
		await waitFor(() => seen["/slow"].length === 1 && seen["/fast"].length === 1);
		now += 30_000;
		pulses.push(run.pulse(installation, endpoints));
		await waitFor(() => seen["/fast"].length === 2);
		now += 30_000;
		pulses.push(run.pulse(installation, endpoints));
		await waitFor(() => seen["/fast"].length === 3);
		assert.deepEqual(seen["/slow"], [1]);
		release();
		await Promise.all([...pulses, flushing]);
		assert.deepEqual(seen["/slow"], [1, 3]);
		assert.deepEqual(seen["/fast"], [1, 2, 3]);
		for (const endpoint of endpoints) assert.deepEqual(await readAll(dir, endpoint.url), []);
		assert.notEqual(new HeartbeatRun({ environment: Promise.resolve(environment) }).runId, run.runId);
	} finally {
		release();
		await Promise.allSettled([...pulses, ...(flushing ? [flushing] : [])]);
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(dir, { recursive: true, force: true });
	}
});

test("心跳确认与统计确认独立匹配，能力缺失时不发送心跳", async () => {
	const dir = await mkdtemp(join(await testRoot(), "telemetry-heartbeat-acks-"));
	const installation = await loadInstallation(dir);
	assert.ok(installation);
	let capable = true;
	let sequence = 2;
	let posts = 0;
	const state = newState("acks", "new", "web");
	const record = {
		...buildRecord(state, { endReason: "", endedAt: Date.now(), contextEntries: undefined }, "test", null),
		v: "0.2",
		revision: 1,
		final: false,
	};
	const heartbeat: Heartbeat = {
		runId: "ack-run",
		sequence: 1,
		sentAt: new Date().toISOString(),
		status: "idle",
		identity: { ...unknownEnvironment().identity, installationId: installation.installationId },
		lastActivityAt: null,
		snapshots: [],
	};
	const server = createServer(async (request, response) => {
		if (request.method === "GET") {
			response.end(
				JSON.stringify({
					protocols: ["0.2"],
					capabilities: ["session-snapshots", "revision-acks", ...(capable ? ["heartbeat"] : [])],
				}),
			);
			return;
		}
		posts++;
		for await (const _chunk of request) {
			/* consume */
		}
		response.writeHead(202);
		response.end(
			JSON.stringify({
				v: "0.2",
				acks: [{ recordId: record.recordId, revision: 1, status: "stored" }],
				heartbeatAck: { runId: heartbeat.runId, sequence, status: "stored" },
			}),
		);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const url = `http://127.0.0.1:${address.port}/api/telemetry`;
	try {
		const mismatch = await sendBatch(url, installation, [record], undefined, heartbeat);
		assert.equal(mismatch.kind, "accepted");
		if (mismatch.kind === "accepted") {
			assert.deepEqual(mismatch.acknowledged, [record]);
			assert.equal(mismatch.heartbeatAcknowledged, false);
		}
		sequence = 1;
		const matched = await sendBatch(url, installation, [], undefined, heartbeat);
		assert.equal(matched.kind, "accepted");
		if (matched.kind === "accepted") assert.equal(matched.heartbeatAcknowledged, true);
		capable = false;
		assert.equal((await sendBatch(url, installation, [], undefined, heartbeat)).kind, "retry");
		assert.equal(posts, 2);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(dir, { recursive: true, force: true });
	}
});

test("空闲心跳复用运行 ID，按当前实例状态发送且不改变统计版本", async () => {
	const dir = await mkdtemp(join(await testRoot(), "telemetry-heartbeat-"));
	const installation = await loadInstallation(dir);
	assert.ok(installation);
	const environment = unknownEnvironment();
	environment.identity.installationId = installation.installationId;
	let now = Date.now();
	const received: Heartbeat[] = [];
	const server = createServer(async (request, response) => {
		if (request.method === "GET") {
			response.end(
				JSON.stringify({ protocols: ["0.2"], capabilities: ["session-snapshots", "revision-acks", "heartbeat"] }),
			);
			return;
		}
		let raw = "";
		for await (const chunk of request) raw += chunk;
		const envelope = JSON.parse(raw);
		assert.deepEqual(envelope.records, []);
		assert.ok(request.headers["x-telemetry-signature"]);
		received.push(envelope.heartbeat);
		response.writeHead(202);
		response.end(
			JSON.stringify({
				v: "0.2",
				acks: [],
				heartbeatAck: { runId: envelope.heartbeat.runId, sequence: envelope.heartbeat.sequence, status: "stored" },
			}),
		);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const endpoints = [{ url: `http://127.0.0.1:${address.port}/api/telemetry` }];
	try {
		const run = new HeartbeatRun({ environment: Promise.resolve(environment), now: () => now });
		let revision = 7;
		run.register("instance", () => revision);
		await run.pulse(installation, endpoints);
		await run.pulse(installation, endpoints);
		assert.equal(received.length, 1);
		assert.equal(received[0].status, "idle");
		assert.equal(received[0].lastActivityAt, null);
		run.activity("instance", true);
		now += 30_000;
		await run.pulse(installation, endpoints);
		assert.equal(received[1].status, "busy");
		assert.deepEqual(received[1].snapshots, [{ recordId: "instance", revision: 7 }]);
		assert.equal(revision, 7);
		revision = 8;
		run.activity("instance", false);
		now += 180_000;
		await run.pulse(installation, endpoints);
		assert.equal(received.length, 3, "唤醒后仅发送当前心跳");
		assert.equal(received[2].status, "idle");
		assert.equal(received[2].sentAt, new Date(now).toISOString());
		assert.equal(new Set(received.map((value) => value.runId)).size, 1);
		assert.deepEqual(
			received.map((value) => value.sequence),
			[1, 2, 3],
		);
		await run.pulse(installation, endpoints, false, true);
		assert.equal(received.length, 4, "页面恢复提示立即发送当前心跳");
		assert.deepEqual(received[3].snapshots, [{ recordId: "instance", revision: 8 }]);
		run.remove("instance");
		await run.pulse(installation, endpoints, true);
		assert.equal(received[4].status, "closed");
		assert.deepEqual(received[4].snapshots, []);
		now += 30_000;
		await run.pulse(installation, endpoints);
		assert.equal(received.length, 5, "同一运行关闭后不发送在线心跳");
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(dir, { recursive: true, force: true });
	}
});
