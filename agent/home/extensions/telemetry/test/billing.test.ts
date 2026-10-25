import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createAgentSessionServices } from "@earendil-works/pi-coding-agent";
import { observeRequests, requestFetch } from "../../oauth/request-observer.ts";
import { testRoot } from "../../web/test/support.ts";
import { billingLookup, CstoaBilling } from "../billing.ts";
import { newState, onTurnEnd } from "../collect.ts";
import { recoverDrafts, writeDraft } from "../draft.ts";
import { buildRecord } from "../record.ts";

test("只向固定 OA 查询账单，过期不查、不刷新，APIKEY 可读且不上传请求 ID", async () => {
	const home = await mkdtemp(join(await testRoot(), "cstoa-billing-auth-"));
	let calls = 0;
	const lookup = billingLookup(home, async (input, init) => {
		calls++;
		assert.equal(String(input), "https://www.cstoa.top/api/agent/llm/billing");
		assert.equal(new Headers(init?.headers).get("authorization"), "Bearer fixture-key");
		return Response.json({
			result: true,
			data: {
				unit: "quota_point",
				requests: [
					{ requestId: "owned", quotaPoints: 198 },
					{ requestId: "unexpected", quotaPoints: 500 },
					{ requestId: "bad", quotaPoints: -1 },
				],
			},
		});
	});
	try {
		const expired = JSON.stringify({ cstoa: { type: "oauth", access: "expired", refresh: "never-use", expires: 1 } });
		await writeFile(join(home, "auth.json"), expired);
		assert.equal((await lookup(["owned"])).size, 0);
		assert.equal(calls, 0);
		assert.equal(await readFile(join(home, "auth.json"), "utf8"), expired);
		await writeFile(join(home, "auth.json"), JSON.stringify({ cstoa: { type: "api_key", key: "fixture-key" } }));
		assert.deepEqual(await lookup(["owned", "bad"]), new Map([["owned", 198]]));
		const state = newState("crash-session", "new", "web");
		state.prompts = 1;
		const billing = new CstoaBilling();
		billing.start()("owned");
		state.cstoaBilling = billing;
		await writeDraft(home, state.sessionId, {
			record: buildRecord(state, { endReason: "", endedAt: Date.now(), contextEntries: undefined }, "test", null),
			lastTurnEndedAt: Date.now(),
			cstoaRequests: billing.snapshot(),
		});
		const records = await recoverDrafts(
			home,
			async (record) => {
				assert.equal(record.recordId, state.recordId);
				assert.equal(record.endReason, "crash");
				assert.deepEqual(record.cstoaBilling, { requests: 1, settledRequests: 1, quotaPoints: 198 });
				assert.equal(JSON.stringify(record).includes("owned"), false);
			},
			async (record, draft) => {
				const restored = new CstoaBilling(draft.cstoaRequests);
				await restored.settle(lookup);
				return { ...record, cstoaBilling: restored.summary() };
			},
		);
		assert.equal(records.length, 1);
	} finally {
		await rm(home, { recursive: true, force: true });
	}
});

test("SDK 实际 provider 重试逐次保留请求 ID，扩展加载器之间共享订阅", async () => {
	const home = await mkdtemp(join(await testRoot(), "cstoa-billing-sdk-"));
	const billing = new CstoaBilling();
	const stop = observeRequests("sdk-session", () => billing.start());
	try {
		await cp(fileURLToPath(new URL("../../oauth/", import.meta.url)), join(home, "extensions", "oauth"), {
			recursive: true,
		});
		const services = await createAgentSessionServices({ cwd: home, agentDir: home });
		const provider = services.modelRuntime.getProvider("cstoa");
		assert.ok(provider);
		let attempts = 0;
		const output = await provider
			.streamSimple(
				{
					id: "mock",
					name: "mock",
					api: "openai-completions",
					provider: "cstoa",
					baseUrl: "https://www.cstoa.top/v1",
					reasoning: false,
					input: ["text"],
					cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
					contextWindow: 16000,
					maxTokens: 32,
				},
				{ messages: [{ role: "user", content: "fixture", timestamp: Date.now() }] },
				{
					apiKey: "fixture",
					sessionId: "sdk-session",
					maxRetries: 1,
					maxRetryDelayMs: 1,
					fetch: async () => {
						attempts++;
						if (attempts === 1)
							return Response.json(
								{ error: { message: "busy" } },
								{ status: 429, headers: { "retry-after-ms": "0", "x-oneapi-request-id": "rid-retry" } },
							);
						return new Response(
							'data: {"id":"fixture","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}\n\ndata: [DONE]\n\n',
							{ headers: { "content-type": "text/event-stream", "x-oneapi-request-id": "rid-success" } },
						);
					},
				},
			)
			.result();
		assert.equal(output.stopReason, "stop");
		assert.equal(attempts, 2);
		assert.deepEqual(
			billing.snapshot().map((item) => item.requestId),
			["rid-retry", "rid-success"],
		);
	} finally {
		stop();
		await rm(home, { recursive: true, force: true });
	}
});

test("CSTOA 零价格占位保持未定价，中断 usage 保留不完整标记", () => {
	const state = newState("session", "new", "web");
	onTurnEnd(
		state,
		{
			role: "assistant",
			provider: "cstoa",
			model: "mock",
			stopReason: "aborted",
			usage: { totalTokens: 0, cost: { total: 0 } },
		},
		undefined,
		{ cstoa: "CNY" },
	);
	const value = buildRecord(state, { endReason: "quit", endedAt: Date.now(), contextEntries: 1 }, "test", false);
	assert.equal(value.models[0].cost, null);
	assert.equal(value.models[0].incompleteUsageTurns, 1);
});

test("每个 HTTP 尝试单独采集，重试、网络失败和并行会话互不归错", async () => {
	const a = new CstoaBilling();
	const b = new CstoaBilling();
	const offA = observeRequests("a", () => a.start());
	const offB = observeRequests("b", () => b.start());
	let calls = 0;
	const fetcher: typeof fetch = async () => {
		calls++;
		if (calls === 3) throw new Error("offline");
		return new Response("", { status: calls === 1 ? 429 : 200, headers: { "x-oneapi-request-id": `rid-${calls}` } });
	};
	try {
		await requestFetch("a", fetcher)("https://www.cstoa.top/v1/chat/completions");
		await requestFetch("a", fetcher)("https://www.cstoa.top/v1/chat/completions");
		await assert.rejects(requestFetch("b", fetcher)("https://www.cstoa.top/v1/chat/completions"));
		assert.deepEqual(a.summary(), { requests: 2, settledRequests: 0, quotaPoints: null });
		assert.deepEqual(b.summary(), { requests: 1, settledRequests: 0, quotaPoints: null });
	} finally {
		offA();
		offB();
	}
	assert.equal(requestFetch("a", fetcher), fetcher);
});

test("账单延迟、真实零、重复 ID、查询失败和草稿恢复保留覆盖数量", async () => {
	const billing = new CstoaBilling();
	billing.start()("rid-1");
	billing.start()("rid-2");
	billing.start()("rid-2");
	billing.start()();
	let complete = false;
	const lookup = async (ids: string[]) => new Map(ids.map((id) => [id, id === "rid-1" ? 198 : complete ? 0 : null]));
	await billing.settle(lookup);
	assert.deepEqual(billing.summary(), { requests: 3, settledRequests: 1, quotaPoints: 198 });
	const restored = new CstoaBilling(billing.snapshot());
	await restored.settle(async () => {
		throw new Error("offline");
	});
	assert.deepEqual(restored.summary(), billing.summary());
	complete = true;
	await restored.settle(lookup);
	assert.deepEqual(restored.summary(), { requests: 3, settledRequests: 2, quotaPoints: 198 });
});
