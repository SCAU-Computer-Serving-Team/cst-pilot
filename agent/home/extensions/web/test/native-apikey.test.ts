import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { createWebApi } from "../server/api.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("APIKEY 多步凭据沿用 Pi：Cloudflare 账号、Bedrock 方式选择与凭据存储", async () => {
	const home = await mkdtemp(join(await testRoot(), "native-apikey-"));
	await writeFile(join(home, "settings.json"), "{}");
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	let api: ReturnType<typeof createWebApi>;
	const server = createServer((request, response) => {
		void api(request, response, new URL(request.url ?? "/", "http://localhost").pathname);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("missing address");
	api = createWebApi(pool, home, address.port);
	const origin = `http://127.0.0.1:${address.port}`;
	type Status = {
		state: string;
		flowId: string;
		prompt?: { id: string; type: string; message: string; options?: { id: string }[] };
	};
	const status = async (id: string): Promise<Status> =>
		(await fetch(`${origin}/api/auth/${id}/api-key/status`)).json();
	const send = (id: string, action: string, input: unknown) =>
		fetch(`${origin}/api/auth/${id}/api-key/${action}`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify(input),
		});
	const until = async (id: string, predicate: (value: Status) => boolean) => {
		for (let attempt = 0; attempt < 100; attempt++) {
			const value = await status(id);
			if (predicate(value)) return value;
			await new Promise((resolve) => setTimeout(resolve, 20));
		}
		throw new Error(`登录状态未更新：${JSON.stringify(await status(id))}`);
	};
	try {
		const started = await send("cloudflare-workers-ai", "start", { key: "test-cloudflare-key" });
		assert.equal(started.status, 202);
		const account = (await started.json()) as Status;
		assert.equal(account.prompt?.type, "text");
		assert.match(account.prompt?.message ?? "", /account/i);
		assert.equal(JSON.stringify(account).includes("test-cloudflare-key"), false);
		assert.equal(
			(
				await send("cloudflare-workers-ai", "respond", {
					flowId: account.flowId,
					promptId: account.prompt?.id,
					value: "test-account-id",
				})
			).status,
			200,
		);
		await until("cloudflare-workers-ai", (value) => value.state === "succeeded");
		const bedrock = (await (await send("amazon-bedrock", "start", {})).json()) as Status;
		assert.equal(bedrock.prompt?.type, "select");
		const option = bedrock.prompt?.options?.find((item) => /bearer/i.test(item.id));
		assert.ok(option);
		assert.equal(
			(
				await send("amazon-bedrock", "respond", {
					flowId: bedrock.flowId,
					promptId: bedrock.prompt?.id,
					value: option.id,
				})
			).status,
			200,
		);
		const secret = await until("amazon-bedrock", (value) => value.prompt?.type === "secret");
		assert.equal(
			(
				await send("amazon-bedrock", "respond", {
					flowId: secret.flowId,
					promptId: secret.prompt?.id,
					value: "test-bedrock-token",
				})
			).status,
			200,
		);
		await until("amazon-bedrock", (value) => value.state === "succeeded");
		const stored = JSON.parse(await readFile(join(home, "auth.json"), "utf8"));
		assert.equal(stored["cloudflare-workers-ai"].key, "test-cloudflare-key");
		assert.equal(stored["cloudflare-workers-ai"].env.CLOUDFLARE_ACCOUNT_ID, "test-account-id");
		assert.equal(stored["amazon-bedrock"].key, "test-bedrock-token");
		const browserView = await (await fetch(`${origin}/api/auth`)).text();
		assert.equal(browserView.includes("test-cloudflare-key") || browserView.includes("test-bedrock-token"), false);
	} finally {
		await api.close();
		await pool.close();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
