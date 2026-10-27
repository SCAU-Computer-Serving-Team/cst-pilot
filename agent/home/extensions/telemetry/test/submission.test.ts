import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { testRoot } from "../../web/test/support.ts";
import { CstoaBilling } from "../billing.ts";
import { newState } from "../collect.ts";
import { draftsDir, recoverDrafts, writeDraft } from "../draft.ts";
import { unknownEnvironment } from "../environment.ts";
import { readAll } from "../outbox.ts";
import { buildRecord } from "../record.ts";
import { SessionSubmission } from "../submission.ts";

const root = await mkdtemp(join(await testRoot(), "telemetry-submission-"));
after(() => rm(root, { recursive: true, force: true }));
const endpoints = [{ url: "http://127.0.0.1:1/" }];

test("环境探测等待期间，后续输入、工具和请求不改变已冻结快照", async () => {
	const dir = join(root, "frozen");
	const state = newState("frozen", "new", "web");
	state.prompts = 1;
	state.tools.set("web_search", {
		name: "web_search",
		calls: 1,
		failures: 0,
		degraded: 0,
		totalMs: 1,
		maxMs: 1,
		resultBytes: 1,
		truncated: 0,
		providers: { fixture: 1 },
	});
	state.cstoaBilling = new CstoaBilling();
	state.cstoaBilling.start()("request-one");
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const writer = new SessionSubmission({
		agentDir: dir,
		state,
		version: "test",
		endpoints,
		environment: gate.then(() => unknownEnvironment()),
		flush: async () => undefined,
	});
	const pending = writer.submit();
	state.prompts++;
	state.tools.get("web_search")!.providers!.fixture++;
	state.cstoaBilling.start()("request-two");
	release();
	await pending;
	const [record] = await readAll(dir, endpoints[0].url);
	assert.equal(record.prompts, 1);
	assert.equal((record.tools[0] as { providers: { fixture: number } }).providers.fixture, 1);
	assert.equal(record.cstoaBilling?.requests, 1);
	const draft = JSON.parse(await readFile(join(draftsDir(dir), `${state.recordId}.json`), "utf8"));
	assert.equal(draft.cstoaRequests.length, 1);
	await writer.submit();
	const [updated] = await readAll(dir, endpoints[0].url);
	assert.equal(updated.prompts, 2);
	assert.equal(updated.cstoaBilling?.requests, 2);
});

test("恢复跳过活跃所有者，PID 碰撞的遗留草稿仍可恢复，失败重试保持已准备版本", async () => {
	const dir = join(root, "owned-recovery");
	const live = newState("live", "new", "web");
	live.prompts = 1;
	const writer = new SessionSubmission({
		agentDir: dir,
		state: live,
		version: "test",
		endpoints,
		environment: Promise.resolve(unknownEnvironment()),
		flush: async () => undefined,
	});
	await writer.submit();
	const dead = newState("dead", "new", "web");
	dead.prompts = 1;
	const original = {
		...buildRecord(dead, { endReason: "", endedAt: Date.now(), contextEntries: undefined }, "test", null),
		v: "0.2",
		revision: 1,
		final: false,
	};
	await writeDraft(dir, dead.sessionId, { record: original, lastTurnEndedAt: Date.now(), ownerPid: process.pid });
	let preparations = 0;
	let first: unknown;
	assert.deepEqual(
		await recoverDrafts(
			dir,
			async (record) => {
				first = structuredClone(record);
				throw new Error("queue failure");
			},
			async (record) => {
				preparations++;
				return { ...record, cstoaBilling: { requests: 1, settledRequests: 1, quotaPoints: preparations } };
			},
		),
		[],
	);
	const recovered = await recoverDrafts(
		dir,
		async () => undefined,
		async (record) => {
			preparations++;
			return record;
		},
	);
	assert.equal(recovered.length, 1);
	assert.equal(recovered[0].recordId, dead.recordId);
	assert.equal(recovered[0].final, true);
	assert.deepEqual(recovered[0], first);
	assert.equal(preparations, 1);
	assert.equal(JSON.parse(await readFile(join(draftsDir(dir), `${live.recordId}.json`), "utf8")).record.final, false);
	await writer.finalize("quit");
});

test("同一会话的新旧实例独立保存，旧实例关闭不删除新实例草稿", async () => {
	const dir = join(root, "same-session");
	const one = newState("same-session", "new", "web");
	const two = newState("same-session", "resume", "web");
	one.prompts = two.prompts = 1;
	const options = {
		agentDir: dir,
		version: "test",
		endpoints,
		environment: Promise.resolve(unknownEnvironment()),
		flush: async () => undefined,
	};
	const old = new SessionSubmission({ ...options, state: one });
	const current = new SessionSubmission({ ...options, state: two });
	await old.submit();
	await current.submit();
	await old.finalize("resume");
	const files = (await readdir(draftsDir(dir))).filter((name) => name.endsWith(".json"));
	assert.equal(files.length, 1);
	assert.equal(JSON.parse(await readFile(join(draftsDir(dir), files[0]), "utf8")).record.recordId, two.recordId);
	await current.finalize("quit");
});

test("空实例关闭等待已有草稿操作后清理，不产生统计记录", async () => {
	const dir = join(root, "empty");
	const state = newState("empty", "new", "web");
	const writer = new SessionSubmission({
		agentDir: dir,
		state,
		version: "test",
		endpoints,
		environment: Promise.resolve(unknownEnvironment()),
		flush: async () => undefined,
	});
	const checkpoint = writer.checkpoint();
	await writer.finalize("quit");
	await checkpoint;
	assert.deepEqual(await readAll(dir, endpoints[0].url), []);
	await assert.rejects(readFile(join(draftsDir(dir), `${state.recordId}.json`)));
});

test("关闭预算到期后封存未知账单，后台查询完成不改写最终版本", async () => {
	const dir = join(root, "close-budget");
	await mkdir(dir, { recursive: true });
	await writeFile(join(dir, "auth.json"), JSON.stringify({ cstoa: { type: "api_key", key: "fixture" } }));
	const state = newState("budget", "new", "web");
	state.prompts = 1;
	state.cstoaBilling = new CstoaBilling();
	state.cstoaBilling.start()("fixture");
	const writer = new SessionSubmission({
		agentDir: dir,
		state,
		version: "test",
		endpoints,
		environment: Promise.resolve(unknownEnvironment()),
		flush: async () => undefined,
	});
	const previousFetch = globalThis.fetch;
	let started!: () => void;
	const querying = new Promise<void>((resolve) => {
		started = resolve;
	});
	let release!: () => void;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	globalThis.fetch = async () => {
		started();
		await blocked;
		return Response.json({
			result: true,
			data: { unit: "quota_point", requests: [{ requestId: "fixture", quotaPoints: 198 }] },
		});
	};
	try {
		await writer.submit();
		writer.settleBilling();
		await querying;
		const controller = new AbortController();
		const timer = setTimeout(() => controller.abort(), 20);
		const timeout = setTimeout(() => release(), 1_000);
		const start = Date.now();
		await writer.finalize("quit", undefined, controller.signal);
		clearTimeout(timer);
		clearTimeout(timeout);
		assert.ok(Date.now() - start < 500, "不等待已有查账任务的独立时限");
		const [final] = await readAll(dir, endpoints[0].url);
		assert.equal(final.final, true);
		assert.equal(final.cstoaBilling?.quotaPoints, null);
		release();
		await new Promise((resolve) => setTimeout(resolve, 20));
		assert.deepEqual(await readAll(dir, endpoints[0].url), [final]);
	} finally {
		release();
		globalThis.fetch = previousFetch;
	}
});

test("最终快照部分端点入队失败后，重试保持已提交内容", async () => {
	const dir = join(root, "partial-final");
	const targets = [...endpoints, { url: "http://127.0.0.1:2/" }];
	const blocked = join(dir, "telemetry", `pending-${createHash("sha256").update(targets[1].url).digest("hex")}.jsonl`);
	await mkdir(blocked, { recursive: true });
	const state = newState("partial", "new", "web");
	state.prompts = 1;
	state.cstoaBilling = new CstoaBilling();
	state.cstoaBilling.start()("fixture");
	const writer = new SessionSubmission({
		agentDir: dir,
		state,
		version: "test",
		endpoints: targets,
		environment: Promise.resolve(unknownEnvironment()),
		flush: async () => undefined,
	});
	await assert.rejects(writer.finalize("quit"));
	const [first] = await readAll(dir, targets[0].url);
	assert.equal(first.cstoaBilling?.quotaPoints, null);
	await state.cstoaBilling.settle(async () => new Map([["fixture", 198]]));
	await rm(blocked, { recursive: true });
	await writer.finalize("quit");
	assert.deepEqual(await readAll(dir, targets[0].url), [first]);
	assert.deepEqual(await readAll(dir, targets[1].url), [first]);
});

test("草稿写入失败会报告失败，修复磁盘后可重试同一实例定稿", async () => {
	const dir = join(root, "failure");
	await mkdir(dir, { recursive: true });
	await writeFile(join(dir, "telemetry"), "blocked directory");
	const state = newState("retry", "new", "tui");
	state.prompts = 1;
	const record = buildRecord(state, { endReason: "", endedAt: Date.now(), contextEntries: undefined }, "test", null);
	await assert.rejects(writeDraft(dir, state.sessionId, { record, lastTurnEndedAt: Date.now() }));
	const writer = new SessionSubmission({
		agentDir: dir,
		state,
		version: "test",
		endpoints,
		environment: Promise.resolve(unknownEnvironment()),
		flush: async () => undefined,
	});
	await assert.rejects(writer.finalize("quit"));
	await rm(join(dir, "telemetry"));
	await writer.finalize("quit");
	const [final] = await readAll(dir, endpoints[0].url);
	assert.equal(final.recordId, state.recordId);
	assert.equal(final.final, true);
	assert.equal(final.revision, 1);
	await writer.finalize("quit");
	assert.deepEqual(await readAll(dir, endpoints[0].url), [final]);
});
