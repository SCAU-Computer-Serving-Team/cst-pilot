import assert from "node:assert/strict";
import { mkdir, mkdtemp } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { Page, type ScreenshotOptions } from "puppeteer-core";
import { createExportRenderer } from "../server/exports/render.ts";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { BrowserProbe } from "./browser.ts";
import { testRoot } from "./support.ts";

test("Puppeteer 原生渲染：分图像素完整、快速编码降级、网络隔离及大 HTML 正文传输", { timeout: 30_000 }, async (t) => {
	const root = await mkdtemp(join(await testRoot(), "export-pixels-"));
	const work = join(root, "work");
	await mkdir(work);
	const bands = [
		{ height: 1801, color: [190, 30, 20, 255] },
		{ height: 2507, color: [30, 180, 40, 255] },
		{ height: 3079, color: [20, 40, 190, 255] },
		{ height: 2113, color: [150, 90, 20, 255] },
	];
	let html = `<!DOCTYPE html><html data-export-receiver="ready"><script>addEventListener('message',async()=>{await fetch('/api/auth').catch(()=>{});document.documentElement.dataset.exportState='ready'})</script><style>body{margin:0}.export-message{width:100%}</style><main class="export-document">${bands.map((band) => `<section class="export-message" style="height:${band.height}px;background:rgb(${band.color.slice(0, 3).join(",")})"></section>`).join("")}</main></html>`;
	let forbiddenRequests = 0,
		fastCalls = 0,
		normalCalls = 0;
	const screenshot = Page.prototype.screenshot;
	t.mock.method(Page.prototype, "screenshot", async function (this: Page, options: ScreenshotOptions) {
		if (options.optimizeForSpeed) {
			fastCalls++;
			throw new Error("Protocol error (Page.captureScreenshot): Invalid parameters: optimizeForSpeed");
		}
		normalCalls++;
		return screenshot.call(this, options);
	});
	const server = createServer((request, response) => {
		if (request.url?.startsWith("/api/")) forbiddenRequests++;
		response.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
		response.end(html);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	let browser: BrowserProbe | undefined;
	const renderer = createExportRenderer({ workDir: work, origin: `http://127.0.0.1:${address.port}` });
	try {
		const pages: Buffer[] = [];
		await renderer.render(
			exportSnapshot(
				"fixture",
				"像素完整性",
				[{ id: "user", message: { role: "user", content: "像素夹具", timestamp: 1 } }],
				exportOptions({ format: "png" }),
			),
			async (_extension, bytes) => {
				pages.push(bytes);
			},
			new AbortController().signal,
		);
		assert.ok(pages.length > 1);
		assert.equal(fastCalls, 1, "不支持快速编码后只尝试一次");
		assert.equal(normalCalls, pages.length, "其余分图使用普通无损编码");
		assert.equal(forbiddenRequests, 0, "页面无法请求账号 API");
		browser = await BrowserProbe.launch(root);
		const runs: { color: number[]; height: number }[] = [];
		for (const bytes of pages) {
			assert.equal(bytes.readUInt32BE(16), 2480);
			const rows = await browser.evaluate<{ color: number[]; height: number }[]>(`(async () => {
				const image=new Image();image.src='data:image/png;base64,${bytes.toString("base64")}';await image.decode();
				const canvas=document.createElement('canvas');canvas.width=1;canvas.height=image.height;
				const context=canvas.getContext('2d');context.drawImage(image,20,0,1,image.height,0,0,1,image.height);
				const data=context.getImageData(0,0,1,image.height).data,runs=[];
				for(let y=0;y<image.height;y++) {
					const color=Array.from(data.slice(y*4,y*4+4)),last=runs.at(-1);
					if(last && last.color.every((v,i)=>v===color[i]))last.height++;else runs.push({color,height:1});
				}
				return runs;
			})()`);
			for (const row of rows) {
				const last = runs.at(-1);
				if (last?.color.every((value, index) => value === row.color[index])) last.height += row.height;
				else runs.push(row);
			}
		}
		assert.deepEqual(
			runs,
			bands.map((band) => ({ color: band.color, height: band.height * 2 })),
		);
		const large = "大正文序列化回归🙂".repeat(160_000);
		html = `<!DOCTYPE html><html data-export-receiver="ready"><script>addEventListener('message',()=>document.documentElement.dataset.exportState='ready')</script><main class="export-document"><details><summary>大正文</summary><p onclick="alert(1)">${large}</p></details><button>不保留</button></main></html>`;
		let file: Buffer = Buffer.alloc(0);
		await renderer.render(
			exportSnapshot(
				"fixture",
				"大 HTML",
				[{ id: "user", message: { role: "user", content: "fixture", timestamp: 1 } }],
				exportOptions({ format: "html" }),
			),
			async (_extension, bytes) => {
				file = bytes;
			},
			new AbortController().signal,
		);
		assert.ok(file.length > 2 * 1024 * 1024);
		assert.ok(file.toString().includes(large), "大正文完整跨进程传输");
		assert.equal(file.toString().includes("onclick="), false);
		assert.equal(file.toString().includes("<button"), false);
		assert.equal(file.toString().includes("<script"), false);
	} finally {
		await browser?.close();
		await renderer.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
