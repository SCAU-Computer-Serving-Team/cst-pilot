import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountStatus } from "../data/api";
import { describeAccount } from "./format.ts";

function status(patch: Partial<AccountStatus> = {}): AccountStatus {
	return {
		providerId: "cstoa",
		signedIn: true,
		requiresLogin: false,
		profile: { supported: true, studentId: "20230001", name: "测试队员" },
		...patch,
	};
}

test("账号文案只包含姓名和学号，不包含额度", () => {
	assert.deepEqual(describeAccount(status()), { identity: "测试队员（20230001）", expired: false });
});

test("资料缺失时给出占位；部分资料仍能展示", () => {
	assert.equal(
		describeAccount(status({ profile: { supported: true, studentId: "20230001", name: null } }))?.identity,
		"20230001",
	);
	assert.equal(
		describeAccount(status({ profile: { supported: true, studentId: null, name: null } }))?.identity,
		"资料暂不可用",
	);
});

test("失效与未登录不展示上一次账号姓名", () => {
	assert.deepEqual(describeAccount(status({ requiresLogin: true })), { identity: "需要重新登录", expired: true });
	assert.equal(describeAccount(null), null);
	assert.equal(describeAccount(undefined), null);
	assert.equal(describeAccount(status({ signedIn: false })), null);
});
