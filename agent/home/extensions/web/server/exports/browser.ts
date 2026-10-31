import { existsSync } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import puppeteer, { type Browser, type BrowserContext, type Page } from "puppeteer-core";
import { ExportError } from "./snapshot.ts";

type BrowserLease = { browser: Browser; profile: string };
type BrowserOptions = { workDir: string; executablePath?: string; idleMs?: number };

function executablePath(explicit?: string): string {
	const configured = explicit ?? process.env.CST_EXPORT_BROWSER ?? process.env.CHROME_PATH;
	const candidates = configured
		? [configured]
		: [
				"C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
				"C:/Program Files/Microsoft/Edge/Application/msedge.exe",
				"C:/Program Files/Google/Chrome/Application/chrome.exe",
				"C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
				join(process.env.LOCALAPPDATA ?? "", "Google/Chrome/Application/chrome.exe"),
			];
	const found = candidates.find((candidate) => existsSync(candidate));
	if (!found)
		throw new ExportError(
			503,
			"export_browser_missing",
			"导出需要已安装的 Edge 或 Chrome，可用 CST_EXPORT_BROWSER 指定浏览器路径。",
		);
	return found;
}

/** 专用浏览器按需启动并复用；每次调用创建独立环境，关闭页面即清除任务数据。 */
export class ExportBrowser {
	private readonly options: BrowserOptions;
	private starting?: Promise<BrowserLease>;
	private readonly retiring = new Set<Promise<void>>();
	private idle?: ReturnType<typeof setTimeout>;
	private operations = 0;
	private stopped = false;
	private closing?: Promise<void>;
	constructor(options: BrowserOptions) {
		this.options = options;
	}
	private async launch(): Promise<BrowserLease> {
		const executable = executablePath(this.options.executablePath);
		await mkdir(this.options.workDir, { recursive: true });
		const profile = await mkdtemp(join(this.options.workDir, "browser-"));
		let browser: Browser | undefined;
		try {
			browser = await puppeteer.launch({
				executablePath: executable,
				userDataDir: profile,
				headless: true,
				pipe: true,
				timeout: 10_000,
				protocolTimeout: 30_000,
				handleSIGINT: false,
				handleSIGTERM: false,
				handleSIGHUP: false,
				defaultViewport: { width: 1240, height: 3200, deviceScaleFactor: 2 },
				args: [
					"--disable-background-networking",
					"--disable-extensions",
					"--disable-sync",
					"--disable-component-update",
					"--hide-scrollbars",
				],
			});
			const version = await browser.version();
			const major = Number(/\/(\d+)\./.exec(version)?.[1]);
			if (!Number.isFinite(major) || major < 111)
				throw new ExportError(
					503,
					"export_browser_incompatible",
					"导出需要 Chromium 111 或更新版本，请更新 Edge / Chrome 或设置 CST_EXPORT_BROWSER。",
				);
			return { browser, profile };
		} catch (cause) {
			if (browser) await this.dispose({ browser, profile });
			else
				await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }).catch(() => undefined);
			if (cause instanceof ExportError) throw cause;
			throw new ExportError(
				503,
				"export_browser_launch_failed",
				"导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
			);
		}
	}
	private async acquire(): Promise<BrowserLease> {
		if (this.stopped) throw new ExportError(503, "export_stopping", "程序正在退出，请稍后导出");
		if (!this.starting) {
			const starting = this.launch().catch((cause) => {
				if (this.starting === starting) this.starting = undefined;
				throw cause;
			});
			this.starting = starting;
		}
		const starting = this.starting;
		const lease = await starting;
		if (this.stopped) throw new ExportError(503, "export_stopping", "程序正在退出，请稍后导出");
		if (!lease.browser.connected) {
			await this.retire(starting);
			return this.acquire();
		}
		return lease;
	}
	async run<T>(signal: AbortSignal, action: (page: Page) => Promise<T>): Promise<T> {
		signal.throwIfAborted();
		if (this.stopped) throw new ExportError(503, "export_stopping", "程序正在退出，请稍后导出");
		clearTimeout(this.idle);
		this.operations++;
		let context: BrowserContext | undefined;
		const stop = () => {
			void context?.close().catch(() => undefined);
		};
		try {
			const { browser } = await this.acquire();
			signal.throwIfAborted();
			context = await browser.createBrowserContext();
			signal.addEventListener("abort", stop, { once: true });
			signal.throwIfAborted();
			const page = await context.newPage();
			page.setDefaultTimeout(15_000);
			page.setDefaultNavigationTimeout(15_000);
			await page.emulateMediaFeatures([{ name: "prefers-reduced-motion", value: "reduce" }]);
			const result = await action(page);
			signal.throwIfAborted();
			return result;
		} finally {
			signal.removeEventListener("abort", stop);
			await context?.close().catch(() => undefined);
			this.operations--;
			if (!this.operations && !this.stopped) {
				this.idle = setTimeout(() => {
					void this.retire();
				}, this.options.idleMs ?? 30_000);
				this.idle.unref();
			}
		}
	}
	private async dispose(lease: BrowserLease): Promise<void> {
		let timer: ReturnType<typeof setTimeout> | undefined;
		await Promise.race([
			lease.browser.close().catch(() => undefined),
			new Promise<void>((resolve) => {
				timer = setTimeout(() => {
					lease.browser.process()?.kill();
					resolve();
				}, 5000);
			}),
		]);
		clearTimeout(timer);
		await rm(lease.profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }).catch(() => undefined);
	}
	private retire(current = this.starting): Promise<void> {
		if (!current || this.starting !== current) return Promise.resolve();
		clearTimeout(this.idle);
		this.starting = undefined;
		const task = current.then(
			(lease) => this.dispose(lease),
			() => undefined,
		);
		this.retiring.add(task);
		void task.finally(() => this.retiring.delete(task));
		return task;
	}
	close(): Promise<void> {
		this.stopped = true;
		this.closing ??= (async () => {
			await this.retire();
			await Promise.all(this.retiring);
		})();
		return this.closing;
	}
}
