import { readFile } from "node:fs/promises";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { getKeybindings } from "@earendil-works/pi-tui";
import { getWebContainer } from "../web/server/container.ts";
import { currentRuntime, installRuntimeAdapter, refreshRuntime } from "./adapter.ts";
import { ownSession } from "./owner.ts";

export default function runtime(pi: ExtensionAPI): void {
	installRuntimeAdapter();
	let off: (() => void) | undefined;
	let replay = false;
	let closed = false;
	pi.on("session_start", (_event, ctx) => {
		closed = false;
		if (ctx.mode !== "tui") return;
		off = ctx.ui.onTerminalInput((data) => {
			if (getWebContainer().parked || replay || !ctx.isIdle()) return;
			const command = ctx.ui.getEditorText().trim();
			const keys = getKeybindings();
			const menuEnter = keys.matches(data, "tui.input.submit") && /^\/(?:model|scoped-models)(?:\s|$)/.test(command);
			if (
				!menuEnter &&
				!keys.matches(data, "app.model.select") &&
				!keys.matches(data, "app.model.cycleForward") &&
				!keys.matches(data, "app.model.cycleBackward")
			)
				return;
			const session = currentRuntime(ctx.sessionManager.getSessionId());
			if (!session) return;
			void refreshRuntime(session)
				.then(() => {
					if (closed) return;
					replay = true;
					try {
						process.stdin.emit("data", data);
					} finally {
						replay = false;
					}
				})
				.catch((error) => {
					if (!closed) ctx.ui.notify(error instanceof Error ? error.message : "共享配置刷新失败", "error");
				});
			return { consume: true };
		});
	});
	// Pi 在扩展命令分发后触发 input；只重读请求配置，模型与凭据错误由 Pi 处理。
	pi.on("input", async (_event, ctx) => {
		const session = currentRuntime(ctx.sessionManager.getSessionId());
		if (session) await refreshRuntime(session, { updateScope: false });
	});
	pi.on("session_before_switch", async (event, ctx) => {
		if (!event.targetSessionFile) return;
		const home = process.env.PI_CODING_AGENT_DIR;
		if (!home) return { cancel: true };
		try {
			const header = JSON.parse((await readFile(event.targetSessionFile, "utf8")).split("\n")[0]);
			if (header.id === ctx.sessionManager.getSessionId()) return;
			if (typeof header.id !== "string") throw new Error("会话文件无效");
			const release = await ownSession(home, header.id);
			release();
		} catch (error) {
			ctx.ui.notify(error instanceof Error ? error.message : "会话无法打开", "error");
			return { cancel: true };
		}
	});
	pi.on("session_shutdown", () => {
		closed = true;
		off?.();
		off = undefined;
	});
}
