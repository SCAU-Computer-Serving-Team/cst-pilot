import { useCallback, useEffect, useRef, useState } from "react";
import {
	cachedSetting,
	cacheSetting,
	invalidateSetting,
	loadSetting,
	subscribeSetting,
} from "./settings-resource-cache";

/** 读请求由设置页共用并预加载；写入串行，失败恢复已确认值。 */
export function useSettingsResource<T>(path: string) {
	const [data, setData] = useState<T | null>(() => cachedSetting<T>(path) ?? null);
	const [error, setError] = useState("");
	const [loading, setLoading] = useState(() => cachedSetting(path) === undefined);
	const [working, setWorking] = useState(false);
	const mounted = useRef(false);
	const sequence = useRef(0);
	const locked = useRef(false);
	const stale = useRef(false);
	const read = useCallback(
		async (force: boolean) => {
			if (locked.current) {
				stale.current = true;
				return;
			}
			if (force) invalidateSetting(path);
			const revision = ++sequence.current;
			try {
				const value = await loadSetting<T>(path);
				if (mounted.current && revision === sequence.current) {
					setData(value);
					setError("");
				}
			} catch (cause) {
				if (mounted.current && revision === sequence.current)
					setError(cause instanceof Error ? cause.message : "设置加载失败，请重试。");
			} finally {
				if (mounted.current && revision === sequence.current) setLoading(false);
			}
		},
		[path],
	);
	const refresh = useCallback(() => read(true), [read]);
	useEffect(() => {
		mounted.current = true;
		const unsubscribe = subscribeSetting(path, (value, cause) => {
			if (!mounted.current) return;
			if (locked.current) {
				stale.current = true;
				return;
			}
			if (cause) setError(cause.message);
			else {
				setData(value as T);
				setError("");
			}
			setLoading(false);
		});
		void read(false);
		return () => {
			mounted.current = false;
			sequence.current++;
			unsubscribe();
		};
	}, [path, read]);
	async function mutate(action: (current: T) => Promise<T>, optimistic?: T) {
		if (locked.current || data === null) return false;
		locked.current = true;
		sequence.current++;
		const previous = data;
		let confirmed: T | undefined;
		setWorking(true);
		setError("");
		if (optimistic !== undefined) setData(optimistic);
		try {
			confirmed = await action(previous);
			if (mounted.current) setData(confirmed);
			return true;
		} catch (cause) {
			if (mounted.current) {
				setData(previous);
				setError(cause instanceof Error ? cause.message : "未能保存，请重试。");
			}
			return false;
		} finally {
			locked.current = false;
			if (confirmed !== undefined) cacheSetting(path, confirmed);
			if (mounted.current) {
				setWorking(false);
				if (stale.current) {
					stale.current = false;
					void refresh();
				}
			}
		}
	}
	return { data, error, loading, working, refresh, mutate };
}
