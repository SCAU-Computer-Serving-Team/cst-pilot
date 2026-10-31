import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

type RadiusSnapshot = {
	overflow: boolean;
	tables: {
		radius: string;
		shape: string;
		outer: string[];
		cell: string[];
		last: string[];
		scroll: boolean;
		keyboard: boolean;
	}[];
	composer: string;
	controls: string[];
	bubble: string;
	context: string;
	supported: boolean;
};

test("圆角层级、Markdown 表格裁切与滚动、全圆例外和渐进降级", { timeout: 60000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "radius-"));
	const now = Date.now();
	const sessions = [{ id: "radius", title: "圆角与表格", updatedAt: new Date(now).toISOString(), running: false }];
	const columns = Array.from({ length: 12 }, (_, i) => `字段 ${i + 1}`);
	const text = [
		"## 检查结果",
		"圆角表格保留完整边框，宽内容在表格区域内滚动。",
		"| 项目 | 结果 |\n| --- | --- |\n| 系统磁盘 | 状态正常 |\n| 剩余空间 | 128 GB |",
		`| ${columns.join(" | ")} |\n| ${columns.map(() => "---").join(" | ")} |\n| ${columns.map(() => "128 GB").join(" | ")} |`,
		"`disk usage`\n\n```text\nC: 128 GB available\n```",
	].join("\n\n");
	const messages = [
		{ role: "user", content: [{ type: "text", text: "检查当前电脑的磁盘。" }], timestamp: now },
		{ role: "assistant", content: [{ type: "text", text }], timestamp: now + 1, stopReason: "stop" },
	];
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "radius-test" }),
		async (_req, res, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				res.writeHead(200, { "Content-Type": "text/event-stream" });
				res.write(": connected\n\n");
				clients.add(res);
				res.on("close", () => clients.delete(res));
				return true;
			}
			let data: unknown = {};
			if (path === "/api/sessions/radius")
				data = {
					id: "radius",
					messages,
					entries: messages.map((message, i) => ({ id: `entry-${i}`, message })),
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/settings") data = { theme: "light" };
			else if (path === "/api/auth") data = { providers: [] };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			res.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	const evidence: unknown[] = [];
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`http://127.0.0.1:${port}/s/radius`);
		await browser.until("document.querySelectorAll('.markdown-table').length===2", "实际 Markdown 表格");
		await browser.until(
			"document.getAnimations().every(a=>a.effect.getTiming().iterations===Infinity)",
			"正文入场结束",
		);
		await browser.evaluate("document.fonts.ready");
		for (const width of [1440, 768, 390]) {
			await browser.call("Emulation.setDeviceMetricsOverride", {
				width,
				height: 1000,
				deviceScaleFactor: 1,
				mobile: false,
			});
			if (
				width <= 768 &&
				(await browser.evaluate("!document.querySelector('.app-layout').classList.contains('sidebar-collapsed')"))
			) {
				await browser.click('.sidebar [aria-label="收起侧栏"]');
				await browser.until(
					"document.getAnimations().every(a=>a.effect.getTiming().iterations===Infinity)",
					"侧栏收起",
				);
			}
			for (const theme of ["light", "dark"]) {
				await browser.evaluate(
					`document.documentElement.dataset.theme='${theme}'; document.querySelector('.conversation').scrollTop=0`,
				);
				const result: RadiusSnapshot = await browser.evaluate<RadiusSnapshot>(`(() => {
					const style=(el)=>getComputedStyle(el), supported=CSS.supports('corner-shape','superellipse(1.25)');
					return {supported,overflow:document.documentElement.scrollWidth>innerWidth,tables:[...document.querySelectorAll('.markdown-table')].map(el=>{
						const s=style(el),first=style(el.querySelector('th')),last=style(el.querySelector('tbody tr:last-child td:last-child'));
						return {radius:s.borderRadius,shape:s.getPropertyValue('corner-shape'),outer:[s.borderTopWidth,s.borderRightWidth,s.borderBottomWidth,s.borderLeftWidth],cell:[first.borderTopWidth,first.borderRightWidth,first.borderBottomWidth,first.borderLeftWidth],last:[last.borderRightWidth,last.borderBottomWidth],scroll:el.scrollWidth>el.clientWidth,keyboard:el.tabIndex===0};
					}),composer:style(document.querySelector('.composer')).borderRadius,controls:[...document.querySelectorAll('.composer-send,.composer-model,.context-meter')].map(el=>style(el).borderRadius),bubble:style(document.querySelector('.user-bubble')).borderRadius,context:style(document.querySelector('.context-meter')).getPropertyValue('corner-shape')};
				})()`);
				assert.equal(result.overflow, false);
				assert.equal(result.composer, "16px");
				assert.equal(result.bubble, "12px");
				assert.ok(result.controls.length > 0 && result.controls.every((value) => value === "4px"));
				for (const table of result.tables) {
					assert.equal(table.radius, "8px");
					assert.deepEqual(table.outer, ["1px", "1px", "1px", "1px"]);
					assert.deepEqual(table.cell, ["0px", "1px", "1px", "0px"]);
					assert.deepEqual(table.last, ["0px", "0px"]);
					assert.equal(table.keyboard, true);
					if (result.supported) assert.equal(table.shape, "superellipse(1.25)");
				}
				if (width === 390) {
					assert.equal(result.tables[1].scroll, true, "窄屏宽表格局部滚动");
					await browser.evaluate("document.querySelectorAll('.markdown-table')[1].focus()");
					await browser.call("Input.dispatchKeyEvent", {
						type: "keyDown",
						key: "ArrowRight",
						code: "ArrowRight",
						windowsVirtualKeyCode: 39,
					});
					await browser.call("Input.dispatchKeyEvent", {
						type: "keyUp",
						key: "ArrowRight",
						code: "ArrowRight",
						windowsVirtualKeyCode: 39,
					});
					await browser.until("document.querySelectorAll('.markdown-table')[1].scrollLeft>0", "键盘横向滚动");
					await browser.evaluate(
						"document.querySelectorAll('.markdown-table')[1].scrollLeft=0; document.activeElement.blur()",
					);
				}
				if (result.supported) assert.equal(result.context, "superellipse(1.25)");
				evidence.push({ width, theme, result });
				await browser.screenshot(join(root, `${width}-${theme}.png`));
			}
		}
		const nested = await browser.evaluate<{ radii: string[]; round: string[] }>(`(() => {
			const probe=document.createElement('div');probe.style.cssText='position:fixed;left:-10000px;width:400px';
			probe.innerHTML='<div class="special-tool"><pre class="special-tool-raw"></pre><div class="special-tool-command-line"></div><div class="special-tool-password"></div><button class="special-tool-primary"></button><progress value="50" max="100"></progress><span class="special-tool-meter-empty"></span></div><div class="composer-popover"><div role="option"></div></div><button class="draft-image-remove"></button>';
			document.body.append(probe);const radii=[...probe.querySelectorAll('.special-tool,.special-tool-raw,.special-tool-command-line,.special-tool-password,.special-tool-primary,.composer-popover,[role=option]')].map(el=>getComputedStyle(el).borderRadius),round=[...probe.querySelectorAll('progress,.special-tool-meter-empty,.draft-image-remove')].map(el=>getComputedStyle(el).getPropertyValue('corner-shape'));probe.remove();return {radii,round};
		})()`);
		assert.deepEqual(nested.radii, ["12px", "4px", "4px", "4px", "4px", "12px", "4px"]);
		if (await browser.evaluate("CSS.supports('corner-shape','superellipse(1.25)')"))
			assert.ok(nested.round.every((value) => ["round", "superellipse(1)"].includes(value)));
		evidence.push({ nested });
		await browser.navigate(`http://127.0.0.1:${port}/settings/general`);
		await browser.until("!!document.querySelector('.settings-segment .t-tabs-pill')", "主题分段控件");
		const segment = await browser.evaluate(
			"[...document.querySelectorAll('.settings-segment,.settings-segment .t-tabs-pill')].map(el=>{const s=getComputedStyle(el);return {radius:s.borderRadius,padding:s.padding,height:s.height,shape:s.getPropertyValue('corner-shape')}})",
		);
		evidence.push({ segment });
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.settings-segment')).borderRadius"),
			"8px",
		);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.settings-segment')).padding"),
			"4px",
		);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.settings-segment .t-tabs-pill')).height"),
			"24px",
		);
		await browser.navigate(`http://127.0.0.1:${port}/login`);
		await browser.until("!!document.querySelector('.login-card')", "登录卡片");
		await browser.evaluate("document.fonts.ready");
		const login = await browser.evaluate<{ radii: string[]; offGrid: string[] }>(`(() => {
			const nodes=[...document.querySelectorAll('.login-card,.login-switch,.login-switch button,.login-field,.login-fields,.login-input,.login-qr-box')];
			return {radii:[...document.querySelectorAll('.login-panel,.login-card,.login-switch,.login-switch button')].map(el=>getComputedStyle(el).borderRadius),offGrid:nodes.flatMap(el=>{const s=getComputedStyle(el);return ['paddingTop','paddingRight','paddingBottom','paddingLeft','rowGap','columnGap'].filter(k=>s[k]!=='normal'&&parseFloat(s[k])%4!==0).map(k=>el.className+':'+k+'='+s[k])})};
		})()`);
		assert.deepEqual(login.offGrid, []);
		assert.equal(login.radii[0], "32px");
		assert.equal(login.radii[1], "24px");
		assert.equal(login.radii[2], "8px");
		assert.ok(login.radii.slice(3).every((value) => value === "4px"));
		evidence.push({ login });
		await browser.screenshot(join(root, "390-login.png"));
		// 删除增强规则，验证基础圆角、边框与局部滚动独立成立。
		await browser.navigate(`http://127.0.0.1:${port}/s/radius`);
		await browser.until("document.querySelectorAll('.markdown-table').length===2", "降级页面");
		await browser.evaluate(
			`(() => {for(const sheet of document.styleSheets)for(let i=sheet.cssRules.length-1;i>=0;i--){const rule=sheet.cssRules[i];if(rule instanceof CSSSupportsRule && rule.conditionText.includes('corner-shape'))sheet.deleteRule(i);}})()`,
		);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.markdown-table')).borderRadius"),
			"8px",
		);
		assert.ok(
			["round", "superellipse(1)"].includes(
				await browser.evaluate(
					"getComputedStyle(document.querySelector('.markdown-table')).getPropertyValue('corner-shape') || 'round'",
				),
			),
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.markdown-table')[1].scrollWidth>document.querySelectorAll('.markdown-table')[1].clientWidth",
			),
			true,
		);
		await writeFile(join(root, "evidence.json"), JSON.stringify(evidence, null, 2));
		console.log("RADIUS_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.destroy();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
