import { resolve } from "node:path";
import { AgentSession, resolveModelScopeWithDiagnostics } from "@earendil-works/pi-coding-agent";
import { checkSharedConfiguration } from "./configuration.ts";
import { getModelCatalog, type ModelCatalog } from "./model-catalog.ts";
import { defaultModelReference, explicitModel } from "./model-selection.ts";
import { ownSession } from "./owner.ts";

interface RuntimeEntry {
	session: AgentSession;
	home: string;
	id: string;
	release: () => void;
	refreshing?: Promise<void>;
	catalog: ModelCatalog;
}
interface AdapterState {
	entries: Map<string, RuntimeEntry>;
	homes: WeakMap<AgentSession, string>;
	catalogs: WeakMap<AgentSession, ModelCatalog>;
	sessions: WeakMap<AgentSession, RuntimeEntry>;
	releases: WeakMap<AgentSession, () => void>;
	installed: WeakSet<object>;
}
const key = Symbol.for("cst-pilot/runtime-adapter-state");
const holder = globalThis as typeof globalThis & { [key]?: AdapterState };
const state = holder[key] ?? {
	entries: new Map(),
	homes: new WeakMap(),
	catalogs: new WeakMap(),
	sessions: new WeakMap(),
	releases: new WeakMap(),
	installed: new WeakSet(),
};
holder[key] = state;
function identity(home: string, id: string): string {
	const root = resolve(home);
	return `${process.platform === "win32" ? root.toLowerCase() : root}\0${id}`;
}
export function registerRuntimeSession(
	session: AgentSession,
	home: string,
	release?: () => void,
	catalog?: ModelCatalog,
): void {
	state.homes.set(session, home);
	if (catalog) state.catalogs.set(session, catalog);
	if (release) state.releases.set(session, release);
}
export async function refreshRuntime(
	session: AgentSession,
	{ checkCatalog = true, updateScope = true }: { checkCatalog?: boolean; updateScope?: boolean } = {},
): Promise<void> {
	const entry = state.sessions.get(session);
	if (!entry) return;
	if (entry.refreshing) return entry.refreshing;
	const task = (async () => {
		await session.settingsManager.reload();
		await session.modelRuntime.refresh({ allowNetwork: false });
		if (checkCatalog) entry.catalog.check();
		checkSharedConfiguration(session.settingsManager, session.modelRuntime);
		if (updateScope && !process.argv.some((arg) => arg === "--models" || arg.startsWith("--models="))) {
			const scope = await resolveModelScopeWithDiagnostics(
				session.settingsManager.getEnabledModels() ?? [],
				session.modelRuntime,
			);
			session.setScopedModels(scope.scopedModels);
		}
		const selected = session.model;
		const latest = selected && session.modelRuntime.getModel(selected.provider, selected.id);
		if (session.isIdle && latest && JSON.stringify(latest) !== JSON.stringify(selected))
			await session.setModel(latest);
	})();
	entry.refreshing = task;
	try {
		await task;
	} finally {
		entry.refreshing = undefined;
	}
}
export function currentRuntime(id: string): AgentSession | undefined {
	const home = process.env.PI_CODING_AGENT_DIR;
	return home ? state.entries.get(identity(home, id))?.session : undefined;
}

/** Pi0.85.1没有扩展获取当前AgentSession的入口。集中适配公开bindExtensions。
 * 在进程内包装原型bindExtensions，再包装已登记实例的公开方法；不访问私有字段或修改SDK文件。
 */
export function installRuntimeAdapter(): void {
	const target = AgentSession.prototype;
	if (state.installed.has(target)) return;
	state.installed.add(target);
	const bind = target.bindExtensions;
	target.bindExtensions = async function (bindings) {
		const home = state.homes.get(this) ?? process.env.PI_CODING_AGENT_DIR;
		if (!home) return bind.call(this, bindings);
		if (!state.sessions.has(this)) {
			const id = this.sessionId,
				release = state.releases.get(this) ?? (await ownSession(home, id)),
				catalog = state.catalogs.get(this) ?? getModelCatalog(home),
				entry = { session: this, home, id, release, catalog };
			try {
				// CLI 的明确选择保持原值；TUI 启动与恢复保留指定身份，凭据缺失时等待手动配置。
				if (
					bindings.mode === "tui" &&
					!process.argv.some((arg) => /^(--model|--provider|--models)(=|$)/.test(arg))
				) {
					const context = this.sessionManager.buildSessionContext();
					const reference = context.messages.length
						? (context.model ?? defaultModelReference(this.settingsManager))
						: defaultModelReference(this.settingsManager);
					const model = explicitModel(this.modelRuntime, reference);
					if (this.model?.provider !== model.provider || this.model.id !== model.id) {
						this.agent.state.model = model;
						this.sessionManager.appendModelChange(model.provider, model.id);
						const thinking = context.messages.length
							? context.thinkingLevel
							: (this.settingsManager.getModelThinkingLevel(model.provider, model.id) ??
								this.settingsManager.getDefaultThinkingLevel() ??
								this.thinkingLevel);
						this.setThinkingLevel(this.getAvailableThinkingLevels().find((level) => level === thinking) ?? "off");
					}
				}
			} catch (error) {
				release();
				throw error;
			}
			const detachCatalog = catalog.attach(this.modelRuntime);
			const unsubscribeCatalog = catalog.subscribe(({ changed }) => {
				if (changed) void refreshRuntime(this, { checkCatalog: false }).catch(() => undefined);
			});
			const unsubscribeSettled = this.subscribe((event) => {
				if (event.type === "agent_settled")
					void refreshRuntime(this, { checkCatalog: false }).catch(() => undefined);
			});
			state.entries.set(identity(home, id), entry);
			state.sessions.set(this, entry);
			const dispose = this.dispose.bind(this),
				cycle = this.cycleModel.bind(this);
			this.dispose = () => {
				if (state.sessions.has(this)) {
					release();
					unsubscribeCatalog();
					unsubscribeSettled();
					detachCatalog();
					state.sessions.delete(this);
					state.entries.delete(identity(home, id));
				}
				dispose();
			};
			this.cycleModel = async (...args) => {
				await refreshRuntime(this);
				const only = this.scopedModels.length === 1 ? this.scopedModels[0] : undefined;
				if (only && (only.model.id !== this.model?.id || only.model.provider !== this.model?.provider)) {
					await this.setModel(only.model);
					if (only.thinkingLevel) this.setThinkingLevel(only.thinkingLevel);
					return { model: only.model, thinkingLevel: this.thinkingLevel, isScoped: true };
				}
				return cycle(...args);
			};
		}
		try {
			await bind.call(this, bindings);
		} catch (error) {
			this.dispose();
			throw error;
		}
	};
}
