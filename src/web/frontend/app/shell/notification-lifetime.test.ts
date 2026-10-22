import assert from "node:assert/strict";
import { test } from "node:test";
import { NotificationLifetime } from "./notification-lifetime.ts";

function fixture() {
	let now = 0;
	let expired = 0;
	const timers = new Map<ReturnType<typeof setTimeout>, { at: number; callback: () => void }>();
	const lifetime = new NotificationLifetime(2600, () => expired++, {
		now: () => now,
		set(callback, delay) {
			const handle = setTimeout(() => {}, 60_000);
			clearTimeout(handle);
			timers.set(handle, { at: now + delay, callback });
			return handle;
		},
		clear: (handle) => {
			timers.delete(handle);
		},
	});
	return {
		lifetime,
		expired: () => expired,
		advance(ms: number) {
			now += ms;
			for (const [handle, timer] of timers) {
				if (timer.at > now) continue;
				timers.delete(handle);
				timer.callback();
			}
		},
	};
}

test("悬停暂停，离开后只继续剩余时间", () => {
	const f = fixture();
	f.lifetime.resume("start");
	f.advance(1700);
	f.lifetime.pause("pointer");
	f.advance(10_000);
	assert.equal(f.expired(), 0);
	f.lifetime.resume("pointer");
	f.advance(899);
	assert.equal(f.expired(), 0);
	f.advance(1);
	assert.equal(f.expired(), 1);
});

test("焦点、鼠标和后台页面的暂停独立，重复进入不重置时间", () => {
	const f = fixture();
	f.lifetime.resume("start");
	f.advance(1000);
	f.lifetime.pause("pointer");
	f.lifetime.pause("pointer");
	f.lifetime.pause("focus");
	f.lifetime.resume("pointer");
	f.advance(3000);
	assert.equal(f.expired(), 0);
	f.lifetime.pause("hidden");
	f.lifetime.resume("focus");
	f.advance(3000);
	assert.equal(f.expired(), 0);
	f.lifetime.resume("hidden");
	f.advance(1600);
	assert.equal(f.expired(), 1);
});

test("卸载与手动关闭清理计时，不再触发通知关闭", () => {
	const f = fixture();
	f.lifetime.resume("start");
	f.lifetime.dispose();
	f.advance(3000);
	f.lifetime.resume("pointer");
	f.advance(3000);
	assert.equal(f.expired(), 0);
});
