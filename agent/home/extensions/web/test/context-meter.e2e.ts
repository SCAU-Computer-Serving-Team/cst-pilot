import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

type ContextGeometry = {
	buttonHeight: number;
	buttonShadow: string;
	ringWidth: number;
	ringHeight: number;
	haloWidth: string;
	haloHeight: string;
	haloRadius: string;
	haloShadow: string;
	haloLeft: string;
	haloTop: string;
	focused: boolean;
	panelLeft: number;
	panelRight: number;
};

test("上下文入口：悬停与键盘焦点呈圆形，保留操作栏高度", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "context-meter");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme: "light" }));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", enabledModels: ["probe/one"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: "http://127.0.0.1:9/v1",
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "上下文验收模型",
							reasoning: false,
							input: ["text"],
							contextWindow: 10000,
							maxTokens: 100,
						},
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		for (const width of [1440, 800]) {
			await browser.call("Emulation.setDeviceMetricsOverride", {
				width,
				height: 1000,
				deviceScaleFactor: 1,
				mobile: false,
			});
			await browser.navigate(`${origin}/s/${slot.id}`);
			await browser.until(
				"document.querySelector('.model-label')?.textContent.includes('上下文验收模型')",
				"上下文入口就绪",
			);
			if (width <= 960) {
				await browser.click('button[aria-label="收起侧栏"]');
				await browser.until(
					"document.querySelector('.sidebar').getBoundingClientRect().right<=0.5",
					"半屏侧栏收起",
				);
			}
			for (const theme of ["light", "dark"]) {
				await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
				for (const state of ["hover", "focus"]) {
					await browser.call("Input.dispatchMouseEvent", { type: "mouseMoved", x: width - 1, y: 1 });
					await browser.evaluate("document.activeElement?.blur()");
					if (state === "hover") {
						const point = await browser.evaluate<{ x: number; y: number }>(
							"(()=>{const r=document.querySelector('.context-meter').getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()",
						);
						await browser.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
					} else {
						await browser.call("Input.dispatchKeyEvent", {
							type: "keyDown",
							key: "Tab",
							code: "Tab",
							windowsVirtualKeyCode: 9,
						});
						await browser.call("Input.dispatchKeyEvent", {
							type: "keyUp",
							key: "Tab",
							code: "Tab",
							windowsVirtualKeyCode: 9,
						});
						await browser.evaluate("document.querySelector('.context-meter').focus()");
					}
					await browser.until(
						"document.querySelector('.context-meter-wrap')?.dataset.open==='true'",
						"打开上下文面板",
					);
					const geometry: ContextGeometry = await browser.evaluate<ContextGeometry>(`(()=>{
							const button=document.querySelector('.context-meter'),ring=button.querySelector('.context-ring');
							const rect=button.getBoundingClientRect(),r=ring.getBoundingClientRect(),halo=getComputedStyle(button,'::before'),panel=document.querySelector('.context-panel').getBoundingClientRect();
							return {buttonHeight:rect.height,buttonShadow:getComputedStyle(button).boxShadow,ringWidth:r.width,ringHeight:r.height,haloWidth:halo.width,haloHeight:halo.height,haloRadius:halo.borderRadius,haloShadow:halo.boxShadow,haloLeft:halo.left,haloTop:halo.top,focused:button.matches(':focus-visible'),panelLeft:panel.left,panelRight:panel.right};
						})()`);
					const label = `chat-${width}-${theme}-${state}`;
					await writeFile(join(root, `${label}.json`), JSON.stringify(geometry, null, 2));
					await browser.screenshot(join(root, `${label}.png`));
					assert.equal(geometry.buttonHeight, 26, `${label} 保留点击区域高度`);
					assert.equal(geometry.ringWidth, 14);
					assert.equal(geometry.ringHeight, 14);
					assert.equal(geometry.haloWidth, "14px", `${label} 悬停环宽度等于进度环`);
					assert.equal(geometry.haloHeight, geometry.haloWidth, `${label} 悬停环宽高相等`);
					assert.equal(geometry.haloRadius, "50%");
					assert.match(geometry.haloShadow, /0px 0px 0px 3px/);
					assert.equal(geometry.buttonShadow, "none", `${label} 高按钮不绘制椭圆阴影`);
					assert.equal(Number.parseFloat(geometry.haloLeft), 0);
					assert.equal(Number.parseFloat(geometry.haloTop), 6);
					if (state === "focus") assert.equal(geometry.focused, true);
					assert.ok(geometry.panelLeft >= 15 && geometry.panelRight <= width - 15, `${label} 面板不越过视口边缘`);
					await browser.call("Input.dispatchKeyEvent", {
						type: "keyDown",
						key: "Escape",
						code: "Escape",
						windowsVirtualKeyCode: 27,
					});
					await browser.call("Input.dispatchKeyEvent", {
						type: "keyUp",
						key: "Escape",
						code: "Escape",
						windowsVirtualKeyCode: 27,
					});
					await browser.until(
						"document.querySelector('.context-meter-wrap')?.dataset.open==='false'",
						"Esc 关闭上下文面板",
					);
				}
			}
		}
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
