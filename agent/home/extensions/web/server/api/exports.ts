import type { IncomingMessage, ServerResponse } from "node:http";
import { join } from "node:path";
import { ARTIFACTS_DIR } from "../../../diagnostics/artifact-paths.ts";
import { createExportRenderer, type ExportRenderer } from "../exports/render.ts";
import { conversationEntries, exportOptions, exportSnapshot } from "../exports/snapshot.ts";
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
	options: { artifactsRoot?: string; exportRenderer?: ExportRenderer } = {},
) {
	const metadata = join(agentDir, "session-exports");
	const store = new SessionExportStore(
		options.artifactsRoot ?? ARTIFACTS_DIR,
		metadata,
		origin,
		options.exportRenderer ?? createExportRenderer(join(metadata, "work")),
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
		const record = /^\/api\/exports\/([a-f0-9-]{36})(?:\/(snapshot|files\/\d+))?$/.exec(pathname);
		if (!record || method !== "GET") return false;
		const [, id, suffix] = record;
		if (suffix === "snapshot") {
			const token = request.headers["x-cst-export-token"];
			send(response, 200, store.snapshot(id, typeof token === "string" ? token : ""));
		} else if (suffix?.startsWith("files/")) {
			const file = await store.file(id, Number(suffix.slice(6)));
			response.writeHead(200, {
				"Content-Type": file.type,
				"Content-Length": file.data.length,
				"Content-Disposition": `attachment; filename*=UTF-8''${encodeURIComponent(file.name)}`,
				"Cache-Control": "no-store",
				"X-Content-Type-Options": "nosniff",
				"Content-Security-Policy":
					"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'",
			});
			response.end(file.data);
		} else send(response, 200, await store.get(id));
		return true;
	};
	return Object.assign(route, { close: () => store.close() });
}
