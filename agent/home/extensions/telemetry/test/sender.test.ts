import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { after, test } from "node:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { testRoot } from "../../web/test/support.ts";
import { newState, onInput, onTurnEnd } from "../collect.ts";
import { readCredential } from "../credential.ts";
import { recoverDrafts, writeDraft } from "../draft.ts";
import telemetry from "../index.ts";
import { appendRecord, OUTBOX_MAX_RECORDS, readAll, removeRecords } from "../outbox.ts";
import { buildRecord, type SessionRecord } from "../record.ts";
import { shared, TELEMETRY_SHARED_KEY } from "../shared.ts";
import { sendBatch } from "../transport.ts";

const root = await mkdtemp(join(await testRoot(), "cst-telemetry-"));
after(() => rm(root, { recursive: true, force: true }));

function record(): SessionRecord {
	const state = newState(randomUUID(), "new", "tui");
	state.prompts = 1;
	state.turns = 1;
	return buildRecord(state, { endReason: "quit", endedAt: Date.now(), contextEntries: 2 }, "test", false);
}

async function home(name: string): Promise<string> {
	const path = join(root, name);
	await mkdir(path, { recursive: true });
	return path;
}

test("未登录时追加也保持 200 条上限，淘汰最旧并保留并发追加", async () => {
	const dir = await home("bounded");
	const records = Array.from({ length: OUTBOX_MAX_RECORDS + 15 }, record);
	await Promise.all(records.map((value) => appendRecord(dir, value)));
	const pending = await readAll(dir);
	assert.equal(pending.length, OUTBOX_MAX_RECORDS);
	assert.deepEqual(
		pending.map((value) => value.recordId),
		records.slice(-OUTBOX_MAX_RECORDS).map((value) => value.recordId),
	);
	await removeRecords(dir, new Set([pending[0].recordId]));
	assert.equal((await readAll(dir)).length, OUTBOX_MAX_RECORDS - 1);
});

test("草稿恢复保持 recordId，结束时间取最后完成轮次", async () => {
	const dir = await home("crash");
	const value = record();
	delete value.contextEntries;
	const lastTurn = Date.now();
	await writeDraft(dir, value.sessionId, { record: value, lastTurnEndedAt: lastTurn });
	const recovered = await recoverDrafts(dir);
	assert.equal(recovered.length, 1);
	assert.equal(recovered[0].recordId, value.recordId);
	assert.equal(recovered[0].endReason, "crash");
	assert.equal(Date.parse(recovered[0].endedAt), Math.floor(lastTurn / 1000) * 1000);
	assert.equal(recovered[0].contextEntries, undefined);
	assert.deepEqual(await recoverDrafts(dir), []);
});

test("缺少或过期凭据不会上传，也不修改 auth.json", async () => {
	const dir = await home("credentials");
	assert.equal(await readCredential(dir, "cstoa"), undefined);
	const text = JSON.stringify({ cstoa: { access: "test-access", expires: Date.now() - 1 } });
	await writeFile(join(dir, "auth.json"), text);
	assert.equal(await readCredential(dir, "cstoa"), undefined);
	assert.equal(await readFile(join(dir, "auth.json"), "utf8"), text);
});

test("采集统计不包含提问、参数或工具正文，报错原文按契约保留", () => {
	const state = newState(randomUUID(), "new", "web");
	onInput(state, "rpc");
	onTurnEnd(
		state,
		{ role: "assistant", provider: "cstoa", model: "mock", stopReason: "error", errorMessage: "test error" },
		undefined,
		{ cstoa: "CNY" },
	);
	const value = buildRecord(state, { endReason: "quit", endedAt: Date.now(), contextEntries: 2 }, "test", false);
	assert.equal(value.prompts, 1);
	assert.equal(value.turns, 1);
	assert.equal(value.models[0].currency, "CNY");
	assert.deepEqual(value.errors, [{ message: "test error", count: 1 }]);
	assert.equal("mid" in value, false);
	assert.equal("name" in value, false);
});

test("批量上报确认后删除，401/403/5xx 和断网保留记录", async () => {
	const dir = await home("upload");
	const value = record();
	await appendRecord(dir, value);
	let status = 202;
	const server = createServer(async (request, response) => {
		assert.equal(request.headers.authorization, "Bearer test-access");
		let body = "";
		for await (const chunk of request) body += chunk;
		const batch = JSON.parse(body);
		assert.equal(batch.v, "0.1");
		assert.equal(batch.records[0].recordId, value.recordId);
		response.writeHead(status, { "Content-Type": "application/json" });
		response.end("{}");
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const endpoint = `http://127.0.0.1:${address.port}/v1/sessions`;
	try {
		for (const [code, kind] of [
			[401, "auth"],
			[403, "auth"],
			[500, "retry"],
			[404, "stop"],
			[400, "bad"],
		] as const) {
			status = code;
			assert.equal((await sendBatch(endpoint, "test-access", [value])).kind, kind);
			assert.equal((await readAll(dir)).length, 1);
		}
		status = 202;
		assert.equal((await sendBatch(endpoint, "test-access", [value])).kind, "accepted");
		await removeRecords(dir, new Set([value.recordId]));
		assert.deepEqual(await readAll(dir), []);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
	assert.equal((await sendBatch(endpoint, "test-access", [value])).kind, "retry");
});

test("TUI shutdown 返回前完成落盘，关闭遥测不产生记录", async () => {
	const holder = globalThis as Record<symbol, unknown>;
	const previousShared = holder[TELEMETRY_SHARED_KEY];
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	try {
		for (const enabled of [true, false]) {
			delete holder[TELEMETRY_SHARED_KEY];
			if (enabled) shared().adminProbe = Promise.resolve(false);
			const dir = await home(enabled ? "shutdown" : "disabled");
			process.env.PI_CODING_AGENT_DIR = dir;
			await writeFile(join(dir, "telemetry.json"), JSON.stringify({ enabled, endpoint: "http://127.0.0.1:1" }));
			const handlers = new Map<string, (event: unknown, context?: unknown) => Promise<void> | void>();
			telemetry({
				on: (name: string, callback: (event: unknown, context?: unknown) => Promise<void> | void) =>
					handlers.set(name, callback),
			} as unknown as ExtensionAPI);
			const id = randomUUID();
			await handlers.get("session_start")!(
				{ reason: "new" },
				{ mode: "tui", sessionManager: { getSessionId: () => id, buildContextEntries: () => [] } },
			);
			await handlers.get("input")!({ source: "interactive" });
			await handlers.get("session_shutdown")!({ reason: "quit" });
			const pending = await readAll(dir, "http://127.0.0.1:1/");
			assert.equal(pending.length, enabled ? 1 : 0);
			if (enabled) {
				assert.equal(pending[0].sessionId, id);
				assert.equal(pending[0].endReason, "quit");
			} else assert.equal(shared().adminProbe, undefined);
		}
	} finally {
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousShared === undefined) delete holder[TELEMETRY_SHARED_KEY];
		else holder[TELEMETRY_SHARED_KEY] = previousShared;
	}
});
