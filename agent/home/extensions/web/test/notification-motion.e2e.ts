import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { BrowserProbe } from "./browser.ts";
import { compactionFixture } from "./compaction-fixture.ts";
import { testRoot } from "./support.ts";

test("通知动效：入退场、打断连续性、退出清理与减少动态", { timeout: 40_000 }, async () => {
	const root = join(await testRoot(), "notification-motion");
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
		await probe.holdAnimations(".notification");
		await submit();
		await probe.until(
			"document.querySelector('.notification[data-state=entering]')?.getAnimations()[0]?.playState==='paused'",
			"入场可采样",
		);
		const enter = await probe.evaluate<{
			duration: number;
			easing: string;
			opacity: number;
			transform: string;
			filter: string;
		}>(`(() => {
			const e=document.querySelector('.notification'),a=e.getAnimations()[0],timing=a.effect.getTiming();
			a.currentTime=100; const s=getComputedStyle(e);
			window.__enterSnapshot={opacity:s.opacity,transform:s.transform,filter:s.filter};
			return {duration:timing.duration,easing:s.animationTimingFunction,opacity:+s.opacity,transform:s.transform,filter:s.filter};
		})()`);
		assert.equal(enter.duration, 250);
		assert.equal(enter.easing, "cubic-bezier(0.22, 1, 0.36, 1)");
		assert.ok(enter.opacity > 0 && enter.opacity < 1);
		await probe.evaluate("document.querySelector('.notification-close').click()");
		await probe.until(
			"document.querySelector('.notification[data-state=closing]')?.getAnimations()[0]?.playState==='paused'",
			"关闭后保留节点完成退场",
		);
		const exit = await probe.evaluate<{
			duration: number;
			delay: number;
			opacity: number;
			transform: string;
			filter: string;
			inert: boolean;
			hidden: string;
			live: string;
		}>(`(() => {
			const e=document.querySelector('.notification'),a=e.getAnimations()[0],timing=a.effect.getTiming(),s=getComputedStyle(e);
			return {duration:timing.duration,delay:timing.delay,opacity:+s.opacity,transform:s.transform,filter:s.filter,inert:e.inert,hidden:e.getAttribute('aria-hidden'),live:e.getAttribute('aria-live')};
		})()`);
		assert.equal(exit.duration, 350);
		assert.equal(exit.delay, 0);
		assert.ok(Math.abs(exit.opacity - enter.opacity) < 0.01, "中途关闭从当前透明度接续");
		assert.equal(exit.transform, enter.transform);
		assert.equal(exit.filter, enter.filter);
		assert.equal(exit.inert, true);
		assert.equal(exit.hidden, "true");
		assert.equal(exit.live, "off");
		await probe.evaluate("document.querySelector('.notification').getAnimations()[0].currentTime=175");
		assert.ok(
			await probe.evaluate<boolean>(
				"+getComputedStyle(document.querySelector('.notification')).opacity<+window.__enterSnapshot.opacity",
			),
		);
		await probe.screenshot(join(root, "closing-midpoint.png"));
		await probe.releaseAnimations();
		await probe.until("!document.querySelector('.notification')", "退场结束才清理节点");
		await submit();
		await probe.until("document.querySelector('.notification')?.dataset.state==='open'", "正常入场完成");
		await probe.evaluate("document.querySelector('.notification-detail-trigger').focus()");
		await probe.until("getComputedStyle(document.querySelector('.notification-detail')).opacity==='1'", "详情出现");
		const tooltip = await probe.evaluate<{ duration: string; delay: string; transform: string }>(
			"(() => {const s=getComputedStyle(document.querySelector('.notification-detail'));return {duration:s.transitionDuration,delay:s.transitionDelay,transform:s.transform};})()",
		);
		assert.ok(tooltip.duration.split(",").every((value) => value.trim() === "0.15s"));
		assert.ok(tooltip.delay.split(",").every((value) => value.trim() === "0.08s"));
		await probe.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		assert.ok(
			await probe.evaluate<boolean>(
				"getComputedStyle(document.querySelector('.notification-detail')).transitionDelay.split(',').every(v=>v.trim()==='0s')",
			),
			"详情关闭无延迟",
		);
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await probe.evaluate("document.querySelector('.notification-close').click()");
		await probe.until("!document.querySelector('.notification')", "减少动态立即关闭");
		await submit();
		await probe.until("document.querySelector('.notification')?.dataset.state==='open'", "减少动态直接显示");
		assert.equal(await probe.evaluate("document.querySelector('.notification').getAnimations().length"), 0);
		await probe.evaluate("document.querySelector('.notification-detail-trigger').focus()");
		assert.equal(
			await probe.evaluate("getComputedStyle(document.querySelector('.notification-detail')).transitionDuration"),
			"0s",
		);
		await probe.evaluate("document.querySelector('.notification-close').click()");
		await probe.until("!document.querySelector('.notification')", "清理减少动态下的通知");
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await probe.holdAnimations(".notification");
		await probe.evaluate("window.dispatchEvent(new Event('offline'))");
		await probe.until(
			"document.querySelector('.connection-status')?.getAnimations()[0]?.playState==='paused'",
			"持续连接提示入场",
		);
		await probe.evaluate("document.querySelector('.connection-status').getAnimations()[0].finish()");
		await probe.until("document.querySelector('.connection-status')?.dataset.state==='open'", "持续连接提示已显示");
		await probe.evaluate("window.dispatchEvent(new Event('online'))");
		await probe.until(
			"document.querySelector('.connection-status')?.dataset.state==='closing'",
			"外部恢复状态也保留退场节点",
		);
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await probe.until("!document.querySelector('.connection-status')", "退场中切换减少动态立即清理");
		await writeFile(
			join(root, "report.json"),
			JSON.stringify({ enter, exit, tooltip, reducedMotion: true, recoveryExit: true }, null, 2),
		);
	} finally {
		await browser?.close();
		await fixture.close();
	}
});
