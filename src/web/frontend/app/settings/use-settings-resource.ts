import { useCallback, useEffect, useRef, useState } from "react";
import {
	cachedSetting,
	cacheSetting,
	invalidateSetting,
	loadSetting,
	settingGeneration,
	subscribeSetting,
} from "./settings-resource-cache";

/** 读请求由设置页共用并预加载；写入支持即时乐观投影与按键合并单飞队列，失败按项回滚。 */
type PendingMutation<T> = {
	coalesceKey?: string;
	updater: (current: T) => T;
	commit: (draft: T, confirmed: T) => Promise<T>;
	onRollback?: (confirmed: T) => void;
	resolvers: Array<(ok: boolean) => void>;
};

export function useSettingsResource<T>(path: string) {
	const initial = cachedSetting<T>(path) ?? null;
	const [data, setData] = useState<T | null>(initial);
	const [error, setError] = useState("");
	const [loading, setLoading] = useState(() => cachedSetting(path) === undefined);
	const [working, setWorking] = useState(false);
	const mounted = useRef(false);
	const sequence = useRef(0);
	const locked = useRef(false);
	const stale = useRef(false);
	const latestData = useRef<T | null>(initial);
	const confirmedData = useRef<T | null>(initial);
	const queue = useRef<PendingMutation<T>[]>([]);

	const applyConfirmed = useCallback((value: T) => {
		confirmedData.current = value;
		latestData.current = value;
		setData(value);
		setError("");
	}, []);

	const projectQueued = useCallback((base: T) => {
		let projected = base;
		for (const item of queue.current) {
			projected = item.updater(projected);
		}
		latestData.current = projected;
		if (mounted.current) setData(projected);
		return projected;
	}, []);

	const read = useCallback(
		async (force: boolean) => {
			if (locked.current || queue.current.length > 0) {
				stale.current = true;
				return;
			}
			if (force) invalidateSetting(path);
			const revision = ++sequence.current;
			try {
				const value = await loadSetting<T>(path);
				if (mounted.current && revision === sequence.current && !locked.current && queue.current.length === 0) {
					applyConfirmed(value);
				}
			} catch (cause) {
				if (mounted.current && revision === sequence.current)
					setError(cause instanceof Error ? cause.message : "设置加载失败，请重试。");
			} finally {
				if (mounted.current && revision === sequence.current) setLoading(false);
			}
		},
		[applyConfirmed, path],
	);
	const refresh = useCallback(() => read(true), [read]);
	useEffect(() => {
		mounted.current = true;
		const unsubscribe = subscribeSetting(path, (value, cause) => {
			if (!mounted.current) return;
			if (locked.current || queue.current.length > 0) {
				stale.current = true;
				return;
			}
			if (cause) setError(cause.message);
			else applyConfirmed(value as T);
			setLoading(false);
		});
		void read(false);
		return () => {
			mounted.current = false;
			sequence.current++;
			unsubscribe();
		};
	}, [applyConfirmed, path, read]);

	async function drainQueue() {
		if (locked.current) return;
		locked.current = true;
		if (mounted.current) setWorking(true);
		try {
			while (queue.current.length > 0) {
				const task = queue.current.shift()!;
				sequence.current++;
				const generation = settingGeneration(path);
				const base = confirmedData.current ?? latestData.current;
				if (base === null) {
					for (const resolve of task.resolvers) resolve(false);
					continue;
				}
				const draft = task.updater(base);
				try {
					const confirmed = await task.commit(draft, base);
					confirmedData.current = confirmed;
					if (!cacheSetting(path, confirmed, generation)) stale.current = true;
					projectQueued(confirmed);
					for (const resolve of task.resolvers) resolve(true);
				} catch (cause) {
					const rollback = confirmedData.current;
					if (rollback !== null) {
						task.onRollback?.(rollback);
						projectQueued(rollback);
					}
					if (mounted.current) {
						setError(cause instanceof Error ? cause.message : "未能保存，请重试。");
					}
					for (const resolve of task.resolvers) resolve(false);
				}
			}
		} finally {
			locked.current = false;
			if (mounted.current) {
				setWorking(false);
				if (stale.current && queue.current.length === 0) {
					stale.current = false;
					void refresh();
				}
			}
		}
	}

	function mutateLatest(
		updater: (current: T) => T,
		commit: (draft: T, confirmed: T) => Promise<T>,
		options?: { coalesceKey?: string; onRollback?: (confirmed: T) => void },
	): Promise<boolean> {
		const current = latestData.current;
		if (current === null) return Promise.resolve(false);
		if (confirmedData.current === null) confirmedData.current = current;
		const next = updater(current);
		latestData.current = next;
		if (mounted.current) {
			setData(next);
			setError("");
		}
		return new Promise<boolean>((resolve) => {
			const tail = queue.current[queue.current.length - 1];
			if (options?.coalesceKey && tail && tail.coalesceKey === options.coalesceKey) {
				const prevUpdater = tail.updater;
				tail.updater = (base) => updater(prevUpdater(base));
				tail.commit = commit;
				tail.onRollback = options.onRollback;
				tail.resolvers.push(resolve);
			} else {
				queue.current.push({
					coalesceKey: options?.coalesceKey,
					updater,
					commit,
					onRollback: options?.onRollback,
					resolvers: [resolve],
				});
			}
			void drainQueue();
		});
	}

	async function mutate(action: (current: T) => Promise<T>, optimistic?: T) {
		if (locked.current || latestData.current === null) return false;
		locked.current = true;
		sequence.current++;
		const previous = latestData.current;
		confirmedData.current = previous;
		const generation = settingGeneration(path);
		let confirmed: T | undefined;
		setWorking(true);
		setError("");
		if (optimistic !== undefined) {
			latestData.current = optimistic;
			setData(optimistic);
		}
		try {
			confirmed = await action(previous);
			confirmedData.current = confirmed;
			latestData.current = confirmed;
			if (mounted.current) setData(confirmed);
			return true;
		} catch (cause) {
			latestData.current = previous;
			if (mounted.current) {
				setData(previous);
				setError(cause instanceof Error ? cause.message : "未能保存，请重试。");
			}
			return false;
		} finally {
			locked.current = false;
			if (confirmed !== undefined && !cacheSetting(path, confirmed, generation)) stale.current = true;
			if (mounted.current) {
				setWorking(false);
				if (stale.current) {
					stale.current = false;
					void refresh();
				}
			}
		}
	}
	return { data, error, loading, working, refresh, mutate, mutateLatest };
}
