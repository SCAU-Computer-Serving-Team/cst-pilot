import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("设置页预加载目标数据后再滑动，快照不含加载占位", { timeout: 60_000 }, async () => {
	const home = await mkdtemp(join(await testRoot(), "cst-settings-transition-"));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	});
	const artifacts = join(await testRoot(), "settings-exit");
	await mkdir(artifacts, { recursive: true });
	const slot = await pool.create();
	slot.session.sessionManager.appendMessage({ role: "user", content: "切页测试会话", timestamp: Date.now() });
	slot.session.sessionManager.appendMessage({
		role: "assistant",
		content: [{ type: "text", text: "切页测试回答" }],
		api: "openai-completions",
		provider: "probe",
		model: "one",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop",
		timestamp: Date.now(),
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "test" }),
		api,
	);
	let browser: BrowserProbe | undefined;
	try {
		await listenWebServer(server, port);
		browser = await BrowserProbe.launch(home);
		await browser.call("Page.addScriptToEvaluateOnNewDocument", {
			source: `(() => {
				const original = window.fetch;
				window.fetch = (...args) => String(args[0]).endsWith('/api/settings')
					? new Promise(resolve => setTimeout(resolve, 500)).then(() => original(...args))
					: original(...args);
			})()`,
		});
		await browser.navigate(`${origin}/settings/shortcuts`);
		await browser.click('a[href="/settings/general"]');
		await browser.until("location.pathname==='/settings/general'", "常规分类开始过渡");
		await browser.until(
			"document.getAnimations().some(a=>a.effect?.pseudoElement==='::view-transition-old(settings-page)')",
			"设置分类快照正在滑动",
		);
		const settingsMotion = await browser.evaluate<
			{ pseudo: string; duration: number; frames: { transform?: string }[] }[]
		>(
			"document.getAnimations().filter(a=>a.effect?.pseudoElement?.includes('settings-page')).map(a=>({pseudo:a.effect.pseudoElement,duration:a.effect.getTiming().duration,frames:a.effect.getKeyframes()}))",
		);
		const oldPage = settingsMotion.find((item) => item.pseudo === "::view-transition-old(settings-page)");
		const newPage = settingsMotion.find((item) => item.pseudo === "::view-transition-new(settings-page)");
		assert.equal(oldPage?.duration, 250);
		assert.equal(newPage?.duration, 250);
		assert.match(oldPage?.frames.at(-1)?.transform ?? "", /translate(?:X)?\(8px\)/);
		assert.match(newPage?.frames[0]?.transform ?? "", /translate(?:X)?\(-8px\)/);
		assert.equal(
			await browser.evaluate(
				"getComputedStyle(document.documentElement,'::view-transition-old(settings-page)').mixBlendMode",
			),
			"plus-lighter",
			"交叉淡化应避免快照透明度叠加导致的闪白",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-route')?.innerText.includes('界面主题')"),
			true,
			"页面快照应直接包含已加载的设置项",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-route')?.innerText.includes('正在加载')"),
			false,
		);
		await browser.until("document.getAnimations().length===0", "内部切换结束");
		for (const theme of ["light", "dark"]) {
			await fetch(`${origin}/api/settings`, {
				method: "PATCH",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": `theme-${theme}`,
				},
				body: JSON.stringify({ theme }),
			});
			for (const destination of ["/", `/s/${slot.id}`]) {
				await browser.navigate(`${origin}/settings`);
				await browser.until(
					"document.querySelector('.settings-route')?.innerText.includes('模型服务')",
					"模型服务加载",
				);
				await browser.until("document.getAnimations().length===0", "设置入场结束");
				await browser.until(`document.documentElement.dataset.theme==='${theme}'`, "主题就绪");
				await browser.evaluate(`(() => {
					const original = document.startViewTransition.bind(document);
					document.startViewTransition = (...args) => {
						window.exitBackground = getComputedStyle(document.querySelector('.settings-main')).backgroundColor;
						const transition = original(...args);
						transition.ready.then(() => {
							window.exitSnapshots = document.getAnimations().map(animation => {
								animation.pause(); animation.currentTime = 40;
								return animation.effect?.pseudoElement;
							});
						});
						return transition;
					};
				})()`);
				await browser.click(`a[href="${destination}"]`);
				await browser.until("Array.isArray(window.exitSnapshots)", "捕获设置退出快照");
				const snapshots = await browser.evaluate<string[]>("window.exitSnapshots");
				assert.ok(
					snapshots.includes("::view-transition-old(settings-surface)"),
					"设置背景与内容应共用整页退出快照",
				);
				assert.ok(!snapshots.includes("::view-transition-old(settings-page)"), "退出设置时不单独提取设置内容");
				assert.ok(!snapshots.includes("::view-transition-old(workspace-header)"), "整页退出包含标题");
				const target = destination === "/" ? "home" : "chat";
				await browser.screenshot(join(artifacts, `${theme}-${target}-40ms.png`));
				assert.deepEqual(
					await browser.evaluate(`(() => {
					const effect = document.getAnimations().find(a=>a.effect?.pseudoElement==='::view-transition-old(settings-surface)').effect;
					return { duration: effect.getTiming().duration, opacity: effect.getKeyframes().at(-1).opacity };
				})()`),
					{ duration: 80, opacity: "0" },
				);
				await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
				await browser.until("document.getAnimations().length===0", "整页退出结束");
				await browser.screenshot(join(artifacts, `${theme}-${target}-finished.png`));
			}
		}
		await browser.navigate(`${origin}/settings`);
		await browser.evaluate(`(() => {
			const original = document.startViewTransition.bind(document);
			document.startViewTransition = (...args) => {
				const transition = original(...args);
				transition.ready.then(() => { window.accountSnapshots = document.getAnimations().map(a => a.effect?.pseudoElement); });
				return transition;
			};
		})()`);
		await browser.click(".sidebar-account-trigger");
		await browser.click('a[href="/account"]');
		await browser.until("location.pathname==='/account'", "进入账号信息");
		await browser.until("Array.isArray(window.accountSnapshots)", "账号页面快照");
		assert.ok(
			await browser.evaluate("window.accountSnapshots.includes('::view-transition-new(other-page-content)')"),
		);
		assert.equal(
			await browser.evaluate(
				"!!document.querySelector('.settings-workspace') || !!document.querySelector('.settings-route')",
			),
			false,
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
