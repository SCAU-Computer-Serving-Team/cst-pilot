import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { after, test } from "node:test";
import { promisify } from "node:util";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { requestFetch } from "../../oauth/request-observer.ts";
import { testRoot } from "../../web/test/support.ts";
import { newState, onInput, onToolCall, onToolExecutionEnd, onToolExecutionStart, onTurnEnd } from "../collect.ts";
import { readCredential } from "../credential.ts";
import { recoverDrafts, writeDraft } from "../draft.ts";
import type { Heartbeat } from "../heartbeat.ts";
import telemetry from "../index.ts";
import { appendRecord, OUTBOX_MAX_RECORDS, readAll, removeRecords } from "../outbox.ts";
import { buildRecord, type SessionRecord } from "../record.ts";
import { shared, submitActive, TELEMETRY_SHARED_KEY } from "../shared.ts";
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
	await removeRecords(dir, [pending[0]]);
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
		await removeRecords(dir, [value]);
		assert.deepEqual(await readAll(dir), []);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
	assert.equal((await sendBatch(endpoint, "test-access", [value])).kind, "retry");
});

test("回答结束上传累计快照，继续输入保留实例且不等待远端确认", async () => {
	const holder = globalThis as Record<symbol, unknown>;
	const previousShared = holder[TELEMETRY_SHARED_KEY];
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	const previousFetch = globalThis.fetch;
	const dir = await home("settled-snapshots");
	const uploaded: SessionRecord[] = [];
	const heartbeats: Heartbeat[] = [];
	let release: (() => void) | undefined;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	try {
		delete holder[TELEMETRY_SHARED_KEY];
		process.env.PI_CODING_AGENT_DIR = dir;
		await writeFile(
			join(dir, "telemetry.json"),
			JSON.stringify({ enabled: true, endpoint: "http://127.0.0.1:19001" }),
		);
		globalThis.fetch = async (_input, init) => {
			if (init?.method === "GET")
				return Response.json({
					protocols: ["0.2"],
					capabilities: ["session-snapshots", "revision-acks", "heartbeat"],
				});
			const batch = JSON.parse(String(init?.body));
			uploaded.push(...batch.records);
			if (batch.heartbeat) heartbeats.push(batch.heartbeat);
			if (uploaded.length === 1) await blocked;
			return Response.json(
				{
					v: "0.2",
					acks: batch.records.map((item: SessionRecord) => ({
						recordId: item.recordId,
						revision: item.revision,
						status: "stored",
					})),
				},
				{ status: 202 },
			);
		};
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
		assert.ok(handlers.has("agent_settled"), "直接订阅回答结束事件");
		const answer = async (stopReason: string) => {
			await handlers.get("input")!({ source: "interactive" });
			await handlers.get("turn_end")!({
				message: { role: "assistant", provider: "mock", model: "test", stopReason, usage: { totalTokens: 10 } },
			});
			await handlers.get("agent_settled")!({ type: "agent_settled" });
		};
		const waitFor = async (condition: () => boolean) => {
			const until = Date.now() + 15_000;
			while (!condition() && Date.now() < until) await new Promise((resolve) => setTimeout(resolve, 20));
			assert.ok(condition(), "等待后台快照上传");
		};
		await answer("stop");
		await waitFor(() => uploaded.length === 1);
		await waitFor(() => heartbeats.length > 0);
		assert.equal(shared().runs.size, 1);
		assert.equal(uploaded[0].final, false);
		assert.equal(uploaded[0].endReason, "");
		assert.equal(uploaded[0].turns, 1);
		assert.ok(shared().finalizers.has(id), "回答结束保留会话实例");
		await answer("aborted");
		release!();
		await waitFor(() => uploaded.some((item) => item.turns === 2));
		const second = uploaded.find((item) => item.turns === 2)!;
		assert.equal(second.recordId, uploaded[0].recordId);
		assert.ok(second.revision! > uploaded[0].revision!);
		assert.equal(second.prompts, 2);
		assert.equal(second.aborted, 1);
		assert.equal(uploaded[0].models[0].totalTokens, 10, "后续统计不改变已提交版本");
		await handlers.get("session_shutdown")!({ reason: "quit" });
		await waitFor(() => uploaded.some((item) => item.final));
		await waitFor(() => heartbeats.some((item) => item.status === "closed"));
		const final = uploaded.find((item) => item.final)!;
		assert.equal(final.recordId, second.recordId);
		assert.ok(final.revision! > second.revision!);
	} finally {
		release?.();
		for (const timer of shared().retries.values()) clearInterval(timer);
		globalThis.fetch = previousFetch;
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousShared === undefined) delete holder[TELEMETRY_SHARED_KEY];
		else holder[TELEMETRY_SHARED_KEY] = previousShared;
	}
});

test("定时提交检查同一 home 的全部实例，空闲重复检查不增加版本", async () => {
	const holder = globalThis as Record<symbol, unknown>;
	const previousShared = holder[TELEMETRY_SHARED_KEY];
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	const dir = await home("all-active");
	const instances: Map<string, (event: unknown, context?: unknown) => Promise<void> | void>[] = [];
	try {
		delete holder[TELEMETRY_SHARED_KEY];
		process.env.PI_CODING_AGENT_DIR = dir;
		await writeFile(join(dir, "telemetry.json"), JSON.stringify({ enabled: true, endpoint: "http://127.0.0.1:1" }));
		for (let i = 0; i < 3; i++) {
			const handlers = new Map<string, (event: unknown, context?: unknown) => Promise<void> | void>();
			telemetry({
				on: (name: string, callback: (event: unknown, context?: unknown) => Promise<void> | void) =>
					handlers.set(name, callback),
			} as unknown as ExtensionAPI);
			await handlers.get("session_start")!(
				{ reason: "new" },
				{ mode: "web", sessionManager: { getSessionId: () => `periodic-${i}`, buildContextEntries: () => [] } },
			);
			if (i < 2) await handlers.get("input")!({ source: "rpc" });
			instances.push(handlers);
		}
		assert.equal(shared().retries.size, 1);
		await submitActive(dir);
		const first = await readAll(dir, "http://127.0.0.1:1/");
		assert.equal(first.length, 2);
		await new Promise((resolve) => setTimeout(resolve, 20));
		await submitActive(dir);
		assert.deepEqual(await readAll(dir, "http://127.0.0.1:1/"), first);
		await instances[1].get("input")!({ source: "rpc" });
		await submitActive(dir);
		const updated = await readAll(dir, "http://127.0.0.1:1/");
		assert.equal(updated.length, 2);
		const changed = updated.find((record) => record.sessionId === "periodic-1")!;
		assert.equal(changed.prompts, 2);
		assert.ok(changed.revision! > first.find((record) => record.recordId === changed.recordId)!.revision!);
		for (const handlers of instances) await handlers.get("session_shutdown")!({ reason: "quit" });
	} finally {
		for (const timer of shared().retries.values()) clearInterval(timer);
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousShared === undefined) delete holder[TELEMETRY_SHARED_KEY];
		else holder[TELEMETRY_SHARED_KEY] = previousShared;
	}
});

test("旧实例关闭失败后，新会话启动不改变旧定稿入口及请求归属", async () => {
	const holder = globalThis as Record<symbol, unknown>;
	const previousShared = holder[TELEMETRY_SHARED_KEY];
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	const dir = await home("bound-finalizer");
	try {
		delete holder[TELEMETRY_SHARED_KEY];
		process.env.PI_CODING_AGENT_DIR = dir;
		await writeFile(join(dir, "telemetry.json"), JSON.stringify({ enabled: true, endpoint: "http://127.0.0.1:1" }));
		const handlers = new Map<string, (event: unknown, context?: unknown) => Promise<void> | void>();
		telemetry({
			on: (name: string, callback: (event: unknown, context?: unknown) => Promise<void> | void) =>
				handlers.set(name, callback),
		} as unknown as ExtensionAPI);
		const start = (id: string) =>
			handlers.get("session_start")!(
				{ reason: "new" },
				{ mode: "web", sessionManager: { getSessionId: () => id, buildContextEntries: () => [] } },
			);
		await start("old-instance");
		await handlers.get("input")!({ source: "rpc" });
		await submitActive(dir);
		const retryOld = shared().finalizers.get("old-instance")!;
		const drafts = join(dir, "telemetry", "drafts");
		await rename(drafts, `${drafts}-saved`);
		await writeFile(drafts, "blocked");
		await handlers.get("session_shutdown")!({ reason: "quit" });
		assert.ok(shared().finalizers.has("old-instance"));
		await start("new-instance");
		await handlers.get("input")!({ source: "rpc" });
		await rm(drafts);
		await rename(`${drafts}-saved`, drafts);
		await retryOld("quit");
		assert.equal(shared().finalizers.has("old-instance"), false);
		assert.equal(shared().finalizers.has("new-instance"), true);
		await requestFetch("new-instance", async () => new Response(""))("https://www.cstoa.top/v1/chat/completions");
		await submitActive(dir);
		const pending = await readAll(dir, "http://127.0.0.1:1/");
		assert.equal(pending.find((item) => item.sessionId === "old-instance")?.final, true);
		const current = pending.find((item) => item.sessionId === "new-instance")!;
		assert.equal(current.final, false);
		assert.equal(current.cstoaBilling?.requests, 1);
		await handlers.get("session_shutdown")!({ reason: "quit" });
	} finally {
		for (const timer of shared().retries.values()) clearInterval(timer);
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousShared === undefined) delete holder[TELEMETRY_SHARED_KEY];
		else holder[TELEMETRY_SHARED_KEY] = previousShared;
	}
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
			await writeFile(
				join(dir, "auth.json"),
				JSON.stringify({ cstoa: { type: "oauth", access: "fixture-only", expires: Date.now() + 60_000 } }),
			);
			const previousFetch = globalThis.fetch;
			let billingQueries = 0;
			globalThis.fetch = async (input, init) => {
				if (String(input).endsWith("/api/agent/llm/billing")) {
					billingQueries++;
					assert.equal(enabled, true, "禁用遥测不查询账单");
					assert.equal(new Headers(init?.headers).get("authorization"), "Bearer fixture-only");
					return Response.json({
						result: true,
						data: { unit: "quota_point", requests: [{ requestId: "fixture-request", quotaPoints: 198 }] },
					});
				}
				throw new Error("offline fixture");
			};
			try {
				await handlers.get("input")!({ source: "interactive" });
				await requestFetch(
					id,
					async () => new Response("", { headers: { "x-oneapi-request-id": "fixture-request" } }),
				)("https://www.cstoa.top/v1/chat/completions");
				await handlers.get("session_shutdown")!({ reason: "quit" });
			} finally {
				globalThis.fetch = previousFetch;
			}
			assert.equal(billingQueries, enabled ? 1 : 0);
			const pending = await readAll(dir, "http://127.0.0.1:1/");
			assert.equal(pending.length, enabled ? 1 : 0);
			if (enabled) {
				assert.equal(pending[0].sessionId, id);
				assert.equal(pending[0].endReason, "quit");
				assert.deepEqual(pending[0].cstoaBilling, { requests: 1, settledRequests: 1, quotaPoints: 198 });
				assert.equal(JSON.stringify(pending).includes("fixture-request"), false);
			} else {
				assert.equal(shared().retries.size, 0);
				assert.equal(shared().runs.size, 0);
				await assert.rejects(readFile(join(dir, "telemetry", "identity.json")), { code: "ENOENT" });
				await assert.rejects(readFile(join(dir, "telemetry", "status.json")), { code: "ENOENT" });
			}
			for (const timer of shared().retries.values()) clearInterval(timer);
		}
	} finally {
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousShared === undefined) delete holder[TELEMETRY_SHARED_KEY];
		else holder[TELEMETRY_SHARED_KEY] = previousShared;
	}
});
