import assert from "node:assert/strict";
import { test } from "node:test";
import { STREAM_STALE_MS, StreamHealth } from "./stream-health.ts";
import type { PacerClock } from "./stream-player.ts";

test("等待本地输入时收到心跳就不重连，45秒没有心跳才恢复连接", () => {
	let now = 0,
		count = 0,
		id = 0;
	const work = new Map<number, { at: number; run: () => void }>();
	const clock: PacerClock = {
		setTimeout: (run, ms) => {
			work.set(++id, { at: now + ms, run });
			return id;
		},
		clearTimeout: (handle) => {
			work.delete(handle as number);
		},
	};
	const advance = (ms: number) => {
		now += ms;
		for (const [id, t] of [...work])
			if (t.at <= now) {
				work.delete(id);
				t.run();
			}
	};
	const health = new StreamHealth(() => {
		count++;
	}, clock);
	health.pulse();
	for (let i = 0; i < 10; i++) {
		advance(20000);
		health.pulse();
	}
	assert.equal(count, 0);
	advance(STREAM_STALE_MS);
	assert.equal(count, 1);
	health.pulse();
	health.stop();
	advance(STREAM_STALE_MS);
	assert.equal(count, 1);
});
