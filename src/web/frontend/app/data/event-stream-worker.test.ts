import assert from "node:assert/strict";
import { test } from "node:test";
import {
	type EventSourceLike,
	EventStreamCoordinator,
	type PortLike,
	parseVectorCursor,
	type WorkerOutgoingMessage,
} from "./event-stream-worker.ts";

class FakeEventSource implements EventSourceLike {
	readonly url: string;
	onopen: ((event: Event) => void) | null = null;
	onerror: ((event: Event) => void) | null = null;
	closed = false;
	readyState = 0;
	private readonly listeners = new Map<string, Set<(event: Event) => void>>();

	constructor(url: string) {
		this.url = url;
	}

	addEventListener(type: string, listener: (event: Event) => void): void {
		let set = this.listeners.get(type);
		if (!set) {
			set = new Set();
			this.listeners.set(type, set);
		}
		set.add(listener);
	}

	close(): void {
		this.closed = true;
		this.readyState = 2;
	}

	emit(type: string, data: unknown, lastEventId = ""): void {
		const evt = new MessageEvent(type, {
			data: typeof data === "string" ? data : JSON.stringify(data),
			lastEventId,
		});
		for (const listener of this.listeners.get(type) ?? []) {
			listener(evt);
		}
	}
}

function createFakePort() {
	const messages: WorkerOutgoingMessage[] = [];
	const port: PortLike = {
		postMessage(msg) {
			messages.push(msg);
		},
	};
	return { port, messages };
}

test("parseVectorCursor 提取多源向量游标", () => {
	const map = parseVectorCursor("global:e0:12,s1:e1:5,s2:e2:99");
	assert.equal(map.get("global"), "e0:12");
	assert.equal(map.get("s1"), "e1:5");
	assert.equal(map.get("s2"), "e2:99");
});

test("只读能力协商：无 multiplexStream 回退 legacy，5xx 退避重试，先建 SSE 待 stream_ready 后再发 PUT", async () => {
	const sources: FakeEventSource[] = [];
	const requests: { url: string; method: string; body?: unknown }[] = [];
	let stateResponse: () => Response = () =>
		new Response(JSON.stringify({ stage: "ready", sessions: [] }), { status: 200 });

	const legacyCoordinator = new EventStreamCoordinator({
		debounceMs: 0,
		createEventSource: (url) => {
			const s = new FakeEventSource(url);
			sources.push(s);
			return s;
		},
		fetchImpl: async (input, init) => {
			const url = String(input);
			requests.push({ url, method: init?.method ?? "GET" });
			return stateResponse();
		},
	});

	const p1 = createFakePort();
	legacyCoordinator.updatePort(p1.port, true, ["s1"]);
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(legacyCoordinator.currentMode, "legacy");
	assert.equal(sources.length, 0, "legacy 模式不在协调器内创建聚合 EventSource");
	assert.equal(
		requests.some((r) => r.method === "PUT"),
		false,
		"能力协商不发送带会话副作用的 PUT",
	);

	// 503 on /api/state retries via backoff instead of falling back to legacy
	let stateCalls = 0;
	stateResponse = () => {
		stateCalls++;
		if (stateCalls === 1) return new Response(null, { status: 503 });
		return new Response(
			JSON.stringify({ stage: "ready", channelMode: "web_active", multiplexStream: true, sessions: [] }),
			{ status: 200 },
		);
	};

	const muxCoordinator = new EventStreamCoordinator({
		debounceMs: 0,
		initialBackoffMs: 15,
		maxBackoffMs: 50,
		createEventSource: (url) => {
			const s = new FakeEventSource(url);
			sources.push(s);
			return s;
		},
		fetchImpl: async (input, init) => {
			const url = String(input);
			const method = init?.method ?? "GET";
			const body = init?.body ? JSON.parse(String(init.body)) : undefined;
			requests.push({ url, method, body });
			if (url === "/api/state") return stateResponse();
			return new Response(JSON.stringify({ subscribedSessions: body?.sessions ?? [] }), { status: 200 });
		},
	});

	requests.length = 0;
	const p2 = createFakePort();
	muxCoordinator.updatePort(p2.port, true, ["s1"]);
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(muxCoordinator.currentMode, "unprobed", "503 不误判为 legacy");
	await new Promise((r) => setTimeout(r, 30));
	assert.equal(muxCoordinator.currentMode, "multiplex");
	assert.equal(sources.length, 1);
	assert.equal(requests.filter((r) => r.method === "PUT").length, 0, "收到 stream_ready 前不发送首次 PUT");

	sources[0]!.emit("state", { type: "stream_ready", subscribedSessions: [] }, "global:e0:1");
	await new Promise((r) => setTimeout(r, 10));
	const puts = requests.filter((r) => r.method === "PUT");
	assert.equal(puts.length, 1);
	assert.deepEqual(puts[0]!.body, { sessions: ["s1"], lastEventIds: {} });
});

test("已关闭的 SSE 退避重建并恢复所有端口，原生重连中的连接不重复创建", async () => {
	const sources: FakeEventSource[] = [];
	let stateCalls = 0;
	let appActive = false;
	const coordinator = new EventStreamCoordinator({
		debounceMs: 0,
		initialBackoffMs: 10,
		maxBackoffMs: 20,
		createEventSource: (url) => {
			const source = new FakeEventSource(url);
			sources.push(source);
			return source;
		},
		fetchImpl: async (input, init) => {
			if (String(input) === "/api/state") {
				stateCalls++;
				return Response.json({ stage: appActive ? "app_active" : "ready", multiplexStream: true });
			}
			const body = JSON.parse(String(init?.body)) as { sessions: string[] };
			return Response.json({ subscribedSessions: body.sessions });
		},
	});
	const tabA = createFakePort();
	const tabB = createFakePort();
	coordinator.updatePort(tabA.port, true, ["s1"]);
	coordinator.updatePort(tabB.port, false, ["s1"]);
	try {
		await new Promise((r) => setTimeout(r, 5));
		const first = sources[0]!;
		first.emit("state", { type: "stream_ready", subscribedSessions: [] });
		await new Promise((r) => setTimeout(r, 5));

		first.readyState = 0;
		first.onerror?.(new Event("error"));
		await new Promise((r) => setTimeout(r, 30));
		assert.equal(sources.length, 1, "CONNECTING 状态由原生 EventSource 恢复");

		first.readyState = 2;
		first.onerror?.(new Event("error"));
		await new Promise((r) => setTimeout(r, 30));
		assert.equal(sources.length, 2, "CLOSED 状态按退避重新建连");
		assert.equal(stateCalls, 2, "重建前重新检查通道状态");
		assert.equal(first.closed, true);
		const recovered = sources[1]!;
		assert.equal(recovered.url, first.url, "保留 clientId 以承接原订阅");
		tabA.messages.length = 0;
		tabB.messages.length = 0;
		recovered.onopen?.(new Event("open"));
		recovered.emit("state", { type: "stream_ready", subscribedSessions: ["s1"] });
		await new Promise((r) => setTimeout(r, 5));
		for (const tab of [tabA, tabB]) {
			assert.ok(tab.messages.some((m) => m.type === "session" && m.name === "open"));
		}
		recovered.emit("session", { sessionId: "s1", event: { type: "agent_end" } });
		assert.ok(tabB.messages.some((m) => m.type === "session" && m.name === "session"));

		appActive = true;
		recovered.readyState = 2;
		recovered.onerror?.(new Event("error"));
		await new Promise((r) => setTimeout(r, 30));
		assert.equal(coordinator.currentMode, "app_active");
		assert.equal(sources.length, 2, "软件端活跃时停止重建");
	} finally {
		coordinator.removePort(tabA.port);
		coordinator.removePort(tabB.port);
	}
});

test("分片销毁后取消 SSE 重建", async () => {
	const sources: FakeEventSource[] = [];
	const coordinator = new EventStreamCoordinator({
		initialBackoffMs: 10,
		createEventSource: (url) => {
			const source = new FakeEventSource(url);
			sources.push(source);
			return source;
		},
		fetchImpl: async () => Response.json({ stage: "ready", multiplexStream: true }),
	});
	const tab = createFakePort();
	coordinator.updatePort(tab.port, true, []);
	await new Promise((r) => setTimeout(r, 5));
	sources[0]!.readyState = 2;
	sources[0]!.onerror?.(new Event("error"));
	coordinator.removePort(tab.port);
	await new Promise((r) => setTimeout(r, 30));
	assert.equal(sources.length, 1);
	assert.equal(coordinator.activeShardCount, 0);
});

test("单在途订阅合并、握手恢复、业务 404 按会话剔除、失败退避与 32 会话自动分片", async () => {
	const sources: FakeEventSource[] = [];
	const puts: { url: string; key: string; body: { sessions: string[]; lastEventIds: Record<string, string> } }[] = [];
	let releaseFirstPut: (() => void) | undefined;
	const firstPutGate = new Promise<void>((resolve) => {
		releaseFirstPut = resolve;
	});
	let putMode: "gate" | "ok" | "not_found" | "fail_503" | "app_active" = "gate";

	const coordinator = new EventStreamCoordinator({
		debounceMs: 0,
		initialBackoffMs: 40,
		maxBackoffMs: 100,
		createEventSource: (url) => {
			const s = new FakeEventSource(url);
			sources.push(s);
			return s;
		},
		fetchImpl: async (input, init) => {
			const url = String(input);
			if (url === "/api/state") {
				return new Response(JSON.stringify({ stage: "ready", channelMode: "web_active", multiplexStream: true }), {
					status: 200,
				});
			}
			const headers = new Headers(init?.headers);
			const body = JSON.parse(String(init?.body)) as {
				sessions: string[];
				lastEventIds: Record<string, string>;
			};
			puts.push({ url, key: headers.get("Idempotency-Key") ?? "", body });

			if (putMode === "gate") {
				putMode = "ok";
				await firstPutGate;
				return new Response(JSON.stringify({ subscribedSessions: body.sessions }), { status: 200 });
			}
			if (putMode === "not_found" && body.sessions.includes("deleted-sess")) {
				return new Response(
					JSON.stringify({
						error: {
							code: "session_not_found",
							message: "订阅的会话不存在",
							sessionId: "deleted-sess",
						},
					}),
					{ status: 404 },
				);
			}
			if (putMode === "fail_503") {
				return new Response(JSON.stringify({ error: { code: "unavailable", message: "busy" } }), {
					status: 503,
				});
			}
			if (putMode === "app_active") {
				return new Response(JSON.stringify({ error: { code: "web_mode_inactive", message: "locked" } }), {
					status: 403,
				});
			}
			return new Response(JSON.stringify({ subscribedSessions: body.sessions }), { status: 200 });
		},
	});

	const tabA = createFakePort();
	const tabB = createFakePort();
	coordinator.updatePort(tabA.port, true, ["s1"]);
	await new Promise((r) => setTimeout(r, 5));
	sources[0]!.emit("state", { type: "stream_ready", subscribedSessions: [] }, "global:e0:1");
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(puts.length, 1, "第一个 PUT(['s1']) 在途");

	// Rapid updates while first PUT is in flight must coalesce without issuing a 2nd concurrent PUT
	coordinator.updatePort(tabB.port, false, ["s2"]);
	coordinator.updatePort(tabA.port, true, ["s3"]);
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(puts.length, 1, "在途期间不并发发起第二个 PUT");

	releaseFirstPut?.();
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(puts.length, 2, "在途请求返回后立即补发合并后的最新集合");
	assert.notEqual(puts[0]!.key, puts[1]!.key, "追赶请求使用新的 Idempotency-Key");
	assert.deepEqual(puts[1]!.body.sessions.sort(), ["s2", "s3"]);

	// Record vector cursor on SSE event and verify stream_ready recovery sends lastEventIds
	sources[0]!.emit("session", { sessionId: "s2", event: { type: "agent_start" } }, "global:e0:2,s2:ep2:15,s3:ep3:4");
	coordinator.updatePort(tabB.port, false, []);
	await new Promise((r) => setTimeout(r, 10));
	coordinator.updatePort(tabB.port, false, ["s2"]);
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(puts.at(-1)?.body.lastEventIds.s2, "ep2:15");

	// Business 404 session_not_found rejects only deleted-sess without switching to legacy mode
	putMode = "not_found";
	const tabDeleted = createFakePort();
	coordinator.updatePort(tabDeleted.port, false, ["deleted-sess"]);
	await new Promise((r) => setTimeout(r, 15));
	assert.equal(coordinator.currentMode, "multiplex", "业务 404 不触发单流回退");
	assert.equal(
		tabDeleted.messages.some((m) => m.type === "session_not_found" && m.sessionId === "deleted-sess"),
		true,
	);

	// 32-session automatic sharding: 33rd session creates a 2nd shard
	putMode = "ok";
	const manySessions = Array.from({ length: 33 }, (_, i) => `bulk-${i + 1}`);
	coordinator.updatePort(tabA.port, true, manySessions);
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(coordinator.activeShardCount, 2, "第 33 个会话自动分配第 2 个分片");
	coordinator.updatePort(tabA.port, true, manySessions.slice(0, 2));
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(coordinator.activeShardCount, 1, "空闲二级分片自动回收");

	// Transient 503 uses backoff instead of tight loop in finally
	const countBefore503 = puts.length;
	putMode = "fail_503";
	coordinator.updatePort(tabA.port, true, ["s-backoff"]);
	await new Promise((r) => setTimeout(r, 10));
	assert.equal(puts.length, countBefore503 + 1, "503 失败不在 finally 中无间隔连发");

	// 403 web_mode_inactive stops shards and enters app_active standby
	putMode = "app_active";
	sources[0]!.emit("state", { type: "stream_ready", subscribedSessions: [] });
	await new Promise((r) => setTimeout(r, 15));
	assert.equal(coordinator.currentMode, "app_active");
	assert.equal(coordinator.activeShardCount, 0);
});
