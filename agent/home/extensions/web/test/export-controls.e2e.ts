import assert from "node:assert/strict";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import type { ExportSnapshot, SessionExport } from "../shared/exports.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("导出选项：浅深窄屏、箭头对齐、菜单开合与键盘、实时分支和离页任务", { timeout: 90_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-controls-"));
	const home = join(root, "home");
	await mkdir(home);
	await configureTestModel(home);
	const pool = new WebSessionPool({
		cwd: root,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	const assistant = {
		role: "assistant" as const,
		api: "openai-completions" as const,
		provider: "test-probe",
		model: "mock",
		stopReason: "stop" as const,
		usage: {
			input: 1,
			output: 1,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 2,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
	};
	function append(question: string, answer: string) {
		slot.session.sessionManager.appendMessage({ role: "user", content: question, timestamp: Date.now() });
		slot.session.sessionManager.appendMessage({
			...assistant,
			content: [{ type: "text", text: answer }],
			timestamp: Date.now(),
		});
		return slot.session.sessionManager.getLeafId()!;
	}
	const common = append(
		"检查磁盘和内存",
		"## 检查结论\n\n磁盘空间充足，内存状态正常。\n\n```powershell\nGet-Process\n```",
	);
	append("未选择的分支", "这一轮不应出现在生成结果中。");
	const captured: ExportSnapshot[] = [];
	let release = () => {};
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port, undefined, {
		artifactsRoot: join(root, "artifacts"),
		exportRenderer: async (snapshot, _url, emit) => {
			captured.push(snapshot);
			await gate;
			await emit("html", Buffer.from("<!DOCTYPE html><p>fixture</p>"));
		},
	});
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "export-controls" }),
		(...args) => api(...args),
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		async function key(key: string, code = key) {
			await probe.call("Input.dispatchKeyEvent", { type: "keyDown", key, code });
			await probe.call("Input.dispatchKeyEvent", { type: "keyUp", key, code });
		}
		await browser.navigate(`${origin}/s/${slot.id}/export`);
		await browser.until("!document.querySelector('.export-generate')?.disabled", "导出预览");
		assert.equal(await browser.evaluate("document.querySelectorAll('.export-select-trigger').length"), 2);
		assert.equal(
			await browser.evaluate(
				"!!document.querySelector('.export-controls input[type=date], .export-turns, .export-controls .export-note')",
			),
			false,
		);
		const geometry = await browser.evaluate<{ height: number; vertical: number; right: number; radius: string }[]>(
			`[...document.querySelectorAll('.export-select-trigger')].map(e=>{const b=e.getBoundingClientRect(), a=e.querySelector('svg').getBoundingClientRect();return {height:b.height,vertical:Math.abs(a.y+a.height/2-b.y-b.height/2),right:b.right-a.right,radius:getComputedStyle(e).borderRadius}})`,
		);
		for (const control of geometry) {
			assert.equal(control.height, 40);
			assert.ok(control.vertical < 1, JSON.stringify(control));
			assert.ok(Math.abs(control.right - 13) < 1, JSON.stringify(control));
			assert.equal(control.radius, "4px");
		}
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.export-generate')).borderRadius"),
			"4px",
		);
		const sample = await browser.evaluate<{
			duration: number;
			frames: { opacity: number; filter: string; transform: string }[];
			width: number;
			trigger: number;
		} | null>(`(async()=>{
			document.querySelector('.export-select-trigger').focus();
			document.querySelector('.export-select-trigger').click();
			await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
			const menu=document.querySelector('.export-select-popover');
			const animation=menu?.getAnimations().find(a=>a.effect?.target===menu);
			if(!animation)return null;
			animation.pause();const duration=animation.effect.getTiming().duration;
			const frames=[0,duration/2,duration].map(t=>{animation.currentTime=t;const s=getComputedStyle(menu);return {opacity:Number(s.opacity),filter:s.filter,transform:s.transform}});
			animation.finish();return {duration,frames,width:menu.getBoundingClientRect().width,trigger:document.querySelector('.export-select-trigger').getBoundingClientRect().width};
		})()`);
		assert.ok(sample, "菜单有开启动画");
		assert.equal(sample.duration, 250);
		assert.equal(sample.frames[0].opacity, 0);
		assert.ok(sample.frames[1].opacity > 0 && sample.frames[1].opacity < 1);
		assert.equal(sample.frames[2].opacity, 1);
		assert.ok(sample.frames.every((frame) => frame.filter === "none"));
		assert.ok(Math.abs(sample.width - sample.trigger) < 1);
		await writeFile(join(root, "motion.json"), JSON.stringify(sample, null, 2));
		await key("Escape");
		await browser.until("!document.querySelector('.export-select-popover')", "Esc 关闭菜单");
		await browser.until(
			"document.activeElement===document.querySelector('.export-select-trigger')",
			"Esc 恢复触发器焦点",
			2000,
		);
		await key("ArrowDown");
		await browser.until("!!document.querySelector('.export-select-popover [role=option]')", "键盘打开菜单");
		await key("End");
		await key("Enter");
		await browser.until(
			"document.querySelector('.export-select-trigger').textContent.includes('HTML') && !document.querySelector('.export-select-popover') && !document.querySelector('.export-generate').disabled",
			"键盘选择格式",
		);
		for (const width of [1440, 390])
			for (const theme of ["light", "dark"]) {
				await browser.call("Emulation.setDeviceMetricsOverride", {
					width,
					height: 900,
					deviceScaleFactor: 1,
					mobile: false,
				});
				if (
					width === 390 &&
					!(await browser.evaluate(
						"document.querySelector('.app-layout').classList.contains('sidebar-collapsed')",
					))
				) {
					await browser.click('[aria-label="收起侧栏"]');
					await browser.until("!document.querySelector('.sidebar-backdrop')", "窄屏收起侧栏");
				}
				await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
				await browser.click(".export-select:nth-of-type(2) .export-select-trigger");
				await browser.click(`.export-select-popover [role=option][data-key=${theme}]`);
				await browser.until(
					`document.querySelector('iframe').contentDocument?.documentElement.dataset.theme===${JSON.stringify(theme)} && !document.querySelector('.export-select-popover')`,
					"文档主题预览",
				);
				assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
				assert.equal(
					await browser.evaluate(
						"getComputedStyle(document.querySelector('.export-preview-viewport')).borderTopColor",
					),
					theme === "dark" ? "rgb(119, 131, 138)" : "rgb(138, 138, 138)",
				);
				await browser.screenshot(join(root, `${width}-${theme}.png`));
				await browser.click(".export-select-trigger");
				await browser.until(
					"!!document.querySelector('.export-select-popover') && !document.querySelector('.export-select-popover').hasAttribute('data-entering')",
					"菜单展开完成",
				);
				const bounds = await browser.evaluate<{ left: number; right: number; width: number }>(
					"(()=>{const r=document.querySelector('.export-select-popover').getBoundingClientRect();return {left:r.left,right:r.right,width:r.width}})()",
				);
				assert.ok(bounds.left >= 0 && bounds.right <= width);
				await browser.screenshot(join(root, `${width}-${theme}-menu.png`));
				await key("Escape");
				await browser.until("!document.querySelector('.export-select-popover')", "菜单关闭完成");
			}
		await browser.click(".export-select-trigger");
		await browser.until(
			"!!document.querySelector('.export-select-popover') && !document.querySelector('.export-select-popover').hasAttribute('data-entering')",
			"关闭动效采样前菜单就绪",
		);
		const hover = await browser.evaluate<{ x: number; y: number }>(
			"(()=>{const r=document.querySelector('.export-select-popover [role=option]:not([aria-selected=true])').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()",
		);
		await browser.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...hover });
		await browser.until(
			"getComputedStyle(document.querySelector('.export-select-popover [role=option]:not([aria-selected=true])')).backgroundColor!=='rgba(0, 0, 0, 0)'",
			"未选项悬停背景",
		);
		assert.equal(
			await browser.evaluate(
				"getComputedStyle(document.querySelector('.export-select-popover [role=option]')).boxShadow",
			),
			"none",
		);
		const closing = await browser.evaluate<{ duration: number; opacity: number[] } | null>(`(async()=>{
			document.activeElement.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",code:"Escape",bubbles:true}));
			await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
			const menu=document.querySelector(".export-select-popover");
			const a=menu?.getAnimations().find(a=>a.effect?.target===menu);
			if(!a)return null;
			a.pause();const duration=a.effect.getTiming().duration;
			const opacity=[0,duration/2,duration].map(t=>{a.currentTime=t;return Number(getComputedStyle(menu).opacity)});
			a.finish();return {duration,opacity};
		})()`);
		assert.ok(closing, "菜单有关闭动画");
		assert.equal(closing.duration, 150);
		assert.equal(closing.opacity[0], 1);
		assert.ok(closing.opacity[1] > 0 && closing.opacity[1] < 1);
		assert.equal(closing.opacity[2], 0);
		await browser.until("!document.querySelector('.export-select-popover')", "关闭动效完成后移除菜单");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.click(".export-select-trigger");
		assert.equal(
			await browser.evaluate("document.querySelector('.export-select-popover').getAnimations().length"),
			0,
		);
		await browser.call("Input.dispatchMouseEvent", {
			type: "mousePressed",
			x: 385,
			y: 80,
			button: "left",
			clickCount: 1,
		});
		await browser.call("Input.dispatchMouseEvent", {
			type: "mouseReleased",
			x: 385,
			y: 80,
			button: "left",
			clickCount: 1,
		});
		await browser.until("!document.querySelector('.export-select-popover')", "外部点击关闭菜单");
		await browser.click(".export-select-trigger");
		await browser.evaluate("document.querySelector('.export-workspace').dispatchEvent(new Event('scroll'))");
		await browser.until("!document.querySelector('.export-select-popover')", "页面滚动关闭菜单");
		await browser.click(".export-check input");
		await browser.until("!document.querySelector('.export-generate').disabled", "包含项刷新预览");
		const response = await fetch(`${origin}/api/sessions/${slot.id}/tree/navigate`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify({ entryId: common }),
		});
		assert.equal(response.status, 200);
		append("导航后的当前分支", "生成时读取这一轮。");
		await browser.click(".export-generate");
		await browser.until(
			"document.querySelector('.export-generate').disabled && document.querySelectorAll('.export-select-trigger:disabled').length===2",
			"生成中禁用所有选项",
		);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.export-check input')].every(e=>e.matches(':disabled'))",
			),
			true,
		);
		await browser.click(".export-back");
		await browser.until("!!document.querySelector('.chat-composer-area')", "离开导出页");
		release();
		for (let i = 0; i < 100 && captured.length === 0; i++) await new Promise((resolve) => setTimeout(resolve, 20));
		assert.equal(captured.length, 1);
		assert.equal(captured[0].options.includeThinking, true);
		assert.ok(JSON.stringify(captured[0]).includes("导航后的当前分支"));
		assert.equal(JSON.stringify(captured[0]).includes("未选择的分支"), false);
		let result: SessionExport | undefined;
		for (let i = 0; i < 100; i++) {
			result = (
				(await (await fetch(`${origin}/api/sessions/${slot.id}/exports`)).json()) as { exports: SessionExport[] }
			).exports[0];
			if (result?.status === "completed") break;
			await new Promise((resolve) => setTimeout(resolve, 20));
		}
		assert.equal(result?.status, "completed", "离页任务继续保存");
	} finally {
		release();
		await browser?.close();
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
