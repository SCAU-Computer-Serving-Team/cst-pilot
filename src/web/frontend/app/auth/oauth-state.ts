import type { OauthStatus } from "../data/api";

/** 新流程只接受自己的状态；迟到的查询与事件不能恢复旧步骤或旧登录结果。 */
export function acceptsOAuthStatus(current: OauthStatus, next: OauthStatus, activeFlow: string): boolean {
	if (!activeFlow || next.flowId !== activeFlow) return false;
	if (current.flowId !== activeFlow) return true;
	if ((next.revision ?? 0) < (current.revision ?? 0)) return false;
	return current.state === "pending" || current.state === "idle" || next.state !== "pending";
}
