import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { after, test } from "node:test";
import { promisify } from "node:util";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { testRoot } from "../../web/test/support.ts";
import { newState, onInput, onToolCall, onToolExecutionEnd, onToolExecutionStart, onTurnEnd } from "../collect.ts";
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

test("四个独立 Pi 进程并发追加，不覆盖记录或重建安装身份", async () => {
	const dir = await home("cross-process");
	const script = join(dir, "writer.mts");
	await writeFile(
		script,
		`
import { appendRecord } from ${JSON.stringify(new URL("../outbox.ts", import.meta.url).href)};
import { loadInstallation } from ${JSON.stringify(new URL("../installation.ts", import.meta.url).href)};
const home=process.argv[2], prefix=process.argv[3];
const installation=await loadInstallation(home);
if (!installation) throw new Error("no installation");
for(let i=0;i<25;i++) await appendRecord(home,{v:"0.1",type:"session",recordId:prefix+"-"+i,identity:{installationId:installation.installationId}});
console.log(installation.installationId);
`,
	);
	const run = promisify(execFile);
	const results = await Promise.all(
		Array.from({ length: 4 }, (_, i) => run(process.execPath, [script, dir, String(i)], { timeout: 20000 })),
	);
	assert.equal(new Set(results.map((result) => result.stdout.trim())).size, 1);
	const records = await readAll(dir);
	assert.equal(records.length, 100);
	assert.equal(new Set(records.map((value) => value.recordId)).size, 100);
});

test("草稿恢复保持 recordId，结束时间取最后完成轮次", async () => {
	const dir = await home("crash");
	const value = record();
	delete value.contextEntries;
	value.endReason = "";
	const lastTurn = Date.now();
	await writeDraft(dir, value.sessionId, { record: value, lastTurnEndedAt: lastTurn });
	const recovered = await recoverDrafts(dir, async () => undefined);
	assert.equal(recovered.length, 1);
	assert.equal(recovered[0].recordId, value.recordId);
	assert.equal(recovered[0].endReason, "crash");
	assert.equal(Date.parse(recovered[0].endedAt), Math.floor(lastTurn / 1000) * 1000);
	assert.equal(recovered[0].contextEntries, undefined);
	assert.deepEqual(await recoverDrafts(dir, async () => undefined), []);
});

test("草稿入队失败仍可重试，恢复保留原系统环境和定稿时间", async () => {
	const dir = await home("recovery-failure");
	const value = record();
	value.os.name = "original-environment";
	await writeDraft(dir, value.sessionId, { record: value, lastTurnEndedAt: Date.now() });
	assert.deepEqual(
		await recoverDrafts(dir, async () => {
			throw new Error("disk unavailable");
		}),
		[],
	);
	const recovered = await recoverDrafts(dir, async () => undefined);
	assert.equal(recovered.length, 1);
	assert.equal(recovered[0].os.name, "original-environment");
	assert.equal(recovered[0].endReason, "quit");
	assert.equal(recovered[0].endedAt, value.endedAt);
	assert.deepEqual(await recoverDrafts(dir, async () => undefined), []);
});

test("缺少或过期 OA 凭据不作为上传凭据，也不修改 auth.json", async () => {
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

test("BitLocker 按 scope 统计调用与降级，不记录恢复密码", () => {
	const state = newState(randomUUID(), "new", "tui");
	const password = Array(8).fill("000000").join("-");
	onToolCall(state, "unlock", "bitlocker", { scope: "unlock", recoveryPassword: password });
	onToolExecutionStart(state, "unlock", "bitlocker");
	onToolExecutionEnd(state, "unlock", { details: { bitlocker: { degraded: true } } }, false);
	onToolCall(state, "status", "bitlocker", {});
	onToolExecutionStart(state, "status", "bitlocker");
	onToolExecutionEnd(state, "status", {}, false);
	assert.equal(state.tools.get("bitlocker(unlock)")?.degraded, 1);
	assert.equal(state.tools.get("bitlocker(status)")?.calls, 1);
	const record = buildRecord(state, { endReason: "quit", endedAt: Date.now(), contextEntries: 2 }, "test", false);
	assert.equal(JSON.stringify(record).includes(password), false);
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
			} else assert.equal(shared().retries.size, 0);
			for (const timer of shared().retries.values()) clearInterval(timer);
		}
	} finally {
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousShared === undefined) delete holder[TELEMETRY_SHARED_KEY];
		else holder[TELEMETRY_SHARED_KEY] = previousShared;
	}
});
