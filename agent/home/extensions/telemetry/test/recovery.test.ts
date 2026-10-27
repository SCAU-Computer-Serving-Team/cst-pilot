import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { testRoot } from "../../web/test/support.ts";
import { newState } from "../collect.ts";
import { draftsDir, recoverDrafts, writeDraft } from "../draft.ts";
import { buildRecord } from "../record.ts";

const root = await mkdtemp(join(await testRoot(), "telemetry-recovery-"));
after(() => rm(root, { recursive: true, force: true }));

test("另一进程的活跃草稿不被恢复，退出后即使 PID 碰撞也可恢复", async () => {
	const dir = join(root, "process-owner");
	await mkdir(dir, { recursive: true });
	const script = join(dir, "owner.mts");
	await writeFile(
		script,
		`
import { newState } from ${JSON.stringify(new URL("../collect.ts", import.meta.url).href)};
import { SessionSubmission } from ${JSON.stringify(new URL("../submission.ts", import.meta.url).href)};
import { unknownEnvironment } from ${JSON.stringify(new URL("../environment.ts", import.meta.url).href)};
const state = newState("child", "new", "web"); state.prompts = 1;
const writer = new SessionSubmission({agentDir:process.argv[2],state,version:"test",environment:Promise.resolve(unknownEnvironment()),endpoints:[],flush:async()=>{}});
await writer.checkpoint();
console.log(state.recordId);
setInterval(()=>{},1000);
`,
	);
	const child = spawn(process.execPath, [script, dir], { stdio: ["ignore", "pipe", "pipe"] });
	const exit = once(child, "exit");
	try {
		const [chunk] = await once(child.stdout, "data");
		const recordId = chunk.toString().trim();
		assert.deepEqual(await recoverDrafts(dir, async () => assert.fail("活跃草稿不应入队")), []);
		child.kill();
		await exit;
		const file = join(draftsDir(dir), `${recordId}.json`);
		const draft = JSON.parse(await readFile(file, "utf8"));
		draft.ownerPid = process.pid;
		await writeFile(file, JSON.stringify(draft));
		const recovered = await recoverDrafts(dir, async () => undefined);
		assert.equal(recovered.length, 1);
		assert.equal(recovered[0].recordId, recordId);
		assert.equal(recovered[0].endReason, "crash");
	} finally {
		child.kill();
		await exit;
	}
});

test("并发恢复只封存一个版本，慢查账不覆盖其他进程已准备的内容", async () => {
	const dir = join(root, "concurrent-recovery");
	const state = newState("concurrent", "new", "web");
	state.prompts = 1;
	const record = {
		...buildRecord(state, { endReason: "", endedAt: Date.now(), contextEntries: undefined }, "test", null),
		v: "0.2",
		revision: 1,
		final: false,
	};
	await writeDraft(dir, state.sessionId, { record, lastTurnEndedAt: Date.now() });
	let ready!: () => void;
	const started = new Promise<void>((resolve) => {
		ready = resolve;
	});
	let release!: () => void;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	const sent: unknown[] = [];
	const slow = recoverDrafts(
		dir,
		async (item) => {
			sent.push(item);
		},
		async (item) => {
			ready();
			await blocked;
			return { ...item, cstoaBilling: { requests: 1, settledRequests: 1, quotaPoints: 1 } };
		},
	);
	try {
		await started;
		const fast = await recoverDrafts(
			dir,
			async (item) => {
				sent.push(item);
			},
			async (item) => ({ ...item, cstoaBilling: { requests: 1, settledRequests: 1, quotaPoints: 2 } }),
		);
		assert.equal(fast.length, 1);
	} finally {
		release();
	}
	assert.deepEqual(await slow, []);
	assert.equal(sent.length, 1);
});

test("复制来的失效事务锁不被本机同 PID 阻塞", async () => {
	const dir = join(root, "copied-lock");
	const target = join(draftsDir(dir), "fixture.json");
	await mkdir(`${target}.lock`, { recursive: true });
	const token = randomUUID();
	await writeFile(
		join(`${target}.lock`, "owner.json"),
		JSON.stringify({
			pid: process.pid,
			proof: { token, address: process.platform === "win32" ? `\\\\.\\pipe\\cst-pilot-telemetry-${token}` : 1 },
		}),
	);
	const state = newState("fixture", "new", "web");
	state.prompts = 1;
	const record = buildRecord(state, { endReason: "", endedAt: Date.now(), contextEntries: undefined }, "test", null);
	await writeDraft(dir, "fixture", { record, lastTurnEndedAt: Date.now() });
	assert.equal(JSON.parse(await readFile(target, "utf8")).record.recordId, state.recordId);
});
