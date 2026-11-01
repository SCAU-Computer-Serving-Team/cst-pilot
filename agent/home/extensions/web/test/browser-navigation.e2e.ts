import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { BrowserProbe } from "./browser.ts";
import { testRoot } from "./support.ts";

test("刷新销毁执行环境时，浏览器等待继续观察新文档", { timeout: 20000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "browser-navigation-"));
	const probe = await BrowserProbe.launch(root);
	try {
		await probe.call("Page.addScriptToEvaluateOnNewDocument", { source: "window.__contextReady = true" });
		const waiting = probe
			.until("window.__contextReady || new Promise(() => { window.__contextPending = true })", "刷新后的文档", 10000)
			.then(
				() => ({ ready: true }),
				(error: unknown) => ({ error: error instanceof Error ? error.message : String(error) }),
			);
		await probe.until("window.__contextPending === true", "旧文档已进入等待");
		await probe.call("Page.reload");
		assert.deepEqual(await waiting, { ready: true });
	} finally {
		await probe.close();
	}
});
