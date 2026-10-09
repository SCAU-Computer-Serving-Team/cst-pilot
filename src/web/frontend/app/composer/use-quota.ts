import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson } from "../data/api";
import { useGlobalEvents } from "../data/web-state";
import { type ProviderQuota, quotaProviders } from "./quota-format";

/** 打开面板时按需读取；模型和登录变化立即清除旧额度，不轮询。 */
export function useProviderQuota(provider: string, open: boolean) {
	const [quota, setQuota] = useState<ProviderQuota | null>(null);
	const [loading, setLoading] = useState(false);
	const requestedAt = useRef(0);
	const sequence = useRef(0);
	const request = useRef<AbortController | null>(null);
	const load = useCallback(
		async (force = false) => {
			if (!quotaProviders.has(provider)) return;
			if (!force && (request.current || (requestedAt.current && Date.now() - requestedAt.current < 60_000))) return;
			request.current?.abort();
			const current = ++sequence.current;
			const controller = new AbortController();
			request.current = controller;
			setLoading(true);
			try {
				const value = await apiJson<ProviderQuota>(
					`/api/quota?provider=${encodeURIComponent(provider)}${force ? "&refresh=1" : ""}`,
					{ signal: controller.signal },
				);
				if (current !== sequence.current) return;
				setQuota(value);
				requestedAt.current = value.supported ? Date.now() : 0;
			} catch {
				if (current !== sequence.current || controller.signal.aborted) return;
				requestedAt.current = 0;
				setQuota({ provider, supported: false, reason: "额度查询失败，请重试。" });
			} finally {
				if (current === sequence.current) {
					request.current = null;
					setLoading(false);
				}
			}
		},
		[provider],
	);
	useEffect(() => {
		setQuota((current) => (current?.provider === provider ? current : null));
		setLoading(false);
		requestedAt.current = 0;
		return () => {
			sequence.current++;
			request.current?.abort();
			request.current = null;
		};
	}, [provider]);
	useEffect(() => {
		if (open) void load();
	}, [open, load]);
	useGlobalEvents((name, event) => {
		let invalidate = name === "open" || name === "reset";
		if (name === "state") {
			try {
				const change = JSON.parse(event?.data ?? "null");
				invalidate = change?.providerId === provider && ["auth_changed", "quota_changed"].includes(change.type);
			} catch {
				return;
			}
		}
		if (!invalidate) return;
		sequence.current++;
		request.current?.abort();
		request.current = null;
		requestedAt.current = 0;
		setQuota(null);
		setLoading(false);
		if (open) void load();
	}, quotaProviders.has(provider));
	return { quota: quota?.provider === provider ? quota : null, loading, retry: () => void load(true) };
}
