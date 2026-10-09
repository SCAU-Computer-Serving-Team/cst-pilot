import {
	createContext,
	createElement,
	type ReactNode,
	useCallback,
	useContext,
	useEffect,
	useRef,
	useState,
} from "react";
import { type AccountStatus, apiJson } from "../data/api";
import { useGlobalEvents } from "../data/web-state";

const AccountContext = createContext<ReturnType<typeof useAccountState> | null>(null);

/** 布局只挂载一次；账号页、侧栏与设置页共用资料和请求。 */
export function AccountProvider({ children, enabled = true }: { children: ReactNode; enabled?: boolean }) {
	const value = useAccountState(enabled);
	return createElement(AccountContext.Provider, { value }, children);
}

export function useAccount() {
	const value = useContext(AccountContext);
	if (!value) throw new Error("账号状态未初始化");
	return value;
}

function useAccountState(enabled: boolean) {
	const [account, setAccount] = useState<AccountStatus | null>(null);
	const [loading, setLoading] = useState(enabled);
	const [working, setWorking] = useState(false);
	const [error, setError] = useState("");
	const sequence = useRef(0);
	const loadedAt = useRef(0);
	const request = useRef<{ controller: AbortController; task: Promise<void> } | null>(null);
	const refresh = useCallback(
		(force = false): Promise<void> => {
			if (!enabled) return Promise.resolve();
			if (!force && request.current) return request.current.task;
			if (!force && loadedAt.current && Date.now() - loadedAt.current < 60_000) return Promise.resolve();
			request.current?.controller.abort();
			const current = ++sequence.current;
			const controller = new AbortController();
			const task = (async () => {
				try {
					const value = await apiJson<AccountStatus>(force ? "/api/account?refresh=1" : "/api/account", {
						signal: controller.signal,
					});
					if (current !== sequence.current) return;
					setAccount(value);
					setError("");
					loadedAt.current = value.profile.reason || value.requiresLogin ? 0 : Date.now();
				} catch (cause) {
					if (current !== sequence.current || controller.signal.aborted) return;
					setAccount((previous) =>
						previous ? { ...previous, profile: { supported: true, studentId: null, name: null } } : null,
					);
					loadedAt.current = 0;
					setError(cause instanceof Error ? cause.message : "账号信息读取失败，请重试。");
				} finally {
					if (current === sequence.current) {
						request.current = null;
						setLoading(false);
					}
				}
			})();
			request.current = { controller, task };
			return task;
		},
		[enabled],
	);
	useEffect(() => {
		void refresh();
		return () => {
			sequence.current++;
			request.current?.controller.abort();
			request.current = null;
		};
	}, [refresh]);
	useGlobalEvents((name, event) => {
		if (name === "open" || name === "reset") {
			loadedAt.current = 0;
			void refresh();
			return;
		}
		if (name !== "state") return;
		try {
			const value = JSON.parse(event?.data ?? "null");
			if (value?.type !== "auth_changed" || value.providerId !== "cstoa") return;
			sequence.current++;
			request.current?.controller.abort();
			request.current = null;
			loadedAt.current = 0;
			setAccount(null);
			setError("");
			setLoading(true);
			void refresh();
		} catch {
			/* 非账号事件不影响当前资料。 */
		}
	}, enabled);
	async function logout() {
		if (working) return;
		setWorking(true);
		try {
			await apiJson("/api/auth/cstoa/logout", { method: "POST" });
			setAccount(null);
			await refresh(true);
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "退出账号失败，请重试。");
		} finally {
			setWorking(false);
		}
	}
	return { account, loading, working, error, refresh, logout };
}
