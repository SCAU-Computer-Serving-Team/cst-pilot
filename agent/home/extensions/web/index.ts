import { spawn } from "node:child_process";
import { stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { CustomEditor, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { createWebApi } from "./server/api.ts";
import { getWebContainer } from "./server/container.ts";
import { createWebServer, listenWebServer, WEB_PORT } from "./server/http.ts";
import { WebSessionPool } from "./server/session/sessions.ts";

const url = `http://127.0.0.1:${WEB_PORT}/`;
const staticDirectory = fileURLToPath(new URL("./static/", import.meta.url));

class ParkedEditor extends CustomEditor {
	handleInput(_data: string): void {
		// Do not dispatch editor bindings: built-in commands run before extension input hooks.
	}
}

function openBrowser(): void {
	if (process.env.CST_WEB_NO_OPEN === "1") return;
	const launcher = spawn("cmd.exe", ["/c", "start", "", url], { detached: true, stdio: "ignore", windowsHide: true });
	launcher.on("error", (error) => console.error("CST Pilot Web: 打开浏览器失败", error));
	launcher.unref();
}

export default function web(pi: ExtensionAPI): void {
	const container = getWebContainer();
	if (container.loadingWebSession > 0) return;

	pi.on("session_start", (_event, ctx) => {
		if (container.parked && ctx.mode === "tui") {
			ctx.ui.setEditorComponent((tui, theme, keys) => new ParkedEditor(tui, theme, keys));
			ctx.ui.setWidget("cst-web-parked", ["会话已由 Web 接管，请在浏览器中操作。关闭终端将结束服务。"]);
		}
	});

	pi.registerCommand("web", {
		description: "在本机浏览器中打开 CST Pilot Web",
		handler: async (_args, ctx) => {
			if (container.server?.listening) {
				openBrowser();
				return;
			}
			if (container.starting) {
				ctx.ui.notify("Web 正在启动，请稍后重试。", "warning");
				return;
			}
			if (ctx.mode !== "tui") return;
			if (ctx.ui.getEditorText().trim()) {
				ctx.ui.notify("编辑器还有未提交的内容，请先保存或清空。", "warning");
				return;
			}
			const candidateFile = ctx.sessionManager.getSessionFile();
			let saved = false;
			try {
				if (candidateFile) saved = (await stat(candidateFile)).isFile();
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
					ctx.ui.notify("无法确认当前会话是否已保存，Web 未启动。", "error");
					return;
				}
			}
			if (
				!saved &&
				ctx.sessionManager.getEntries().some((entry) => entry.type === "message" || entry.type === "custom_message")
			) {
				ctx.ui.notify("当前会话尚未保存，不能接管。请先完成本轮对话。", "error");
				return;
			}
			const cwd = ctx.cwd;
			const sessionDir = ctx.sessionManager.getSessionDir();
			const oldFile = saved ? candidateFile : undefined;
			const oldId = ctx.sessionManager.getSessionId();
			const agentDir = process.env.PI_CODING_AGENT_DIR;
			if (!agentDir) {
				ctx.ui.notify("缺少隔离的 agent/home 路径，Web 未启动。", "error");
				return;
			}
			let parkedContext: typeof ctx | undefined;
			let switched = false;
			const start = (async () => {
				const pool = new WebSessionPool({
					cwd,
					agentDir,
					sessionDir,
					withLoader: async (load) => {
						container.loadingWebSession++;
						try {
							return await load();
						} finally {
							container.loadingWebSession--;
						}
					},
				});
				container.pool = pool;
				// Bind the socket before releasing TUI; a failed port bind must not park it.
				const api = createWebApi(pool, agentDir, WEB_PORT);
				const server = createWebServer(
					staticDirectory,
					WEB_PORT,
					async () => ({
						sessions: await pool.list(),
						stage: "api-inbox",
					}),
					api,
				);
				container.closeApi = api.close;
				try {
					await listenWebServer(server);
					container.server = server;
					await ctx.waitForIdle();
					container.parked = true;
					const result = await ctx.newSession({
						withSession: async (fresh) => {
							parkedContext = fresh;
						},
					});
					if (result.cancelled) throw new Error("TUI 会话切换被取消");
					switched = true;
					if (oldFile) await pool.openHandoff(oldId, oldFile);
					else await pool.create();
				} catch (error) {
					container.parked = false;
					if (switched && parkedContext) {
						parkedContext.ui.setEditorComponent(undefined);
						parkedContext.ui.setWidget("cst-web-parked", undefined);
					}
					await pool.close();
					await api.close();
					container.closeApi = undefined;
					container.pool = undefined;
					container.server = undefined;
					server.closeAllConnections();
					await new Promise<void>((resolve) => server.close(() => resolve()));
					throw error;
				}
			})();
			container.starting = start;
			try {
				await start;
				openBrowser();
				// After ctx.newSession, the old command context is stale. Use only the fresh context here.
				parkedContext?.ui.notify("Web 已启动，终端已进入待机。请在浏览器中继续操作。", "info");
			} catch (error) {
				const message =
					(error as NodeJS.ErrnoException).code === "EADDRINUSE"
						? "Web 端口已被占用。请关闭占用端口的程序后重试。"
						: `Web 启动失败：${error instanceof Error ? error.message : String(error)}`;
				(parkedContext ?? ctx).ui.notify(message, "error");
			} finally {
				container.starting = undefined;
			}
		},
	});

	pi.on("session_shutdown", async (event) => {
		if (event.reason !== "quit") return;
		if (container.starting) await container.starting.catch(() => undefined);
		const server = container.server;
		container.server = undefined;
		if (server?.listening) {
			server.closeAllConnections();
			await new Promise<void>((resolve) => server.close(() => resolve()));
		}
		await container.pool?.close();
		await container.closeApi?.();
		container.closeApi = undefined;
		container.pool = undefined;
		container.parked = false;
	});
}
