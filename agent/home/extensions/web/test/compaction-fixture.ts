import { randomUUID } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

export async function compactionFixture() {
	const home = await mkdtemp(join(await testRoot(), "manual-compaction-"));
	const requests: string[] = [];
	const modelState = { status: 200, wait: Promise.resolve() };
	const model = createServer(async (request, response) => {
		const chunks: Buffer[] = [];
		for await (const chunk of request) chunks.push(chunk);
		requests.push(Buffer.concat(chunks).toString("utf8"));
		await modelState.wait;
		if (modelState.status !== 200) {
			response.writeHead(modelState.status, { "Content-Type": "application/json" });
			response.end(JSON.stringify({ error: { message: "Unauthorized" } }));
			return;
		}
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.end(
			`data: ${JSON.stringify({ id: "mock", choices: [{ index: 0, delta: { content: "诊断结论已保存" }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	const address = model.address();
	if (!address || typeof address === "string") throw new Error("模型端口不可用");
	const settingsFile = join(home, "settings.json");
	await writeFile(
		settingsFile,
		JSON.stringify({
			defaultProvider: "probe",
			defaultModel: "mock",
			compaction: { enabled: false },
			retry: { enabled: false },
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					api: "openai-completions",
					baseUrl: `http://127.0.0.1:${address.port}/v1`,
					apiKey: "test-only",
					models: [
						{
							id: "mock",
							name: "压缩验收模型",
							input: ["text"],
							reasoning: false,
							contextWindow: 100000,
							maxTokens: 500,
						},
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	return {
		home,
		pool,
		origin,
		requests,
		modelState,
		settingsFile,
		async conversation() {
			const slot = await pool.createWithKey(randomUUID());
			await slot.session.prompt("检查系统状态");
			await slot.session.prompt("记录检查结果");
			await slot.session.settingsManager.flush();
			return slot;
		},
		send(id: string, key = randomUUID()) {
			return fetch(`${origin}/api/sessions/${id}/compact`, {
				method: "POST",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": key,
				},
				body: JSON.stringify({ instructions: "保留诊断结论" }),
			});
		},
		async close() {
			await api.close();
			await pool.close();
			await new Promise<void>((resolve) => server.close(() => resolve()));
			await new Promise<void>((resolve) => model.close(() => resolve()));
			await rm(home, { recursive: true, force: true });
		},
	};
}
