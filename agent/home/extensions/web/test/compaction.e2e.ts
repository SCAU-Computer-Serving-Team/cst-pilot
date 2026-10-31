import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { BrowserProbe } from "./browser.ts";
import { compactionFixture } from "./compaction-fixture.ts";
import { testRoot } from "./support.ts";

test("压缩交互：顶部通知、草稿保留、关闭、实际失败与成功", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "compaction-ui");
	await mkdir(root, { recursive: true });
	const fixture = await compactionFixture();
	let browser: BrowserProbe | undefined;
	try {
		const empty = await fixture.pool.createWithKey(randomUUID());
		const slot = await fixture.conversation();
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		async function submit() {
			await probe.fill(".composer-input", "/compact 保留诊断结论");
			await probe.click(".composer-send");
		}
		await probe.navigate(`${fixture.origin}/s/${empty.id}`);
		await probe.until("document.querySelector('.model-name')?.textContent.includes('压缩验收模型')", "模型已加载");
		await submit();
		await probe.until(
			"document.querySelector('.error-notification')?.textContent.includes('暂无可压缩内容')",
			"空会话提示",
		);
		assert.equal(
			await probe.evaluate("!!document.querySelector('.composer .error-state, .composer .error-notification')"),
			false,
			"错误不占输入框空间",
		);
		assert.equal(await probe.evaluate("document.querySelector('.error-notification').dataset.kind"), "info");
		assert.equal(await probe.evaluate("!!document.querySelector('.error-notification details')"), false);
		assert.equal(
			await probe.evaluate("getComputedStyle(document.querySelector('.notification-detail')).visibility"),
			"hidden",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.composer-input').textContent.includes('保留诊断结论')"),
			true,
		);
		await probe.screenshot(join(root, "empty-light.png"));
		const icon = await probe.evaluate<{ x: number; y: number }>(
			"(() => { const r=document.querySelector('.notification-detail-trigger').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}; })()",
		);
		await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...icon });
		await probe.until(
			"getComputedStyle(document.querySelector('.notification-detail')).opacity==='1'",
			"悬停图标展示详情",
		);
		const explanation = await probe.evaluate<string>(
			"document.querySelector('.notification-detail-description').textContent",
		);
		assert.match(explanation, /保留近期原文/);
		assert.match(explanation, /不足以分出/);
		const detailPoint = await probe.evaluate<{ x: number; y: number }>(
			"(() => {const r=document.querySelector('.notification-detail').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()",
		);
		await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...detailPoint });
		await new Promise((resolve) => setTimeout(resolve, 2800));
		assert.equal(
			await probe.evaluate("!!document.querySelector('.error-notification')"),
			true,
			"查看详情期间暂停关闭计时",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.error-notification').getBoundingClientRect().height"),
			44,
		);
		await probe.screenshot(join(root, "detail-light.png"));
		await probe.call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 30, y: 200 });
		await probe.until("!document.querySelector('.error-notification')", "移开后按剩余时长关闭");
		await submit();
		await probe.until("!!document.querySelector('.error-notification')", "再次提示");
		await probe.click(".error-notification .notification-close");
		await probe.until("!document.querySelector('.error-notification')", "通知可关闭");
		assert.equal(
			await probe.evaluate("document.querySelector('.composer-input').textContent.includes('保留诊断结论')"),
			true,
		);
		await probe.navigate(`${fixture.origin}/s/${slot.id}`);
		await probe.until(
			"document.querySelector('.model-name')?.textContent.includes('压缩验收模型')",
			"会话模型已加载",
		);
		fixture.modelState.status = 401;
		await submit();
		await probe.until(
			"document.querySelector('.error-notification')?.dataset.kind==='danger'",
			"真正失败采用错误通知",
		);
		await probe.evaluate("document.documentElement.dataset.theme='dark'");
		await probe.call("Emulation.setDeviceMetricsOverride", {
			width: 960,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await probe.click('button[aria-label="收起侧栏"]');
		await probe.until(
			"document.querySelector('.app-layout')?.classList.contains('sidebar-collapsed')",
			"窄窗口侧栏收起",
		);
		await probe.evaluate("document.fonts.ready");
		await probe.evaluate("Promise.all(document.getAnimations().map(a=>a.finished.catch(()=>{})))");
		await probe.screenshot(join(root, "failed-dark-960.png"));
		assert.equal(
			slot.session.sessionManager.getBranch().some((entry) => entry.type === "compaction"),
			false,
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.composer-input').textContent.includes('保留诊断结论')"),
			true,
		);
		fixture.modelState.status = 200;
		await probe.click(".error-notification .notification-close");
		await probe.click(".composer-send");
		await probe.until(
			"!document.querySelector('.composer-input').textContent.trim() && !document.querySelector('.error-notification')",
			"重试成功清空草稿",
		);
		assert.ok(slot.session.sessionManager.getBranch().some((entry) => entry.type === "compaction"));
		await submit();
		await probe.until(
			"document.querySelector('.error-notification')?.textContent.includes('会话已压缩')",
			"重复压缩给出具体状态",
		);
		await probe.evaluate("document.querySelector('.notification-detail-trigger').focus()");
		await probe.until(
			"document.querySelector('.notification-detail-trigger').getAttribute('aria-expanded')==='true'",
			"键盘聚焦展示详情",
		);
		assert.match(
			await probe.evaluate<string>("document.querySelector('.notification-detail-description').textContent"),
			/上次压缩后没有新增对话/,
		);
		await new Promise((resolve) => setTimeout(resolve, 2800));
		assert.equal(
			await probe.evaluate("!!document.querySelector('.error-notification')"),
			true,
			"键盘阅读暂停关闭计时",
		);
		await probe.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		assert.equal(
			await probe.evaluate("document.querySelector('.notification-detail-trigger').getAttribute('aria-expanded')"),
			"false",
		);
		await probe.click(".error-notification .notification-close");
		await probe.until("!document.querySelector('.error-notification')", "退场结束后切换窄屏");
		await probe.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await probe.evaluate(
			`window.__notificationFetch=window.fetch; window.fetch=(input,init)=>String(input).endsWith('/compact') ? Promise.resolve(new Response(JSON.stringify({error:{code:'server_error',message:'provider: '+ 'failure-detail '.repeat(120)+' apiKey=private-value'}}),{status:500,headers:{'Content-Type':'application/json'}})) : window.__notificationFetch(input,init); document.documentElement.dataset.theme='light'`,
		);
		await submit();
		await probe.until("!!document.querySelector('.error-notification')", "窄屏长错误提示");
		await probe.evaluate("document.querySelector('.notification-detail-trigger').focus()");
		await probe.until(
			"getComputedStyle(document.querySelector('.notification-detail')).opacity==='1'",
			"窄屏详情可读",
		);
		const overflow = await probe.evaluate<boolean>(
			"(() => {const e=document.querySelector('.notification-detail'),r=e.getBoundingClientRect();return r.left<24-.5||r.right>innerWidth-24+.5||e.scrollWidth>e.clientWidth||e.textContent.includes('private-value');})()",
		);
		assert.equal(overflow, false, "长详情换行、约束窄屏边距并遮蔽密钥");
		await probe.evaluate("document.fonts.ready");
		await probe.screenshot(join(root, "detail-light-390.png"));
		await probe.click(".error-notification .notification-close");
		await probe.until("!document.querySelector('.error-notification')", "长详情通知退场完成");
		await probe.evaluate(
			`window.fetch=(input,init)=>String(input).endsWith('/compact') ? Promise.resolve(new Response(JSON.stringify({error:{code:'nothing_to_compact',message:'当前会话无需压缩'}}),{status:409,headers:{'Content-Type':'application/json'}})) : window.__notificationFetch(input,init)`,
		);
		await submit();
		await probe.until(
			"document.querySelector('.notification-message')?.textContent==='暂时无法压缩'",
			"无需压缩的返回文案显示专门提示",
		);
		await probe.evaluate("document.querySelector('.notification-detail-trigger').focus()");
		await probe.until(
			"getComputedStyle(document.querySelector('.notification-detail')).opacity==='1'",
			"无法压缩的原因可查看",
		);
		assert.match(
			await probe.evaluate<string>("document.querySelector('.notification-detail-description').textContent"),
			/内容较少.*没有新增对话/,
		);
		assert.equal(await probe.evaluate("document.querySelector('.error-notification').dataset.kind"), "info");
		await probe.screenshot(join(root, "compact-unavailable-reason.png"));
		await probe.evaluate("window.fetch=window.__notificationFetch");
	} finally {
		await browser?.close();
		await fixture.close();
	}
});
