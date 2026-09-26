import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
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

test("an SDK extension dialog crosses HTTP and accepts one browser answer", async () => {
	const home = await mkdtemp(join("E:/tmp", date, "cst-web-ui-e2e-"));
	const extension = join(home, "extensions", "ask");
	await mkdir(extension, { recursive: true });
	await writeFile(
		join(extension, "index.ts"),
		`export default function(pi) { pi.on("session_start", (_event, ctx) => { void ctx.ui.select("Choose", ["A", "B"]); }); }`,
	);
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const port = (probe.address() as AddressInfo).port;
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	const origin = `http://127.0.0.1:${port}`;
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "test" }),
		api,
	);
	try {
		await listenWebServer(server, port);
		const slot = await pool.create();
		const post = (path: string, input: unknown) =>
			fetch(`${origin}${path}`, {
				method: "POST",
				headers: {
					Origin: origin,
					"Content-Type": "application/json",
					"X-CST-Web-Request": "1",
					"Idempotency-Key": randomUUID(),
				},
				body: JSON.stringify(input),
			});
		const path = `/api/sessions/${slot.id}`;
		let question: { requestId: string } | undefined;
		for (let attempt = 0; attempt < 100; attempt++) {
			const snapshot = await (await fetch(`${origin}${path}`)).json();
			question = snapshot.ui[0];
			if (question) break;
			await new Promise((resolve) => setTimeout(resolve, 30));
		}
		assert.ok(question);
		assert.equal((await post(`${path}/ui/${question.requestId}/response`, { value: "B" })).status, 200);
		const final = await (await fetch(`${origin}${path}`)).json();
		assert.equal(final.ui.length, 0);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await pool.close();
		await api.close();
		await rm(home, { recursive: true, force: true });
	}
});
