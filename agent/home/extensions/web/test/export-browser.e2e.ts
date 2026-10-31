import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import puppeteer from "puppeteer-core";
import { ExportBrowser } from "../server/exports/browser.ts";
import { testRoot } from "./support.ts";

test("Puppeteer 导出浏览器：复用进程、任务隔离、取消、崩溃重建与退出清理", { timeout: 45_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-browser-"));
	const work = join(root, "work");
	await mkdir(work);
	const server = createServer((_request, response) => response.end("<!DOCTYPE html><title>fixture</title>"));
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	server.unref();
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const url = `http://127.0.0.1:${address.port}`;
	const browser = new ExportBrowser({ workDir: work, idleMs: 30_000 });
	const signal = new AbortController().signal;
	try {
		const first = await browser.run(signal, async (page) => {
			await page.goto(url);
			await page.evaluate(() => localStorage.setItem("secret", "first-job"));
			await page
				.browserContext()
				.setCookie({ name: "secret", value: "first-job", domain: new URL(url).hostname, path: "/" });
			return { page, pid: page.browser().process()?.pid };
		});
		assert.ok(first.pid);
		assert.equal(first.page.isClosed(), true);
		const second = await browser.run(signal, async (page) => {
			await page.goto(url);
			assert.equal(await page.evaluate(() => localStorage.getItem("secret")), null);
			assert.equal(await page.evaluate(() => document.cookie), "");
			return page.browser().process()?.pid;
		});
		assert.equal(second, first.pid);
		const controller = new AbortController();
		await assert.rejects(
			browser.run(controller.signal, async (page) => {
				controller.abort();
				await page.waitForFunction(() => false, { timeout: 15_000 });
			}),
		);
		await assert.rejects(
			browser.run(signal, async (page) => {
				page.browser().process()?.kill();
				await page.waitForFunction(() => false, { timeout: 15_000 });
			}),
		);
		const recovered = await Promise.all(
			Array.from({ length: 6 }, () =>
				browser.run(signal, async (page) => {
					await page.goto(url);
					return page.browser().process()?.pid;
				}),
			),
		);
		assert.ok(recovered[0]);
		assert.notEqual(recovered[0], first.pid);
		assert.equal(new Set(recovered).size, 1, "并发恢复共用同一个新进程");
		await browser.close();
		await browser.close();
		assert.deepEqual(await readdir(work), []);
		await assert.rejects(
			browser.run(signal, async () => 1),
			/退出/,
		);
	} finally {
		await browser.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});

test("浏览器启动中退出：阻止任务执行并等待进程和配置回收", { timeout: 20_000 }, async (t) => {
	const root = await mkdtemp(join(await testRoot(), "export-start-close-"));
	const browser = new ExportBrowser({ workDir: root });
	let markLaunched!: () => void, release!: () => void;
	const launched = new Promise<void>((resolve) => {
		markLaunched = resolve;
	});
	const released = new Promise<void>((resolve) => {
		release = resolve;
	});
	const launch = puppeteer.launch.bind(puppeteer);
	t.mock.method(puppeteer, "launch", async (options: Parameters<typeof puppeteer.launch>[0]) => {
		const instance = await launch(options);
		markLaunched();
		await released;
		return instance;
	});
	let executed = false;
	const operation = browser.run(new AbortController().signal, async () => {
		executed = true;
	});
	const rejected = assert.rejects(operation, { code: "export_stopping" });
	try {
		await launched;
		const closing = browser.close();
		release();
		await Promise.all([closing, rejected]);
		assert.equal(executed, false);
		assert.deepEqual(await readdir(root), []);
	} finally {
		release();
		await browser.close();
		await rejected;
	}
});

test("浏览器空闲回收；显式路径无效返回可操作错误", { timeout: 20_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-idle-"));
	const browser = new ExportBrowser({ workDir: root, idleMs: 50 });
	try {
		const child = await browser.run(new AbortController().signal, async (page) => page.browser().process());
		assert.ok(child);
		for (let attempt = 0; attempt < 100 && child.exitCode === null; attempt++)
			await new Promise((resolve) => setTimeout(resolve, 50));
		assert.notEqual(child.exitCode, null);
		const missing = new ExportBrowser({ workDir: root, executablePath: join(root, "missing.exe") });
		await assert.rejects(
			missing.run(new AbortController().signal, async () => 1),
			{ code: "export_browser_missing" },
		);
		await missing.close();
	} finally {
		await browser.close();
	}
});
