import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { AgentSession, AgentSessionServices } from "@earendil-works/pi-coding-agent";
import {
	createAgentSessionFromServices,
	createAgentSessionServices,
	SessionManager,
} from "@earendil-works/pi-coding-agent";
import { WebUiBridge } from "./ui.ts";

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
	readonly ui: WebUiBridge;
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
	private readonly removed = new Set<string>();
	private createdWrites: Promise<void> = Promise.resolve();
	private canonical?: Promise<AgentSessionServices>;
	private closed = false;
	private readonly options: WebSessionPoolOptions;

	constructor(options: WebSessionPoolOptions) {
		this.options = options;
	}

	getServices(): Promise<AgentSessionServices> {
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
		const ui = new WebUiBridge();
		try {
			if (extensionsResult.errors.length) throw new Error("Web 会话扩展加载失败");
			await session.bindExtensions({ uiContext: ui.context(), mode: "rpc" });
			if (this.closed) throw new Error("Web 已退出");
			return { session, ui, id: session.sessionId, file: session.sessionFile };
		} catch (error) {
			ui.close();
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
		if (this.removed.has(id) || (await this.isDeleted(id))) throw new Error("会话不存在");
		const existing = this.slots.get(id) ?? this.opening.get(id);
		if (existing) return existing;
		const matches = (await SessionManager.listAll(this.options.sessionDir)).filter((entry) => entry.id === id);
		if (matches.length > 1) throw new Error("会话不存在或 ID 不唯一");
		if (matches.length === 1)
			return this.open(id, () => SessionManager.open(matches[0].path, this.options.sessionDir));
		const created = await this.readCreated();
		if (!Object.values(created).includes(id)) throw new Error("会话不存在或 ID 不唯一");
		return this.open(id, () => SessionManager.create(this.options.cwd, this.options.sessionDir, { id }));
	}

	async create(): Promise<WebSessionSlot> {
		if (this.closed) throw new Error("Web 已退出");
		const manager = SessionManager.create(this.options.cwd, this.options.sessionDir);
		return this.open(manager.getSessionId(), () => manager);
	}

	private deletedFile(id: string): string {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(id)) throw new Error("会话 ID 无效");
		return join(this.options.agentDir, "web-inbox", "deleted", id);
	}

	private async isDeleted(id: string): Promise<boolean> {
		return stat(this.deletedFile(id)).then(
			() => true,
			(error: NodeJS.ErrnoException) => {
				if (error.code === "ENOENT") return false;
				throw error;
			},
		);
	}

	private createdFile(): string {
		return join(this.options.agentDir, "web-inbox", "created.json");
	}

	private async readCreated(): Promise<Record<string, string | null>> {
		try {
			const data: unknown = JSON.parse(await readFile(this.createdFile(), "utf8"));
			if (
				!data ||
				typeof data !== "object" ||
				Array.isArray(data) ||
				!Object.entries(data).every(
					([key, value]) =>
						/^[a-zA-Z0-9_-]{1,128}$/.test(key) &&
						(value === null || (typeof value === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(value))),
				)
			) {
				throw new Error("会话索引损坏");
			}
			return data as Record<string, string | null>;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return {};
			throw error;
		}
	}

	private async writeCreated(entries: Record<string, string | null>): Promise<void> {
		const file = this.createdFile();
		await mkdir(join(file, ".."), { recursive: true });
		const temp = `${file}.${randomUUID()}.tmp`;
		try {
			await writeFile(temp, JSON.stringify(entries), { flag: "wx" });
			await rename(temp, file);
		} catch (error) {
			await rm(temp, { force: true }).catch(() => undefined);
			throw error;
		}
	}

	/** Reserve the Pi session ID durably before instantiating a writer. */
	async createWithKey(key: string): Promise<WebSessionSlot> {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(key) || this.closed) throw new Error("创建请求无效");
		const task = this.createdWrites.then(async () => {
			const entries = await this.readCreated();
			if (Object.hasOwn(entries, key)) {
				if (entries[key] === null) throw new Error("会话已删除");
				return entries[key]!;
			}
			const id = randomUUID();
			await this.writeCreated({ ...entries, [key]: id });
			return id;
		});
		this.createdWrites = task.then(
			() => undefined,
			() => undefined,
		);
		return this.openSaved(await task);
	}

	async fork(id: string, entryId: string): Promise<WebSessionSlot> {
		const source = await this.openSaved(id);
		if (source.session.isStreaming) throw new Error("会话正在执行");
		if (!source.session.sessionManager.getEntry(entryId)) throw new Error("分支节点不存在");
		const file = source.session.sessionManager.createBranchedSession(entryId);
		if (!file) throw new Error("会话尚未保存");
		const manager = SessionManager.open(file, this.options.sessionDir);
		return this.open(manager.getSessionId(), () => manager);
	}

	async deleteSaved(id: string): Promise<void> {
		const slot = await this.openSaved(id);
		if (slot.session.isStreaming) throw new Error("会话正在执行");
		this.removed.add(id);
		let tombstoned = false;
		try {
			const entries = await SessionManager.listAll(this.options.sessionDir);
			const matches = entries.filter((entry) => entry.id === id);
			if (matches.length > 1) throw new Error("会话 ID 不唯一");
			const tombstone = this.deletedFile(id);
			await mkdir(join(tombstone, ".."), { recursive: true });
			await writeFile(tombstone, "deleted", { flag: "wx" });
			tombstoned = true;
			slot.ui.close();
			slot.session.dispose();
			this.slots.delete(id);
			if (matches.length === 1) await rm(matches[0].path, { force: true });
			const created = await this.readCreated();
			for (const key of Object.keys(created)) if (created[key] === id) created[key] = null;
			await this.writeCreated(created);
		} catch (error) {
			if (!tombstoned) this.removed.delete(id);
			throw error;
		}
	}

	get(id: string): WebSessionSlot | undefined {
		return this.slots.get(id);
	}

	async list(): Promise<{ id: string; title: string; updatedAt?: string; running: boolean }[]> {
		const saved = (await SessionManager.listAll(this.options.sessionDir)).filter(
			(entry) => !this.removed.has(entry.id),
		);
		const visible = (
			await Promise.all(saved.map(async (entry) => ({ entry, deleted: await this.isDeleted(entry.id) })))
		)
			.filter(({ deleted }) => !deleted)
			.map(({ entry }) => entry);
		const rows: { id: string; title: string; updatedAt?: string; running: boolean }[] = visible.map((entry) => ({
			id: entry.id,
			title: entry.name ?? entry.firstMessage ?? "新对话",
			updatedAt: entry.modified.toISOString(),
			running: this.slots.get(entry.id)?.session.isStreaming ?? false,
		}));
		for (const id of new Set([...Object.values(await this.readCreated()), ...this.slots.keys()])) {
			if (id && !this.removed.has(id) && !(await this.isDeleted(id)) && !rows.some((entry) => entry.id === id))
				rows.push({
					id,
					title: "新对话",
					updatedAt: undefined,
					running: this.slots.get(id)?.session.isStreaming ?? false,
				});
		}
		return rows;
	}

	setScopedModels(models: Parameters<AgentSession["setScopedModels"]>[0]): void {
		for (const { session } of this.slots.values()) session.setScopedModels(models);
	}

	snapshot(): { id: string; running: boolean }[] {
		return [...this.slots.values()].map(({ id, session }) => ({ id, running: session.isStreaming }));
	}

	/** Never dispose a writer while it is still running. */
	async close(): Promise<void> {
		this.closed = true;
		await this.createdWrites;
		await Promise.allSettled([...this.opening.values()]);
		await Promise.allSettled([...this.slots.values()].map(({ session }) => session.abort()));
		for (const { session, ui } of this.slots.values()) {
			ui.close();
			session.dispose();
		}
		this.slots.clear();
	}
}
