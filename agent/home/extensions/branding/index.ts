import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { installBrandingHeader } from "./header.ts";

/** cst-pilot 品牌页眉：session_start 后分多拍安装，压过包扩展（pi-open-tui）装的默认页眉。 */
export default function branding(pi: ExtensionAPI): void {
	let installed = false;
	let cleanup: (() => void) | undefined;
	let timers: Array<ReturnType<typeof setTimeout>> = [];

	pi.on("session_start", async (_event, ctx: ExtensionContext) => {
		if (!ctx.hasUI || installed) return;
		installed = true;
		const install = () => {
			try {
				cleanup?.();
				cleanup = installBrandingHeader(pi, ctx);
			} catch {
				// 会话可能已被替换；旧 ctx 不再可用，停止本次安装。
				cleanup = undefined;
			}
		};
		// 包扩展（pi-open-tui）的页眉安装时序不确定（其 session_start 处理
		// 中有同步的配置加载，可能晚于本扩展的首个 timer），单次延后安装
		// 会被它反超；多次重试保证最后一次写入者是我们。
		timers = [0, 150, 400, 1000].map((delay) => setTimeout(install, delay));
	});

	pi.on("session_shutdown", async () => {
		installed = false;
		for (const timer of timers) clearTimeout(timer);
		timers = [];
		cleanup?.();
		cleanup = undefined;
	});
}
