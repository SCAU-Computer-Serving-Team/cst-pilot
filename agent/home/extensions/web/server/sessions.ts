import type { AgentSession, AgentSessionServices } from "@earendil-works/pi-coding-agent";
import {
	createAgentSessionFromServices,
	createAgentSessionServices,
	SessionManager,
} from "@earendil-works/pi-coding-agent";

// The browser must never choose tools. Keep the set no broader than the kit's read/diagnostic tools.
const WEB_TOOLS = [
	"read",
	"ls",
	"disk",
	"sys",
	"startup",
	"eventlog",
	"driver",
	"runbook",
	"web_search",
	"source_check",
	"fetch_content",
	"get_search_content",
	"ffgrep",
	"fffind",
];

export interface WebSessionSlot {
	readonly session: AgentSession;
	readonly id: string;
	readonly file?: string;
}

export interface WebSessionPoolOptions {
	cwd: string;
	agentDir: string;
	sessionDir: string;
	/** Suppress registration of the Web extension while its own loader runs. */
	withLoader<T>(load: () => Promise<T>): Promise<T>;
}

export class WebSessionPool {
	private readonly slots = new Map<string, WebSessionSlot>();
	private readonly opening = new Map<string, Promise<WebSessionSlot>>();
	private canonical?: Promise<AgentSessionServices>;
	private closed = false;
	private readonly options: WebSessionPoolOptions;

	constructor(options: WebSessionPoolOptions) {
		this.options = options;
	}

	private getServices(): Promise<AgentSessionServices> {
		this.canonical ??= this.options.withLoader(() =>
			createAgentSessionServices({ cwd: this.options.cwd, agentDir: this.options.agentDir }),
		);
		return this.canonical;
	}

	private async build(manager: SessionManager): Promise<WebSessionSlot> {
		const canonical = await this.getServices();
		const services = await this.options.withLoader(() =>
			createAgentSessionServices({
				cwd: this.options.cwd,
				agentDir: this.options.agentDir,
				settingsManager: canonical.settingsManager,
				modelRuntime: canonical.modelRuntime,
			}),
		);
		if (services.diagnostics.some((item) => item.type === "error")) {
			throw new Error("Web 会话资源加载失败");
		}
		const { session, extensionsResult } = await createAgentSessionFromServices({
			services,
			sessionManager: manager,
			tools: WEB_TOOLS,
		});
		try {
			if (extensionsResult.errors.length) throw new Error("Web 会话扩展加载失败");
			await session.bindExtensions({});
			if (this.closed) throw new Error("Web 已退出");
			return { session, id: session.sessionId, file: session.sessionFile };
		} catch (error) {
			session.dispose();
			throw error;
		}
	}

	private async open(key: string, manager: () => SessionManager): Promise<WebSessionSlot> {
		if (this.closed) throw new Error("Web 已退出");
		const existing = this.slots.get(key);
		if (existing) return existing;
		const pending = this.opening.get(key);
		if (pending) return pending;
		const task = (async () => {
			const slot = await this.build(manager());
			if (slot.id !== key) {
				slot.session.dispose();
				throw new Error("会话 ID 与记录不一致");
			}
			this.slots.set(key, slot);
			return slot;
		})();
		this.opening.set(key, task);
		try {
			return await task;
		} finally {
			this.opening.delete(key);
		}
	}

	/** Called only with the TUI's saved file, after the TUI has switched away. */
	openHandoff(id: string, file: string): Promise<WebSessionSlot> {
		return this.open(id, () => SessionManager.open(file, this.options.sessionDir));
	}

	/** Resolve IDs against the kit's session directory, never against an HTTP path. */
	async openSaved(id: string): Promise<WebSessionSlot> {
		if (this.closed) throw new Error("Web 已退出");
		const existing = this.slots.get(id) ?? this.opening.get(id);
		if (existing) return existing;
		const matches = (await SessionManager.listAll(this.options.sessionDir)).filter((entry) => entry.id === id);
		if (matches.length !== 1) throw new Error("会话不存在或 ID 不唯一");
		return this.open(id, () => SessionManager.open(matches[0].path, this.options.sessionDir));
	}

	async create(): Promise<WebSessionSlot> {
		if (this.closed) throw new Error("Web 已退出");
		const manager = SessionManager.create(this.options.cwd, this.options.sessionDir);
		return this.open(manager.getSessionId(), () => manager);
	}

	get(id: string): WebSessionSlot | undefined {
		return this.slots.get(id);
	}

	snapshot(): { id: string; running: boolean }[] {
		return [...this.slots.values()].map(({ id, session }) => ({ id, running: session.isStreaming }));
	}

	/** Never dispose a writer while it is still running. */
	async close(): Promise<void> {
		this.closed = true;
		await Promise.allSettled([...this.opening.values()]);
		await Promise.allSettled([...this.slots.values()].map(({ session }) => session.abort()));
		for (const { session } of this.slots.values()) session.dispose();
		this.slots.clear();
	}
}
