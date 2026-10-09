import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { testRoot } from "../../web/test/support.ts";
import { newState } from "../collect.ts";
import { loadConfig } from "../config.ts";
import { flushEndpoints, migratePending, queueRecord } from "../delivery.ts";
import { appendRecord, readAll } from "../outbox.ts";
import { buildRecord, type SessionRecord } from "../record.ts";
import { shared } from "../shared.ts";

const root = await mkdtemp(join(await testRoot(), "cst-telemetry-dual-"));
after(() => rm(root, { recursive: true, force: true }));
async function home(name: string): Promise<string> {
	const dir = join(root, name);
	await mkdir(dir, { recursive: true });
	return dir;
}
function record(): SessionRecord {
	const state = newState(randomUUID(), "new", "web");
	state.prompts = 1;
	return buildRecord(state, { endReason: "quit", endedAt: Date.now(), contextEntries: 1 }, "test", false);
}
async function mock(status = 202, delay = 0) {
	const payloads: SessionRecord[][] = [];
	let active = 0,
		peak = 0;
	const state = { status };
	const server: Server = createServer(async (request, response) => {
		active++;
		peak = Math.max(active, peak);
		let raw = "";
		for await (const chunk of request) raw += chunk;
		payloads.push(JSON.parse(raw).records);
		if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
		active--;
		response.writeHead(state.status);
		response.end("{}");
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	return {
		endpoint: { url: `http://127.0.0.1:${address.port}/v1/sessions` },
		state,
		payloads,
		peak: () => peak,
		close: async () => {
			server.closeAllConnections();
			await new Promise<void>((resolve) => server.close(() => resolve()));
		},
	};
}

test("双端确认独立：一端 503 时另一端完成；重启后只补发失败端", async () => {
	const dir = await home("independent");
	const one = await mock();
	const two = await mock(503);
	try {
		const endpoints = [one.endpoint, two.endpoint];
		const value = record();
		await queueRecord(dir, value, endpoints);
		await flushEndpoints(dir, "test-token", endpoints);
		assert.deepEqual(await readAll(dir, one.endpoint.url), []);
		assert.equal((await readAll(dir, two.endpoint.url))[0].recordId, value.recordId);
		assert.equal(one.payloads.length, 1);
		assert.equal(two.payloads.length, 1);
		shared().deliveries.clear();
		two.state.status = 202;
		await flushEndpoints(dir, "test-token", endpoints);
		assert.equal(one.payloads.length, 1);
		assert.equal(two.payloads.length, 2);
		assert.equal(two.payloads[1][0].recordId, one.payloads[0][0].recordId);
		assert.deepEqual(await readAll(dir, two.endpoint.url), []);
	} finally {
		await one.close();
		await two.close();
	}
});

test("404 停发只作用于对应端点，不影响另一端的新会话", async () => {
	const dir = await home("stop");
	const one = await mock();
	const two = await mock(404);
	try {
		const endpoints = [one.endpoint, two.endpoint];
		await queueRecord(dir, record(), endpoints);
		await flushEndpoints(dir, "test-token", endpoints);
		await queueRecord(dir, record(), endpoints);
		await flushEndpoints(dir, "test-token", endpoints);
		assert.equal(one.payloads.length, 2);
		assert.equal(two.payloads.length, 1);
		assert.equal((await readAll(dir, two.endpoint.url)).length, 2);
		assert.deepEqual(await readAll(dir, one.endpoint.url), []);
	} finally {
		await one.close();
		await two.close();
	}
});

test("单端点队列迁移到两端，迁移重试不会复制同一 recordId", async () => {
	const dir = await home("migration");
	const value = record();
	const endpoints = [{ url: "https://first.example/api" }, { url: "https://second.example/api" }];
	await appendRecord(dir, value);
	await migratePending(dir, endpoints);
	assert.deepEqual(await readAll(dir), []);
	await appendRecord(dir, value);
	await migratePending(dir, endpoints);
	for (const endpoint of endpoints) {
		const values = await readAll(dir, endpoint.url);
		assert.equal(values.length, 1);
		assert.equal(values[0].recordId, value.recordId);
		assert.equal("endpoints" in values[0], false);
	}
});

test("每端点 200 条上限独立，同一记录重写只占一个位置", async () => {
	const dir = await home("bounds");
	const endpoints = [{ url: "https://first.example/" }, { url: "https://second.example/" }];
	const values = Array.from({ length: 202 }, record);
	for (const value of values) await queueRecord(dir, value, endpoints);
	await queueRecord(dir, values[201], endpoints);
	for (const endpoint of endpoints) assert.equal((await readAll(dir, endpoint.url)).length, 200);
});

test("并发触发单飞；发送期间追加的新记录继续发送", async () => {
	const dir = await home("single-flight");
	const target = await mock(202, 80);
	try {
		const first = record(),
			second = record();
		await queueRecord(dir, first, [target.endpoint]);
		const flushing = flushEndpoints(dir, "test-token", [target.endpoint]);
		while (!target.payloads.length) await new Promise((resolve) => setTimeout(resolve, 5));
		await queueRecord(dir, second, [target.endpoint]);
		await Promise.all([
			flushing,
			...Array.from({ length: 5 }, () => flushEndpoints(dir, "test-token", [target.endpoint])),
		]);
		assert.equal(target.peak(), 1);
		assert.deepEqual(
			target.payloads.flat().map((value) => value.recordId),
			[first.recordId, second.recordId],
		);
		assert.deepEqual(await readAll(dir, target.endpoint.url), []);
	} finally {
		await target.close();
	}
});

test("配置支持双端与单端，去重并拒绝明文公网及带账号的 URL", async () => {
	const dir = await home("config");
	await writeFile(
		join(dir, "telemetry.json"),
		JSON.stringify({
			enabled: true,
			endpoints: [
				{ url: "https://www.cstoa.top/api/telemetry" },
				{ url: "https://www.cstoa.top/api/telemetry" },
				{ url: "https://8.163.28.9:8445/api/telemetry", caFile: "timserver_1.crt" },
				{ url: "http://public.example/api" },
				{ url: "https://user:pass@example.test/api" },
			],
		}),
	);
	const extension = new URL("../", import.meta.url);
	const config = await loadConfig(dir, fileURLToPath(extension));
	assert.equal(config?.endpoints.length, 2);
	assert.equal(config?.authProvider, "cstoa");
	assert.ok(config?.endpoints[1].ca?.includes("BEGIN CERTIFICATE"));
	await writeFile(join(dir, "telemetry.json"), JSON.stringify({ endpoint: "https://www.cstoa.top/api/telemetry" }));
	assert.deepEqual((await loadConfig(dir))?.endpoints, [{ url: "https://www.cstoa.top/api/telemetry" }]);
});

test("CA 缺失保留该端点队列；不降级为忽略证书验证", async () => {
	const dir = await home("missing-ca");
	await writeFile(
		join(dir, "telemetry.json"),
		JSON.stringify({ endpoints: [{ url: "https://8.163.28.9:8445/api/telemetry", caFile: "missing.crt" }] }),
	);
	const config = await loadConfig(dir, dir);
	assert.ok(config);
	assert.equal(config.endpoints[0].unavailable, true);
	await queueRecord(dir, record(), config.endpoints);
	await flushEndpoints(dir, "test-token", config.endpoints);
	assert.equal((await readAll(dir, config.endpoints[0].url)).length, 1);
	assert.equal(await readFile(join(dir, "telemetry.json"), "utf8").then((text) => text.includes("test-token")), false);
});
