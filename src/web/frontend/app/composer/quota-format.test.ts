import assert from "node:assert/strict";
import { test } from "node:test";
import { formatQuotaBalance, quotaProviders } from "./quota-format.ts";

test("CSTOA 使用额度点，0 为真实有效值", () => {
	assert.equal(
		formatQuotaBalance({ provider: "cstoa", supported: true, balance: 12345, unit: "credit" }),
		"12,345 额度点",
	);
	assert.equal(formatQuotaBalance({ provider: "cstoa", supported: true, balance: 0, unit: "credit" }), "0 额度点");
	assert.equal(quotaProviders.has("cstoa"), true);
});

test("缺失、错误单位和无效数字不伪造额度", () => {
	assert.equal(formatQuotaBalance(null), null);
	for (const balance of [null, undefined, -1, 1.5, NaN, Infinity, 1e20])
		assert.equal(formatQuotaBalance({ provider: "cstoa", supported: true, balance, unit: "credit" }), null);
	assert.equal(formatQuotaBalance({ provider: "cstoa", supported: true, balance: 2, currency: "CNY" }), null);
	assert.equal(formatQuotaBalance({ provider: "cstoa", supported: false, balance: 0, unit: "credit" }), null);
});

test("DeepSeek 沿用余额单位，其他服务不显示 CSTOA 额度", () => {
	assert.equal(formatQuotaBalance({ provider: "deepseek", supported: true, balance: 2, currency: "CNY" }), "¥ 2.00");
	assert.equal(formatQuotaBalance({ provider: "deepseek", supported: true, balance: 2, currency: "USD" }), "US$ 2.00");
	assert.equal(formatQuotaBalance({ provider: "other", supported: true, balance: 2, unit: "credit" }), null);
	assert.equal(quotaProviders.has("other"), false);
});
