import type { AccountStatus } from "../data/api";

export type AccountSummary = {
	identity: string;
	quota: string;
	expired: boolean;
};

export function formatCredit(value: number): string {
	return `${value.toLocaleString("zh-CN")} 额度点`;
}

/** 账号菜单、账号页与设置页共用的展示文案；资料缺失时给出占位，不伪造数值。 */
export function describeAccount(account: AccountStatus | null | undefined): AccountSummary | null {
	if (!account?.signedIn) return null;
	const { name, studentId } = account.profile;
	const identity = name && studentId ? `${name}（${studentId}）` : (name ?? studentId ?? "资料暂不可用");
	const expired = account.requiresLogin;
	const quota = expired
		? "登录已过期，请重新登录"
		: account.quota.supported && account.quota.balance !== null
			? `剩余 ${formatCredit(account.quota.balance)}`
			: "额度暂不可用";
	return { identity, quota, expired };
}
