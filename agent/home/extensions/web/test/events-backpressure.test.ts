import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import { SessionEvents } from "../server/session/events.ts";

test("正常读取的大帧不会断开 SSE，后续正文仍即时到达", { timeout: 5000 }, async () => {
	const events = new SessionEvents();
	const server = createServer((request, response) => events.serve(request, response));
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
	let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
	try {
		const response = await fetch(origin);
		reader = response.body?.getReader();
		assert.ok(reader);
		await reader.read();
		events.publish("session", { type: "message_update", thinking: "思考".repeat(50_000) });
		let received = "";
		while (!received.includes("\n\n")) {
			const next = await reader.read();
			assert.equal(next.done, false, "大帧仍在传输时连接不能结束");
			received += new TextDecoder().decode(next.value);
		}
		events.publish("session", { type: "message_update", text: "后续正文" });
		let second = "";
		while (!second.includes("后续正文")) {
			const next = await reader.read();
			assert.equal(next.done, false, "不能因 write 返回 false 而结束正常连接");
			second += new TextDecoder().decode(next.value);
		}
		assert.match(second, /后续正文/);
	} finally {
		await reader?.cancel();
		events.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
