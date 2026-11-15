import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ModelCatalog } from "../../runtime/model-catalog.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("交互竞态：重命名生命周期与排序后的主动焦点切换", { timeout: 90_000 }, async (t) => {
	const root = await mkdtemp(join(await testRoot(), "interaction-races-"));
	const home = join(root, "home");
	await mkdir(home);
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "test-probe", defaultModel: "alpha", defaultTools: ["read", "ls"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				"test-probe": {
					api: "openai-completions",
					baseUrl: "http://127.0.0.1:9/v1",
					apiKey: "test-only",
					models: ["Alpha", "Beta", "Gamma"].map((name) => ({
						id: name.toLowerCase(),
						name,
						reasoning: false,
						input: ["text"],
						contextWindow: 10000,
						maxTokens: 100,
					})),
				},
			},
		}),
	);
	const catalog = new ModelCatalog(home, { automatic: false });
	t.mock.method(catalog, "refresh", async () => ({ aborted: false, errors: new Map() }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		catalog,
		withLoader: (load) => load(),
	});
	const services = await pool.getServices();
	services.settingsManager.setEnabledModels(["test-probe/alpha", "test-probe/beta", "test-probe/gamma"]);
	const sessions = [];
	for (const title of ["会话 A", "会话 B"]) {
		const slot = await pool.createWithKey(randomUUID());
		slot.session.setSessionName(title);
		slot.session.sessionManager.appendMessage({ role: "user", content: title, timestamp: Date.now() });
		sessions.push(slot);
	}
	const [first, second] = sessions;
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "interaction-races" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		const row = (id: string) => `.sidebar-session[data-session-id="${id}"]`;
		async function openRename(id: string) {
			await probe.until(`!!document.querySelector(${JSON.stringify(row(id))})`, "重命名目标就绪");
			await probe.evaluate(
				"Promise.all(document.querySelector('.sidebar').getAnimations({ subtree: true }).map(animation => animation.finished.catch(() => {})))",
			);
			await probe.evaluate(`(() => {
				const row = document.querySelector(${JSON.stringify(row(id))});
				row.focus();
				row.dispatchEvent(new KeyboardEvent('keydown', { key: 'ContextMenu', bubbles: true }));
			})()`);
			await probe.until(
				"document.activeElement?.closest('.session-menu') && document.activeElement.textContent.trim() === '重命名'",
				"键盘菜单聚焦重命名",
			);
			await probe.evaluate("document.querySelector('.session-menu button:has(svg.lucide-pencil)').click()");
			await probe.until("!!document.querySelector('.session-rename-dialog[open] input')", "重命名弹窗打开");
		}
		await probe.navigate(`${origin}/s/${first.id}`);
		await probe.until(
			"!!document.querySelector('.sidebar-session') && !document.querySelector('.sidebar[inert]')",
			"侧栏就绪",
		);
		await probe.evaluate(
			"Promise.all(document.querySelector('.sidebar').getAnimations({ subtree: true }).map(animation => animation.finished.catch(() => {})))",
		);
		await probe.click(row(second.id));
		await probe.until(`location.pathname === '/s/${second.id}'`, "建立站内返回记录");
		await t.test("旧重命名响应不能关闭后开的弹窗或丢失输入", async () => {
			await probe.evaluate(`(() => {
				window.__renameFetch = window.fetch;
				window.fetch = async (...args) => {
					if (args[1]?.method === 'PATCH' && String(args[0]).startsWith('/api/sessions/')) {
						await new Promise(resolve => window.__releaseRename = resolve);
					}
					return window.__renameFetch(...args);
				};
			})()`);
			await openRename(first.id);
			await probe.fill(".session-rename-dialog input", "A 已保存");
			await probe.click(".session-rename-dialog button[type=submit]");
			await probe.until("typeof window.__releaseRename === 'function'", "旧重命名请求等待响应");
			await probe.evaluate("history.back()");
			await probe.until(
				`location.pathname === '/s/${first.id}' && !document.querySelector('.session-rename-dialog')`,
				"返回后卸载旧弹窗",
			);
			await openRename(second.id);
			await probe.fill(".session-rename-dialog input", "B 未保存");
			await probe.evaluate("window.__releaseRename()");
			await probe.until(
				`document.querySelector(${JSON.stringify(row(first.id))})?.textContent.trim() === 'A 已保存'`,
				"旧请求完成并刷新列表",
			);
			await probe.evaluate("new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
			assert.equal(
				await probe.evaluate("document.querySelector('.session-rename-dialog[open] input')?.value"),
				"B 未保存",
				"旧请求完成后保留新弹窗与未保存输入",
			);
			await probe.evaluate(`(() => {
				window.fetch = window.__renameFetch;
				document.querySelector('.session-rename-dialog').dispatchEvent(new Event('cancel', { cancelable: true }));
			})()`);
			await probe.until("!document.querySelector('.session-rename-dialog')", "关闭未保存的新弹窗");
		});
		await t.test("重命名取消后关闭动画期间按 Enter 不发送保存请求", async () => {
			await openRename(second.id);
			await probe.fill(".session-rename-dialog input", "B 已取消");
			await probe.evaluate(`(() => {
				window.__cancelWrites = 0;
				window.fetch = (...args) => {
					if (args[1]?.method === 'PATCH') window.__cancelWrites++;
					return window.__renameFetch(...args);
				};
			})()`);
			for (const [key, code] of [
				["Escape", 27],
				["Enter", 13],
			] as const) {
				await probe.call("Input.dispatchKeyEvent", {
					type: "keyDown",
					key,
					code: key,
					windowsVirtualKeyCode: code,
					...(key === "Enter" ? { text: "\r" } : {}),
				});
				await probe.call("Input.dispatchKeyEvent", { type: "keyUp", key, code: key, windowsVirtualKeyCode: code });
			}
			await probe.until("!document.querySelector('.session-rename-dialog')", "取消关闭完成");
			assert.equal(await probe.evaluate("window.__cancelWrites"), 0, "取消后不再保存名称");
			assert.equal((await pool.list()).find((session) => session.id === second.id)?.title, "会话 B");
			await probe.evaluate("window.fetch = window.__renameFetch");
		});
		await t.test("排序保存期间允许转移焦点并完整输入搜索词", async () => {
			await probe.navigate(`${origin}/settings/models`);
			const grip = 'button.settings-grip[data-model-id="test-probe/alpha"]';
			await probe.until(`!!document.querySelector(${JSON.stringify(grip)})`, "排序柄就绪");
			await probe.evaluate(`(() => {
				window.__sortFetch = window.fetch;
				window.fetch = async (...args) => {
					const response = await window.__sortFetch(...args);
					if (String(args[0]) === '/api/models/scoped' && args[1]?.method === 'POST') {
						await new Promise(resolve => window.__releaseSort = resolve);
					}
					return response;
				};
				document.querySelector(${JSON.stringify(grip)}).focus();
			})()`);
			await probe.call("Input.dispatchKeyEvent", {
				type: "keyDown",
				key: "ArrowDown",
				code: "ArrowDown",
				windowsVirtualKeyCode: 40,
				modifiers: 1,
			});
			await probe.call("Input.dispatchKeyEvent", {
				type: "keyUp",
				key: "ArrowDown",
				code: "ArrowDown",
				windowsVirtualKeyCode: 40,
			});
			await probe.until("typeof window.__releaseSort === 'function'", "排序响应等待放行");
			assert.equal(
				await probe.evaluate(`document.activeElement === document.querySelector(${JSON.stringify(grip)})`),
				true,
			);
			await probe.click(".settings-search input");
			for (const text of ["A", "l"]) {
				await probe.call("Input.insertText", { text });
				await probe.evaluate("new Promise(resolve => requestAnimationFrame(resolve))");
			}
			assert.equal(await probe.evaluate("document.querySelector('.settings-search input').value"), "Al");
			assert.equal(await probe.evaluate("document.activeElement.matches('.settings-search input')"), true);
			await probe.evaluate("window.__releaseSort(); window.fetch = window.__sortFetch");
			await probe.until(
				"document.querySelector('.settings-model-card')?.getAttribute('aria-busy') === 'false'",
				"排序保存完成",
			);
			assert.equal(await probe.evaluate("document.activeElement.matches('.settings-search input')"), true);
		});
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
