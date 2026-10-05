import { Button, Input, Label, ListBox, Select, TextField } from "@heroui/react";
import { ExternalLink } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import { type FormEvent, type MouseEvent, useEffect, useRef, useState } from "react";
import { type OauthPrompt, respondOauthLogin } from "../data/api";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { finishAuthorization, hasLocalCallback, openAuthorization } from "./oauth-window";
import { CSTOA_PROVIDER } from "./providers";
import { useOAuth } from "./use-oauth";

function PromptField({
	providerId,
	flowId,
	prompt,
	onAccepted,
	standalone,
	method,
	onCancel,
}: {
	providerId: string;
	flowId: string;
	prompt: OauthPrompt;
	onAccepted: () => Promise<void>;
	standalone: boolean;
	method: "oauth" | "api_key";
	onCancel?: () => void;
}) {
	const [value, setValue] = useState("");
	const [choiceOpen, setChoiceOpen] = useState(false);
	usePanelDismiss(choiceOpen, () => setChoiceOpen(false));
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	async function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (busy || (prompt.type !== "text" && !value.trim())) return;
		setBusy(true);
		try {
			await respondOauthLogin(providerId, flowId, prompt.id, value, method);
			setValue("");
			await onAccepted();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "授权信息未提交，请重试。");
		} finally {
			setBusy(false);
		}
	}
	return (
		<form className="oauth-prompt" onSubmit={submit}>
			{prompt.type === "select" ? (
				<Select
					isOpen={choiceOpen}
					onOpenChange={setChoiceOpen}
					className="login-field login-provider"
					selectedKey={value || null}
					onSelectionChange={(key) => setValue(key == null ? "" : String(key))}
					isDisabled={busy}
				>
					<Label>{prompt.message}</Label>
					<Select.Trigger className="login-input">
						<Select.Value>
							{prompt.options.find((option) => option.id === value)?.label ?? "选择账号或组织"}
						</Select.Value>
					</Select.Trigger>
					<Select.Popover className="login-provider-popover">
						<ListBox className="login-provider-list">
							{prompt.options.map((option) => (
								<ListBox.Item
									key={option.id}
									id={option.id}
									textValue={option.label}
									className="login-provider-option"
								>
									{option.label}
								</ListBox.Item>
							))}
						</ListBox>
					</Select.Popover>
				</Select>
			) : (
				<TextField className="login-field" value={value} onChange={setValue} isDisabled={busy}>
					<Label>{prompt.type === "manual_code" ? "授权码或回调地址" : prompt.message}</Label>
					<Input
						className="login-input"
						type={prompt.type === "secret" ? "password" : "text"}
						placeholder={prompt.type === "manual_code" ? "粘贴授权码或回调地址" : prompt.placeholder}
						autoComplete="off"
						spellCheck={false}
					/>
				</TextField>
			)}
			{error && (
				<p className="login-message login-message--inline login-message--error" role="alert">
					{error}
				</p>
			)}
			<div className="provider-form-actions">
				{standalone ? (
					<Button type="button" variant="ghost" className="provider-cancel" onPress={onCancel}>
						取消
					</Button>
				) : null}
				<Button
					type="submit"
					className="login-submit"
					isDisabled={busy || (prompt.type !== "text" && !value.trim())}
				>
					{busy ? "正在提交…" : "继续"}
				</Button>
			</div>
		</form>
	);
}

export function OAuthPanel({
	providerId,
	providerName,
	standalone = false,
	method = "oauth",
	initialBody,
	onCancel,
	onSuccess,
}: {
	providerId: string;
	providerName: string;
	standalone?: boolean;
	method?: "oauth" | "api_key";
	initialBody?: { key?: string; baseUrl?: string };
	onCancel?: () => void;
	onSuccess: () => void;
}) {
	const authorizationWindow = useRef<Window | null>(null);
	const { status, busy, expiresAt, connectionError, restart, cancel, sync } = useOAuth(
		providerId,
		() => {
			finishAuthorization(authorizationWindow.current);
			authorizationWindow.current = null;
			onSuccess();
		},
		method,
		initialBody,
	);
	const localCallback = !!status.authUrl && hasLocalCallback(status.authUrl.url);
	function authorize(event: MouseEvent<HTMLAnchorElement>) {
		if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
		const popup = openAuthorization(event.currentTarget.href);
		if (!popup) return; // 弹窗被拦截时保留链接的新标签页行为。
		event.preventDefault();
		authorizationWindow.current?.close();
		authorizationWindow.current = popup;
	}
	useEffect(
		() => () => {
			authorizationWindow.current?.close();
		},
		[],
	);
	const [clock, setClock] = useState(() => Date.now());
	useEffect(() => {
		if (!expiresAt || status.state !== "pending") return;
		const timer = window.setInterval(() => setClock(Date.now()), 1000);
		return () => window.clearInterval(timer);
	}, [expiresAt, status.state]);
	const remaining = expiresAt ? Math.max(0, Math.ceil((expiresAt - clock) / 1000)) : 0;
	const expired = status.state === "pending" && !!status.deviceCode && remaining <= 0;
	const terminal = status.state === "failed" || status.state === "cancelled" || expired;
	const error = connectionError || status.error;
	const team = method === "oauth" && providerId === CSTOA_PROVIDER;
	return (
		<div className={`login-fields ${team || status.deviceCode ? "login-fields--qr" : "login-fields--oauth"}`}>
			{error && (
				<p className="login-message login-message--inline login-message--error" role="alert">
					{error}
				</p>
			)}
			{status.state === "idle" && (
				<output className="login-message login-message--inline">
					{team ? "正在获取二维码…" : "正在获取授权信息…"}
				</output>
			)}
			{terminal ? (
				<>
					{!error && (
						<output className="login-message login-message--inline">
							{expired ? "二维码已过期" : status.state === "cancelled" ? "已取消" : "登录未完成"}
						</output>
					)}
					<Button type="button" className="login-submit" isDisabled={busy} onPress={() => void restart()}>
						{team ? "重新获取二维码" : "重新授权"}
					</Button>
				</>
			) : (
				<>
					{status.deviceCode && (
						<div className="login-qr">
							<div className="login-qr-box">
								<QRCodeSVG
									value={status.deviceCode.verificationUri}
									size={168}
									level="M"
									bgColor="#ffffff"
									fgColor="#1a2024"
									title={team ? "OA 授权二维码" : `${providerName} 授权二维码`}
								/>
							</div>
							<p className="login-qr-hint">{team ? "OA 扫码授权" : "扫码授权"}</p>
							<p className="login-qr-code">授权码：{status.deviceCode.userCode}</p>
							<p className="login-qr-countdown">
								二维码剩余 {Math.floor(remaining / 60)}:{String(remaining % 60).padStart(2, "0")}
							</p>
							{!team && (
								<a
									onClick={authorize}
									className="oauth-auth-link"
									href={status.deviceCode.verificationUri}
									target="_blank"
									rel="noopener noreferrer"
								>
									打开授权页面 <ExternalLink size={14} aria-hidden="true" />
								</a>
							)}
						</div>
					)}
					{status.authUrl && (
						<div className="oauth-authorization">
							<a
								className="oauth-auth-link"
								onClick={authorize}
								href={status.authUrl.url}
								target="_blank"
								rel="noopener noreferrer"
							>
								打开授权页面 <ExternalLink size={16} aria-hidden="true" />
							</a>
						</div>
					)}
					{status.prompt &&
						status.flowId &&
						(localCallback && status.prompt.type === "manual_code" ? (
							<details className="oauth-manual" key={status.prompt.id}>
								<summary>手动输入</summary>
								<PromptField
									providerId={providerId}
									flowId={status.flowId}
									prompt={status.prompt}
									onAccepted={sync}
									standalone={false}
									method={method}
								/>
							</details>
						) : (
							<PromptField
								key={status.prompt.id}
								providerId={providerId}
								flowId={status.flowId}
								prompt={status.prompt}
								onAccepted={sync}
								standalone={standalone}
								method={method}
								onCancel={onCancel}
							/>
						))}
					{status.state === "pending" && !status.prompt && !status.deviceCode && (
						<output className="login-message login-message--inline">
							{status.authUrl ? "等待授权…" : status.message || "等待授权…"}
						</output>
					)}
					{status.links?.map((link) => (
						<a
							key={link.url}
							className="oauth-auth-link"
							href={link.url}
							target="_blank"
							rel="noopener noreferrer"
						>
							{link.label || "查看授权说明"}
						</a>
					))}
					{status.state === "pending" && (!standalone || !status.prompt || localCallback) && (
						<div className="login-qr-actions">
							{team && (
								<Button type="button" variant="ghost" isDisabled={busy} onPress={() => void restart()}>
									刷新二维码
								</Button>
							)}
							<Button
								type="button"
								variant="ghost"
								isDisabled={busy}
								onPress={() => (standalone ? onCancel?.() : void cancel())}
							>
								取消
							</Button>
						</div>
					)}
				</>
			)}
		</div>
	);
}
