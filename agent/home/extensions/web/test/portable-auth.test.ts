import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createAgentSessionServices } from "@earendil-works/pi-coding-agent";
import { type MockOptions, startMockOa } from "../../oauth/test/mock-oa.ts";
import { CstoaAccount } from "../server/api/account.ts";
import { testRoot } from "./support.ts";

async function portableLogin(options: MockOptions, run: (home: string, target: string) => Promise<void>) {
	const root = await mkdtemp(join(await testRoot(), "portable-auth-"));
	const home = join(root, "first-computer", "agent", "home");
	const target = join(root, "other-computer", "agent", "home");
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	const previousHost = process.env.CSTOA_OA_HOST;
	const mock = await startMockOa(options);
	try {
		await cp(fileURLToPath(new URL("../../oauth/", import.meta.url)), join(home, "extensions", "oauth"), {
			recursive: true,
		});
		await writeFile(join(home, "settings.json"), "{}");
		process.env.PI_CODING_AGENT_DIR = home;
		process.env.CSTOA_OA_HOST = mock.host;
		const { modelRuntime } = await createAgentSessionServices({
			cwd: home,
			agentDir: home,
			resourceLoaderOptions: { noContextFiles: true },
		});
		await modelRuntime.login("cstoa", "oauth", {
			prompt: async () => {
				throw new Error("设备码登录不应请求输入");
			},
			notify: () => {},
		});
		await cp(home, target, { recursive: true });
		process.env.PI_CODING_AGENT_DIR = target;
		await run(home, target);
	} finally {
		await mock.close();
		await rm(root, { recursive: true, force: true });
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previousHost;
	}
}

test("工具包移动后新运行时复用凭据和设备标识，无刷新令牌时按实际过期时间使用", async () => {
	await portableLogin({ refreshToken: "", expiresInSeconds: 240 }, async (home, target) => {
		const original = await readFile(join(home, "auth.json"), "utf8");
		const { modelRuntime } = await createAgentSessionServices({
			cwd: target,
			agentDir: target,
			resourceLoaderOptions: { noContextFiles: true },
		});
		const account = await new CstoaAccount().getAccount(modelRuntime);
		assert.equal(account.signedIn, true);
		assert.equal(account.requiresLogin, false, "未到 expires 的凭据移动后仍可用");
		assert.equal(account.profile.studentId, "20230001");
		assert.equal(await readFile(join(target, "auth.json"), "utf8"), original);
		assert.equal(
			await readFile(join(target, "device.json"), "utf8"),
			await readFile(join(home, "device.json"), "utf8"),
		);
	});
});

test("移动后访问令牌到期，刷新服务暂不可用时提示重试，恢复后自动续期", async () => {
	const options: MockOptions = {};
	await portableLogin(options, async (_home, target) => {
		await writeFile(
			join(target, "auth.json"),
			JSON.stringify({ cstoa: { type: "oauth", access: "access-1", refresh: "refresh-1", expires: 0 } }),
		);
		const { modelRuntime } = await createAgentSessionServices({
			cwd: target,
			agentDir: target,
			resourceLoaderOptions: { noContextFiles: true },
		});
		const original = await readFile(join(target, "auth.json"), "utf8");
		const service = new CstoaAccount();
		options.refreshStatus = 503;
		const unavailable = await service.getAccount(modelRuntime);
		assert.equal(unavailable.signedIn, true);
		assert.equal(unavailable.requiresLogin, false, "刷新暂时失败不能判为凭据失效");
		assert.match(unavailable.profile.reason ?? "", /重试/);
		assert.equal(await readFile(join(target, "auth.json"), "utf8"), original);
		options.refreshStatus = 200;
		const recovered = await service.getAccount(modelRuntime);
		assert.equal(recovered.requiresLogin, false);
		assert.equal(recovered.profile.studentId, "20230001");
		const stored = JSON.parse(await readFile(join(target, "auth.json"), "utf8"));
		assert.equal(stored.cstoa.access, "access-2");
		assert.equal(stored.cstoa.refresh, "refresh-2");
	});
});

test("移动后真实到期且无刷新令牌或刷新授权已失效时仍要求登录", async () => {
	await portableLogin({}, async (_home, target) => {
		const { modelRuntime } = await createAgentSessionServices({
			cwd: target,
			agentDir: target,
			resourceLoaderOptions: { noContextFiles: true },
		});
		for (const refresh of ["", "stale"]) {
			await writeFile(
				join(target, "auth.json"),
				JSON.stringify({ cstoa: { type: "oauth", access: "access-1", refresh, expires: 0 } }),
			);
			const account = await new CstoaAccount().getAccount(modelRuntime);
			assert.equal(account.signedIn, true);
			assert.equal(account.requiresLogin, true);
			assert.equal(account.profile.studentId, null);
		}
	});
});
