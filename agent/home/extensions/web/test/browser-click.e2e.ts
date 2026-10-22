import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("浏览器点击等待异步定位进入视口，永久遮挡仍失败", { timeout: 15000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "browser-click-redo-")),
		port = await freePort();
	const server = createServer((_req, res) => {
		res.writeHead(200, { "Content-Type": "text/html" });
		res.end(
			'<main class="app-layout"><button id="target" style="position:fixed;left:-300px;top:40px;width:100px;height:32px" onclick="window.clicked=true">target</button><button id="blocked" style="position:fixed;left:80px;top:300px;width:100px;height:32px">blocked</button><div style="position:fixed;left:60px;top:280px;width:200px;height:80px;z-index:2"></div></main>',
		);
	});
	await new Promise<void>((r) => server.listen(port, "127.0.0.1", r));
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`http://127.0.0.1:${port}/`);
		await browser.evaluate("setTimeout(()=>document.querySelector('#target').style.left='80px',250)");
		await browser.click("#target");
		assert.equal(await browser.evaluate("window.clicked"), true);
		await browser.evaluate(`(() => {
			const list = document.createElement('div');
			list.style.cssText = 'position:fixed;left:400px;top:80px;width:120px;height:64px;overflow:auto';
			list.innerHTML = '<div style="height:1200px"></div><button id="option" onclick="window.optionClicked=true">option</button>';
			list.addEventListener('pointerenter', () => { list.scrollTop = 0; window.listReset = true; }, { once: true });
			document.body.append(list);
		})()`);
		await browser.click("#option");
		assert.equal(await browser.evaluate("window.listReset && window.optionClicked"), true);
		const current = browser;
		await assert.rejects(() => current.click("#blocked"), /遮挡|不可点击|未停止/);
	} finally {
		await browser?.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
	}
});
