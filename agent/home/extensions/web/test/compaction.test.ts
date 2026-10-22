import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { compactionFixture } from "./compaction-fixture.ts";

test("手动压缩：短会话、落盘、幂等、空会话与重复压缩", { timeout: 20_000 }, async () => {
	const fixture = await compactionFixture();
	try {
		const slot = await fixture.conversation();
		const settings = await readFile(fixture.settingsFile, "utf8");
		const key = randomUUID();
		const result = await fixture.send(slot.id, key);
		const payload = await result.json();
		assert.equal(result.status, 200, JSON.stringify(payload));
		assert.equal(payload.compacted, true);
		const compacted = slot.session.sessionManager.getBranch().find((entry) => entry.type === "compaction");
		assert.ok(compacted);
		assert.ok(slot.session.sessionFile);
		assert.match(await readFile(slot.session.sessionFile, "utf8"), /"type":"compaction"/);
		assert.match(fixture.requests.at(-1) ?? "", /保留诊断结论/);
		assert.equal(slot.session.settingsManager.getCompactionKeepRecentTokens(), 20000);
		assert.equal(await readFile(fixture.settingsFile, "utf8"), settings);
		const count = fixture.requests.length;
		assert.equal((await fixture.send(slot.id, key)).status, 200);
		assert.equal(fixture.requests.length, count, "幂等重放不再次调用模型");
		slot.session.sessionManager.appendCustomEntry("test-compaction-metadata", {});
		const again = await fixture.send(slot.id);
		assert.equal(again.status, 409);
		assert.equal((await again.json()).error.code, "already_compacted");
		const empty = await fixture.pool.createWithKey(randomUUID());
		const nothing = await fixture.send(empty.id);
		assert.equal(nothing.status, 409);
		assert.equal((await nothing.json()).error.code, "nothing_to_compact");
		await slot.session.prompt("继续检查");
		assert.equal((await fixture.send(slot.id)).status, 200, "新增内容后可再次压缩");
	} finally {
		await fixture.close();
	}
});

test("压缩失败保留原分支；并行会话和共享设置不受本次保留范围影响", { timeout: 20_000 }, async () => {
	const fixture = await compactionFixture();
	let release = () => {};
	try {
		const slot = await fixture.conversation();
		const other = await fixture.conversation();
		const branch = slot.session.sessionManager.getBranch();
		fixture.modelState.status = 401;
		fixture.modelState.wait = new Promise<void>((resolve) => {
			release = resolve;
		});
		const pending = fixture.send(slot.id);
		for (let attempt = 0; attempt < 100 && fixture.requests.length < 5; attempt++)
			await new Promise((resolve) => setTimeout(resolve, 10));
		assert.equal(fixture.requests.length, 5);
		assert.equal(slot.session.settingsManager, other.session.settingsManager);
		assert.equal(other.session.settingsManager.getCompactionSettings().keepRecentTokens, 20000);
		release();
		assert.equal((await pending).status, 500);
		assert.deepEqual(slot.session.sessionManager.getBranch(), branch);
		assert.equal(slot.session.isIdle, true);
		fixture.modelState.status = 200;
		assert.equal((await fixture.send(slot.id)).status, 200, "修复服务后可重试");
	} finally {
		release();
		await fixture.close();
	}
});
