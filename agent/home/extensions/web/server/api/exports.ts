import type { IncomingMessage, ServerResponse } from "node:http";
import { join } from "node:path";
import { ARTIFACTS_DIR } from "../../../diagnostics/artifact-paths.ts";
import { EXPORT_CSP } from "../exports/html.ts";
import { openExportDirectory } from "../exports/open.ts";
import { createExportRenderer, type ExportRenderer } from "../exports/render.ts";
import { conversationEntries, ExportError, exportOptions, exportSnapshot } from "../exports/snapshot.ts";
import { SessionExportStore } from "../exports/store.ts";
import type { WebSessionPool } from "../session/sessions.ts";
import { body, send } from "./http.ts";

type WriteOnce = <T>(
	request: IncomingMessage,
	pathname: string,
	input: unknown,
	action: () => Promise<T>,
) => Promise<T>;

export function createExportRoutes(
	pool: WebSessionPool,
	agentDir: string,
	origin: string,
	writeOnce: WriteOnce,
	options: {
		artifactsRoot?: string;
		exportRenderer?: ExportRenderer;
		openExportDirectory?: (directory: string) => Promise<void>;
	} = {},
) {
	const metadata = join(agentDir, "session-exports");
	const store = new SessionExportStore(
		options.artifactsRoot ?? ARTIFACTS_DIR,
		metadata,
		options.exportRenderer ?? createExportRenderer({ origin, workDir: join(metadata, "work", "browsers") }),
	);
	const route = async (
		request: IncomingMessage,
		response: ServerResponse,
		pathname: string,
		method: string,
	): Promise<boolean> => {
		const session = /^\/api\/sessions\/([\w-]{1,128})\/exports(\/preview)?$/.exec(pathname);
		if (session) {
			const id = session[1];
			if (!session[2] && method === "GET") {
				send(response, 200, { exports: await store.list(id) });
				return true;
			}
			if (method !== "POST") return false;
			const input = await body(request);
			const config = exportOptions(input);
			const prepare = async () => {
				const slot = await pool.openSaved(id);
				const title = (await pool.list()).find((row) => row.id === id)?.title ?? "会话";
				return exportSnapshot(id, title, conversationEntries(slot), config);
			};
			if (session[2]) send(response, 200, await prepare());
			else send(response, 202, await writeOnce(request, pathname, input, async () => store.create(await prepare())));
			return true;
		}
		const record = /^\/api\/exports\/([a-f0-9-]{36})(?:\/(files\/\d+|open))?$/.exec(pathname);
		if (!record) return false;
		const [, id, suffix] = record;
		if (suffix === "open") {
			if (method !== "POST") return false;
			const input = await body(request, 4096);
			if (Object.keys(input).length) throw new Error("请求只需导出 ID，不接受目录路径");
			send(
				response,
				200,
				await writeOnce(request, pathname, input, async () => {
					const directory = await store.directory(id);
					try {
						await (options.openExportDirectory ?? openExportDirectory)(directory);
					} catch {
						throw new ExportError(503, "export_launch_failed", "无法启动文件管理器，请重试或按显示路径手动打开");
					}
					return { launched: true };
				}),
			);
			return true;
		}
		if (method !== "GET") return false;
		if (suffix?.startsWith("files/")) {
			const file = await store.file(id, Number(suffix.slice(6)));
			response.writeHead(200, {
				"Content-Type": file.type,
				"Content-Length": file.data.length,
				"Content-Disposition": `attachment; filename*=UTF-8''${encodeURIComponent(file.name)}`,
				"Cache-Control": "no-store",
				"X-Content-Type-Options": "nosniff",
				"Content-Security-Policy": EXPORT_CSP,
			});
			response.end(file.data);
		} else send(response, 200, await store.get(id));
		return true;
	};
	return Object.assign(route, { close: () => store.close() });
}
