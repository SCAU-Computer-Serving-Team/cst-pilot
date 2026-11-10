import { type ChildProcess, execFile, spawn } from "node:child_process";
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

async function waitForExit(child: ChildProcess, timeout: number): Promise<void> {
	if (child.exitCode !== null || child.signalCode !== null) return;
	await new Promise<void>((resolve) => {
		const done = () => {
			clearTimeout(timer);
			child.removeListener("exit", done);
			resolve();
		};
		const timer = setTimeout(done, timeout);
		child.once("exit", done);
	});
}

async function terminateBrowser(child: ChildProcess): Promise<void> {
	if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
	if (process.platform === "win32") {
		await new Promise<void>((resolve) => {
			execFile("taskkill", ["/pid", String(child.pid), "/t", "/f"], { windowsHide: true, timeout: 5000 }, () =>
				resolve(),
			);
		});
	} else child.kill("SIGKILL");
	await waitForExit(child, 1000);
}

/** 隔离 Chrome 配置，浏览器验证不接触用户的标签页、账号或 Cookie。 */
export class BrowserProbe {
	private sequence = 0;
	private readonly observers = new Map<string, Set<(params: unknown) => void>>();
	private readonly pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
	private session = "";
	private readonly socket: WebSocket;
	private readonly child: ChildProcess;
	private readonly root: string;
	private closing: Promise<void> | undefined;
	private rejectPending(error: Error): void {
		for (const request of this.pending.values()) request.reject(error);
		this.pending.clear();
	}
	private constructor(socket: WebSocket, child: ChildProcess, root: string) {
		this.root = root;
		this.socket = socket;
		this.child = child;
		socket.addEventListener("close", () => this.rejectPending(new Error("Chrome 调试连接已关闭")));
		socket.addEventListener("error", () => this.rejectPending(new Error("Chrome 调试连接失败")));
		socket.addEventListener("message", (event) => {
			const message = JSON.parse(String((event as MessageEvent).data)) as {
				method?: string;
				params?: unknown;
				id?: number;
				result?: unknown;
				error?: { message: string };
			};
			if (message.method) for (const listener of this.observers.get(message.method) ?? []) listener(message.params);
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
				"--use-angle=swiftshader-webgl",
				"--window-size=1600,1000",
				"about:blank",
			],
			{ stdio: ["ignore", "ignore", "pipe"], windowsHide: true },
		);
		let stderr = "";
		let spawnError: Error | undefined;
		child.stderr?.on("data", (data: Buffer) => {
			stderr = (stderr + data.toString()).slice(-16_384);
		});
		child.on("error", (error) => {
			spawnError = error;
		});
		let socket: WebSocket | undefined;
		try {
			const deadline = Date.now() + 15_000;
			let version: { webSocketDebuggerUrl: string } | undefined;
			while (Date.now() < deadline && !version) {
				if (spawnError) throw spawnError;
				if (child.exitCode !== null || child.signalCode !== null) throw new Error("Chrome 启动后退出");
				version = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(500) })
					.then((response) => response.json())
					.catch(() => undefined);
				if (!version) await new Promise((resolve) => setTimeout(resolve, 100));
			}
			if (!version?.webSocketDebuggerUrl) throw new Error("Chrome 调试端口未就绪");
			socket = new WebSocket(version.webSocketDebuggerUrl);
			const connection = socket;
			await new Promise<void>((resolve, reject) => {
				const finish = (error?: Error) => {
					clearTimeout(timer);
					connection.removeEventListener("open", opened);
					connection.removeEventListener("error", failed);
					connection.removeEventListener("close", failed);
					if (error) reject(error);
					else resolve();
				};
				const opened = () => finish();
				const failed = () => finish(new Error("Chrome 调试连接失败"));
				const timer = setTimeout(() => finish(new Error("Chrome 调试连接超时")), 5000);
				connection.addEventListener("open", opened);
				connection.addEventListener("error", failed);
				connection.addEventListener("close", failed);
			});
			const browser = new BrowserProbe(socket, child, root);
			const target = await browser.call<{ targetId: string }>("Target.createTarget", { url: "about:blank" });
			const attached = await browser.call<{ sessionId: string }>("Target.attachToTarget", {
				targetId: target.targetId,
				flatten: true,
			});
			browser.session = attached.sessionId;
			await browser.call("Page.enable");
			const environment = await browser.evaluate<{ userAgent: string; reducedMotion: boolean }>(
				"({userAgent:navigator.userAgent,reducedMotion:matchMedia('(prefers-reduced-motion: reduce)').matches})",
			);
			// CI 主机可能关闭系统动画；常规动效用固定偏好，降级用例再显式切换。
			await browser.call("Emulation.setEmulatedMedia", {
				features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
			});
			await writeFile(
				join(root, "browser-environment.json"),
				JSON.stringify({ ...environment, baselineReducedMotion: false }, null, 2),
			);
			return browser;
		} catch (error) {
			socket?.close();
			await terminateBrowser(child);
			const message = error instanceof Error ? error.message : String(error);
			const diagnostic = {
				executable: chrome,
				pid: child.pid,
				exitCode: child.exitCode,
				signalCode: child.signalCode,
				error: message,
				stderr,
			};
			await writeFile(join(root, "browser-startup.json"), JSON.stringify(diagnostic, null, 2));
			throw new Error(`${message} (exit=${child.exitCode}, signal=${child.signalCode})\n${stderr}`, {
				cause: error,
			});
		}
	}
	onEvent(method: string, listener: (params: unknown) => void): () => void {
		const listeners = this.observers.get(method) ?? new Set();
		listeners.add(listener);
		this.observers.set(method, listeners);
		return () => {
			listeners.delete(listener);
			if (!listeners.size) this.observers.delete(method);
		};
	}
	async call<T = unknown>(method: string, params: Record<string, unknown> = {}, sessionId = this.session): Promise<T> {
		if (this.socket.readyState !== WebSocket.OPEN) throw new Error("Chrome 调试连接已关闭");
		const id = ++this.sequence;
		const result = new Promise<unknown>((resolve, reject) => {
			const timer = setTimeout(() => {
				this.pending.delete(id);
				reject(new Error(`Chrome 调试请求超时：${method}`));
			}, 30_000);
			this.pending.set(id, {
				resolve: (value) => {
					clearTimeout(timer);
					resolve(value);
				},
				reject: (error) => {
					clearTimeout(timer);
					reject(error);
				},
			});
			try {
				this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
			} catch (error) {
				clearTimeout(timer);
				this.pending.delete(id);
				reject(error);
			}
		});
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
				if (
					!(error instanceof Error) ||
					!/Execution context was destroyed|Cannot find context|Inspected target navigated or closed/.test(
						error.message,
					)
				)
					throw error;
			}
			await new Promise((resolve) => setTimeout(resolve, 40));
		}
		const diagnostic = await this.evaluate(`(() => ({
			label: ${JSON.stringify(label)}, url: location.pathname, hidden: document.hidden,
			layout: document.querySelector('.app-layout')?.className,
			transition: document.querySelector('.app-layout')?.dataset.homeTransition,
			animations: document.getAnimations().map(a=>({state:a.playState,time:a.currentTime,pseudo:a.effect?.pseudoElement,target:a.effect?.target?.className})),
			body: document.body.innerText.slice(0,1200)
		}))()`);
		await writeFile(join(this.root, "failure-state.json"), JSON.stringify(diagnostic, null, 2));
		throw new Error(`${label} 未出现：${JSON.stringify(diagnostic)}`);
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
		let point = await this.evaluate<{ x: number; y: number }>(`(() => {
			const element = document.querySelector(${JSON.stringify(selector)});
			if (!element) throw new Error("找不到可点击控件");
			let rect = element.getBoundingClientRect();
			const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
			if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) {
				element.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
				rect = element.getBoundingClientRect();
			}
			return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
		})()`);
		await this.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
		point = await this.evaluate<{ x: number; y: number }>(`(async () => {
			const element = document.querySelector(${JSON.stringify(selector)});
			let previous, stable = 0, obstacle = "";
			for (let frame = 0; frame < 90; frame++) {
				await new Promise(resolve => requestAnimationFrame(resolve));
				const rect = element.getBoundingClientRect();
				stable = previous && Math.abs(rect.x - previous.x) < .25 && Math.abs(rect.y - previous.y) < .25 && Math.abs(rect.width - previous.width) < .25 && Math.abs(rect.height - previous.height) < .25 ? stable + 1 : 0;
				previous = rect;
				if (stable >= 2) {
					const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
					const target = document.elementFromPoint(x, y);
					if (!element.contains(target)) {
						obstacle = target?.outerHTML.slice(0,600) ?? "视口之外";
						if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) {
							element.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
							stable = 0;
						}
						continue;
					}
					return { x, y };
				}
			}
			throw new Error("点击控件持续不可点击："+${JSON.stringify(selector)}+"；移动或遮挡："+obstacle);
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
	close(): Promise<void> {
		this.closing ??= this.closeBrowser();
		return this.closing;
	}
	private async closeBrowser(): Promise<void> {
		// 正常关闭整个隔离实例；异常时仅回收本实例的进程树。
		if (this.socket.readyState === WebSocket.OPEN) {
			this.socket.send(JSON.stringify({ id: ++this.sequence, method: "Browser.close" }));
			await waitForExit(this.child, 5000);
		}
		await terminateBrowser(this.child);
		this.rejectPending(new Error("Chrome 调试连接已关闭"));
		this.socket.close();
	}
}
