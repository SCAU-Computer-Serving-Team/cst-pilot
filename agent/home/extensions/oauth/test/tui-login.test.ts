import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
	createAgentSession,
	DefaultResourceLoader,
	type ModelRuntime,
	SessionManager,
} from "@earendil-works/pi-coding-agent";
import { startMockOa } from "./mock-oa.ts";

type AuthEvent = Parameters<Parameters<ModelRuntime["login"]>[2]["notify"]>[0];

for (const mode of ["tui", "rpc"] as const) {
	test(`${mode} 登录：保留授权链接与凭据保存，按通道展示授权信息`, async () => {
		const home = await mkdtemp(join(tmpdir(), "cstoa-login-mode-"));
		const previousHome = process.env.PI_CODING_AGENT_DIR;
		const previousHost = process.env.CSTOA_OA_HOST;
		const mock = await startMockOa();
		process.env.PI_CODING_AGENT_DIR = home;
		process.env.CSTOA_OA_HOST = mock.host;
		const loader = new DefaultResourceLoader({
			cwd: home,
			agentDir: home,
			noExtensions: true,
			noSkills: true,
			noContextFiles: true,
			additionalExtensionPaths: [fileURLToPath(new URL("../index.ts", import.meta.url))],
		});
		try {
			await loader.reload();
			assert.deepEqual(loader.getExtensions().errors, []);
			const { session } = await createAgentSession({
				cwd: home,
				agentDir: home,
				resourceLoader: loader,
				sessionManager: SessionManager.inMemory(home),
				tools: [],
			});
			try {
				await session.bindExtensions({ mode });
				const events: AuthEvent[] = [];
				const credential = await session.modelRuntime.login("cstoa", "oauth", {
					prompt: async () => {
						throw new Error("CSTOA 设备授权无需在终端输入验证码");
					},
					notify: (event) => events.push(event),
				});
				const url = `${mock.host}/#/oauth/device?code=123456`;
				if (mode === "tui") {
					assert.equal(
						events.some((event) => event.type === "device_code"),
						false,
						"TUI 不显示 Enter code",
					);
					assert.equal(
						events.some((event) => event.type === "auth_url"),
						false,
						"不自动打开机主浏览器",
					);
					const info = events.find((event) => event.type === "info");
					assert.ok(info);
					assert.match(info.message, /手机/);
					assert.equal(info.links?.[0]?.url, url);
				} else {
					const device = events.find((event) => event.type === "device_code");
					assert.ok(device, "Web 保留设备码事件以生成二维码和倒计时");
					assert.equal(device.userCode, "123456");
					assert.equal(device.verificationUri, url);
					assert.equal(device.expiresInSeconds, 60);
				}
				assert.equal(credential.type, "oauth");
				const stored = JSON.parse(await readFile(join(home, "auth.json"), "utf8"));
				assert.equal(stored.cstoa.access, "access-1");

				const controller = new AbortController();
				await assert.rejects(
					session.modelRuntime.login("cstoa", "oauth", {
						signal: controller.signal,
						prompt: async () => {
							throw new Error("取消登录不应触发输入");
						},
						notify: () => controller.abort(),
					}),
					(error: unknown) => error === controller.signal.reason,
				);
				assert.deepEqual(JSON.parse(await readFile(join(home, "auth.json"), "utf8")), stored);
			} finally {
				session.dispose();
			}
		} finally {
			if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
			else process.env.PI_CODING_AGENT_DIR = previousHome;
			if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
			else process.env.CSTOA_OA_HOST = previousHost;
			await mock.close();
			await rm(home, { recursive: true, force: true });
		}
	});
}
