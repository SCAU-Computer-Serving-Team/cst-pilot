/**
 * OA 登录扩展测试：用本地 mock 服务跑 RFC 8628 设备流的完整路径。
 * 运行：node --test home/extensions/oauth/test/*.test.ts（Node 内置 TS 支持）。
 */

import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";
import { getDeviceId, resetDeviceIdCache } from "../device.ts";
import { login, refresh } from "../oa.ts";
import { startMockOa } from "./mock-oa.ts";

const tempDirs: string[] = [];

async function makeAgentDir(): Promise<string> {
	const dir = await mkdtemp(join(tmpdir(), "cstoa-oauth-"));
	tempDirs.push(dir);
	return dir;
}

afterEach(async () => {
	delete process.env.CSTOA_OA_HOST;
	resetDeviceIdCache();
	await Promise.all(tempDirs.splice(0).map((dir) => rm(dir, { recursive: true, force: true })));
});

test("login：完整设备流返回凭据、通知完整授权链接并生成设备标识", async () => {
	const mock = await startMockOa();
	process.env.CSTOA_OA_HOST = mock.host;
	const agentDir = await makeAgentDir();
	try {
		const events: { userCode: string; verificationUri: string }[] = [];
		const credentials = await login(agentDir, {
			onDeviceCode: (info) => events.push({ userCode: info.userCode, verificationUri: info.verificationUri }),
		});

		assert.equal(credentials.access, "access-1");
		assert.equal(credentials.refresh, "refresh-1");
		assert.ok(credentials.expires > Date.now());
		assert.equal(events.length, 1);
		assert.equal(events[0].userCode, "123456");
		assert.equal(events[0].verificationUri, `${mock.host}/#/oauth/device?code=123456`);

		const deviceFile = JSON.parse(await readFile(join(agentDir, "device.json"), "utf8")) as {
			device_id?: string;
		};
		assert.match(deviceFile.device_id ?? "", /^dev_[0-9a-f]{32}$/);

		const authorization = mock.requests[0];
		assert.equal(authorization.path, "/api/oauth/device_authorization");
		assert.equal(authorization.body.device_id, deviceFile.device_id);
		assert.deepEqual(authorization.body.scope, ["llm:chat"]);
	} finally {
		await mock.close();
	}
});

test("login：授权被拒绝时提示重新发起", async () => {
	const mock = await startMockOa({ outcome: "deny" });
	process.env.CSTOA_OA_HOST = mock.host;
	const agentDir = await makeAgentDir();
	try {
		await assert.rejects(login(agentDir, { onDeviceCode: () => {} }), /授权已被拒绝/);
	} finally {
		await mock.close();
	}
});

test("login：设备码过期时提示重新发起", async () => {
	const mock = await startMockOa({ outcome: "expire" });
	process.env.CSTOA_OA_HOST = mock.host;
	const agentDir = await makeAgentDir();
	try {
		await assert.rejects(login(agentDir, { onDeviceCode: () => {} }), /设备码已过期/);
	} finally {
		await mock.close();
	}
});

test("login：authorization_pending 后继续轮询直到成功", async () => {
	const mock = await startMockOa({ pendingTimes: 1 });
	process.env.CSTOA_OA_HOST = mock.host;
	const agentDir = await makeAgentDir();
	try {
		const credentials = await login(agentDir, { onDeviceCode: () => {} });
		assert.equal(credentials.access, "access-1");
	} finally {
		await mock.close();
	}
});

test("login：slow_down 后放慢间隔并最终成功", async () => {
	const mock = await startMockOa({ slowDownOnce: true });
	process.env.CSTOA_OA_HOST = mock.host;
	const agentDir = await makeAgentDir();
	try {
		const credentials = await login(agentDir, { onDeviceCode: () => {} });
		assert.equal(credentials.access, "access-1");
	} finally {
		await mock.close();
	}
});

test("login：OA 不可达时提示检查网络", async () => {
	process.env.CSTOA_OA_HOST = "http://127.0.0.1:9";
	const agentDir = await makeAgentDir();
	await assert.rejects(login(agentDir, { onDeviceCode: () => {} }), /无法连接 OA 服务器/);
});

test("login：用户取消时抛出内核约定的取消文案", async () => {
	const mock = await startMockOa({ pendingTimes: 5 });
	process.env.CSTOA_OA_HOST = mock.host;
	const agentDir = await makeAgentDir();
	const controller = new AbortController();
	try {
		const pending = login(agentDir, { onDeviceCode: () => {}, signal: controller.signal });
		setTimeout(() => controller.abort(), 100);
		await assert.rejects(pending, /Login cancelled/);
	} finally {
		await mock.close();
	}
});

test("refresh：轮换并返回新凭据", async () => {
	const mock = await startMockOa();
	process.env.CSTOA_OA_HOST = mock.host;
	try {
		const credentials = await refresh(
			{ access: "access-1", refresh: "refresh-1", expires: Date.now() + 1000 },
			AbortSignal.timeout(5000),
		);
		assert.equal(credentials.access, "access-2");
		assert.equal(credentials.refresh, "refresh-2");
		assert.ok(credentials.expires > Date.now());
	} finally {
		await mock.close();
	}
});

test("refresh：刷新令牌失效时提示重新登录", async () => {
	const mock = await startMockOa();
	process.env.CSTOA_OA_HOST = mock.host;
	try {
		await assert.rejects(
			refresh({ access: "access-1", refresh: "stale", expires: Date.now() + 1000 }, AbortSignal.timeout(5000)),
			/重新登录/,
		);
	} finally {
		await mock.close();
	}
});

test("device：device.json 持久化，重复读取返回同一标识", async () => {
	const agentDir = await makeAgentDir();
	const first = await getDeviceId(agentDir);
	resetDeviceIdCache();
	const second = await getDeviceId(agentDir);
	assert.equal(first, second);
	assert.match(first, /^dev_[0-9a-f]{32}$/);
	resetDeviceIdCache();
	const third = await getDeviceId(agentDir);
	assert.equal(third, first);
});
