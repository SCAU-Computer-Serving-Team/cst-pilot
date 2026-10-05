import { useCallback, useEffect, useRef, useState } from "react";
import { type AccountStatus, apiJson } from "../data/api";
import { useGlobalEvents } from "../data/web-state";

export function useAccount() {
	const [account, setAccount] = useState<AccountStatus | null>(null);
	const [loading, setLoading] = useState(true);
	const [working, setWorking] = useState(false);
	const [error, setError] = useState("");
	const sequence = useRef(0);
	const refresh = useCallback(async () => {
		const current = ++sequence.current;
		try {
			const value = await apiJson<AccountStatus>("/api/account");
			if (current !== sequence.current) return;
			setAccount(value);
			setError("");
		} catch (cause) {
			if (current === sequence.current)
				setError(cause instanceof Error ? cause.message : "账号信息读取失败，请重试。");
		} finally {
			if (current === sequence.current) setLoading(false);
		}
	}, []);
	useEffect(() => {
		void refresh();
		return () => {
			sequence.current++;
		};
	}, [refresh]);
	useGlobalEvents((name, event) => {
		if (name === "open" || name === "reset") {
			void refresh();
			return;
		}
		if (name !== "state") return;
		try {
			const value = JSON.parse(event?.data ?? "null");
			if (value?.type === "auth_changed" && value.providerId === "cstoa") void refresh();
		} catch {
			/* 非账号事件不影响当前资料。 */
		}
	});
	async function logout() {
		if (working) return;
		setWorking(true);
		try {
			await apiJson("/api/auth/cstoa/logout", { method: "POST" });
			await refresh();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "退出账号失败，请重试。");
		} finally {
			setWorking(false);
		}
	}
	return { account, loading, working, error, refresh, logout };
}
