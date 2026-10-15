import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import type { BitLockerVolume } from "../../diagnostics/bitlocker-data.ts";
import { bitLockerWebInteractions } from "../../diagnostics/bitlocker-web.ts";
import { openDeliveredRunbook } from "../server/api/runbook.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const volume: BitLockerVolume = {
	volumeId: "fixture-volume",
	drive: "D:",
	volumeType: 1,
	conversionStatus: 1,
	encryptionPercentage: 100,
	encryptionMethod: 7,
	protectionStatus: 1,
	lockStatus: 1,
	autoUnlockEnabled: false,
	autoUnlockKeysStored: null,
	keyProtectors: [],
	collectionErrors: [],
};
test("真实 HTTP：凭据一次消费、正确错误码与清单请求去重", { timeout: 15000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "special-tool-api-redo-")),
		home = join(root, "home"),
		outbox = join(root, "outbox");
	await mkdir(home);
	await mkdir(outbox);
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const file = join(outbox, "fixture.txt");
	await writeFile(file, "fixture command");
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create(),
		other = await pool.create(),
		port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	let launches = 0;
	let failLaunch = false;
	const api = createWebApi(pool, home, port, undefined, {
		openRunbook: (file) =>
			openDeliveredRunbook(
				file,
				async () => {
					if (failLaunch) throw new Error("fixture launch failure");
					launches++;
				},
				outbox,
			),
	});
	const server = createWebServer(home, port, undefined, api);
	await listenWebServer(server, port);
	const post = (suffix: string, value: unknown, key = "fixture-key", id = slot.id) =>
		fetch(`${origin}/api/sessions/${id}/${suffix}`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": key,
			},
			body: JSON.stringify(value),
		});
	try {
		const channel = bitLockerWebInteractions.get(slot.id)!;
		const pending = channel.forCall("reused-call", "entry-a").password(volume);
		const request = channel.snapshot()[0]!;
		const invalid = await post(`bitlocker/${request.requestId}`, { password: "invalid" });
		assert.equal(invalid.status, 400);
		assert.match((await invalid.json()).error.message, /八组六位数字/);
		assert.equal((await post(`bitlocker/${request.requestId}`, { password: "invalid", targets: [] })).status, 400);
		assert.equal(
			(await post(`bitlocker/${request.requestId}`, { cancel: true }, "cross-session", other.id)).status,
			409,
		);
		const password = Array(8).fill("123456").join("-");
		const responses = await Promise.all([
			post(`bitlocker/${request.requestId}`, { password }, "same-credential-key"),
			post(`bitlocker/${request.requestId}`, { password }, "same-credential-key"),
		]);
		assert.deepEqual(responses.map((r) => r.status).sort(), [200, 409]);
		assert.equal(await pending, password);
		assert.equal((await readFile(join(home, "web-access.log"), "utf8")).includes(password), false);
		assert.equal(
			await readFile(
				join(
					home,
					"web-inbox",
					"receipts",
					`${createHash("sha256").update("same-credential-key").digest("hex")}.json`,
				),
				"utf8",
			).then(
				() => true,
				() => false,
			),
			false,
		);
		const snapshot = await (await fetch(`${origin}/api/sessions/${slot.id}`)).json();
		assert.equal(snapshot.bitlocker.requests.length, 0);
		assert.equal(snapshot.bitlocker.revision, 2);
		const entryId = slot.session.sessionManager.appendMessage({
			role: "assistant",
			content: [{ type: "toolCall", id: "runbook-call", name: "runbook", arguments: { title: "fixture" } }],
			api: "openai-completions",
			provider: "fixture",
			model: "fixture",
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			stopReason: "toolUse",
			timestamp: Date.now(),
		});
		slot.session.sessionManager.appendMessage({
			role: "toolResult",
			toolCallId: "runbook-call",
			toolName: "runbook",
			content: [{ type: "text", text: "fixture" }],
			details: { runbook: { file } },
			isError: false,
			timestamp: Date.now(),
		});
		const body = { entryId, toolCallId: "runbook-call" };
		const opens = await Promise.all([
			post("runbook/open", body, "open-once"),
			post("runbook/open", body, "open-once"),
		]);
		assert.deepEqual(
			opens.map((r) => r.status),
			[200, 200],
		);
		assert.equal(launches, 1);
		assert.equal((await post("runbook/open", { ...body, entryId: "different" }, "open-once")).status, 409);
		assert.equal((await post("runbook/open", { ...body, file }, "injected-path")).status, 400);
		assert.equal((await post("runbook/open", body, "other-session", other.id)).status, 400);
		await rm(file);
		const missing = await post("runbook/open", body, "open-missing");
		assert.equal(missing.status, 404);
		assert.equal((await missing.json()).error.code, "runbook_missing");
		await writeFile(file, "fixture command");
		failLaunch = true;
		const failed = await post("runbook/open", body, "open-failed");
		assert.equal(failed.status, 503);
		assert.equal((await failed.json()).error.code, "runbook_launch_failed");
		assert.equal(launches, 1);
	} finally {
		await pool.close();
		await api.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(root, { recursive: true, force: true });
	}
});
