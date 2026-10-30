import { createContext, type ReactNode, useContext } from "react";
import { useSessions } from "../data/web-state";

const SessionsContext = createContext<ReturnType<typeof useSessions> | null>(null);

/** 工作区只维护一份列表请求、事件订阅与乐观删除状态。 */
export function SessionsProvider({ children }: { children: ReactNode }) {
	const state = useSessions();
	return <SessionsContext value={state}>{children}</SessionsContext>;
}

export function useWorkspaceSessions() {
	const state = useContext(SessionsContext);
	if (!state) throw new Error("会话列表需要 SessionsProvider");
	return state;
}
