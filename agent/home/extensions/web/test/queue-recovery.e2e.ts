import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("失败队列保留重试、编辑、删除，重试不丢图片和技能且不重复投递", { timeout: 60_000 }, async () => {
	const artifacts = join(await testRoot(), "queue-recovery");
	await mkdir(artifacts, { recursive: true });
	const home = await mkdtemp(join(artifacts, "home-"));
	await configureTestModel(home);
	await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme: "dark" }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	let fail = true;
	const calls: { text: string; images: unknown }[] = [];
	slot.session.prompt = async (text, options) => {
		calls.push({ text, images: options?.images });
		if (fail) throw new Error("测试服务暂不可用");
	};
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const path = `/api/sessions/${slot.id}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "test" }),
		api,
	);
	const send = (suffix: string, value: unknown, key = randomUUID()) =>
		fetch(`${origin}${path}/${suffix}`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": key,
			},
			body: JSON.stringify(value),
		});
	const state = async () => (await (await fetch(`${origin}${path}`)).json()).queue;
	const waitFailed = async () => {
		for (let i = 0; i < 100; i++) {
			const queue = await state();
			if (queue.items.at(-1)?.status === "failed") return queue;
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		throw new Error("failed item not reached");
	};
	let browser: BrowserProbe | undefined;
	try {
		await listenWebServer(server, port);
		const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=";
		assert.equal(
			(
				await send("messages", {
					id: "recover",
					text: "你好",
					delivery: "queue",
					images: [{ mimeType: "image/png", data: png }],
					skill: "sys",
				})
			).status,
			202,
		);
		const failed = await waitFailed();
		const key = randomUUID();
		const input = { version: failed.version };
		assert.equal((await send("queue/recover/retry", input, key)).status, 200);
		await waitFailed();
		assert.equal((await send("queue/recover/retry", input, key)).status, 200);
		assert.equal((await send("queue/recover/retry", input)).status, 409);
		assert.equal(calls.length, 2, "同一重试请求只投递一次");
		browser = await BrowserProbe.launch(home);
		await browser.navigate(`${origin}/s/${slot.id}`);
		await browser.until("document.querySelector('.queue-panel')?.innerText.includes('投递失败')", "失败队列显示");
		assert.equal(await browser.evaluate("document.querySelector('.queue-steer')?.textContent.trim()"), "重试");
		assert.equal(
			await browser.evaluate(
				"!!document.querySelector('[aria-label=\"编辑排队消息\"]') && !!document.querySelector('[aria-label=\"删除排队消息\"]')",
			),
			true,
		);
		await browser.until("document.documentElement.dataset.theme==='dark'", "深色主题就绪");
		await browser.screenshot(join(artifacts, "failed-dark.png"));
		await browser.evaluate("document.documentElement.dataset.theme='light'");
		await browser.screenshot(join(artifacts, "failed-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.click('[aria-label="编辑排队消息"]');
		await browser.fill(".queue-edit-input", "检查系统");
		await browser.evaluate(
			"Array.from(document.querySelectorAll('.queue-item button')).find(b=>b.textContent==='保存').click()",
		);
		await browser.until(
			"document.querySelector('.queue-item')?.innerText.includes('检查系统') && !document.querySelector('.queue-edit-input')",
			"失败项保存编辑",
		);
		assert.equal(calls.length, 2, "编辑不触发投递");
		fail = false;
		await browser.click(".queue-steer");
		await browser.until("!document.querySelector('.queue-panel')", "重试成功移除队列卡片");
		assert.equal(calls.length, 3);
		assert.equal(calls[2].text, "/skill:sys 检查系统");
		assert.deepEqual(calls[2].images, calls[0].images);
		assert.equal((await state()).items.length, 1, "重试沿用原消息 ID");
		fail = true;
		await send("messages", { id: "remove", text: "删除失败消息", delivery: "queue" });
		await waitFailed();
		await browser.until(
			"document.querySelector('.queue-item')?.innerText.includes('删除失败消息')",
			"另一失败项显示",
		);
		await browser.click('[aria-label="删除排队消息"]');
		await browser.until("!document.querySelector('.queue-panel')", "删除失败项");
		assert.equal((await state()).items.at(-1).status, "cancelled");
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
