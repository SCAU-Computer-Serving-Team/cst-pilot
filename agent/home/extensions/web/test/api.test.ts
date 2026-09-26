import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/sessions.ts";
import { testRoot } from "./support.ts";

const base = await testRoot();
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
const api = createWebApi(pool, home, port);
const server = createWebServer(
	fileURLToPath(new URL("../static/", import.meta.url)),
	port,
	() => ({ sessions: [], stage: "test" }),
	api,
);
await listenWebServer(server, port);
after(async () => {
	server.closeAllConnections();
	await new Promise<void>((resolve) => server.close(() => resolve()));
	await pool.close();
	await api.close();
	await rm(home, { recursive: true, force: true });
});
const post = (path: string, value: unknown, headers: Record<string, string> = {}) =>
	fetch(`${origin}${path}`, {
		method: "POST",
		headers: {
			Origin: origin,
			"X-CST-Web-Request": "1",
			"Content-Type": "application/json",
			"Idempotency-Key": randomUUID(),
			...headers,
		},
		body: JSON.stringify(value),
	});

test("creating a session with the same request key returns the same session", async () => {
	const key = "new-session-once";
	const first = await post("/api/sessions", {}, { "Idempotency-Key": key });
	assert.equal(first.status, 201);
	const created = await first.json();
	const repeated = await post("/api/sessions", {}, { "Idempotency-Key": key });
	assert.equal(repeated.status, 201);
	assert.equal((await repeated.json()).id, created.id);
	assert.equal((await (await fetch(`${origin}/api/sessions/${created.id}`)).json()).id, created.id);
});

test("deleting an idle session removes it without resurrecting the create key", async () => {
	const created = await (await post("/api/sessions", {}, { "Idempotency-Key": "delete-this-session" })).json();
	const path = `/api/sessions/${created.id}`;
	const headers = { Origin: origin, "X-CST-Web-Request": "1", "Idempotency-Key": randomUUID() };
	const [first, retry] = await Promise.all([
		fetch(`${origin}${path}`, { method: "DELETE", headers }),
		fetch(`${origin}${path}`, { method: "DELETE", headers }),
	]);
	assert.equal(first.status, 200);
	assert.equal(retry.status, 200);
	assert.equal((await fetch(`${origin}${path}`)).status, 404);
	assert.equal((await post("/api/sessions", {}, { "Idempotency-Key": "delete-this-session" })).status, 409);
});

test("API shares one session, accepts once, and does not expose absolute paths", async () => {
	const list = await (await fetch(`${origin}/api/sessions`)).json();
	assert.ok(list.sessions.some((entry: { id: string }) => entry.id === slot.id));
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

test("API key login, status and logout keep the secret out of responses", async () => {
	const secret = "test-secret-never-in-response";
	const saved = await fetch(`${origin}/api/auth/openai/api-key`, {
		method: "PUT",
		headers: {
			Origin: origin,
			"X-CST-Web-Request": "1",
			"Content-Type": "application/json",
			"Idempotency-Key": randomUUID(),
		},
		body: JSON.stringify({ key: secret }),
	});
	assert.equal(saved.status, 200);
	assert.equal((await saved.text()).includes(secret), false);
	const status = await fetch(`${origin}/api/auth`);
	assert.equal(status.status, 200);
	const details = await status.text();
	assert.equal(details.includes(secret), false);
	assert.ok(details.includes("openai"));
	const logout = await post("/api/auth/openai/logout", {});
	assert.equal(logout.status, 200);
});

test("an extension question can be answered once from another browser request", async () => {
	const waiting = slot.ui.context().select("选择设备", ["A", "B"]);
	const detail = await (await fetch(`${origin}/api/sessions/${slot.id}`)).json();
	assert.equal(detail.ui.length, 1);
	const requestId = detail.ui[0].requestId;
	const path = `/api/sessions/${slot.id}/ui/${requestId}/response`;
	assert.equal((await post(path, { value: "invalid" })).status, 400);
	assert.equal((await post(path, { value: "B" })).status, 200);
	assert.equal(await waiting, "B");
	assert.equal((await post(path, { value: "A" })).status, 409);
});

test("settings expose only supported fields and persist theme changes", async () => {
	const initial = await fetch(`${origin}/api/settings`);
	assert.equal(initial.status, 200);
	assert.deepEqual(Object.keys(await initial.json()).sort(), ["theme"]);
	const changed = await fetch(`${origin}/api/settings`, {
		method: "PATCH",
		headers: {
			Origin: origin,
			"X-CST-Web-Request": "1",
			"Content-Type": "application/json",
			"Idempotency-Key": randomUUID(),
		},
		body: JSON.stringify({ theme: "dark" }),
	});
	assert.equal(changed.status, 200);
	assert.equal((await (await fetch(`${origin}/api/settings`)).json()).theme, "dark");
	const invalid = await fetch(`${origin}/api/settings`, {
		method: "PATCH",
		headers: { Origin: origin, "X-CST-Web-Request": "1", "Content-Type": "application/json" },
		body: JSON.stringify({ defaultTools: ["bash"] }),
	});
	assert.equal(invalid.status, 400);
});

test("global events can signal the client to reload state", async () => {
	const response = await fetch(`${origin}/api/events`, { headers: { "Last-Event-ID": "unknown:4" } });
	assert.equal(response.status, 200);
	const reader = response.body?.getReader();
	assert.ok(reader);
	assert.match(new TextDecoder().decode((await reader.read()).value), /event: reset/);
	await reader.cancel();
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

test("an accepted image can be read back and retried without duplicate delivery", async () => {
	const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScL/nwAAAABJRU5ErkJggg==";
	const path = `/api/sessions/${slot.id}`;
	const input = {
		id: "with-image",
		text: "inspect image",
		delivery: "queue",
		images: [{ mimeType: "image/png", data: png }],
	};
	const first = await post(`${path}/messages`, input);
	assert.equal(first.status, 202);
	const item = (await first.json()).item;
	assert.equal(item.images.length, 1);
	const image = await fetch(`${origin}${path}/attachments/${item.images[0].id}`);
	assert.equal(image.status, 200);
	assert.equal(Buffer.from(await image.arrayBuffer()).toString("base64"), png);
	const repeat = await post(`${path}/messages`, input);
	assert.equal((await repeat.json()).item.sequence, item.sequence);
	const changed = await post(`${path}/messages`, { ...input, images: [] });
	assert.equal(changed.status, 409);
	const imageOnly = await post(`${path}/messages`, { ...input, id: "image-only", text: "" });
	assert.equal(imageOnly.status, 202);
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
