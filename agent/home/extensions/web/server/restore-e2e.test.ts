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

async function boot(home: string) {
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const port = (probe.address() as AddressInfo).port;
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	return {
		origin: `http://127.0.0.1:${port}`,
		pool,
		close: async () => {
			server.closeAllConnections();
			await new Promise<void>((resolve) => server.close(() => resolve()));
			await pool.close();
			await api.close();
		},
	};
}

test("accepted images and empty session IDs are still available after server restart", async () => {
	const home = await mkdtemp(join("E:/tmp", date, "cst-web-restore-"));
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScL/nwAAAABJRU5ErkJggg==";
	let first: Awaited<ReturnType<typeof boot>> | undefined;
	let second: Awaited<ReturnType<typeof boot>> | undefined;
	try {
		first = await boot(home);
		const slot = await first.pool.createWithKey("restore-session");
		const message = await fetch(`${first.origin}/api/sessions/${slot.id}/messages`, {
			method: "POST",
			headers: { Origin: first.origin, "Content-Type": "application/json", "X-CST-Web-Request": "1" },
			body: JSON.stringify({
				id: "img",
				text: "",
				delivery: "queue",
				images: [{ mimeType: "image/png", data: png }],
			}),
		});
		assert.equal(message.status, 202);
		const ref = (await message.json()).item.images[0].id;
		await first.close();
		first = undefined;
		second = await boot(home);
		const snapshot = await (await fetch(`${second.origin}/api/sessions/${slot.id}`)).json();
		assert.equal(snapshot.id, slot.id);
		assert.equal(snapshot.queue.items[0].images[0].id, ref);
		const image = await fetch(`${second.origin}/api/sessions/${slot.id}/attachments/${ref}`);
		assert.equal(Buffer.from(await image.arrayBuffer()).toString("base64"), png);
	} finally {
		await second?.close();
		await first?.close();
		await rm(home, { recursive: true, force: true });
	}
});
