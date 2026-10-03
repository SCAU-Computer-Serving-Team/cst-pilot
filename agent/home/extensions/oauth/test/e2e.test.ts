/**
 * OA 登录扩展 e2e：pi 内核真实加载扩展，走完整设备流并把凭据写入 auth.json。
 * 依赖 agent/node_modules 里的 pi（npm ci 后可用）。
 */

import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { afterEach, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createAgentSessionServices } from "@earendil-works/pi-coding-agent";
import { startMockOa } from "./mock-oa.ts";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = join(here, "..");

let agentDir: string | undefined;
let previousAgentDir: string | undefined;

afterEach(async () => {
	delete process.env.CSTOA_OA_HOST;
	if (previousAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
	else process.env.PI_CODING_AGENT_DIR = previousAgentDir;
	previousAgentDir = undefined;
	if (agentDir) await rm(agentDir, { recursive: true, force: true }).catch(() => undefined);
	agentDir = undefined;
});

test("e2e：内核加载扩展完成设备流，凭据写入 auth.json 且模型清单就绪", async () => {
	agentDir = await mkdtemp(join(tmpdir(), "cstoa-oauth-e2e-"));
	await mkdir(join(agentDir, "extensions"), { recursive: true });
	await cp(extensionDir, join(agentDir, "extensions", "oauth"), { recursive: true });
	// 发行版由 pi.cmd 设置该变量；测试显式指向隔离目录，保证 device.json 不落到真实配置。
	previousAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;

	const mock = await startMockOa();
	process.env.CSTOA_OA_HOST = mock.host;
	try {
		const services = await createAgentSessionServices({ cwd: process.cwd(), agentDir });
		const provider = services.modelRuntime.getProvider("cstoa");
		assert.ok(provider, "cstoa provider 应已注册");
		assert.equal(provider.getModels().length, 4);

		const events: unknown[] = [];
		const credential = await services.modelRuntime.login("cstoa", "oauth", {
			prompt: async () => {
				throw new Error("cstoa 设备流不应触发交互输入");
			},
			notify: (event) => {
				events.push(event);
			},
		});
		assert.equal(credential.type, "oauth");
		if (credential.type !== "oauth") throw new Error("unreachable");
		assert.equal(credential.access, "access-1");
		assert.equal(credential.refresh, "refresh-1");

		const deviceCode = events.find((event) => (event as { type?: string }).type === "device_code") as
			| { userCode?: string; verificationUri?: string }
			| undefined;
		assert.ok(deviceCode, "应收到 device_code 通知");
		assert.equal(deviceCode.userCode, "123456");
		assert.equal(deviceCode.verificationUri, `${mock.host}/oauth/device?user_code=123456`);

		const stored = JSON.parse(await readFile(join(agentDir, "auth.json"), "utf8")) as {
			cstoa?: { type?: string; access?: string; refresh?: string };
		};
		assert.equal(stored.cstoa?.type, "oauth");
		assert.equal(stored.cstoa?.access, "access-1");
		assert.equal(stored.cstoa?.refresh, "refresh-1");
		// 设备标识与凭据使用同一个隔离目录。
		const device = JSON.parse(await readFile(join(agentDir, "device.json"), "utf8")) as { device_id?: string };
		assert.match(device.device_id ?? "", /^dev_[0-9a-f]{32}$/);

		const requestAuth = await provider.auth.oauth?.toAuth(credential);
		assert.equal(requestAuth?.apiKey, "access-1");
	} finally {
		await mock.close();
	}
});
