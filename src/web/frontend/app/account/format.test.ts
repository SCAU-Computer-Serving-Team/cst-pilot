import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountStatus } from "../data/api";
import { describeAccount, formatCredit } from "./format.ts";

function status(patch: Partial<AccountStatus> = {}): AccountStatus {
	return {
		providerId: "cstoa",
		signedIn: true,
		requiresLogin: false,
		profile: { supported: true, studentId: "202333210102", name: "陈佳庆" },
		quota: { supported: true, balance: 12345 },
		...patch,
	};
}

test("姓名与额度齐全时显示姓名（学号）与剩余额度点", () => {
	assert.deepEqual(describeAccount(status()), {
		identity: "陈佳庆（202333210102）",
		quota: "剩余 12,345 额度点",
		expired: false,
	});
	assert.equal(formatCredit(0), "0 额度点");
});

test("姓名缺失时降级为学号，额度缺失时给占位", () => {
	assert.equal(
		describeAccount(status({ profile: { supported: true, studentId: "202333210102", name: null } }))?.identity,
		"202333210102",
	);
	assert.equal(describeAccount(status({ quota: { supported: false, balance: null } }))?.quota, "额度暂不可用");
	assert.equal(describeAccount(status({ quota: { supported: true, balance: 0 } }))?.quota, "剩余 0 额度点");
});

test("登录失效时提示重新登录", () => {
	assert.deepEqual(describeAccount(status({ requiresLogin: true, quota: { supported: false, balance: null } })), {
		identity: "陈佳庆（202333210102）",
		quota: "登录已过期，请重新登录",
		expired: true,
	});
});

test("未登录时不显示账号行", () => {
	assert.equal(describeAccount(null), null);
	assert.equal(describeAccount(undefined), null);
	assert.equal(describeAccount(status({ signedIn: false })), null);
});
