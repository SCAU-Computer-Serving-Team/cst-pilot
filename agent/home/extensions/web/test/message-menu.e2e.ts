import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { BrowserProbe } from "./browser.ts";
import { compactionFixture } from "./compaction-fixture.ts";
import { testRoot } from "./support.ts";

test("消息菜单仅分支树可用，导出与分享置灰且不发请求", { timeout: 40_000 }, async () => {
	const root = join(await testRoot(), "message-menu");
	await mkdir(root, { recursive: true });
	const fixture = await compactionFixture();
	let browser: BrowserProbe | undefined;
	try {
		const slot = await fixture.conversation();
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		await probe.navigate(`${fixture.origin}/s/${slot.id}`);
		await probe.until("!!document.querySelector('.answer-actions')", "会话内容已加载");
		assert.equal(
			await probe.evaluate("!!document.querySelector('.answer-actions [aria-label=导出会话],a[download]')"),
			false,
		);
		await probe.evaluate(
			"window.__plannedRequests=[];const original=window.fetch;window.fetch=(input,...args)=>{if(/\\/(export|share)$/.test(String(input)))window.__plannedRequests.push(String(input));return original(input,...args)}",
		);
		await probe.click('button[aria-label="消息菜单"]');
		await probe.until("!!document.querySelector('.sidebar-account-menu button[disabled]')", "计划入口显示");
		assert.deepEqual(
			await probe.evaluate(
				"[...document.querySelectorAll('.sidebar-account-menu a,.sidebar-account-menu button')].map(e=>({text:e.textContent.trim(),disabled:e.disabled===true}))",
			),
			[
				{ text: "分支树", disabled: false },
				{ text: "导出会话", disabled: true },
				{ text: "分享", disabled: true },
			],
		);
		assert.deepEqual(
			await probe.evaluate(
				"[...document.querySelectorAll('.sidebar-account-menu button')].map(e=>getComputedStyle(e).opacity)",
			),
			["0.45", "0.45"],
		);
		await probe.evaluate("document.querySelectorAll('.sidebar-account-menu button').forEach(e=>e.click())");
		assert.deepEqual(await probe.evaluate("window.__plannedRequests"), []);
		await probe.evaluate("document.fonts.ready");
		await probe.evaluate("Promise.all(document.getAnimations().map(a=>a.finished.catch(()=>{})))");
		await probe.screenshot(join(root, "message-menu-light.png"));
		await probe.evaluate("document.documentElement.dataset.theme='dark'");
		await probe.screenshot(join(root, "message-menu-dark.png"));
		await probe.click(".sidebar-account-menu a");
		await probe.until(`location.pathname==='/s/${slot.id}/tree'`, "分支树仍可导航");
		for (const suffix of ["export", "share"]) {
			const response = await fetch(`${fixture.origin}/api/sessions/${slot.id}/${suffix}`);
			assert.equal(response.status, 404);
		}
	} finally {
		await browser?.close();
		await fixture.close();
	}
});
