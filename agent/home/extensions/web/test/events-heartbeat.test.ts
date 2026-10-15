import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import { SessionEvents } from "../server/session/events.ts";

test("心跳是浏览器可接收的事件，不占用业务回放序号", { timeout: 5000 }, async () => {
	const events = new SessionEvents(undefined, 10),
		server = createServer((req, res) => events.serve(req, res));
	await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
	const response = await fetch(`http://127.0.0.1:${(server.address() as AddressInfo).port}`),
		reader = response.body!.getReader();
	try {
		await reader.read();
		const text = new TextDecoder().decode((await reader.read()).value);
		assert.match(text, /event: heartbeat/);
		assert.equal(text.includes("id:"), false);
	} finally {
		await reader.cancel();
		events.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
	}
});
