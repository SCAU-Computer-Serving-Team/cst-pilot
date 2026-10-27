import { apiJson } from "../data/api.ts";

export const settingsPaths = ["/api/settings", "/api/auth", "/api/models"] as const;

type Listener = (value: unknown | undefined, error?: Error) => void;
type Entry = {
	value?: unknown;
	fresh: boolean;
	generation: number;
	pending?: Promise<unknown>;
	listeners: Set<Listener>;
};
const entries = new Map<string, Entry>();
function entry(path: string): Entry {
	let current = entries.get(path);
	if (!current) {
		current = { fresh: false, generation: 0, listeners: new Set() };
		entries.set(path, current);
	}
	return current;
}

export function cachedSetting<T>(path: string): T | undefined {
	return settingReady(path) ? (entry(path).value as T) : undefined;
}
export function settingReady(path: string): boolean {
	const current = entry(path);
	return current.fresh && current.value !== undefined;
}
export function subscribeSetting(path: string, listener: Listener): () => void {
	const current = entry(path);
	current.listeners.add(listener);
	return () => current.listeners.delete(listener);
}
export function invalidateSetting(path: string): void {
	const current = entry(path);
	current.generation++;
	current.fresh = false;
	current.pending = undefined;
}
export function cacheSetting<T>(path: string, value: T): void {
	const current = entry(path);
	current.generation++;
	current.pending = undefined;
	current.value = value;
	current.fresh = true;
	for (const listener of current.listeners) listener(value);
}
export function loadSetting<T>(path: string): Promise<T> {
	const current = entry(path);
	if (settingReady(path)) return Promise.resolve(current.value as T);
	if (current.pending) return current.pending as Promise<T>;
	const generation = current.generation;
	const request = apiJson<T>(path)
		.then(
			(value) => {
				if (current.generation !== generation) return loadSetting<T>(path);
				current.value = value;
				current.fresh = true;
				for (const listener of current.listeners) listener(value);
				return value;
			},
			(cause: unknown) => {
				if (current.generation !== generation) return loadSetting<T>(path);
				const error = cause instanceof Error ? cause : new Error("设置加载失败，请重试。");
				for (const listener of current.listeners) listener(undefined, error);
				throw error;
			},
		)
		.finally(() => {
			if (current.pending === request) current.pending = undefined;
		});
	current.pending = request;
	return request;
}
export function preloadSettings(paths: readonly string[] = settingsPaths): Promise<PromiseSettledResult<unknown>[]> {
	return Promise.allSettled(paths.map((path) => loadSetting(path)));
}
