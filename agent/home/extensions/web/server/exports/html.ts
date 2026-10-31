import type { ExportDocumentSource, ExportSnapshot } from "../../shared/exports.ts";
import { ExportError } from "./snapshot.ts";

export const EXPORT_CSP =
	"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'";
const URL_PATTERN = /url\(\s*(?:"([^"]*)"|'([^']*)'|([^)]*))\s*\)/g;
const MAX_ASSET = 16 * 1024 * 1024;
const MAX_CACHE = 24 * 1024 * 1024;
const MAX_HTML = 200 * 1024 * 1024;
const escapeHtml = (value: string) =>
	value.replace(
		/[&<>"']/g,
		(character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character] ?? character,
	);

/** 后端离线打包器：只读取同源随包资源，四路读取并复用有界资源缓存。 */
export class OfflineHtml {
	private readonly origin: string;
	private readonly cache = new Map<string, string>();
	private cacheBytes = 0;
	constructor(origin: string) {
		this.origin = new URL(origin).origin;
	}
	private resolve(raw: string, base: string): string {
		const url = new URL(raw, base);
		if (url.protocol === "data:") return raw;
		if (url.origin !== this.origin || !/^\/(fonts|assets)\//.test(url.pathname))
			throw new ExportError(400, "export_asset_forbidden", "导出样式引用了非本机随包资源");
		return url.href;
	}
	private async asset(url: string, signal: AbortSignal): Promise<string> {
		const cached = this.cache.get(url);
		if (cached) {
			this.cache.delete(url);
			this.cache.set(url, cached);
			return cached;
		}
		const response = await fetch(url, { signal, redirect: "error" });
		if (!response.ok || !response.body) {
			await response.body?.cancel();
			throw new ExportError(503, "export_asset_unavailable", "导出字体或样式资源无法读取，请检查静态文件");
		}
		const chunks: Uint8Array[] = [];
		let bytes = 0;
		for await (const chunk of response.body) {
			signal.throwIfAborted();
			bytes += chunk.byteLength;
			if (bytes > MAX_ASSET) throw new ExportError(413, "export_too_large", "导出资源超过大小限制");
			chunks.push(chunk);
		}
		signal.throwIfAborted();
		const mime = response.headers.get("content-type")?.split(";")[0] ?? "application/octet-stream";
		const value = `data:${mime};base64,${Buffer.concat(chunks).toString("base64")}`;
		const size = Buffer.byteLength(value);
		const previous = this.cache.get(url);
		if (previous) {
			this.cache.delete(url);
			this.cacheBytes -= Buffer.byteLength(previous);
		}
		while (this.cacheBytes + size > MAX_CACHE && this.cache.size) {
			const first = this.cache.entries().next().value;
			if (!first) break;
			this.cache.delete(first[0]);
			this.cacheBytes -= Buffer.byteLength(first[1]);
		}
		if (size <= MAX_CACHE) {
			this.cache.set(url, value);
			this.cacheBytes += size;
		}
		return value;
	}
	async render(snapshot: ExportSnapshot, source: ExportDocumentSource, signal: AbortSignal): Promise<Buffer> {
		signal.throwIfAborted();
		const urls = new Map<string, { count: number; replacedBytes: number }>();
		let estimatedBytes =
			Buffer.byteLength(source.body) + source.styles.reduce((sum, sheet) => sum + Buffer.byteLength(sheet.css), 0);
		const sheets = source.styles.map(({ css, base }) => {
			const matches = [...css.matchAll(URL_PATTERN)].map((match) => {
				const url = this.resolve((match[1] ?? match[2] ?? match[3]).trim(), base);
				if (!url.startsWith("data:")) {
					const usage = urls.get(url) ?? { count: 0, replacedBytes: 0 };
					usage.count++;
					usage.replacedBytes += Buffer.byteLength(match[0]);
					urls.set(url, usage);
				}
				return { match, url };
			});
			return { css, matches };
		});
		if (urls.size > 128) throw new ExportError(413, "export_too_large", "导出资源数量超过限制");
		if (estimatedBytes > MAX_HTML) throw new ExportError(413, "export_too_large", "导出正文或样式超过大小限制");
		const pending = [...urls.keys()],
			values = new Map<string, string>();
		let index = 0;
		const controller = new AbortController();
		const combined = AbortSignal.any([signal, controller.signal]);
		const workers = Array.from({ length: Math.min(4, pending.length) }, async () => {
			while (index < pending.length) {
				combined.throwIfAborted();
				const url = pending[index++];
				const value = await this.asset(url, combined);
				const usage = urls.get(url)!;
				estimatedBytes += (Buffer.byteLength(value) + 'url("")'.length) * usage.count - usage.replacedBytes;
				if (estimatedBytes > MAX_HTML)
					throw new ExportError(413, "export_too_large", "产物超过大小限制，请缩短当前分支后重试");
				values.set(url, value);
			}
		});
		try {
			await Promise.all(workers);
		} catch (cause) {
			controller.abort(cause);
			await Promise.allSettled(workers);
			throw cause;
		}
		signal.throwIfAborted();
		const css = sheets
			.map(({ css, matches }) => {
				let output = "",
					cursor = 0;
				for (const { match, url } of matches) {
					output += `${css.slice(cursor, match.index)}url("${values.get(url) ?? url}")`;
					cursor = match.index + match[0].length;
				}
				return output + css.slice(cursor);
			})
			.join("\n")
			.replace(/<\/style/gi, "<\\/style");
		const html = `<!DOCTYPE html><html lang="zh-CN" data-theme="${snapshot.options.theme}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="${EXPORT_CSP}"><title>${escapeHtml(snapshot.title)}</title><style>${css}</style></head><body class="export-page">${source.body}</body></html>`;
		if (Buffer.byteLength(html) > MAX_HTML)
			throw new ExportError(413, "export_too_large", "产物超过大小限制，请缩短当前分支后重试");
		return Buffer.from(html, "utf8");
	}
	clear(): void {
		this.cache.clear();
		this.cacheBytes = 0;
	}
}
