import { join, resolve } from "node:path";
import type { ModelsRefreshOptions, ModelsRefreshResult } from "@earendil-works/pi-ai";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { catalogProviderFactories } from "../oauth/catalog-factories.ts";

export interface CatalogUpdate {
	changed: boolean;
	aborted: boolean;
	failedProviders: string[];
}
interface CatalogOptions {
	createRuntime?: () => Promise<ModelRuntime>;
	automatic?: boolean;
}
interface Binding {
	refresh: ModelRuntime["refresh"];
	login: ModelRuntime["login"];
	logout: ModelRuntime["logout"];
	setRuntimeApiKey: ModelRuntime["setRuntimeApiKey"];
	removeRuntimeApiKey: ModelRuntime["removeRuntimeApiKey"];
	tail: Promise<unknown>;
	users: number;
}
const cancelled = (): ModelsRefreshResult => ({ aborted: true, errors: new Map() });

/** 目录联网独占一个运行时；界面运行时只重读共享缓存，互不取消 Provider 刷新。 */
export class ModelCatalog {
	private readonly createRuntime: () => Promise<ModelRuntime>;
	private readonly automatic: boolean;
	private worker?: Promise<ModelRuntime>;
	private readonly appliedRuntimeKeys = new Map<string, string>();
	private readonly home: string;
	private readonly bindings = new Map<ModelRuntime, Binding>();
	private readonly listeners = new Set<(update: CatalogUpdate) => void>();
	private readonly pending = new Map<string, Promise<ModelsRefreshResult>>();
	private tail: Promise<unknown> = Promise.resolve();
	private active?: AbortController;
	private generation = 0;
	private credentialWrites = 0;
	private readonly changedCredentials = new Set<string>();
	private nextCheckAt = 0;
	private timer?: ReturnType<typeof setInterval>;
	private closed = false;

	constructor(home: string, options: CatalogOptions = {}) {
		this.automatic = options.automatic ?? true;
		this.home = home;
		this.createRuntime =
			options.createRuntime ??
			(async () => {
				const runtime = await ModelRuntime.create({
					authPath: join(home, "auth.json"),
					modelsPath: join(home, "models.json"),
					modelsStorePath: join(home, "models-store.json"),
					refreshOnCreate: false,
				});
				return runtime;
			});
	}

	attach(runtime: ModelRuntime): () => void {
		this.closed = false;
		let binding = this.bindings.get(runtime);
		if (!binding) {
			binding = {
				refresh: runtime.refresh.bind(runtime),
				login: runtime.login.bind(runtime),
				logout: runtime.logout.bind(runtime),
				setRuntimeApiKey: runtime.setRuntimeApiKey.bind(runtime),
				removeRuntimeApiKey: runtime.removeRuntimeApiKey.bind(runtime),
				tail: Promise.resolve(),
				users: 0,
			};
			const original = binding;
			this.bindings.set(runtime, binding);
			// 原生 TUI 选择器调用 refresh({signal})，仍沿用其缓存首屏与异步更新流程。
			runtime.refresh = (options = {}) =>
				options.allowNetwork === false ? this.reload(original, options) : this.refresh(options);
			runtime.login = (...args) => this.changeCredential(args[0], () => original.login(...args));
			runtime.logout = (...args) => this.changeCredential(args[0], () => original.logout(...args));
			runtime.setRuntimeApiKey = (...args) =>
				this.changeCredential(args[0], () => original.setRuntimeApiKey(...args));
			runtime.removeRuntimeApiKey = (...args) =>
				this.changeCredential(args[0], () => original.removeRuntimeApiKey(...args));
		}
		binding.users++;
		if (this.automatic && !this.timer) {
			this.timer = setInterval(() => {
				this.check();
			}, 5 * 60_000);
			this.timer.unref();
			this.check();
		}
		let released = false;
		return () => {
			if (released) return;
			released = true;
			if (--binding.users > 0) return;
			runtime.refresh = binding.refresh;
			runtime.login = binding.login;
			runtime.logout = binding.logout;
			runtime.setRuntimeApiKey = binding.setRuntimeApiKey;
			runtime.removeRuntimeApiKey = binding.removeRuntimeApiKey;
			this.bindings.delete(runtime);
			if (!this.bindings.size) void this.close();
		};
	}

	/** 凭据操作共用取消和刷新入口，连续变更只在完成后刷新受影响的服务。 */
	private async changeCredential<T>(providerId: string, action: () => Promise<T>): Promise<T> {
		this.credentialWrites++;
		this.invalidate();
		try {
			const result = await action();
			this.changedCredentials.add(providerId);
			return result;
		} finally {
			this.credentialWrites--;
			this.invalidate();
			if (!this.credentialWrites && this.changedCredentials.size) {
				const providers = [...this.changedCredentials];
				this.changedCredentials.clear();
				this.nextCheckAt = Date.now() + 30_000;
				void this.refresh({ providers, force: true });
			}
		}
	}

	subscribe(listener: (update: CatalogUpdate) => void): () => void {
		this.listeners.add(listener);
		return () => {
			this.listeners.delete(listener);
		};
	}

	private reload(binding: Binding, options: ModelsRefreshOptions): Promise<ModelsRefreshResult> {
		const task = binding.tail.then(() => binding.refresh({ ...options, allowNetwork: false }));
		binding.tail = task.catch(() => undefined);
		return task;
	}

	/** 登录、退出或服务配置变更后，旧任务不得继续发布目录。 */
	invalidate(): void {
		this.generation++;
		this.active?.abort();
		this.pending.clear();
		this.nextCheckAt = 0;
	}

	/** 页面重读、发送输入与定时检查在 30 秒内合并，失败不会形成 SSE 重读循环。 */
	check(): void {
		if (this.closed || this.credentialWrites || Date.now() < this.nextCheckAt) return;
		this.nextCheckAt = Date.now() + 30_000;
		void this.refresh();
	}

	refresh(options: ModelsRefreshOptions = {}): Promise<ModelsRefreshResult> {
		if (this.closed || options.signal?.aborted) return Promise.resolve(cancelled());
		const key = JSON.stringify([options.providers ? [...new Set(options.providers)].sort() : null, !!options.force]);
		let task = this.pending.get(key);
		if (!task) {
			const generation = this.generation;
			task = this.tail.then(() => this.run(options, generation));
			this.pending.set(key, task);
			this.tail = task.catch(() => undefined);
			const current = task;
			void task.finally(() => {
				if (this.pending.get(key) === current) this.pending.delete(key);
			});
		}
		const signal = options.signal;
		if (!signal) return task;
		// 单个菜单关闭只取消等待；其他菜单、Web 与后台调度继续共用这次刷新。
		return new Promise((resolve) => {
			const abort = () => {
				resolve(cancelled());
			};
			signal.addEventListener("abort", abort, { once: true });
			void task.then(resolve).finally(() => signal.removeEventListener("abort", abort));
		});
	}

	/** 临时密钥只复制到后台运行时内存；文件凭据与 OAuth 继续由 Pi 管理。 */
	private async syncRuntimeKeys(worker: ModelRuntime, signal: AbortSignal): Promise<void> {
		const keys = new Map<string, string>();
		for (const runtime of this.bindings.keys()) {
			for (const provider of runtime.getProviders()) {
				if (runtime.getProviderAuthStatus(provider.id).source !== "runtime" || !worker.getProvider(provider.id))
					continue;
				const resolved = await runtime.getAuth(provider.id, { signal });
				if (resolved?.auth.apiKey) keys.set(provider.id, resolved.auth.apiKey);
			}
		}
		for (const id of this.appliedRuntimeKeys.keys()) {
			if (keys.has(id)) continue;
			await worker.removeRuntimeApiKey(id, { signal });
			this.appliedRuntimeKeys.delete(id);
		}
		for (const [id, key] of keys) {
			if (this.appliedRuntimeKeys.get(id) === key) continue;
			await worker.setRuntimeApiKey(id, key, { signal });
			this.appliedRuntimeKeys.set(id, key);
		}
	}

	private async run(options: ModelsRefreshOptions, generation: number): Promise<ModelsRefreshResult> {
		if (this.closed || this.credentialWrites || generation !== this.generation) return cancelled();
		const controller = new AbortController();
		this.active = controller;
		const timeout = setTimeout(() => controller.abort(), 15_000);
		let result: ModelsRefreshResult;
		let changed = false;
		try {
			this.worker ??= this.createRuntime();
			const worker = await this.worker;
			for (const [id, factory] of catalogProviderFactories(this.home))
				if (!worker.getRegisteredNativeProvider(id)) worker.registerNativeProvider(factory());
			await this.syncRuntimeKeys(worker, controller.signal);
			if (controller.signal.aborted) return cancelled();
			const before = JSON.stringify(worker.getModels());
			result = await worker.refresh({
				providers: options.providers,
				force: options.force,
				allowNetwork: true,
				signal: controller.signal,
			});
			if (generation !== this.generation || this.closed) return cancelled();
			changed = before !== JSON.stringify(worker.getModels());
			const errors = new Map(result.errors);
			await Promise.all(
				[...this.bindings.values()].map(async (binding) => {
					try {
						await this.reload(binding, { allowNetwork: false });
					} catch {
						errors.set("configuration", new Error("本地模型目录无法重读。"));
					}
				}),
			);
			result = { aborted: result.aborted, errors };
		} catch {
			this.worker = undefined;
			this.appliedRuntimeKeys.clear();
			result = {
				aborted: controller.signal.aborted,
				errors: new Map([["catalog", new Error("模型目录暂时无法更新。")]]),
			};
		} finally {
			clearTimeout(timeout);
			if (this.active === controller) this.active = undefined;
		}
		if (generation !== this.generation || this.closed) return cancelled();
		if (changed || result.aborted || result.errors.size) {
			const update = { changed, aborted: result.aborted, failedProviders: [...result.errors.keys()] };
			for (const listener of this.listeners) {
				try {
					listener(update);
				} catch {
					/* 订阅者失败不影响目录与其他订阅者。 */
				}
			}
		}
		return result;
	}

	async close(): Promise<void> {
		this.closed = true;
		if (this.timer) clearInterval(this.timer);
		this.timer = undefined;
		this.invalidate();
		await this.tail;
		this.worker = undefined;
		this.appliedRuntimeKeys.clear();
	}
}

const key = Symbol.for("cst-pilot/model-catalogs");
const holder = globalThis as typeof globalThis & { [key]?: Map<string, ModelCatalog> };
const catalogs = holder[key] ?? new Map<string, ModelCatalog>();
holder[key] = catalogs;
export function getModelCatalog(home: string): ModelCatalog {
	const path = resolve(home);
	const id = process.platform === "win32" ? path.toLowerCase() : path;
	let catalog = catalogs.get(id);
	if (!catalog) {
		catalog = new ModelCatalog(path);
		catalogs.set(id, catalog);
	}
	return catalog;
}
