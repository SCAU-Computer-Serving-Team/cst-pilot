import { Input, Label, TextField } from "@heroui/react";
import { ChevronRight, Eye, EyeOff, KeyRound, ScanLine, Trash2 } from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { OAuthPanel } from "../auth/oauth-panel";
import { initialLogin, type LoginMethod, returnPath } from "../auth/providers";
import type { ProviderStatus } from "../data/api";
import { apiJson } from "../data/api";
import { CredentialDialog } from "./credential-dialog";
import { SettingsHeading, useSettingsFeedback } from "./settings-frame";
import { SettingsSegment } from "./settings-segment";

const methods = [
	{ id: "oauth", label: "账号授权", icon: ScanLine },
	{ id: "api_key", label: "API Key", icon: KeyRound },
];
export function ProviderConfigurationForm({
	provider,
	requestedMethod,
	returnTo,
	onRefresh,
}: {
	provider: ProviderStatus;
	requestedMethod: string | null;
	returnTo?: string | null;
	onRefresh: () => Promise<void>;
}) {
	const [method, setMethod] = useState<LoginMethod>(
		() => initialLogin([provider], provider.id, requestedMethod)?.method ?? "api_key",
	);
	const [apiKey, setApiKey] = useState("");
	const [baseUrl, setBaseUrl] = useState("");
	const [showKey, setShowKey] = useState(false);
	const [advanced, setAdvanced] = useState(false);
	const [oauth, setOauth] = useState(
		() =>
			initialLogin([provider], provider.id, requestedMethod)?.method === "oauth" &&
			(!provider.type || provider.requiresLogin),
	);
	const navigate = useNavigate();
	const [flow, setFlow] = useState<{ id: string; body: { key: string; baseUrl?: string } } | null>(null);
	const [error, setError] = useState("");
	const [confirming, setConfirming] = useState(false);
	const [removing, setRemoving] = useState(false);
	const lock = useRef(false);
	const mounted = useRef(true);
	useEffect(() => {
		mounted.current = true;
		return () => {
			mounted.current = false;
		};
	}, []);
	const feedback = useSettingsFeedback();
	const allowedMethods = methods.filter((item) =>
		item.id === "oauth" ? provider.supportsOAuth : provider.supportsApiKey,
	);
	const pending = oauth || !!flow;
	const isConnected = Boolean(provider.type) && !provider.requiresLogin;

	function saveKey(event?: FormEvent) {
		event?.preventDefault();
		if (pending || lock.current || !apiKey.trim()) return;
		if (baseUrl.trim()) {
			try {
				const url = new URL(baseUrl);
				if (
					url.protocol !== "https:" &&
					!(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))
				)
					throw new Error();
			} catch {
				setError("服务地址须使用 HTTPS 或本机 HTTP，请修改后重新输入密钥。");
				return;
			}
		}
		setError("");
		setFlow({
			id: crypto.randomUUID(),
			body: { key: apiKey, ...(baseUrl.trim() ? { baseUrl: baseUrl.trim() } : {}) },
		});
		setApiKey("");
		setShowKey(false);
	}
	async function success() {
		if (!mounted.current) return;
		setFlow(null);
		setOauth(false);
		setApiKey("");
		setError("");
		feedback();
		await onRefresh();
		if (mounted.current && returnTo) void navigate(returnPath(returnTo));
		else if (mounted.current && provider.id === "cstoa") void navigate("/account");
	}
	async function remove() {
		if (lock.current) return;
		lock.current = true;
		setRemoving(true);
		setError("");
		try {
			await apiJson(`/api/auth/${encodeURIComponent(provider.id)}/logout`, { method: "POST" });
			if (mounted.current) {
				setConfirming(false);
				setFlow(null);
				setOauth(false);
				feedback("已移除凭据");
				await onRefresh();
			}
		} catch (cause) {
			if (mounted.current) {
				setConfirming(false);
				setError(cause instanceof Error ? cause.message : "凭据未移除，请重试。");
			}
		} finally {
			lock.current = false;
			if (mounted.current) setRemoving(false);
		}
	}
	return (
		<>
			<SettingsHeading
				title={provider.name}
				status={
					isConnected ? (
						<span className="settings-status-inline">
							<span className="settings-status-dot" aria-hidden="true" />
							已连接
						</span>
					) : undefined
				}
				back
			/>
			{error && (
				<p className="settings-load-error" role="alert">
					{error}
				</p>
			)}
			<div className="settings-provider-config">
				{allowedMethods.length > 1 && (
					<div className="settings-flat-row">
						<span className="settings-custom-field-label">登录方式</span>
						<SettingsSegment
							label="登录方式"
							value={method}
							items={allowedMethods}
							disabled={removing}
							onChange={(value) => {
								if (value !== "oauth" && value !== "api_key") return;
								setMethod(value);
								setFlow(null);
								setApiKey("");
								setShowKey(false);
								setError("");
								setOauth(value === "oauth");
							}}
						/>
					</div>
				)}
				{method === "api_key" && !flow && (
					<form onSubmit={saveKey} className="settings-credential-form">
						<div className="settings-flat-row">
							<span className="settings-custom-field-label">API 密钥</span>
							<TextField
								className="settings-key-input"
								value={apiKey}
								onChange={(value) => {
									setApiKey(value);
									setError("");
								}}
								aria-label="API 密钥"
								onBlur={(event) => {
									if (!event.currentTarget.contains(event.relatedTarget)) saveKey();
								}}
							>
								<Label className="visually-hidden">API 密钥</Label>
								<div className="settings-input-with-action">
									<Input
										type={showKey ? "text" : "password"}
										autoComplete="off"
										spellCheck={false}
										maxLength={8192}
										placeholder={provider.type === "api_key" ? "••••••••••••••••" : "输入 API Key"}
										disabled={removing}
										onKeyDown={(event) => {
											if (
												event.key === "Enter" &&
												!event.nativeEvent.isComposing &&
												event.nativeEvent.keyCode !== 229
											) {
												event.preventDefault();
												saveKey();
											}
										}}
									/>
									<button
										type="button"
										className="btn-icon sm-28"
										disabled={!apiKey || removing}
										aria-label={showKey ? "隐藏新密钥" : "显示新密钥"}
										onClick={() => setShowKey(!showKey)}
									>
										{showKey ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
									</button>
									<button
										type="submit"
										className="settings-inline-action settings-inline-action--accent"
										disabled={!apiKey.trim() || removing}
									>
										保存密钥
									</button>
								</div>
							</TextField>
						</div>
					</form>
				)}
				{method === "oauth" && !oauth && (
					<div className="settings-flat-row">
						<span className="settings-custom-field-label">账号授权</span>
						<button
							type="button"
							className="settings-inline-action settings-inline-action--primary"
							onClick={() => setOauth(true)}
						>
							{provider.type ? "重新授权" : "开始授权"}
						</button>
					</div>
				)}
				{pending && (
					<div className="settings-auth-flow">
						<OAuthPanel
							key={flow?.id ?? `oauth-${provider.id}`}
							providerId={provider.id}
							providerName={provider.name}
							method={flow ? "api_key" : "oauth"}
							initialBody={flow?.body}
							standalone
							onCancel={() => {
								setFlow(null);
								setOauth(false);
							}}
							onSuccess={() => void success()}
						/>
					</div>
				)}
			</div>
			{method === "api_key" && (
				<div className="settings-advanced t-acc" data-open={advanced}>
					<button
						className="settings-advanced-trigger"
						type="button"
						aria-expanded={advanced}
						aria-controls="provider-advanced"
						disabled={removing || pending}
						onClick={() => setAdvanced(!advanced)}
					>
						<ChevronRight size={14} aria-hidden="true" />
						高级设置
					</button>
					<div className="t-acc-panel" id="provider-advanced" inert={!advanced}>
						<div className="t-acc-panel-inner">
							<div className="settings-flat-row">
								<span className="settings-custom-field-label">服务地址</span>
								<TextField
									className="settings-url-input"
									aria-label="服务地址"
									value={baseUrl}
									onChange={setBaseUrl}
									isDisabled={provider.id === "cstoa" || removing || pending}
								>
									<Label className="visually-hidden">服务地址</Label>
									<Input
										type="url"
										placeholder={provider.id === "cstoa" ? "CSTOA 使用固定地址" : "保留现有服务地址"}
										autoComplete="off"
									/>
								</TextField>
							</div>
						</div>
					</div>
				</div>
			)}
			{(provider.type || provider.requiresLogin) && (
				<section className="settings-danger-zone">
					<p>移除后将隐藏该服务下的关联模型</p>
					<button
						type="button"
						className="settings-inline-action settings-inline-action--danger"
						disabled={removing || pending}
						onClick={() => setConfirming(true)}
					>
						<Trash2 size={14} aria-hidden="true" />
						移除
					</button>
				</section>
			)}
			{confirming && (
				<CredentialDialog
					name={provider.name}
					busy={removing}
					onCancel={() => {
						if (!removing) setConfirming(false);
					}}
					onConfirm={() => void remove()}
				/>
			)}
		</>
	);
}
