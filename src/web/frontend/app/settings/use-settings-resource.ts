import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson } from "../data/api";
import { useGlobalEvents } from "../data/web-state";

/** 一个设置资源串行写入；写入期间失效的读请求不再覆盖本地状态。 */
export function useSettingsResource<T>(path: string) {
	const [data, setData] = useState<T | null>(null);
	const [error, setError] = useState("");
	const [loading, setLoading] = useState(true);
	const [working, setWorking] = useState(false);
	const mounted = useRef(false);
	const request = useRef<AbortController | null>(null);
	const sequence = useRef(0);
	const locked = useRef(false);
	const stale = useRef(false);
	const refresh = useCallback(async () => {
		if (locked.current) {
			stale.current = true;
			return;
		}
		request.current?.abort();
		const controller = new AbortController();
		request.current = controller;
		const revision = ++sequence.current;
		try {
			const value = await apiJson<T>(path, { signal: controller.signal });
			if (mounted.current && revision === sequence.current) {
				setData(value);
				setError("");
			}
		} catch (cause) {
			if (mounted.current && !controller.signal.aborted && revision === sequence.current)
				setError(cause instanceof Error ? cause.message : "设置加载失败，请重试。");
		} finally {
			if (mounted.current && revision === sequence.current) setLoading(false);
		}
	}, [path]);
	useEffect(() => {
		mounted.current = true;
		void refresh();
		return () => {
			mounted.current = false;
			sequence.current++;
			request.current?.abort();
		};
	}, [refresh]);
	useGlobalEvents((name, raw) => {
		if (name === "open" || name === "reset") {
			void refresh();
			return;
		}
		if (name !== "state") return;
		try {
			const event = JSON.parse(raw?.data ?? "null");
			if (event?.type === "auth_changed" || event?.type === "settings_changed") void refresh();
		} catch {
			/* 重连时重读。 */
		}
	});
	async function mutate(action: (current: T) => Promise<T>, optimistic?: T) {
		if (locked.current || data === null) return false;
		locked.current = true;
		request.current?.abort();
		sequence.current++;
		const previous = data;
		setWorking(true);
		setError("");
		if (optimistic !== undefined) setData(optimistic);
		try {
			const value = await action(previous);
			if (mounted.current) setData(value);
			return true;
		} catch (cause) {
			if (mounted.current) {
				setData(previous);
				setError(cause instanceof Error ? cause.message : "未能保存，请重试。");
			}
			return false;
		} finally {
			locked.current = false;
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
