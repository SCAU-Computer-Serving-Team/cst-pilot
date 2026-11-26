import { useCallback, useEffect, useRef, useState } from "react";
import {
	cachedSetting,
	cacheSetting,
	invalidateSetting,
	loadSetting,
	settingGeneration,
	subscribeSetting,
} from "./settings-resource-cache";

/** 读请求由设置页共用并预加载；写入支持即时乐观投影与最新态单飞合并，失败恢复已确认值。 */
type PendingMutation<T> = {
	rev: number;
	next: T;
	commit: (next: T, confirmed: T) => Promise<T>;
	onRollback?: (confirmed: T) => void;
	resolve: (ok: boolean) => void;
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
	const pending = useRef<PendingMutation<T> | null>(null);
	const mutationRev = useRef(0);

	const applyConfirmed = useCallback((value: T) => {
		confirmedData.current = value;
		latestData.current = value;
		setData(value);
		setError("");
	}, []);

	const read = useCallback(
		async (force: boolean) => {
			if (locked.current || pending.current !== null) {
				stale.current = true;
				return;
			}
			if (force) invalidateSetting(path);
			const revision = ++sequence.current;
			try {
				const value = await loadSetting<T>(path);
				if (mounted.current && revision === sequence.current && !locked.current && pending.current === null) {
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
			if (locked.current || pending.current !== null) {
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
			while (pending.current !== null) {
				const task = pending.current;
				pending.current = null;
				sequence.current++;
				const generation = settingGeneration(path);
				const base = confirmedData.current ?? task.next;
				try {
					const confirmed = await task.commit(task.next, base);
					confirmedData.current = confirmed;
					if (!cacheSetting(path, confirmed, generation)) stale.current = true;
					if (pending.current === null && mutationRev.current === task.rev) {
						latestData.current = confirmed;
						if (mounted.current) setData(confirmed);
					}
					task.resolve(true);
				} catch (cause) {
					if (pending.current !== null || mutationRev.current !== task.rev) {
						task.resolve(false);
						continue;
					}
					const rollback = confirmedData.current;
					if (rollback !== null) {
						latestData.current = rollback;
						task.onRollback?.(rollback);
						if (mounted.current) setData(rollback);
					}
					if (mounted.current) {
						setError(cause instanceof Error ? cause.message : "未能保存，请重试。");
					}
					task.resolve(false);
				}
			}
		} finally {
			locked.current = false;
			if (mounted.current) {
				setWorking(false);
				if (stale.current && pending.current === null) {
					stale.current = false;
					void refresh();
				}
			}
		}
	}

	function mutateLatest(
		updater: (current: T) => T,
		commit: (next: T, confirmed: T) => Promise<T>,
		options?: { onRollback?: (confirmed: T) => void },
	): Promise<boolean> {
		const current = latestData.current;
		if (current === null) return Promise.resolve(false);
		if (confirmedData.current === null) confirmedData.current = current;
		const next = updater(current);
		latestData.current = next;
		const rev = ++mutationRev.current;
		if (mounted.current) {
			setData(next);
			setError("");
		}
		if (pending.current !== null) {
			pending.current.resolve(true);
		}
		return new Promise<boolean>((resolve) => {
			pending.current = {
				rev,
				next,
				commit,
				onRollback: options?.onRollback,
				resolve,
			};
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
