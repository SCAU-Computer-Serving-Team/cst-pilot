import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("跨项目文件补全：从A切到B，浏览器缓存和实际清单都使用历史cwd", { timeout: 45000 }, async () => {
	const root = join(await testRoot(), "cwd-completion");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-")),
		a = join(home, "A"),
		b = join(home, "B");
	await mkdir(a);
	await mkdir(b);
	await writeFile(join(a, "only-A.txt"), "A");
	await writeFile(join(b, "only-B.txt"), "B");
	const model = createServer(async (req, res) => {
		for await (const _ of req) {
		}
		res.writeHead(200, { "Content-Type": "text/event-stream" });
		res.end(
			`data: ${JSON.stringify({ id: "cwd", choices: [{ index: 0, delta: { role: "assistant", content: "READY" }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ id: "cwd", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const mp = await freePort();
	await new Promise<void>((r) => model.listen(mp, "127.0.0.1", r));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", defaultTools: ["read"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${mp}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{ id: "one", name: "one", reasoning: false, input: ["text"], contextWindow: 10000, maxTokens: 100 },
					],
				},
			},
		}),
	);
	const options = {
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	};
	const pool = new WebSessionPool({ ...options, cwd: a }),
		other = new WebSessionPool({ ...options, cwd: b });
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`,
		api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "cwd" }),
		api,
	);
	let browser: BrowserProbe | undefined;
	try {
		const one = await pool.create(),
			two = await other.create();
		await one.session.prompt("Project A");
		await two.session.prompt("Project B");
		await other.close();
		await listenWebServer(server, port);
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/s/${one.id}`);
		await browser.fill(".composer-input", "@only-");
		await browser.until(
			"document.querySelector('.file-suggestions')?.textContent.includes('only-A.txt')",
			"A的文件补全",
		);
		await browser.click(`.sidebar-session[href="/s/${two.id}"]`);
		await browser.until(
			`location.pathname==='/s/${two.id}'&&document.querySelector('.chat-header h1')?.textContent==='Project B'&&!document.querySelector('[data-home-transition]')`,
			"切换到B",
		);
		await browser.fill(".composer-input", "@only-");
		await browser.until(
			"document.querySelector('.file-suggestions')?.textContent.includes('only-B.txt')",
			"B的历史文件补全",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.file-suggestions')?.textContent.includes('only-A.txt')"),
			false,
		);
		await browser.screenshot(join(root, "project-B-completion.png"));
		await browser.click(`.sidebar-session[href="/s/${one.id}"]`);
		await browser.until("document.querySelector('.chat-header h1')?.textContent==='Project A'", "回到A页面提交完成");
		await browser.fill(".composer-input", "@only-");
		await browser.until(
			"document.querySelector('.file-suggestions')?.textContent.includes('only-A.txt')",
			"回到A重新读取",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.file-suggestions')?.textContent.includes('only-B.txt')"),
			false,
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await other.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		model.closeAllConnections();
		await new Promise<void>((r) => model.close(() => r()));
		await rm(home, { recursive: true, force: true });
	}
});
