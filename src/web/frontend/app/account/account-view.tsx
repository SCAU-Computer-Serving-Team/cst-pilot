import { Button } from "@heroui/react";
import { KeyRound, LogOut, ScanQrCode } from "lucide-react";
import { Link } from "react-router";
import { loginPath } from "../auth/providers";
import { formatQuotaBalance } from "../composer/quota-format";
import { useProviderQuota } from "../composer/use-quota";
import type { AccountStatus } from "../data/api";

export function AccountView({
	account,
	loading,
	error,
	working,
	onRetry,
	onLogout,
}: {
	account: AccountStatus | null;
	loading: boolean;
	error: string;
	working: boolean;
	onRetry: () => void;
	onLogout: () => void;
}) {
	const usable = !!account?.signedIn && !account.requiresLogin;
	const { quota, loading: quotaLoading, retry: retryQuota } = useProviderQuota("cstoa", usable);
	const balance = formatQuotaBalance(quota);
	const quotaText = !usable ? "登录后查看" : quotaLoading ? "正在查询…" : (balance ?? quota?.reason ?? "额度暂不可用");
	const notice = error || (usable ? account.profile.reason : "");
	const placeholder = error ? "暂不可用" : usable ? "暂未提供" : "登录后查看";
	return (
		<div className="settings-content account-content">
			<header className="settings-group-head">
				<h2>CSTOA 账号</h2>
			</header>
			{loading ? (
				<output className="settings-note">正在读取账号信息…</output>
			) : (
				<>
					<dl className="settings-card account-fields">
						{[
							["学号", usable ? (account.profile.studentId ?? placeholder) : placeholder],
							["姓名", usable ? (account.profile.name ?? placeholder) : placeholder],
						].map(([label, value]) => (
							<div className="settings-row account-field" key={label}>
								<dt>{label}</dt>
								<dd title={value}>{value}</dd>
							</div>
						))}
						<div className="settings-row account-field">
							<dt>剩余额度</dt>
							<dd className="account-quota" aria-live="polite">
								<span title={quotaText}>{quotaText}</span>
								{usable && quota && balance === null && !quota.requiresLogin && (
									<button type="button" className="context-more" disabled={quotaLoading} onClick={retryQuota}>
										重试
									</button>
								)}
							</dd>
						</div>
					</dl>
					{notice || account?.requiresLogin ? (
						<div className="account-notice">
							{notice ? (
								<>
									<p className="message-error" role="alert">
										{notice}
									</p>
									<Button
										type="button"
										variant="ghost"
										className="settings-action"
										isDisabled={working}
										onPress={onRetry}
									>
										重试
									</Button>
								</>
							) : (
								<output className="settings-note account-warning">登录已失效</output>
							)}
						</div>
					) : null}
				</>
			)}
			<div className="account-actions">
				<Link className="settings-action" to={loginPath("cstoa", "oauth", "/account")}>
					<ScanQrCode size={16} aria-hidden="true" />
					{account?.signedIn ? "重新扫码登录" : "扫码登录"}
				</Link>
				<Link className="settings-action" to={loginPath("cstoa", "api_key", "/account")}>
					<KeyRound size={16} aria-hidden="true" />
					APIKEY 登录
				</Link>
				{account?.signedIn ? (
					<Button
						type="button"
						variant="ghost"
						className="settings-action settings-action-danger"
						isDisabled={working}
						onPress={onLogout}
					>
						<LogOut size={16} aria-hidden="true" />
						{working ? "正在退出…" : "退出 CSTOA"}
					</Button>
				) : null}
			</div>
		</div>
	);
}
