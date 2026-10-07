import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { ownSession, SessionOwnerConflict } from "../../runtime/owner.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("会话写权：同身份拒绝重复、不同身份独立、释放后可再次持有", async () => {
	const home = await mkdtemp(join(await testRoot(), "session-owner-"));
	const releases: (() => void)[] = [];
	try {
		const one = await ownSession(home, "one");
		releases.push(one);
		await assert.rejects(ownSession(home, "one"), SessionOwnerConflict);
		releases.push(await ownSession(home, "two"));
		one();
		one();
		releases.push(await ownSession(home, "one"));
	} finally {
		for (const release of releases) release();
		await rm(home, { recursive: true, force: true });
	}
});

test("创建分支保留原会话ID、独立写权及cwd", async () => {
	const home = await mkdtemp(join(await testRoot(), "fork-identity-"));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	try {
		await writeFile(join(home, "settings.json"), "{}");
		const slot = await pool.create();
		const manager = slot.session.sessionManager;
		const entry = manager.appendMessage({
			role: "user",
			content: [{ type: "text", text: "original" }],
			timestamp: Date.now(),
		});
		manager.appendMessage({
			role: "assistant",
			content: [{ type: "text", text: "done" }],
			api: "openai-completions",
			provider: "probe",
			model: "mock",
			stopReason: "stop",
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			timestamp: Date.now(),
		});
		const before = await readFile(slot.file!, "utf8");
		const fork = await pool.fork(slot.id, entry);
		assert.equal(slot.session.sessionId, slot.id);
		assert.notEqual(fork.id, slot.id);
		assert.equal(fork.session.sessionManager.getCwd(), home);
		assert.equal(await readFile(slot.file!, "utf8"), before);
		await assert.rejects(ownSession(home, slot.id), SessionOwnerConflict);
		await assert.rejects(ownSession(home, fork.id), SessionOwnerConflict);
	} finally {
		await pool.close();
		await rm(home, { recursive: true, force: true });
	}
});
