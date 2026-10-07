import { Button, Checkbox, Input, Label, TextField } from "@heroui/react";
import { Check, Eye, EyeOff } from "lucide-react";
import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { apiJson, type ProviderStatus } from "../data/api";
import { OAuthPanel } from "./oauth-panel";
import { ProviderPicker } from "./provider-picker";
import {
	CSTOA_PROVIDER,
	chooseLoginTab,
	initialLogin,
	type LoginTab,
	loginSuccessPath,
	loginTabs,
	orderedProviders,
	returnPath,
	tabProviders,
} from "./providers";

export function ProviderLoginForm({ standalone = false }: { standalone?: boolean }) {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const [oauthProviders, setOauthProviders] = useState<ProviderStatus[]>([]);
	const [mode, setMode] = useState<LoginTab>("cstoa");
	const [provider, setProvider] = useState("");
	const [oauthProvider, setOauthProvider] = useState("");
	const [customProvider, setCustomProvider] = useState(false);
	const [baseUrl, setBaseUrl] = useState("");
	const [apiKey, setApiKey] = useState("");
	const [showKey, setShowKey] = useState(false);
	const [loading, setLoading] = useState(true);
	const [apiFlow, setApiFlow] = useState<{ id: string; body: { key?: string; baseUrl?: string } } | null>(null);
	const [error, setError] = useState("");
	const requestedProvider = params.get("provider");
	const requestedMethod = params.get("method");
	const destination = returnPath(params.get("returnTo") ?? (standalone ? "/settings#accounts" : null));
	const exited = useRef(false);
	useEffect(() => {
		exited.current = false;
		return () => {
			exited.current = true;
		};
	}, []);
	const close = useCallback(() => {
		exited.current = true;
		setApiKey("");
		void navigate(destination);
	}, [navigate, destination]);
	useEffect(() => {
		const onEscape = (event: KeyboardEvent) => {
			if (
				event.key !== "Escape" ||
				event.defaultPrevented ||
				event.isComposing ||
				document.querySelector(".login-provider-popover")
			)
				return;
			event.preventDefault();
			close();
		};
		document.addEventListener("keydown", onEscape);
		return () => document.removeEventListener("keydown", onEscape);
	}, [close]);
	useEffect(() => {
		const controller = new AbortController();
		setLoading(true);
		setApiFlow(null);
		apiJson<{ providers: ProviderStatus[] }>("/api/auth", { signal: controller.signal })
			.then((data) => {
				const all = orderedProviders(data.providers);
				const keys = all.filter((item) => item.supportsApiKey);
				const oauth = all.filter((item) => item.supportsOAuth);
				setProviders(all);
				setOauthProviders(oauth);
				if (requestedProvider && !all.some((item) => item.id === requestedProvider)) {
					setError("请求的模型服务不存在，请重新选择。");
					setMode("api_key");
					setProvider(keys[0]?.id ?? "");
					return;
				}
				const initial = initialLogin(all, requestedProvider, requestedMethod);
				setMode(
					initial?.method === "oauth" && initial.providerId === CSTOA_PROVIDER
						? "cstoa"
						: (initial?.method ?? "api_key"),
				);
				setProvider(initial?.providerId ?? "");
				setOauthProvider(initial?.providerId ?? "");
				setError(
					all.some((item) => typeof item.supportsOAuth !== "boolean")
						? "当前服务端未更新登录能力，请重启 CST Pilot 后重试。"
						: "",
				);
			})
			.catch((cause: unknown) => {
				if (!controller.signal.aborted)
					setError(cause instanceof Error ? cause.message : "无法读取模型服务，请重试。");
			})
			.finally(() => {
				if (!controller.signal.aborted) setLoading(false);
			});
		return () => controller.abort();
	}, [requestedProvider, requestedMethod]);
	function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (loading || !provider || (customProvider && !baseUrl.trim())) return;
		setError("");
		setApiFlow({
			id: crypto.randomUUID(),
			body: { ...(apiKey.trim() ? { key: apiKey } : {}), ...(customProvider ? { baseUrl: baseUrl.trim() } : {}) },
		});
		setApiKey("");
	}
	const selectedOAuth = oauthProviders.find((item) => item.id === oauthProvider);
	const selected = providers.find((item) => item.id === (mode !== "api_key" ? oauthProvider : provider));
	function chooseMode(tab: LoginTab) {
		const choice = chooseLoginTab(providers, tab, selected?.id ?? "");
		if (!choice) return;
		setMode(tab);
		setApiFlow(null);
		setProvider(choice.id);
		setOauthProvider(choice.id);
		setApiKey("");
		setCustomProvider(false);
		setError("");
	}
	function chooseProvider(id: string) {
		const choice = initialLogin(providers, id, mode === "cstoa" ? "oauth" : mode);
		if (!choice) return;
		setProvider(choice.providerId);
		setApiFlow(null);
		setOauthProvider(choice.providerId);
		setMode(choice.method === "oauth" && choice.providerId === CSTOA_PROVIDER ? "cstoa" : choice.method);
		setApiKey("");
		setCustomProvider(false);
		setError("");
	}
	const picker = (
		<ProviderPicker
			providers={standalone ? providers : tabProviders(providers, mode)}
			selected={selected?.id ?? ""}
			standalone={standalone}
			disabled={loading}
			onChange={chooseProvider}
		/>
	);
	return (
		<div className={standalone ? "provider-config-form settings-card" : "login-card"} data-method={mode}>
			{standalone ? picker : null}
			<div className="provider-method-row">
				{standalone ? <span className="provider-field-label">登录方式</span> : null}
				<section className="login-switch" aria-label="登录方式">
					{loginTabs.map((tab) => (
						<button
							key={tab.id}
							type="button"
							className={mode === tab.id ? "login-switch-active" : "login-switch-inactive"}
							aria-pressed={mode === tab.id}
							disabled={loading || !tabProviders(providers, tab.id).length}
							title={
								tab.id === "cstoa"
									? "CSTOA 专属扫码登录"
									: tab.id === "oauth"
										? "使用 Pi 支持的 OAuth 授权"
										: "使用 Pi 支持的 API KEY 登录"
							}
							onClick={() => chooseMode(tab.id)}
						>
							{tab.label}
						</button>
					))}
				</section>
			</div>
			{standalone ? null : picker}
			{error ? (
				<p
					className={`login-message login-message--error${standalone ? " login-message--inline" : ""}`}
					role="alert"
				>
					{error}
				</p>
			) : null}
			{mode !== "api_key" && selectedOAuth ? (
				<div className={standalone ? "provider-oauth-row" : "provider-oauth-login"}>
					{standalone ? <span className="provider-field-label">授权</span> : null}
					<OAuthPanel
						key={oauthProvider}
						providerId={oauthProvider}
						providerName={selectedOAuth.name}
						standalone={standalone}
						onCancel={close}
						onSuccess={() => {
							if (!exited.current) void navigate(loginSuccessPath(oauthProvider, "oauth", destination));
						}}
					/>
				</div>
			) : apiFlow ? (
				<div className={standalone ? "provider-oauth-row" : "provider-oauth-login"}>
					{standalone ? <span className="provider-field-label">凭据</span> : null}
					<OAuthPanel
						key={apiFlow.id}
						method="api_key"
						providerId={provider}
						providerName={selected?.name ?? provider}
						initialBody={apiFlow.body}
						standalone={standalone}
						onCancel={close}
						onSuccess={() => {
							if (!exited.current) void navigate(loginSuccessPath(provider, "api_key", destination));
						}}
					/>
				</div>
			) : (
				<form className="login-api-form" onSubmit={submit}>
					<div className="login-fields">
						<div className="provider-custom-row">
							{standalone ? <span className="provider-field-label">自定义地址</span> : null}
							<Checkbox
								isSelected={customProvider}
								isDisabled={loading}
								onChange={(selected) => {
									setCustomProvider(selected);
									setError("");
								}}
								className="login-custom-checkbox"
							>
								<Checkbox.Content>
									<Checkbox.Control>
										<Checkbox.Indicator>
											<Check size={12} aria-hidden="true" />
										</Checkbox.Indicator>
									</Checkbox.Control>
									<Label>自定义 Provider</Label>
								</Checkbox.Content>
							</Checkbox>
						</div>
						{customProvider && (
							<TextField className="login-field" value={baseUrl} onChange={setBaseUrl}>
								<Label>{standalone ? "服务地址" : "BaseURL"}</Label>
								<Input
									className="login-input"
									type="url"
									placeholder="https://api.example.com/v1"
									autoComplete="url"
									required
									disabled={loading}
								/>
							</TextField>
						)}
						<TextField className="login-field" value={apiKey} onChange={setApiKey}>
							<Label>{standalone ? "API KEY" : "APIKEY"}</Label>
							<div className="login-key-field">
								<Input
									className="login-input"
									type={showKey ? "text" : "password"}
									placeholder={standalone ? "输入 API KEY" : "输入 APIKEY"}
									autoComplete="off"
									spellCheck={false}
									disabled={loading}
								/>
								<Button
									variant="ghost"
									isIconOnly
									className="login-key-visibility"
									onPress={() => setShowKey((visible) => !visible)}
									aria-label={showKey ? "隐藏 APIKEY" : "显示 APIKEY"}
								>
									{showKey ? <EyeOff size={16} /> : <Eye size={16} />}
								</Button>
							</div>
						</TextField>
					</div>
					<div className="provider-form-actions">
						{standalone ? (
							<Button type="button" variant="ghost" className="provider-cancel" onPress={close}>
								取消
							</Button>
						) : null}
						<Button
							type="submit"
							isDisabled={loading || !provider || (customProvider && !baseUrl.trim())}
							className="login-submit"
						>
							{standalone ? "保存" : "登录"}
						</Button>
					</div>
				</form>
			)}
		</div>
	);
}
