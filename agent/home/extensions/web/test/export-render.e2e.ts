import assert from "node:assert/strict";
import { mkdir, mkdtemp } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { createExportRenderer } from "../server/exports/render.ts";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { BrowserProbe } from "./browser.ts";
import { testRoot } from "./support.ts";

test("PNG 分图逐行保留像素：视口滚动无空白、重复、遗漏及尾页裁切", { timeout: 30_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-pixels-"));
	const work = join(root, "work");
	await mkdir(work);
	const bands = [
		{ height: 1801, color: [190, 30, 20, 255] },
		{ height: 2507, color: [30, 180, 40, 255] },
		{ height: 3079, color: [20, 40, 190, 255] },
		{ height: 2113, color: [150, 90, 20, 255] },
	];
	const html = `<!DOCTYPE html><html data-export-state="ready"><style>body{margin:0}.export-message{width:100%}</style><main class="export-document">${bands.map((band) => `<section class="export-message" style="height:${band.height}px;background:rgb(${band.color.slice(0, 3).join(",")})"></section>`).join("")}</main></html>`;
	const server = createServer((_request, response) => {
		response.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
		response.end(html);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	let browser: BrowserProbe | undefined;
	try {
		const pages: Buffer[] = [];
		await createExportRenderer(work)(
			exportSnapshot(
				"fixture",
				"像素完整性",
				[{ id: "user", message: { role: "user", content: "像素夹具", timestamp: 1 } }],
				exportOptions({ format: "png" }),
			),
			`http://127.0.0.1:${address.port}`,
			async (_extension, bytes) => {
				pages.push(bytes);
			},
			new AbortController().signal,
		);
		assert.ok(pages.length > 1);
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
	} finally {
		await browser?.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
