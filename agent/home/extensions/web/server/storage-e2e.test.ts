import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "./api.ts";
import { createWebServer, listenWebServer } from "./http.ts";
import { WebSessionPool } from "./sessions.ts";

const now = new Date();
const date = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
await mkdir(join("E:/tmp", date), { recursive: true });

test("an unwritable inbox reports a storage error without sending to the model", async () => {
	const home = await mkdtemp(join("E:/tmp", date, "cst-web-storage-"));
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	await writeFile(join(home, "web-inbox"), "occupied");
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const port = (probe.address() as AddressInfo).port;
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "test" }),
		api,
	);
	try {
		await listenWebServer(server, port);
		const response = await fetch(`${origin}/api/sessions/${slot.id}/messages`, {
			method: "POST",
			headers: { Origin: origin, "X-CST-Web-Request": "1", "Content-Type": "application/json" },
			body: JSON.stringify({ id: "failed-write", text: "must not run", delivery: "queue" }),
		});
		assert.equal(response.status, 500);
		assert.equal((await response.json()).error.code, "storage_unavailable");
		assert.equal(slot.session.messages.length, 0);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await pool.close();
		await api.close();
		await rm(home, { recursive: true, force: true });
	}
});
