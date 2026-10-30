import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { BrowserProbe } from "./browser.ts";
import { compactionFixture } from "./compaction-fixture.ts";
import { testRoot } from "./support.ts";

test("通知关闭清理、详情键盘操作、断线恢复与减少动态", { timeout: 40_000 }, async () => {
	const root = join(await testRoot(), "notification-lifecycle");
	await mkdir(root, { recursive: true });
	const fixture = await compactionFixture();
	let browser: BrowserProbe | undefined;
	try {
		const empty = await fixture.pool.createWithKey(randomUUID());
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		await probe.navigate(`${fixture.origin}/s/${empty.id}`);
		await probe.until("document.querySelector('.model-label')?.textContent.includes('压缩验收模型')", "模型已加载");
		async function submit() {
			await probe.fill(".composer-input", "/compact");
			await probe.click(".composer-send");
		}
		await submit();
		await probe.until("!!document.querySelector('.notification')", "通知出现");
		await probe.evaluate("document.querySelector('.notification-detail-trigger').focus()");
		await probe.until(
			"getComputedStyle(document.querySelector('.notification-detail')).visibility==='visible'",
			"键盘焦点展示详情",
		);
		await probe.evaluate(
			"window.__copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>window.__copied.push(text)}})",
		);
		await probe.click(".notification-detail .code-copy-button");
		assert.equal(
			await probe.evaluate("window.__copied[0]===document.querySelector('.notification-detail pre').textContent"),
			true,
			"技术详情只复制原文",
		);
		await probe.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		await probe.until(
			"getComputedStyle(document.querySelector('.notification-detail')).visibility==='hidden'",
			"Esc 关闭详情",
		);
		assert.equal(
			await probe.evaluate("document.activeElement===document.querySelector('.notification-detail-trigger')"),
			true,
			"复制后 Esc 关闭详情并返回入口焦点",
		);
		await probe.evaluate("document.querySelector('.notification-close').click()");
		assert.equal(
			await probe.evaluate(
				"(()=>{const e=document.querySelector('.notification');return !e||(e.inert&&e.getAttribute('aria-hidden')==='true'&&e.getAttribute('aria-live')==='off')})()",
			),
			true,
			"关闭后不再交互或重复播报",
		);
		await probe.until("!document.querySelector('.notification')", "关闭后清理通知");
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await submit();
		await probe.until("!!document.querySelector('.notification')", "减少动态仍可显示通知");
		await probe.click(".notification-close");
		await probe.until("!document.querySelector('.notification')", "减少动态仍可关闭通知");
		await probe.evaluate("window.dispatchEvent(new Event('offline'))");
		await probe.until("!!document.querySelector('.connection-status')", "断线提示");
		await probe.evaluate("window.dispatchEvent(new Event('online'))");
		await probe.until("!document.querySelector('.connection-status')", "恢复后清理断线提示");
		await writeFile(
			join(root, "report.json"),
			JSON.stringify({ closeCleanup: true, keyboardDetails: true, reducedMotion: true, recovery: true }, null, 2),
		);
	} finally {
		await browser?.close();
		await fixture.close();
	}
});
