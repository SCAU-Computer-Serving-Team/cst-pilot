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
	private readonly observers = new Map<string, Set<(params: unknown) => void>>();
	private readonly pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
	private session = "";
	private readonly socket: WebSocket;
	private readonly child: ChildProcess;
	private readonly root: string;
	private constructor(socket: WebSocket, child: ChildProcess, root: string) {
		this.root = root;
		this.socket = socket;
		this.child = child;
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
			child.kill();
			throw error;
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
			element.scrollIntoView({ block: "center" });
			const rect = element.getBoundingClientRect();
			return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
		})()`);
		await this.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
		point = await this.evaluate<{ x: number; y: number }>(`(async () => {
			const element = document.querySelector(${JSON.stringify(selector)});
			let previous, stable = 0;
			for (let frame = 0; frame < 90; frame++) {
				await new Promise(resolve => requestAnimationFrame(resolve));
				const rect = element.getBoundingClientRect();
				stable = previous && Math.abs(rect.x - previous.x) < .25 && Math.abs(rect.y - previous.y) < .25 && Math.abs(rect.width - previous.width) < .25 && Math.abs(rect.height - previous.height) < .25 ? stable + 1 : 0;
				previous = rect;
				if (stable >= 2) {
					const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
					const target = document.elementFromPoint(x, y);
					if (!element.contains(target)) throw new Error("点击控件被遮挡："+${JSON.stringify(selector)}+"；遮挡元素："+target?.outerHTML.slice(0,600));
					return { x, y };
				}
			}
			throw new Error("点击控件未停止移动");
		})()`);
		await this.call("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
		await this.call("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
	}
	async holdAnimations(selector: string): Promise<void> {
		await this.evaluate(`(() => {
			window.__heldAnimations = [];
			window.__holdSelector = ${JSON.stringify(selector)};
			if (!window.__nativeAnimate) {
				window.__nativeAnimate = Element.prototype.animate;
				Element.prototype.animate = function(...args) {
					const animation = window.__nativeAnimate.apply(this, args);
					if (window.__holdSelector && this.matches(window.__holdSelector)) {
						animation.pause(); animation.currentTime = 0;
						window.__heldAnimations.push(animation);
					}
					return animation;
				};
				const holdCss = event => {
					if (!window.__holdSelector || !event.target.matches(window.__holdSelector)) return;
					for (const animation of event.target.getAnimations()) {
						if (!window.__heldAnimations.includes(animation)) {
							animation.pause(); animation.currentTime = 0;
							window.__heldAnimations.push(animation);
						}
					}
				};
				document.addEventListener('transitionrun', holdCss, true);
				document.addEventListener('animationstart', holdCss, true);
			}
		})()`);
	}
	async releaseAnimations(): Promise<void> {
		await this.evaluate("window.__holdSelector=null;window.__heldAnimations.forEach(a=>a.finish())");
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
		const exited = new Promise<void>((resolve) => {
			if (this.child.exitCode !== null) resolve();
			else this.child.once("exit", () => resolve());
		});
		// Browser.close 关闭整个隔离实例；单独终止主进程可能残留 renderer/GPU 子进程。
		const id = ++this.sequence;
		this.socket.send(JSON.stringify({ id, method: "Browser.close" }));
		let timer: ReturnType<typeof setTimeout> | undefined;
		await Promise.race([
			exited,
			new Promise<void>((resolve) => {
				timer = setTimeout(resolve, 5000);
			}),
		]);
		clearTimeout(timer);
		if (this.child.exitCode === null) this.child.kill();
		this.socket.close();
	}
}
