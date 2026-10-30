import { type ChildProcess, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { ExportError } from "./snapshot.ts";

type Pending = {
	resolve: (value: unknown) => void;
	reject: (error: Error) => void;
	timer: ReturnType<typeof setTimeout>;
};

/** 仅供产物渲染的隔离浏览器；不读取用户配置，不安装或下载运行时。 */
export class ExportBrowser {
	private sequence = 0;
	private closing?: Promise<void>;
	private session = "";
	private readonly pending = new Map<number, Pending>();
	private readonly child: ChildProcess;
	private readonly socket: WebSocket;
	private readonly profile: string;
	private constructor(child: ChildProcess, socket: WebSocket, profile: string) {
		this.child = child;
		this.socket = socket;
		this.profile = profile;
		socket.addEventListener("message", (event) => {
			const message = JSON.parse(String(event.data)) as {
				id?: number;
				result?: unknown;
				error?: { message: string };
			};
			if (!message.id) return;
			const pending = this.pending.get(message.id);
			if (!pending) return;
			this.pending.delete(message.id);
			clearTimeout(pending.timer);
			if (message.error) pending.reject(new Error(message.error.message));
			else pending.resolve(message.result);
		});
		socket.addEventListener("close", () => this.rejectPending());
		socket.addEventListener("error", () => this.rejectPending());
	}
	private rejectPending() {
		for (const item of this.pending.values()) {
			clearTimeout(item.timer);
			item.reject(new Error("导出浏览器连接已关闭"));
		}
		this.pending.clear();
	}
	static async launch(root: string): Promise<ExportBrowser> {
		const executable = [
			process.env.CST_EXPORT_BROWSER,
			"C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
			"C:/Program Files/Microsoft/Edge/Application/msedge.exe",
			"C:/Program Files/Google/Chrome/Application/chrome.exe",
			"C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
			join(process.env.LOCALAPPDATA ?? "", "Google/Chrome/Application/chrome.exe"),
			process.env.CHROME_PATH,
		].find((candidate) => candidate && existsSync(candidate));
		if (!executable)
			throw new ExportError(
				503,
				"export_browser_missing",
				"导出需要已安装的 Edge 或 Chrome，可用 CST_EXPORT_BROWSER 指定浏览器路径。",
			);
		const profile = await mkdtemp(join(root, "browser-"));
		const child = spawn(
			executable,
			[
				"--headless=new",
				"--remote-debugging-port=0",
				"--remote-debugging-address=127.0.0.1",
				`--user-data-dir=${profile}`,
				"--no-first-run",
				"--no-default-browser-check",
				"--disable-background-networking",
				"--disable-extensions",
				"--disable-sync",
				"--disable-component-update",
				"--hide-scrollbars",
				"--window-size=1240,900",
				"about:blank",
			],
			{ stdio: "ignore", windowsHide: true },
		);
		let spawnError: Error | undefined;
		child.on("error", (error) => {
			spawnError = error;
		});
		let browser: ExportBrowser | undefined;
		try {
			let endpoint: string | undefined;
			for (let attempt = 0; attempt < 100; attempt++) {
				if (spawnError || child.exitCode !== null) throw new Error("导出浏览器未能启动");
				const active = await readFile(join(profile, "DevToolsActivePort"), "utf8").catch(() => "");
				const [port, path] = active.trim().split(/\r?\n/);
				if (/^\d+$/.test(port ?? "") && path?.startsWith("/devtools/browser/")) {
					endpoint = `ws://127.0.0.1:${port}${path}`;
					break;
				}
				await delay(100);
			}
			if (!endpoint) throw new Error("导出浏览器启动超时");
			const socket = new WebSocket(endpoint);
			await new Promise<void>((resolve, reject) => {
				const timer = setTimeout(() => {
					socket.close();
					reject(new Error("导出浏览器连接超时"));
				}, 10_000);
				socket.addEventListener(
					"open",
					() => {
						clearTimeout(timer);
						resolve();
					},
					{ once: true },
				);
				socket.addEventListener(
					"error",
					() => {
						clearTimeout(timer);
						reject(new Error("导出浏览器连接失败"));
					},
					{ once: true },
				);
			});
			browser = new ExportBrowser(child, socket, profile);
			const target = await browser.call<{ targetId: string }>("Target.createTarget", { url: "about:blank" });
			const attached = await browser.call<{ sessionId: string }>("Target.attachToTarget", {
				targetId: target.targetId,
				flatten: true,
			});
			browser.session = attached.sessionId;
			await browser.call("Page.enable");
			await browser.call("Emulation.setDeviceMetricsOverride", {
				width: 1240,
				height: 900,
				deviceScaleFactor: 2,
				mobile: false,
			});
			await browser.call("Emulation.setEmulatedMedia", {
				features: [{ name: "prefers-reduced-motion", value: "reduce" }],
			});
			return browser;
		} catch (error) {
			if (browser) await browser.close();
			else {
				child.kill();
				await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }).catch(() => undefined);
			}
			throw error;
		}
	}
	async call<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
		const id = ++this.sequence;
		return new Promise<T>((resolve, reject) => {
			const timer = setTimeout(() => {
				this.pending.delete(id);
				reject(new Error("导出浏览器操作超时"));
			}, 30_000);
			this.pending.set(id, { resolve: (value) => resolve(value as T), reject, timer });
			try {
				this.socket.send(
					JSON.stringify({ id, method, params, ...(this.session ? { sessionId: this.session } : {}) }),
				);
			} catch (error) {
				clearTimeout(timer);
				this.pending.delete(id);
				reject(error);
			}
		});
	}
	async evaluate<T>(expression: string): Promise<T> {
		const result = await this.call<{ result: { value: T }; exceptionDetails?: unknown }>("Runtime.evaluate", {
			expression,
			awaitPromise: true,
			returnByValue: true,
		});
		if (result.exceptionDetails) throw new Error("导出页面渲染失败");
		return result.result.value;
	}
	async ready(url: string): Promise<void> {
		await this.call("Page.navigate", { url });
		for (let attempt = 0; attempt < 600; attempt++) {
			if (this.socket.readyState !== WebSocket.OPEN) throw new Error("导出浏览器已关闭");
			const state = await this.evaluate<string>("document.documentElement.dataset.exportState || ''").catch(
				() => "",
			);
			if (state === "ready") return;
			if (state === "failed") throw new Error("导出内容未能加载");
			await delay(100);
		}
		throw new Error("导出页面准备超时");
	}
	close(): Promise<void> {
		this.closing ??= this.dispose();
		return this.closing;
	}
	private async dispose(): Promise<void> {
		this.rejectPending();
		if (this.socket.readyState === WebSocket.OPEN) {
			this.socket.send(JSON.stringify({ id: ++this.sequence, method: "Browser.close" }));
			for (let attempt = 0; attempt < 50 && this.child.exitCode === null; attempt++) await delay(100);
		}
		if (this.child.exitCode === null) this.child.kill();
		this.socket.close();
		await rm(this.profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }).catch(() => undefined);
	}
}
