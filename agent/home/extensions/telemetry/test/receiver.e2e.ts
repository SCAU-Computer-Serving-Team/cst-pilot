import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { RELEASE_SETTINGS, RELEASE_TELEMETRY } from "../../../../../pack/release-settings.mjs";
import { WebSessionPool } from "../../web/server/session/sessions.ts";
import { freePort } from "../../web/test/browser.ts";
import { testRoot } from "../../web/test/support.ts";
import { newState } from "../collect.ts";
import { flushEndpoints } from "../delivery.ts";
import { loadInstallation } from "../installation.ts";
import { appendRecord, readAll } from "../outbox.ts";
import { buildRecord } from "../record.ts";
import { sendBatch } from "../transport.ts";

const binary = process.env.CST_TELEMETRY_RECEIVER;

test(
	"发行遥测自动加载：匿名双端入库、失败补发、登录归属与环境快照",
	{
		skip: !binary,
		timeout: 60_000,
	},
	async () => {
		assert.ok(binary);
		const root = await mkdtemp(join(await testRoot(), "cst-telemetry-receiver-"));
		const home = join(root, "home");
		await mkdir(home);
		await cp(fileURLToPath(new URL("../", import.meta.url)), join(home, "extensions", "telemetry"), {
			recursive: true,
			filter: (file) => !file.replaceAll("\\", "/").includes("/test"),
		});
		const ports = await Promise.all([freePort(), freePort(), freePort(), freePort()]);
		const endpoints = ports.slice(0, 2).map((port) => ({ url: `http://127.0.0.1:${port}/v1/sessions` }));
		await writeFile(join(home, "telemetry.json"), JSON.stringify({ ...RELEASE_TELEMETRY, endpoints }));
		// Only external services and package resolution differ from a release; Pi discovers the real extension.
		await writeFile(
			join(home, "settings.json"),
			JSON.stringify({
				...RELEASE_SETTINGS,
				packages: [],
				defaultProvider: "fixture",
				defaultModel: "one",
				retry: { enabled: false },
				compaction: { enabled: false },
			}),
		);
		await writeFile(
			join(home, "models.json"),
			JSON.stringify({
				providers: {
					fixture: {
						baseUrl: `http://127.0.0.1:${ports[3]}/v1`,
						api: "openai-completions",
						apiKey: "fixture-only",
						models: [
							{
								id: "one",
								name: "one",
								reasoning: false,
								input: ["text"],
								contextWindow: 16000,
								maxTokens: 100,
							},
						],
					},
				},
			}),
		);
		let introspections = 0;
		const oa = createServer(async (request, response) => {
			assert.equal(request.headers.authorization, "Bearer test-service-token");
			let body = "";
			for await (const chunk of request) body += chunk;
			introspections++;
			const active = JSON.parse(body).token === "test-agent-token";
			response.setHeader("Content-Type", "application/json");
			response.end(
				JSON.stringify(
					active
						? { active: true, mid: "test-member", device_id: "test-device" }
						: { active: false, reason: "invalid" },
				),
			);
		});
		const model = createServer(async (request, response) => {
			for await (const _ of request) {
				/* consume request */
			}
			response.writeHead(200, { "Content-Type": "text/event-stream" });
			const frames = [
				{
					id: "fixture",
					choices: [{ index: 0, delta: { role: "assistant", content: "fixture reply" }, finish_reason: null }],
				},
				{
					id: "fixture",
					choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
					usage: { prompt_tokens: 5, completion_tokens: 3, total_tokens: 8 },
				},
			];
			response.end(`${frames.map((frame) => `data: ${JSON.stringify(frame)}\n\n`).join("")}data: [DONE]\n\n`);
		});
		await Promise.all([
			new Promise<void>((resolve) => oa.listen(ports[2], "127.0.0.1", resolve)),
			new Promise<void>((resolve) => model.listen(ports[3], "127.0.0.1", resolve)),
		]);
		const children: ReturnType<typeof spawn>[] = [];
		const previousHome = process.env.PI_CODING_AGENT_DIR;
		const previousOffline = process.env.PI_OFFLINE;
		let pool: WebSessionPool | undefined;
		const health = async (index: number): Promise<{ ok: boolean; sessions: number }> =>
			(await fetch(`http://127.0.0.1:${ports[index]}/healthz`)).json();
		const until = async (predicate: () => Promise<boolean>, label: string) => {
			for (let i = 0; i < 120; i++) {
				if (await predicate().catch(() => false)) return;
				await new Promise((resolve) => setTimeout(resolve, 50));
			}
			assert.fail(label);
		};
		const startReceiver = async (index: number) => {
			const child = spawn(binary, ["serve"], {
				cwd: root,
				env: {
					...process.env,
					TELEMETRY_ALLOW_STUB: "0",
					TELEMETRY_HOST: "127.0.0.1",
					TELEMETRY_PORT: String(ports[index]),
					TELEMETRY_DB: join(root, `receiver-${index}.db`),
					OA_INTROSPECT_URL: `http://127.0.0.1:${ports[2]}/api/oauth/introspect`,
					OA_SERVICE_TOKEN: "test-service-token",
				},
				stdio: ["ignore", "pipe", "pipe"],
			});
			children.push(child);
			let logs = "";
			child.stdout?.on("data", (data) => {
				logs += String(data);
			});
			child.stderr?.on("data", (data) => {
				logs += String(data);
			});
			child.on("error", (error) => {
				logs += error.message;
			});
			await until(async () => (await health(index)).ok, `接收端未就绪：${logs}`);
			return child;
		};
		try {
			for (let index = 0; index < 2; index++) await startReceiver(index);
			process.env.PI_CODING_AGENT_DIR = home;
			process.env.PI_OFFLINE = "1";
			const options = {
				cwd: home,
				agentDir: home,
				sessionDir: join(home, "sessions"),
				withLoader: <T>(load: () => Promise<T>) => load(),
			};
			pool = new WebSessionPool(options);
			let slot = await pool.create();
			await slot.session.prompt("未登录遥测测试");
			await pool.close();
			pool = undefined;
			await until(
				async () => (await health(0)).sessions === 1 && (await health(1)).sessions === 1,
				"未登录未自动上传",
			);
			await until(
				async () =>
					(await readAll(home, endpoints[0].url)).length === 0 &&
					(await readAll(home, endpoints[1].url)).length === 0,
				"匿名队列未确认",
			);
			// 模拟电脑 A 上留下的待发记录，在电脑 B 启动后补发，环境不得重算。
			const installation = await loadInstallation(home);
			assert.ok(installation);
			const csvFile = join(root, "anonymous.csv");
			execFileSync(binary, ["export-csv", csvFile], {
				env: { ...process.env, TELEMETRY_DB: join(root, "receiver-0.db") },
			});
			const firstCsv = await readFile(csvFile, "utf8");
			assert.ok(firstCsv.includes(installation.installationId));
			assert.ok(firstCsv.includes(process.versions.node));
			assert.ok(!firstCsv.includes("test-member"));
			const offline = buildRecord(
				newState(crypto.randomUUID(), "new", "web"),
				{ endReason: "quit", endedAt: Date.now(), contextEntries: 2 },
				"offline-fixture",
				null,
			);
			offline.identity = {
				installationId: installation.installationId,
				environmentId: "environment-on-computer-a",
				environmentSource: "windows-machine-guid",
			};
			offline.os.name = "Offline Windows fixture";
			await appendRecord(home, offline, endpoints[1].url);
			const pending = await readAll(home, endpoints[1].url);
			assert.equal(pending.length, 1, "隔离的环境 A 待发记录应只进入对应端点");
			assert.equal((await readAll(home, endpoints[0].url)).length, 0);
			assert.equal(pending[0].channel, "web");
			assert.equal((await health(0)).sessions, 1);
			assert.equal((await health(1)).sessions, 1);
			assert.equal(introspections, 0, "匿名上传不依赖 OA");
			await writeFile(
				join(home, "auth.json"),
				JSON.stringify({
					cstoa: {
						type: "oauth",
						access: "test-agent-token",
						refresh: "test-refresh",
						expires: Date.now() + 60_000,
					},
				}),
			);
			pool = new WebSessionPool(options);
			slot = await pool.create();
			await until(
				async () => (await health(0)).sessions === 1 && (await health(1)).sessions === 2,
				"新会话启动未独立补发缓存",
			);
			await slot.session.prompt("已登录遥测测试");
			await pool.close();
			pool = undefined;
			await until(
				async () => (await health(0)).sessions === 2 && (await health(1)).sessions === 3,
				"两端未收到定稿",
			);
			await until(
				async () =>
					(await Promise.all(endpoints.map((endpoint) => readAll(home, endpoint.url)))).every(
						(records) => records.length === 0,
					),
				"两端未独立确认清空队列",
			);
			for (const [index, endpoint] of endpoints.entries()) {
				const csvFile = join(root, `receiver-${index}.csv`);
				execFileSync(binary, ["export-csv", csvFile], {
					env: { ...process.env, TELEMETRY_DB: join(root, `receiver-${index}.db`) },
					timeout: 5000,
				});
				const csv = await readFile(csvFile, "utf8");
				assert.equal(csv.trim().split("\n").length, index === 0 ? 3 : 4);
				assert.ok(csv.includes(",test-member,test-device,"), "上传账号来自 OA 内省");
				if (index === 1) {
					assert.ok(csv.includes("environment-on-computer-a"));
					assert.ok(csv.includes("Offline Windows fixture"));
					assert.equal((await sendBatch(endpoint.url, "test-agent-token", pending)).kind, "accepted");
					assert.equal((await sendBatch(endpoint.url, installation, pending)).kind, "accepted");
				}
				assert.equal((await health(index)).sessions, index === 0 ? 2 : 3, "重传不能重复入库或更新归属");
			}
			assert.equal((await sendBatch(endpoints[0].url, "invalid-test-token", pending)).kind, "auth");
			assert.equal((await health(0)).sessions, 2, "无效令牌不能新增记录");
			// 一端不可达时，另一端仍能完成确认。
			const unavailable = { url: "http://127.0.0.1:1" };
			await appendRecord(home, offline, endpoints[0].url);
			await appendRecord(home, offline, unavailable.url);
			await flushEndpoints(home, installation, [unavailable, endpoints[0]]);
			assert.equal((await health(0)).sessions, 3);
			assert.equal((await readAll(home, unavailable.url)).length, 1);
			assert.equal((await readAll(home, endpoints[0].url)).length, 0);
			// 接收端断开后恢复，不再派发会话事件，验证 30 秒定时补发。
			const exited = once(children[0], "exit");
			children[0].kill();
			await exited;
			await writeFile(
				join(home, "auth.json"),
				JSON.stringify({ cstoa: { access: "invalid-but-not-expired", expires: Date.now() + 60_000 } }),
			);
			const retry = { ...offline, recordId: crypto.randomUUID() };
			await appendRecord(home, retry, endpoints[0].url);
			await flushEndpoints(home, installation, [endpoints[0]]);
			assert.equal((await readAll(home, endpoints[0].url)).length, 1);
			await startReceiver(0);
			for (let i = 0; i < 350 && (await readAll(home, endpoints[0].url)).length; i++)
				await new Promise((resolve) => setTimeout(resolve, 100));
			assert.equal((await readAll(home, endpoints[0].url)).length, 0, "没有新会话事件时也应补发");
			assert.equal((await health(0)).sessions, 4);
			execFileSync(binary, ["export-csv", csvFile], {
				env: { ...process.env, TELEMETRY_DB: join(root, "receiver-0.db") },
			});
			assert.ok(
				(await readFile(csvFile, "utf8")).includes(`${retry.recordId},,,`),
				"OA 凭据被拒后应降级安装签名，上传者为空",
			);
		} finally {
			await pool?.close();
			for (const child of children) {
				if (child.exitCode !== null || child.signalCode !== null) continue;
				const exited = once(child, "exit");
				child.kill();
				await exited;
			}
			for (const server of [oa, model]) {
				server.closeAllConnections();
				await new Promise<void>((resolve) => server.close(() => resolve()));
			}
			if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
			else process.env.PI_CODING_AGENT_DIR = previousHome;
			if (previousOffline === undefined) delete process.env.PI_OFFLINE;
			else process.env.PI_OFFLINE = previousOffline;
			await rm(root, { recursive: true, force: true });
		}
	},
);
