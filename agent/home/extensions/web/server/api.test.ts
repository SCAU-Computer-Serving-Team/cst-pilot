import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "./api.ts";
import { createWebServer, listenWebServer } from "./http.ts";
import { WebSessionPool } from "./sessions.ts";

const today = new Date().toISOString().slice(0, 10);
const base = join("E:/tmp", today);
await mkdir(base, { recursive: true });
const home = await mkdtemp(join(base, "cst-web-api-"));
await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
const pool = new WebSessionPool({
	cwd: home,
	agentDir: home,
	sessionDir: join(home, "sessions"),
	withLoader: (load) => load(),
});
const slot = await pool.create();
const probe = createNetServer();
await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
const { port } = probe.address() as AddressInfo;
await new Promise<void>((resolve) => probe.close(() => resolve()));
const origin = `http://127.0.0.1:${port}`;
const server = createWebServer(
	fileURLToPath(new URL("../static/", import.meta.url)),
	port,
	() => ({ sessions: [], stage: "test" }),
	createWebApi(pool, home, port),
);
await listenWebServer(server, port);
after(async () => {
	server.closeAllConnections();
	await new Promise<void>((resolve) => server.close(() => resolve()));
	await pool.close();
	await rm(home, { recursive: true, force: true });
});
const post = (path: string, value: unknown, headers: Record<string, string> = {}) =>
	fetch(`${origin}${path}`, {
		method: "POST",
		headers: { Origin: origin, "X-CST-Web-Request": "1", "Content-Type": "application/json", ...headers },
		body: JSON.stringify(value),
	});

test("API shares one session, accepts once, and does not expose absolute paths", async () => {
	const list = await (await fetch(`${origin}/api/sessions`)).json();
	assert.equal(list.sessions[0].id, slot.id);
	assert.equal(JSON.stringify(list).includes(home.replaceAll("\\", "\\\\")), false);
	const path = `/api/sessions/${slot.id}`;
	const detail = await (await fetch(`${origin}${path}`)).json();
	assert.equal(detail.id, slot.id);
	const submitted = { id: "idempotent-a", text: "inspect", delivery: "queue" };
	const first = await post(`${path}/messages`, submitted);
	assert.equal(first.status, 202);
	const second = await post(`${path}/messages`, submitted);
	assert.equal(second.status, 202);
	assert.equal((await first.json()).item.sequence, (await second.json()).item.sequence);
	const conflict = await post(`${path}/messages`, { ...submitted, text: "different" });
	assert.equal(conflict.status, 409);
});

test("stale event IDs request a full snapshot", async () => {
	const response = await fetch(`${origin}/api/sessions/${slot.id}/events`, {
		headers: { "Last-Event-ID": "stale:1" },
	});
	assert.equal(response.status, 200);
	assert.match(response.headers.get("content-type") ?? "", /text\/event-stream/);
	const reader = response.body?.getReader();
	assert.ok(reader);
	const { value } = await reader.read();
	assert.match(new TextDecoder().decode(value), /event: reset/);
	await reader.cancel();
});

test("cross-origin writes and unsupported attachments are rejected", async () => {
	const path = `/api/sessions/${slot.id}/messages`;
	assert.equal(
		(await post(path, { id: "b", text: "hi", delivery: "queue" }, { Origin: "https://evil.example" })).status,
		403,
	);
	assert.equal(
		(await post(path, { id: "c", text: "hi", delivery: "queue", images: [{ data: "secret" }] })).status,
		400,
	);
	assert.equal(
		(await fetch(`${origin}/api/sessions/${slot.id}/messages`, { method: "POST", body: "{}" })).status,
		403,
	);
});
