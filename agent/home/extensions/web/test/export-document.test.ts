import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { OfflineHtml } from "../server/exports/html.ts";
import { exportPages } from "../server/exports/pagination.ts";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";

test("分页覆盖全部高度，优先段落切点，过滤无效切点并保持单页上限", () => {
	for (const height of [1, 1600, 3200, 3201, 9500, 100_000]) {
		const pages = exportPages(height, [3000, 1700, 1700, -1, Number.NaN, 10_000_000, 6000]);
		let top = 0;
		for (const page of pages) {
			assert.equal(page.top, top);
			assert.ok(page.height > 0 && page.height <= 3200);
			top += page.height;
		}
		assert.equal(top, height);
	}
	assert.equal(exportPages(7000, [3100, 2500])[0].height, 3100);
	assert.throws(() => exportPages(100_001, []), { code: "export_too_tall" });
	assert.throws(() => exportPages(0, []), { code: "export_layout_invalid" });
});

test("离线 HTML 后端打包：并行资源、URL 去重、跨任务缓存、失败可重试及外部资源拒绝", { timeout: 10_000 }, async () => {
	const requests = new Map<string, number>();
	let active = 0,
		peak = 0,
		fail = true;
	const server = createServer((request, response) => {
		const url = request.url ?? "";
		requests.set(url, (requests.get(url) ?? 0) + 1);
		active++;
		peak = Math.max(peak, active);
		setTimeout(() => {
			active--;
			if (url === "/fonts/retry.woff2" && fail) {
				response.writeHead(503);
				response.end();
				return;
			}
			response.writeHead(200, { "Content-Type": "font/woff2" });
			response.end(`font:${url}`);
		}, 30);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const origin = `http://127.0.0.1:${address.port}`;
	const pack = new OfflineHtml(origin);
	const snapshot = exportSnapshot(
		"fixture",
		"<导出 & 标题>",
		[{ id: "u", message: { role: "user", content: "fixture", timestamp: 1 } }],
		exportOptions({ format: "html" }),
	);
	const source = {
		body: '<main class="export-document"><details><summary>过程</summary>完整内容</details></main>',
		styles: [
			{
				base: origin,
				css: '@font-face{src:url("/fonts/a.woff2")}@font-face{src:url("/fonts/b.woff2")}p{background:url("/fonts/a.woff2")}',
			},
		],
	};
	const signal = new AbortController().signal;
	try {
		const first = (await pack.render(snapshot, source, signal)).toString();
		assert.ok(first.startsWith("<!DOCTYPE html>"));
		assert.ok(first.includes("&lt;导出 &amp; 标题&gt;"));
		assert.ok(first.includes(Buffer.from("font:/fonts/a.woff2").toString("base64")));
		assert.ok(first.includes("完整内容"));
		assert.ok(peak > 1 && peak <= 4);
		assert.equal(requests.get("/fonts/a.woff2"), 1);
		await pack.render(snapshot, source, signal);
		assert.equal(requests.get("/fonts/b.woff2"), 1);
		const retry = { ...source, styles: [{ base: origin, css: 'p{src:url("/fonts/retry.woff2")}' }] };
		await assert.rejects(pack.render(snapshot, retry, signal), { code: "export_asset_unavailable" });
		fail = false;
		await pack.render(snapshot, retry, signal);
		assert.equal(requests.get("/fonts/retry.woff2"), 2);
		for (const url of ["https://external.invalid/font.woff2", `${origin}/api/auth`])
			await assert.rejects(
				pack.render(snapshot, { ...source, styles: [{ base: origin, css: `p{src:url("${url}")}` }] }, signal),
				{ code: "export_asset_forbidden" },
			);
		const controller = new AbortController();
		controller.abort();
		await assert.rejects(pack.render(snapshot, source, controller.signal));
		pack.clear();
		await pack.render(snapshot, source, signal);
		assert.equal(requests.get("/fonts/a.woff2"), 2);
	} finally {
		pack.clear();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});

test("资源失败时取消同批读取并回收响应，失败资源不进入缓存", { timeout: 10_000 }, async () => {
	const closed = Array.from({ length: 2 }, () => {
		let resolve!: () => void;
		const promise = new Promise<void>((done) => {
			resolve = done;
		});
		return { promise, resolve };
	});
	const requests = new Map<string, number>();
	let fail = true;
	const server = createServer((request, response) => {
		const url = request.url ?? "";
		requests.set(url, (requests.get(url) ?? 0) + 1);
		if (!fail) {
			response.writeHead(200, { "Content-Type": "font/woff2" });
			response.end(`font:${url}`);
			return;
		}
		const index = url === "/fonts/slow.woff2" ? 0 : 1;
		response.once("close", () => closed[index].resolve());
		response.writeHead(index ? 503 : 200, { "Content-Type": "font/woff2" });
		response.flushHeaders();
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const origin = `http://127.0.0.1:${address.port}`;
	const pack = new OfflineHtml(origin);
	const snapshot = exportSnapshot(
		"fixture",
		"取消资源读取",
		[{ id: "u", message: { role: "user", content: "fixture", timestamp: 1 } }],
		exportOptions({ format: "html" }),
	);
	const source = {
		body: "<main>fixture</main>",
		styles: [{ base: origin, css: 'a{src:url("/fonts/slow.woff2")}b{src:url("/fonts/failure.woff2")}' }],
	};
	try {
		await assert.rejects(pack.render(snapshot, source, new AbortController().signal), {
			code: "export_asset_unavailable",
		});
		await Promise.all(closed.map((item) => item.promise));
		fail = false;
		await pack.render(snapshot, source, new AbortController().signal);
		assert.equal(requests.get("/fonts/slow.woff2"), 2);
		assert.equal(requests.get("/fonts/failure.woff2"), 2);
	} finally {
		pack.clear();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});

test("HTML 资源展开超过总预算时立即拒绝并取消未完成读取", { timeout: 10_000 }, async () => {
	const bytes = Buffer.alloc(16 * 1024 * 1024 - 8, 65);
	const server = createServer((request, response) => {
		response.writeHead(200, { "Content-Type": "font/woff2" });
		if (request.url === "/fonts/large.woff2") response.end(bytes);
		else response.flushHeaders();
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	const origin = `http://127.0.0.1:${address.port}`;
	const pack = new OfflineHtml(origin);
	const snapshot = exportSnapshot(
		"fixture",
		"资源预算",
		[{ id: "u", message: { role: "user", content: "fixture", timestamp: 1 } }],
		exportOptions({ format: "html" }),
	);
	try {
		await assert.rejects(
			pack.render(
				snapshot,
				{
					body: "<main>fixture</main>",
					styles: [
						{
							base: origin,
							css: `${'a{src:url("/fonts/large.woff2")}'.repeat(10)}b{src:url("/fonts/pending.woff2")}`,
						},
					],
				},
				AbortSignal.timeout(3000),
			),
			{ code: "export_too_large" },
		);
	} finally {
		pack.clear();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
