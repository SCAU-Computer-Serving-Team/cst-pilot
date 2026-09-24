import { readFile, realpath, stat } from "node:fs/promises";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { extname, isAbsolute, relative, resolve, sep } from "node:path";

export const WEB_PORT = 52831;
const WEB_HOST = "127.0.0.1";

const contentTypes: Record<string, string> = {
	".css": "text/css; charset=utf-8",
	".html": "text/html; charset=utf-8",
	".ico": "image/x-icon",
	".js": "text/javascript; charset=utf-8",
	".json": "application/json; charset=utf-8",
	".otf": "font/otf",
	".png": "image/png",
	".svg": "image/svg+xml",
	".webp": "image/webp",
	".woff2": "font/woff2",
};

function json(response: ServerResponse, status: number, body: unknown): void {
	response.writeHead(status, {
		"Cache-Control": "no-store",
		"Content-Type": "application/json; charset=utf-8",
		"X-Content-Type-Options": "nosniff",
	});
	response.end(response.req.method === "HEAD" ? undefined : JSON.stringify(body));
}

function fail(response: ServerResponse, status: number, code: string, message: string): void {
	json(response, status, { error: { code, message } });
}

function themeFromCookie(cookie: string | undefined): "light" | "dark" | "system" {
	const value = cookie?.match(/(?:^|;\s*)cst-theme=(light|dark|system)(?:;|$)/)?.[1];
	return value === "light" || value === "dark" ? value : "system";
}

function isInside(root: string, file: string): boolean {
	const path = relative(root, file);
	return path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path));
}

export function createWebServer(staticDirectory: string, port = WEB_PORT): Server {
	const root = resolve(staticDirectory);
	return createServer(async (request: IncomingMessage, response: ServerResponse) => {
		response.setHeader("X-Content-Type-Options", "nosniff");
		response.setHeader("Referrer-Policy", "no-referrer");
		if (request.headers.host !== `${WEB_HOST}:${port}`) {
			fail(response, 403, "invalid_host", "此页面只允许从本机打开。");
			return;
		}
		if (request.method !== "GET" && request.method !== "HEAD") {
			response.setHeader("Allow", "GET, HEAD");
			fail(response, 405, "method_not_allowed", "当前操作暂不可用。");
			return;
		}
		let pathname: string;
		try {
			pathname = decodeURIComponent(new URL(request.url ?? "/", `http://${WEB_HOST}:${port}`).pathname);
		} catch {
			fail(response, 400, "invalid_path", "页面地址无效。");
			return;
		}
		if (pathname === "/api/state") {
			json(response, 200, { version: "0.0.0", sessions: [], connected: true, commands: [], stage: "foundation" });
			return;
		}
		if (pathname === "/api" || pathname.startsWith("/api/")) {
			fail(response, 404, "not_found", "接口尚未提供。");
			return;
		}
		if (
			pathname.includes("\\") ||
			pathname.includes("\0") ||
			pathname.split("/").some((part) => part.startsWith("."))
		) {
			fail(response, 404, "not_found", "页面不存在。");
			return;
		}
		try {
			const rootReal = await realpath(root);
			let path = resolve(root, `.${pathname}`);
			if (!isInside(root, path)) {
				fail(response, 404, "not_found", "页面不存在。");
				return;
			}
			let file = await stat(path)
				.then((info) => (info.isFile() ? path : undefined))
				.catch(() => undefined);
			const reservedAssetPath = ["/assets", "/fonts"].some(
				(directory) => pathname === directory || pathname.startsWith(`${directory}/`),
			);
			if (
				!file &&
				pathname !== "/" &&
				!reservedAssetPath &&
				!extname(pathname) &&
				request.headers.accept?.includes("text/html")
			) {
				path = resolve(root, "index.html");
				file = path;
			}
			if (pathname === "/") file = resolve(root, "index.html");
			if (!file) {
				fail(response, 404, "not_found", "页面不存在。");
				return;
			}
			if (!isInside(rootReal, await realpath(file)) || !(await stat(file)).isFile()) {
				fail(response, 404, "not_found", "页面不存在。");
				return;
			}
			let contents = await readFile(file);
			const isHtml = extname(file) === ".html";
			if (isHtml) {
				contents = Buffer.from(
					contents
						.toString("utf8")
						.replace(/<html\b/, `<html data-theme="${themeFromCookie(request.headers.cookie)}"`),
				);
			}
			const hashed = /^\/assets\/[^/]+-[A-Za-z0-9_-]{8,}\./.test(pathname);
			response.writeHead(200, {
				"Cache-Control": isHtml ? "no-cache" : hashed ? "public, max-age=31536000, immutable" : "no-cache",
				"Content-Type": contentTypes[extname(file)] ?? "application/octet-stream",
				"Content-Length": contents.length,
			});
			response.end(request.method === "HEAD" ? undefined : contents);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") {
				fail(response, 503, "static_missing", "页面文件尚未构建。请先构建 Web 前端。");
				return;
			}
			fail(response, 500, "server_error", "页面暂时无法打开。");
		}
	});
}

export async function listenWebServer(server: Server, port = WEB_PORT): Promise<void> {
	await new Promise<void>((resolveListen, rejectListen) => {
		server.once("error", rejectListen);
		server.listen(port, WEB_HOST, () => {
			server.removeListener("error", rejectListen);
			resolveListen();
		});
	});
}
