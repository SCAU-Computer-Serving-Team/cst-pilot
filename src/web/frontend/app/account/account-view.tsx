import { Button } from "@heroui/react";
import { LogOut, ScanQrCode } from "lucide-react";
import { Link } from "react-router";
import { loginPath } from "../auth/providers";
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
	const usable = account?.signedIn && !account.requiresLogin;
	const loginLabel = account?.requiresLogin ? "重新扫码登录" : account?.signedIn ? "重新扫码登录" : "扫码登录";
	return (
		<div className="settings-content account-content">
			<header className="settings-group-head">
				<h2>CSTOA 账号</h2>
			</header>
			{loading ? (
				<output className="settings-note">正在读取账号信息…</output>
			) : error ? (
				<div className="account-notice">
					<p className="message-error" role="alert">
						{error}
					</p>
					<Button type="button" variant="ghost" className="settings-action" onPress={onRetry}>
						重试
					</Button>
				</div>
			) : (
				account && (
					<>
						<dl className="settings-card account-fields">
							{[
								["学号", usable ? (account.profile.studentId ?? "暂未提供") : "登录后查看"],
								["姓名", usable ? (account.profile.name ?? "暂未提供") : "登录后查看"],
								["额度", "暂未接入"],
							].map(([label, value]) => (
								<div className="settings-row account-field" key={label}>
									<dt>{label}</dt>
									<dd>{value}</dd>
								</div>
							))}
						</dl>
						<div className="account-notice">
							{account.profile.reason && usable ? (
								<>
									<p className="message-error" role="alert">
										{account.profile.reason}
									</p>
									<Button type="button" variant="ghost" className="settings-action" onPress={onRetry}>
										重试
									</Button>
								</>
							) : null}
							{account.requiresLogin ? (
								<output className="settings-note account-warning">登录已失效</output>
							) : null}
						</div>
						<div className="account-actions">
							<Link className="settings-action" to={loginPath("cstoa", "oauth", "/account")}>
								<ScanQrCode size={16} aria-hidden="true" />
								{loginLabel}
							</Link>
							{account.signedIn && (
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
							)}
						</div>
					</>
				)
			)}
		</div>
	);
}
