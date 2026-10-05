import assert from "node:assert/strict";
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";

export async function freePort(): Promise<number> {
	const server = createServer();
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("无法分配测试端口");
	await new Promise<void>((resolve) => server.close(() => resolve()));
	return address.port;
}

/** 隔离 Chrome 配置，浏览器验证不接触用户的标签页、账号或 Cookie。 */
export class BrowserProbe {
	private sequence = 0;
	private readonly pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
	private session = "";
	private readonly socket: WebSocket;
	private readonly child: ChildProcess;
	private constructor(socket: WebSocket, child: ChildProcess) {
		this.socket = socket;
		this.child = child;
		socket.addEventListener("message", (event) => {
			const message = JSON.parse(String((event as MessageEvent).data)) as {
				id?: number;
				result?: unknown;
				error?: { message: string };
			};
			if (!message.id) return;
			const request = this.pending.get(message.id);
			if (!request) return;
			this.pending.delete(message.id);
			if (message.error) request.reject(new Error(message.error.message));
			else request.resolve(message.result);
		});
	}
	static async launch(root: string): Promise<BrowserProbe> {
		const chrome = [
			process.env.CHROME_PATH,
			"C:/Program Files/Google/Chrome/Application/chrome.exe",
			"C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
			join(process.env.LOCALAPPDATA ?? "", "Google/Chrome/Application/chrome.exe"),
		].find((path) => path && existsSync(path));
		if (!chrome) throw new Error("找不到 Chrome；使用 CHROME_PATH 指定");
		const port = await freePort();
		const profile = await mkdtemp(join(root, "ui-chrome-"));
		const child = spawn(
			chrome,
			[
				"--headless=new",
				`--remote-debugging-port=${port}`,
				`--user-data-dir=${profile}`,
				"--no-first-run",
				"--no-default-browser-check",
				"--disable-background-networking",
				"--enable-unsafe-swiftshader",
				"--use-angle=swiftshader",
				"--window-size=1600,1000",
				"about:blank",
			],
			{ stdio: "ignore" },
		);
		try {
			let version: { webSocketDebuggerUrl: string } | undefined;
			for (let attempt = 0; attempt < 100 && !version; attempt++) {
				version = await fetch(`http://127.0.0.1:${port}/json/version`)
					.then((response) => response.json())
					.catch(() => undefined);
				if (!version) await new Promise((resolve) => setTimeout(resolve, 100));
			}
			assert.ok(version, "Chrome 未启动");
			const socket = new WebSocket(version.webSocketDebuggerUrl);
			await new Promise<void>((resolve, reject) => {
				socket.addEventListener("open", () => resolve());
				socket.addEventListener("error", () => reject(new Error("Chrome 调试连接失败")));
			});
			const browser = new BrowserProbe(socket, child);
			const target = await browser.call<{ targetId: string }>("Target.createTarget", { url: "about:blank" });
			const attached = await browser.call<{ sessionId: string }>("Target.attachToTarget", {
				targetId: target.targetId,
				flatten: true,
			});
			browser.session = attached.sessionId;
			await browser.call("Page.enable");
			return browser;
		} catch (error) {
			child.kill();
			throw error;
		}
	}
	async call<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
		const id = ++this.sequence;
		const result = new Promise<unknown>((resolve, reject) => {
			this.pending.set(id, { resolve, reject });
		});
		this.socket.send(JSON.stringify({ id, method, params, ...(this.session ? { sessionId: this.session } : {}) }));
		return (await result) as T;
	}
	async evaluate<T = unknown>(expression: string): Promise<T> {
		const result = await this.call<{ result: { value?: T; description?: string }; exceptionDetails?: unknown }>(
			"Runtime.evaluate",
			{ expression, awaitPromise: true, returnByValue: true },
		);
		if (result.exceptionDetails)
			throw new Error(result.result.description ?? JSON.stringify(result.exceptionDetails));
		return result.result.value as T;
	}
	async until(expression: string, label: string, timeout = 15_000): Promise<void> {
		const deadline = Date.now() + timeout;
		while (Date.now() < deadline) {
			try {
				if (await this.evaluate(expression)) return;
			} catch (error) {
				if (!(error instanceof Error) || !/Execution context was destroyed|Cannot find context/.test(error.message))
					throw error;
			}
			await new Promise((resolve) => setTimeout(resolve, 40));
		}
		throw new Error(`${label} 未出现：${await this.evaluate("document.body.innerText.slice(0, 1200)")}`);
	}
	async navigate(url: string): Promise<void> {
		await this.call("Page.navigate", { url });
		await this.until(
			`location.href === ${JSON.stringify(url)} && document.readyState === "complete" && !!document.querySelector(".app-layout, .login-panel")`,
			"页面就绪",
		);
	}
	async click(selector: string): Promise<void> {
		await this.until(
			`(() => { const element = document.querySelector(${JSON.stringify(selector)}); return !!element && !element.disabled && element.getAttribute("aria-disabled") !== "true" && element.getBoundingClientRect().width > 0 && element.getBoundingClientRect().height > 0; })()`,
			`控件可操作：${selector}`,
		);
		const point = await this.evaluate<{ x: number; y: number }>(`(() => {
			const element = document.querySelector(${JSON.stringify(selector)});
			if (!element) throw new Error("找不到可点击控件");
			element.scrollIntoView({ block: "center" });
			const rect = element.getBoundingClientRect();
			return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
		})()`);
		await this.call("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
		await this.call("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
	}
	async fill(selector: string, value: string): Promise<void> {
		await this.evaluate(`(() => {
			const element = document.querySelector(${JSON.stringify(selector)});
			if (!element) throw new Error("找不到输入控件");
			element.focus();
			if (element.isContentEditable) {
				const range = document.createRange(); range.selectNodeContents(element);
				const selection = window.getSelection(); selection.removeAllRanges(); selection.addRange(range);
			} else element.select();
		})()`);
		await this.call("Input.insertText", { text: value });
	}
	async screenshot(path: string): Promise<void> {
		const result = await this.call<{ data: string }>("Page.captureScreenshot", { format: "png" });
		await writeFile(path, Buffer.from(result.data, "base64"));
	}
	async close(): Promise<void> {
		this.socket.close();
		this.child.kill();
	}
}
