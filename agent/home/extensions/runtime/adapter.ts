import { resolve } from "node:path";
import { AgentSession, resolveModelScopeWithDiagnostics } from "@earendil-works/pi-coding-agent";
import { checkSharedConfiguration } from "./configuration.ts";
import { ownSession } from "./owner.ts";

interface RuntimeEntry {
	session: AgentSession;
	home: string;
	id: string;
	release: () => void;
	refreshing?: Promise<void>;
}
interface AdapterState {
	entries: Map<string, RuntimeEntry>;
	homes: WeakMap<AgentSession, string>;
	sessions: WeakMap<AgentSession, RuntimeEntry>;
	releases: WeakMap<AgentSession, () => void>;
	installed: WeakSet<object>;
}
const key = Symbol.for("cst-pilot/runtime-adapter-state");
const holder = globalThis as typeof globalThis & { [key]?: AdapterState };
const state = holder[key] ?? {
	entries: new Map(),
	homes: new WeakMap(),
	sessions: new WeakMap(),
	releases: new WeakMap(),
	installed: new WeakSet(),
};
holder[key] = state;
function identity(home: string, id: string): string {
	const root = resolve(home);
	return `${process.platform === "win32" ? root.toLowerCase() : root}\0${id}`;
}
export function registerRuntimeSession(session: AgentSession, home: string, release?: () => void): void {
	state.homes.set(session, home);
	if (release) state.releases.set(session, release);
}
export async function refreshRuntime(session: AgentSession): Promise<void> {
	const entry = state.sessions.get(session);
	if (!entry) return;
	if (entry.refreshing) return entry.refreshing;
	const task = (async () => {
		await session.settingsManager.reload();
		await session.modelRuntime.refresh({ allowNetwork: false });
		checkSharedConfiguration(session.settingsManager, session.modelRuntime);
		if (!process.argv.some((arg) => arg === "--models" || arg.startsWith("--models="))) {
			const scope = await resolveModelScopeWithDiagnostics(
				session.settingsManager.getEnabledModels() ?? [],
				session.modelRuntime,
			);
			if (scope.diagnostics.length) throw new Error("启用的模型当前不可用，请检查登录状态与模型配置。");
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
 * 只包装本实例公开方法，不访问私有字段、不修改SDK文件；重载复用进程内登记。
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
				entry = { session: this, home, id, release };
			state.entries.set(identity(home, id), entry);
			state.sessions.set(this, entry);
			const dispose = this.dispose.bind(this),
				cycle = this.cycleModel.bind(this),
				prompt = this.prompt.bind(this);
			this.dispose = () => {
				if (state.sessions.has(this)) {
					release();
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
			this.prompt = async (...args) => {
				await refreshRuntime(this);
				return prompt(...args);
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
