import type { AccountStatus } from "../data/api";

/** 身份展示共用文案；失效时不展示上一个账号的资料。 */
export function describeAccount(
	account: AccountStatus | null | undefined,
): { identity: string; expired: boolean } | null {
	if (!account?.signedIn) return null;
	if (account.requiresLogin) return { identity: "需要重新登录", expired: true };
	const { name, studentId } = account.profile;
	return {
		identity: name && studentId ? `${name}（${studentId}）` : (name ?? studentId ?? "资料暂不可用"),
		expired: false,
	};
}
