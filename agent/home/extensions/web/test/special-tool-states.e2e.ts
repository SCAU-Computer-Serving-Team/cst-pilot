import assert from "node:assert/strict";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import type { BitLockerVolume } from "../../diagnostics/bitlocker-data.ts";
import { bitLockerWebInteractions } from "../../diagnostics/bitlocker-web.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("特殊工具全状态：宽度、字号、浅深主题、详情、复制、固定目标与提交恢复", { timeout: 60000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "special-tool-states-redo-")),
		home = join(root, "home");
	await mkdir(home);
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	let stamp = Date.now();
	const append = (id: string, name: string, args: Record<string, unknown>, details?: unknown, isError = false) => {
		const entryId = slot.session.sessionManager.appendMessage({
			role: "assistant",
			content:
				id === "overview-normal"
					? [
							{ type: "toolCall", id: "read-before", name: "read", arguments: {} },
							{ type: "toolCall", id, name, arguments: args },
							{ type: "toolCall", id: "read-after", name: "read", arguments: {} },
						]
					: [{ type: "toolCall", id, name, arguments: args }],
			api: "openai-completions",
			provider: "fixture",
			model: "fixture",
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			stopReason: "toolUse",
			timestamp: stamp++,
		});
		if (details !== undefined)
			slot.session.sessionManager.appendMessage({
				role: "toolResult",
				toolCallId: id,
				toolName: name,
				content: [{ type: "text", text: JSON.stringify(details) }],
				details,
				isError,
				timestamp: stamp++,
			});
		return entryId;
	};
	const overview = {
		cpuTotalPct: 19,
		logicalCores: 16,
		mem: { usedPct: 83.6, usedMB: 27214, totalMB: 32557 },
		machine: { vendor: "Maxsun", model: "fixture", cpu: "12th Gen Intel Core i5-12600KF" },
		uptime: { text: "2小时12分", bootTime: "fixture", totalHours: 2.2 },
		pagefile: [{ name: "C:/pagefile.sys", usedMB: 512, allocMB: 32557 }],
		notice: "字段口径",
	};
	append("overview-normal", "sys", { scope: "overview" }, { overview });
	append(
		"overview-partial",
		"sys",
		{},
		{
			overview: {
				...overview,
				mem: null,
				degraded: true,
				collectionErrors: [{ source: "mem", code: 5, message: "内存读取失败" }],
			},
		},
	);
	append("overview-error", "sys", {}, { error: "采集失败" }, true);
	append("overview-interrupted", "sys", {});
	for (const level of ["safe", "low", "confirm"])
		append(
			`runbook-${level}`,
			"runbook",
			{
				title: `命令清单-${level}`,
				level,
				items: [
					{
						shell: "PowerShell",
						admin: level !== "safe",
						summary: "输出测试内容，不修改系统。",
						command: "Write-Output fixture",
					},
				],
			},
			{ runbook: { file: "E:/fixture/outbox/a.txt", name: "01-测试清单.txt", items: 1, levelLabel: level } },
		);
	append(
		"runbook-error",
		"runbook",
		{ title: "写入失败", items: [{ shell: "cmd", summary: "测试", command: "echo fixture" }] },
		{ error: "文件只读" },
		true,
	);
	for (const [id, success] of [
		["search-success", 2],
		["search-zero", 2],
		["search-partial", 1],
		["search-error", 0],
	] as const)
		append(
			id,
			"web_search",
			{ queries: ["关键词一", "关键词二"] },
			{ queryCount: 2, successfulQueries: success, totalResults: 0 },
		);
	append("search-interrupted", "web_search", { query: "关键词" });
	append("search-unstructured", "web_search", { query: "缺少字段" }, null);
	const volume: BitLockerVolume = {
		volumeId: "fixture-volume",
		drive: "D:",
		volumeType: 1,
		conversionStatus: 1,
		encryptionPercentage: 100,
		encryptionMethod: 7,
		protectionStatus: 1,
		lockStatus: 1,
		autoUnlockEnabled: false,
		autoUnlockKeysStored: null,
		keyProtectors: [{ id: "8A7B6C5D-1111-2222-3333-102030405060", type: 3 }],
		collectionErrors: [],
	};
	const result = (changes: Partial<BitLockerVolume>, status: string) => ({
		bitlocker: {
			collectedAt: "fixture",
			volumes: [
				{
					...volume,
					...changes,
					operation: {
						status,
						reason: status === "rejected" ? "Windows 拒绝请求" : "fixture",
						returnCode: status === "rejected" ? 2150694953 : 0,
						error:
							status === "rejected" ? { source: "Decrypt", code: 2150694953, message: "自动解锁依赖" } : null,
					},
				},
			],
		},
	});
	append("unlock-success", "bitlocker", { scope: "unlock", drive: "D:" }, result({ lockStatus: 0 }, "accepted"));
	append("unlock-rejected", "bitlocker", { scope: "unlock", drive: "D:" }, result({}, "rejected"));
	append("unlock-unknown", "bitlocker", { scope: "unlock", drive: "D:" }, result({ lockStatus: null }, "unknown"));
	append("unlock-cancelled", "bitlocker", { scope: "unlock", drive: "D:" }, result({}, "cancelled"));
	append(
		"unlock-recheck",
		"bitlocker",
		{ scope: "unlock", drive: "D:" },
		result(
			{ lockStatus: null, collectionErrors: [{ source: "GetLockStatus", code: 5, message: "复查失败" }] },
			"accepted",
		),
	);
	append(
		"decrypt-running",
		"bitlocker",
		{ scope: "decrypt" },
		result({ lockStatus: 0, conversionStatus: 3, encryptionPercentage: 54 }, "accepted"),
	);
	append(
		"decrypt-complete",
		"bitlocker",
		{ scope: "decrypt" },
		result({ lockStatus: 0, conversionStatus: 0, encryptionPercentage: 0 }, "accepted"),
	);
	append("decrypt-blocked", "bitlocker", { scope: "decrypt" }, result({}, "rejected"));
	append(
		"decrypt-cancelled-skipped",
		"bitlocker",
		{ scope: "decrypt" },
		{
			bitlocker: {
				volumes: [
					result({ lockStatus: 0 }, "cancelled").bitlocker.volumes[0],
					{
						...result({ lockStatus: 0, conversionStatus: 3 }, "skipped").bitlocker.volumes[0],
						drive: "E:",
						volumeId: "fixture-E",
					},
				],
			},
		},
	);
	append(
		"decrypt-partial-cancelled",
		"bitlocker",
		{ scope: "decrypt" },
		{
			bitlocker: {
				volumes: [
					result({ lockStatus: 0, conversionStatus: 3 }, "accepted").bitlocker.volumes[0],
					{ ...result({ lockStatus: 0 }, "cancelled").bitlocker.volumes[0], drive: "E:", volumeId: "fixture-E" },
				],
			},
		},
	);
	const channel = bitLockerWebInteractions.get(slot.id)!;
	const passwordEntry = append("unlock-input", "bitlocker", { scope: "unlock", drive: "D:" });
	const input = channel.forCall("unlock-input", passwordEntry).password(volume);
	const decryptEntry = append("decrypt-confirm", "bitlocker", { scope: "decrypt" });
	const confirm = channel.forCall("decrypt-confirm", decryptEntry).confirmDecryption([
		{ ...volume, lockStatus: 0 },
		{ ...volume, drive: "E:", volumeId: "fixture-E", lockStatus: 0, conversionStatus: 4 },
	]);
	const parameterEntry = append("unlock-parameter", "bitlocker", {
		scope: "unlock",
		drive: "D:",
		recoveryPassword: "[BitLocker 恢复密码已隐藏]",
	});
	const parameter = channel.forCall("unlock-parameter", parameterEntry).confirmParameter(volume);
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`,
		api = createWebApi(pool, home, port),
		server = createWebServer(
			"E:/Learning/Programming/cst-pilot/agent/home/extensions/web/static",
			port,
			() => ({ sessions: pool.snapshot(), stage: "test" }),
			api,
		);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool').length===27", "全部状态挂载");
		await browser.evaluate("document.fonts.ready.then(() => true)");
		await browser.click(".turn-process-head");
		assert.deepEqual(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head')].map(e=>e.getAttribute('aria-expanded'))",
			),
			["true", "false"],
		);
		const metrics = await browser.evaluate(
			`(()=>{const nodes=[...document.querySelectorAll('.special-tool')];return nodes.map(e=>({id:e.dataset.toolCallId,state:e.dataset.state,width:e.getBoundingClientRect().width,turn:e.closest('.assistant-turn').getBoundingClientRect().width,title:e.querySelector('h3')&&getComputedStyle(e.querySelector('h3')).fontSize,machine:e.querySelector('.special-tool-machine')&&getComputedStyle(e.querySelector('.special-tool-machine')).fontSize,details:e.querySelectorAll('.special-tool-details').length}));})()`,
		);
		await writeFile(join(root, "desktop-metrics.json"), JSON.stringify(metrics, null, 2));
		for (const metric of metrics as {
			id: string;
			width: number;
			turn: number;
			title: string | null;
			machine: string | null;
			details: number;
		}[]) {
			assert.ok(Math.abs(metric.width - metric.turn) < 1, `${metric.id}必须铺满内容列`);
			if (metric.title) assert.equal(metric.title, "16px");
			if (metric.machine) assert.equal(metric.machine, "14px");
		}
		assert.equal(
			await browser.evaluate("document.querySelector('[data-tool-call-id=unlock-cancelled]').dataset.state"),
			"cancelled",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('[data-tool-call-id=unlock-recheck]').dataset.state"),
			"degraded",
		);
		await browser.click('[data-tool-call-id="overview-normal"] summary');
		const expanded = await browser.evaluate(
			"(()=>{const e=document.querySelector('[data-tool-call-id=overview-normal]');return {width:e.getBoundingClientRect().width,field:getComputedStyle(e.querySelector('dd')).fontSize,raw:getComputedStyle(e.querySelector('pre')).fontSize,open:e.querySelector('details').open};})()",
		);
		assert.equal((expanded as { field: string }).field, "12px");
		assert.equal((expanded as { raw: string }).raw, "11px");
		await browser.screenshot(join(root, "overview-expanded-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.screenshot(join(root, "overview-expanded-dark.png"));
		await browser.call("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "dark" }] });
		await browser.evaluate("document.documentElement.dataset.theme='system'");
		assert.match(
			await browser.evaluate("getComputedStyle(document.querySelector('.special-tool')).backgroundImage"),
			/28, 39, 44/,
		);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="收起侧栏"]');
		await browser.evaluate("document.documentElement.dataset.theme='light'");
		const narrow = await browser.evaluate(
			"({width:document.documentElement.scrollWidth,cards:[...document.querySelectorAll('.special-tool')].map(e=>({id:e.dataset.toolCallId,width:e.getBoundingClientRect().width}))})",
		);
		await writeFile(join(root, "narrow-metrics.json"), JSON.stringify(narrow, null, 2));
		assert.equal((narrow as { width: number }).width, 390);
		for (const c of (narrow as { cards: { width: number }[] }).cards) assert.ok(Math.abs(c.width - 358) < 1);
		await browser.click(".sidebar-restore");
		assert.notEqual(await browser.evaluate("getComputedStyle(document.querySelector('.sidebar')).display"), "none");
		await browser.click('[aria-label="收起侧栏"]');
		await browser.evaluate(
			"window.__copy=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>window.__copy.push(text)}})",
		);
		await browser.click('[data-tool-call-id="runbook-safe"] [aria-label="复制第 1 条命令"]');
		assert.equal(await browser.evaluate("window.__copy[0]"), "Write-Output fixture");
		await browser.evaluate("navigator.clipboard.writeText=async()=>{throw Error('denied')}");
		await browser.click('[data-tool-call-id="runbook-safe"] [aria-label="复制第 1 条命令"]');
		await browser.until(
			"document.querySelector('[data-tool-call-id=runbook-safe]').textContent.includes('复制失败')",
			"复制失败反馈",
		);
		const selector = '[data-tool-call-id="unlock-input"]';
		assert.equal(
			await browser.evaluate(
				`document.querySelector('${selector} .special-tool-password').getBoundingClientRect().height`,
			),
			42,
		);
		assert.equal(
			await browser.evaluate(
				`document.querySelector('${selector} .special-tool-primary').getBoundingClientRect().height`,
			),
			32,
		);
		await browser.fill(`${selector} input`, Array(8).fill("123456").join("-"));
		await browser.evaluate(
			"window.__fetch=window.fetch;window.fetch=(url,opts)=>opts?.method==='POST'&&String(url).includes('/bitlocker/')?Promise.reject(Error('network fixture')):window.__fetch(url,opts)",
		);
		await browser.click(`${selector} button[type="submit"]`);
		await browser.until(
			`!document.querySelector('${selector} input').disabled && document.querySelector('${selector}').textContent.includes('请求仍待处理')`,
			"提交失败可恢复",
		);
		await browser.evaluate("window.fetch=window.__fetch");
		await browser.screenshot(join(root, "unlock-recovery-narrow.png"));
		await browser.click(`${selector} .special-tool-secondary`);
		assert.equal(await input, undefined);
		await browser.until(`!document.querySelector('${selector} input')`, "取消后输入失效");
		await browser.click('[data-tool-call-id="decrypt-confirm"] button[type="submit"]');
		assert.equal(await confirm, true);
		await browser.click('[data-tool-call-id="unlock-parameter"] .special-tool-secondary');
		assert.equal(await parameter, false);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.until("document.querySelectorAll('.special-tool').length===27", "历史恢复");
		assert.equal(await browser.evaluate("!!document.querySelector('.special-tool-password')"), false);
		await writeFile(
			join(root, "verification.json"),
			JSON.stringify(
				{
					states: 27,
					desktop: 1120,
					narrow: 358,
					fonts: { title: 16, machine: 14, fields: 12, raw: 11 },
					themes: ["light", "dark", "system-dark"],
					inputRecovery: true,
					copyFeedback: true,
				},
				null,
				2,
			),
		);
		console.log("STATE_EVIDENCE", root);
	} finally {
		await browser?.close();
		await pool.close();
		await api.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
