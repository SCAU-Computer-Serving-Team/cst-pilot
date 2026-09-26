import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import { SessionEvents } from "./events.ts";

test("an event ID older than the retained SSE history asks the client to reload", async () => {
	const events = new SessionEvents();
	const server = createServer((request, response) => events.serve(request, response));
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/events`;
	try {
		const live = await fetch(url);
		const reader = live.body?.getReader();
		assert.ok(reader);
		await reader.read(); // connected marker
		events.publish("queue", { first: true });
		const initial = new TextDecoder().decode((await reader.read()).value);
		const id = /id: ([^\n]+)/.exec(initial)?.[1];
		assert.ok(id);
		await reader.cancel();
		for (let i = 0; i < 300; i++) events.publish("queue", { index: i });
		const resumed = await fetch(url, { headers: { "Last-Event-ID": id } });
		const resumedReader = resumed.body?.getReader();
		assert.ok(resumedReader);
		assert.match(new TextDecoder().decode((await resumedReader.read()).value), /event: reset/);
		await resumedReader.cancel();
	} finally {
		events.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
