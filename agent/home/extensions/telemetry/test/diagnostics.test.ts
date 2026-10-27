import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { testRoot } from "../../web/test/support.ts";
import { newState } from "../collect.ts";
import { flushEndpoints, queueRecord } from "../delivery.ts";
import { diagnose } from "../diagnostics.ts";
import { unknownEnvironment } from "../environment.ts";
import { HeartbeatRun } from "../heartbeat.ts";
import { loadInstallation } from "../installation.ts";
import { readAll } from "../outbox.ts";
import { SessionSubmission } from "../submission.ts";

test("本地诊断分别记录保存、精确确认、心跳成功和失败类型，不保存凭据或响应正文", async () => {
	const home = await mkdtemp(join(await testRoot(), "telemetry-diagnostics-"));
	const installation = await loadInstallation(home);
	assert.ok(installation);
	const environment = unknownEnvironment();
	environment.identity.installationId = installation.installationId;
	let confirm = false;
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
		response.writeHead(202);
		response.end(
			JSON.stringify({
				v: "0.2",
				secret: "response-secret",
				acks: confirm
					? body.records.map((record: { recordId: string; revision: number }) => ({
							recordId: record.recordId,
							revision: record.revision,
							status: "stored",
						}))
					: [],
				...(body.heartbeat
					? { heartbeatAck: { runId: body.heartbeat.runId, sequence: body.heartbeat.sequence, status: "stored" } }
					: {}),
			}),
		);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const endpoints = [{ url: `http://127.0.0.1:${address.port}/private-endpoint` }];
	const auth = { installation, access: "access-secret" };
	const status = async () => JSON.parse(await readFile(join(home, "telemetry", "status.json"), "utf8"));
	try {
		const state = newState("diagnostic-session", "new", "web");
		state.prompts = 1;
		const writer = new SessionSubmission({
			agentDir: home,
			state,
			version: "test",
			environment: Promise.resolve(environment),
			endpoints,
			flush: async () => {},
		});
		await writer.submit();
		assert.equal((await status()).saved[0].revision, writer.snapshotRevision);
		assert.equal(
			(Object.values((await status()).endpoints)[0] as { queued: { revision: number }[] }).queued[0].revision,
			writer.snapshotRevision,
		);
		const pending = (await readAll(home, endpoints[0].url))[0];
		await queueRecord(home, { ...pending, revision: pending.revision! + 1 }, endpoints);
		await queueRecord(home, pending, endpoints);
		assert.equal(
			(Object.values((await status()).endpoints)[0] as { queued: { revision: number }[] }).queued[0].revision,
			pending.revision! + 1,
			"低版本恢复不回退已入队诊断",
		);
		await flushEndpoints(home, auth, endpoints);
		const missing = Object.values((await status()).endpoints)[0] as { failure: { kind: string }; acks?: unknown[] };
		assert.equal(missing.failure.kind, "protocol");
		assert.equal(missing.acks?.length ?? 0, 0);
		confirm = true;
		await flushEndpoints(home, auth, endpoints);
		const run = new HeartbeatRun({ environment: Promise.resolve(environment), agentDir: home });
		await run.pulse(auth, endpoints);
		const updated = Object.values((await status()).endpoints)[0] as {
			acks: { revision: number }[];
			heartbeat: { sequence: number };
			failure?: unknown;
		};
		assert.equal(updated.acks[0].revision, pending.revision! + 1);
		assert.equal(updated.heartbeat.sequence, 1);
		const serialized = JSON.stringify(await status());
		for (const secret of ["access-secret", "response-secret", "private-endpoint", installation.identityKey])
			assert.equal(serialized.includes(secret), false);
		for (let i = 0; i < 55; i++) await diagnose(home, { saved: { recordId: `bounded-${i}`, revision: 1 } });
		for (let i = 0; i < 10; i++) await diagnose(home, { endpoint: `https://endpoint-${i}.test`, failure: "network" });
		assert.equal((await status()).saved.length, 50);
		assert.equal(Object.keys((await status()).endpoints).length, 8);
		assert.ok((await readFile(join(home, "telemetry", "status.json"))).byteLength < 64 * 1024);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
