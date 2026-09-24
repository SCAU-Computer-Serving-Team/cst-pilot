import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { getWebContainer } from "./server/container.ts";
import { createWebServer, listenWebServer, WEB_PORT } from "./server/http.ts";

const url = `http://127.0.0.1:${WEB_PORT}/`;
const staticDirectory = fileURLToPath(new URL("./static/", import.meta.url));

function openBrowser(): void {
	if (process.env.CST_WEB_NO_OPEN === "1") return;
	const launcher = spawn("cmd.exe", ["/c", "start", "", url], { detached: true, stdio: "ignore", windowsHide: true });
	launcher.on("error", (error) => console.error("CST Pilot Web: 打开浏览器失败", error));
	launcher.unref();
}

export default function web(pi: ExtensionAPI): void {
	const container = getWebContainer();
	if (container.loadingWebSession) return;

	pi.registerCommand("web", {
		description: "在本机浏览器中打开 CST Pilot Web",
		handler: async (_args, ctx) => {
			if (!container.server?.listening) {
				container.starting ??= (async () => {
					const server = createWebServer(staticDirectory);
					try {
						await listenWebServer(server);
						container.server = server;
					} catch (error) {
						server.close();
						throw error;
					}
				})();
				try {
					await container.starting;
				} catch (error) {
					const message =
						(error as NodeJS.ErrnoException).code === "EADDRINUSE"
							? "Web 端口已被占用。请关闭占用端口的程序后重试。"
							: `Web 启动失败：${error instanceof Error ? error.message : String(error)}`;
					ctx.ui.notify(message, "error");
					return;
				} finally {
					container.starting = undefined;
				}
			}
			openBrowser();
			ctx.ui.notify("已打开 Web 工程预览。会话操作仍在 TUI 中完成。", "info");
		},
	});

	pi.on("session_shutdown", async (event) => {
		if (event.reason !== "quit") return;
		if (container.starting) await container.starting.catch(() => undefined);
		const server = container.server;
		container.server = undefined;
		if (server?.listening) await new Promise<void>((resolveClose) => server.close(() => resolveClose()));
	});
}
