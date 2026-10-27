import assert from "node:assert/strict";
import { test } from "node:test";
import { attachTelemetryHints } from "./telemetry-hints.ts";

test("页面提示带同源写入标记且离开使用 keepalive，卸载后停止通知", async () => {
	const target = new EventTarget();
	const sent: { url: string; options?: RequestInit }[] = [];
	const detach = attachTelemetryHints(target, async (url, options) => {
		sent.push({ url: String(url), options });
		return new Response("{}", { status: 202 });
	});
	for (const name of ["pagehide", "pageshow", "online"]) target.dispatchEvent(new Event(name));
	assert.equal(sent.length, 3);
	assert.deepEqual(
		sent.map((entry) => JSON.parse(String(entry.options?.body)).reason),
		["pagehide", "pageshow", "online"],
	);
	for (const entry of sent) {
		assert.equal(entry.url, "/api/lifecycle/telemetry");
		assert.equal(entry.options?.keepalive, true);
		assert.equal(new Headers(entry.options?.headers).get("X-CST-Web-Request"), "1");
	}
	detach();
	target.dispatchEvent(new Event("pagehide"));
	assert.equal(sent.length, 3);
});
