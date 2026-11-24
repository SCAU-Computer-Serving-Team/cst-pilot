import { Checkbox, Input, Label, TextField } from "@heroui/react";
import { ArrowRight, Check, Eye, EyeOff, KeyRound, QrCode, ShieldCheck, SlidersHorizontal } from "lucide-react";
import { type FormEvent, type MutableRefObject, useCallback, useEffect, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
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

type LoginStep = "choice" | "cstoa" | "cstoa_key" | "api_key" | "oauth";

function inferInitialStep(requestedProvider: string | null, requestedMethod: string | null): LoginStep {
	if (!requestedProvider && !requestedMethod) return "choice";
	if (requestedProvider === CSTOA_PROVIDER) {
		return requestedMethod === "api_key" ? "cstoa_key" : "cstoa";
	}
	if (requestedMethod === "oauth") return "oauth";
	if (requestedMethod === "api_key") return "api_key";
	return requestedProvider ? "api_key" : "choice";
}

export function ProviderLoginForm({
	standalone = false,
	backHandlerRef,
}: {
	standalone?: boolean;
	backHandlerRef?: MutableRefObject<(() => boolean) | null>;
}) {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const [oauthProviders, setOauthProviders] = useState<ProviderStatus[]>([]);
	const requestedProvider = params.get("provider");
	const requestedMethod = params.get("method");
	const hasDirectRequest = Boolean(requestedProvider || requestedMethod);
	const [step, setStep] = useState<LoginStep>(() =>
		standalone ? "cstoa" : inferInitialStep(requestedProvider, requestedMethod),
	);
	const [cameFromChoice, setCameFromChoice] = useState(!standalone && !hasDirectRequest);
	const [mode, setMode] = useState<LoginTab>("cstoa");
	const [provider, setProvider] = useState("");
	const [oauthProvider, setOauthProvider] = useState("");
	const [customProvider, setCustomProvider] = useState(false);
	const [baseUrl, setBaseUrl] = useState("");
	const [apiKey, setApiKey] = useState("");
	const [showKey, setShowKey] = useState(false);
	const [loading, setLoading] = useState(true);
	const [loadFailed, setLoadFailed] = useState(false);
	const [reloadNonce, setReloadNonce] = useState(0);
	const [apiFlow, setApiFlow] = useState<{ id: string; body: { key?: string; baseUrl?: string } } | null>(null);
	const [error, setError] = useState("");
	const stackRef = useRef<HTMLDivElement>(null);
	const prevStepRef = useRef<LoginStep>(step);
	const destination = returnPath(params.get("returnTo") ?? (standalone ? "/settings#accounts" : null));
	const exited = useRef(false);
	useEffect(() => {
		exited.current = false;
		return () => {
			exited.current = true;
		};
	}, []);
	useEffect(() => {
		if (prevStepRef.current === step) return;
		prevStepRef.current = step;
		requestAnimationFrame(() => {
			const container = stackRef.current;
			if (!container) return;
			const target =
				container.querySelector<HTMLElement>("input:not([disabled]), button:not([disabled])") ?? container;
			target.focus({ preventScroll: true });
		});
	}, [step]);
	const close = useCallback(() => {
		exited.current = true;
		setApiKey("");
		void navigate(destination);
	}, [navigate, destination]);
	const handleStepBack = useCallback(() => {
		if (apiFlow) {
			setApiFlow(null);
			setError("");
			return true;
		}
		if (!standalone && cameFromChoice && step !== "choice") {
			setStep("choice");
			setApiKey("");
			setBaseUrl("");
			setCustomProvider(false);
			setShowKey(false);
			setError("");
			return true;
		}
		return false;
	}, [standalone, cameFromChoice, step, apiFlow]);
	useEffect(() => {
		if (!backHandlerRef) return;
		backHandlerRef.current = handleStepBack;
		return () => {
			backHandlerRef.current = null;
		};
	}, [backHandlerRef, handleStepBack]);
	useEffect(() => {
		const onEscape = (event: KeyboardEvent) => {
			if (
				event.key !== "Escape" ||
				event.defaultPrevented ||
				event.isComposing ||
				event.keyCode === 229 ||
				document.querySelector(".login-provider-popover")
			)
				return;
			event.preventDefault();
			if (!handleStepBack()) close();
		};
		document.addEventListener("keydown", onEscape);
		return () => document.removeEventListener("keydown", onEscape);
	}, [close, handleStepBack]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: reloadNonce 显式用于失败后手动重新拉取服务列表
	useEffect(() => {
		const controller = new AbortController();
		setLoading(true);
		setLoadFailed(false);
		setError("");
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
					setStep("api_key");
					setCameFromChoice(false);
					setProvider(keys[0]?.id ?? "");
					setOauthProvider("");
					return;
				}
				const initial = initialLogin(all, requestedProvider, requestedMethod);
				const nextMode: LoginTab =
					initial?.method === "oauth" && initial.providerId === CSTOA_PROVIDER
						? "cstoa"
						: (initial?.method ?? "api_key");
				setMode(nextMode);
				setProvider(initial?.providerId ?? "");
				setOauthProvider(initial?.providerId ?? "");
				if (!standalone) {
					if (!hasDirectRequest) {
						setStep("choice");
						setCameFromChoice(true);
					} else {
						setCameFromChoice(false);
						if (initial?.providerId === CSTOA_PROVIDER && initial.method === "api_key") setStep("cstoa_key");
						else if (nextMode === "cstoa") setStep("cstoa");
						else if (nextMode === "oauth") setStep("oauth");
						else setStep("api_key");
					}
				}
				setError(
					all.some((item) => typeof item.supportsOAuth !== "boolean")
						? "当前服务端未更新登录能力，请重启 CST Pilot 后重试。"
						: "",
				);
			})
			.catch((cause: unknown) => {
				if (!controller.signal.aborted) {
					setLoadFailed(true);
					setError(cause instanceof Error ? cause.message : "无法读取模型服务，请重试。");
				}
			})
			.finally(() => {
				if (!controller.signal.aborted) setLoading(false);
			});
		return () => controller.abort();
	}, [requestedProvider, requestedMethod, standalone, hasDirectRequest, reloadNonce]);
	function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (loading || !provider || (customProvider && !baseUrl.trim())) return;
		if (step === "cstoa_key" && !apiKey.trim()) return;
		setError("");
		setApiFlow({
			id: crypto.randomUUID(),
			body: { ...(apiKey.trim() ? { key: apiKey } : {}), ...(customProvider ? { baseUrl: baseUrl.trim() } : {}) },
		});
		setApiKey("");
	}
	const selectedOAuth = oauthProviders.find((item) => item.id === oauthProvider);
	const selected = providers.find((item) => item.id === (mode !== "api_key" ? oauthProvider : provider));
	const cstoaProvider = providers.find((item) => item.id === CSTOA_PROVIDER);
	const hasCstoaOAuth = !!cstoaProvider?.supportsOAuth;
	const hasCstoaApiKey = !!cstoaProvider?.supportsApiKey;
	const hasOtherOAuth = tabProviders(providers, "oauth").length > 0;
	const hasApiKey = tabProviders(providers, "api_key").length > 0;

	function enterStep(target: LoginStep) {
		setApiFlow(null);
		setApiKey("");
		setBaseUrl("");
		setShowKey(false);
		setCustomProvider(false);
		setError("");
		if (target === "cstoa") {
			setMode("cstoa");
			setProvider(CSTOA_PROVIDER);
			setOauthProvider(CSTOA_PROVIDER);
		} else if (target === "cstoa_key") {
			setMode("api_key");
			setProvider(CSTOA_PROVIDER);
			setOauthProvider(CSTOA_PROVIDER);
		} else if (target === "oauth") {
			const choice = chooseLoginTab(providers, "oauth", selected?.id ?? "");
			setMode("oauth");
			if (choice) {
				setProvider(choice.id);
				setOauthProvider(choice.id);
			}
		} else if (target === "api_key") {
			const candidates = tabProviders(providers, "api_key");
			const preferred = candidates.find((item) => item.id !== CSTOA_PROVIDER) ?? candidates[0];
			setMode("api_key");
			if (preferred) {
				setProvider(preferred.id);
				setOauthProvider(preferred.id);
			}
		}
		setStep(target);
	}

	function chooseMode(tab: LoginTab) {
		const choice = chooseLoginTab(providers, tab, selected?.id ?? "");
		if (!choice) return;
		setMode(tab);
		setApiFlow(null);
		setProvider(choice.id);
		setOauthProvider(choice.id);
		setApiKey("");
		setBaseUrl("");
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
		setBaseUrl("");
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
	const retryButton = loadFailed ? (
		<button
			type="button"
			className="tier-1-btn-outline login-action-btn"
			onClick={() => setReloadNonce((n) => n + 1)}
		>
			重试读取服务
		</button>
	) : null;

	if (standalone) {
		return (
			<div className="provider-config-form settings-card" data-method={mode}>
				{picker}
				<div className="provider-method-row">
					<span className="provider-field-label">登录方式</span>
					<section className="login-switch" aria-label="登录方式">
						{loginTabs.map((tab) => (
							<button
								key={tab.id}
								type="button"
								className={mode === tab.id ? "login-switch-active" : "login-switch-inactive"}
								aria-pressed={mode === tab.id}
								disabled={loading || !tabProviders(providers, tab.id).length}
								onClick={() => chooseMode(tab.id)}
							>
								{tab.label}
							</button>
						))}
					</section>
				</div>
				{error ? (
					<p className="login-message login-message--error login-message--inline" role="alert">
						{error}
					</p>
				) : null}
				{mode !== "api_key" && selectedOAuth ? (
					<div className="provider-oauth-row">
						<span className="provider-field-label">授权</span>
						<OAuthPanel
							key={oauthProvider}
							providerId={oauthProvider}
							providerName={selectedOAuth.name}
							standalone
							onCancel={close}
							onSuccess={() => {
								if (!exited.current) void navigate(loginSuccessPath(oauthProvider, "oauth", destination));
							}}
						/>
					</div>
				) : apiFlow ? (
					<div className="provider-oauth-row">
						<span className="provider-field-label">凭据</span>
						<OAuthPanel
							key={apiFlow.id}
							method="api_key"
							providerId={provider}
							providerName={selected?.name ?? provider}
							initialBody={apiFlow.body}
							standalone
							onCancel={() => setApiFlow(null)}
							onSuccess={() => {
								if (!exited.current) void navigate(loginSuccessPath(provider, "api_key", destination));
							}}
						/>
					</div>
				) : (
					<form className="login-api-form" onSubmit={submit}>
						<div className="login-fields">
							<div className="provider-custom-row">
								<span className="provider-field-label">自定义地址</span>
								<Checkbox
									isSelected={customProvider}
									isDisabled={loading || selected?.id === CSTOA_PROVIDER}
									onChange={(checked) => {
										setCustomProvider(checked);
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
										<Label>自定义服务地址</Label>
									</Checkbox.Content>
								</Checkbox>
							</div>
							{customProvider && (
								<TextField className="login-field" value={baseUrl} onChange={setBaseUrl}>
									<Label>服务地址</Label>
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
								<Label>API KEY</Label>
								<div className="login-key-field">
									<Input
										className="login-input"
										type={showKey ? "text" : "password"}
										placeholder="输入 API KEY"
										autoComplete="off"
										spellCheck={false}
										disabled={loading}
									/>
									<button
										type="button"
										className="btn-icon xs-24 login-key-visibility"
										onClick={() => setShowKey((visible) => !visible)}
										aria-label={showKey ? "隐藏 APIKEY" : "显示 APIKEY"}
									>
										{showKey ? <EyeOff size={16} aria-hidden="true" /> : <Eye size={16} aria-hidden="true" />}
									</button>
								</div>
							</TextField>
						</div>
						<div className="provider-form-actions">
							<button type="button" className="btn-ghost provider-cancel" onClick={close}>
								取消
							</button>
							<button
								type="submit"
								disabled={loading || !provider || (customProvider && !baseUrl.trim())}
								className="tier-1-btn-primary login-submit"
							>
								保存
							</button>
						</div>
					</form>
				)}
			</div>
		);
	}

	/* 视图 1：tM7YQ 登录 · 选择方式（300px 栈列） */
	if (step === "choice") {
		return (
			<div ref={stackRef} tabIndex={-1} className="login-stack" data-step="choice">
				<header className="login-stack-head">
					<h1>登录 CST Pilot</h1>
					<p>选择登录方式</p>
				</header>
				{error ? (
					<p className="login-message login-message--error login-message--inline" role="alert">
						{error}
					</p>
				) : null}
				{retryButton}
				<div className="login-stack-group">
					<button
						type="button"
						className="tier-1-btn-primary login-choice-primary"
						disabled={loading || (!hasCstoaOAuth && !hasCstoaApiKey)}
						onClick={() => enterStep(hasCstoaOAuth ? "cstoa" : "cstoa_key")}
					>
						{!hasCstoaOAuth && hasCstoaApiKey ? (
							<>
								<KeyRound size={14} aria-hidden="true" />
								使用 CSTOA APIKEY 登录
							</>
						) : (
							<>
								<QrCode size={14} aria-hidden="true" />
								使用 CSTOA 扫码登录
							</>
						)}
					</button>
					<button
						type="button"
						className="tier-1-btn-outline login-choice-btn"
						disabled={loading || !hasApiKey}
						onClick={() => enterStep("api_key")}
					>
						<KeyRound size={14} aria-hidden="true" />
						APIKEY 登录
					</button>
					<button
						type="button"
						className="tier-1-btn-outline login-choice-btn"
						disabled={loading || !hasOtherOAuth}
						onClick={() => enterStep("oauth")}
					>
						<ShieldCheck size={14} aria-hidden="true" />
						OAuth 登录
					</button>
				</div>
				<div className="login-divider" aria-hidden="true">
					<span />
					<small>其他连接方式</small>
					<span />
				</div>
				<div className="login-stack-group">
					<Link to="/settings/custom" className="tier-1-btn-outline login-choice-btn">
						<SlidersHorizontal size={14} aria-hidden="true" />
						自定义网关
					</Link>
					<button type="button" className="btn-ghost login-skip-btn" onClick={close}>
						跳过登录
						<ArrowRight size={12} aria-hidden="true" />
					</button>
				</div>
			</div>
		);
	}

	/* 视图 2：q6DJ3u CSTOA 登录 · 手机扫码（268px 居中主体列） */
	if (step === "cstoa") {
		return (
			<div ref={stackRef} tabIndex={-1} className="login-stack login-stack--qr" data-step="cstoa">
				<header className="login-stack-head login-stack-head--center">
					<h1>扫码登录 CSTOA</h1>
				</header>
				{error ? (
					<p className="login-message login-message--error login-message--inline" role="alert">
						{error}
					</p>
				) : null}
				{loading ? (
					<output className="login-message login-message--inline">正在读取登录服务…</output>
				) : loadFailed ? (
					retryButton
				) : selectedOAuth ? (
					<OAuthPanel
						key={oauthProvider}
						providerId={oauthProvider}
						providerName={selectedOAuth.name}
						onCancel={close}
						onSuccess={() => {
							if (!exited.current) void navigate(loginSuccessPath(oauthProvider, "oauth", destination));
						}}
					/>
				) : (
					<output className="login-message login-message--inline">当前未启用 CSTOA 扫码登录。</output>
				)}
				{hasCstoaApiKey && (
					<div className="login-stack-group">
						<button
							type="button"
							className="tier-1-btn-outline login-action-btn"
							onClick={() => enterStep("cstoa_key")}
						>
							<KeyRound size={14} aria-hidden="true" />
							改用 CSTOA APIKEY
						</button>
					</div>
				)}
			</div>
		);
	}

	/* 视图 3：KpugI CSTOA 登录 · APIKEY（300px 密钥登录栈） */
	if (step === "cstoa_key") {
		return (
			<div ref={stackRef} tabIndex={-1} className="login-stack" data-step="cstoa_key">
				<header className="login-stack-head">
					<h1>使用 CSTOA APIKEY 登录</h1>
					<p>填写在 CSTOA AI Console 创建的个人密钥，登录后自动同步团队模型与可用额度。</p>
				</header>
				{error ? (
					<p className="login-message login-message--error login-message--inline" role="alert">
						{error}
					</p>
				) : null}
				{loadFailed ? (
					retryButton
				) : apiFlow ? (
					<>
						<OAuthPanel
							key={apiFlow.id}
							method="api_key"
							providerId={CSTOA_PROVIDER}
							providerName={cstoaProvider?.name ?? "CSTOA"}
							initialBody={apiFlow.body}
							onCancel={() => setApiFlow(null)}
							onSuccess={() => {
								if (!exited.current) void navigate(loginSuccessPath(CSTOA_PROVIDER, "api_key", destination));
							}}
						/>
						{hasCstoaOAuth && (
							<div className="login-stack-group">
								<button
									type="button"
									className="tier-1-btn-outline login-action-btn"
									onClick={() => enterStep("cstoa")}
								>
									<QrCode size={14} aria-hidden="true" />
									改用手机扫码登录
								</button>
							</div>
						)}
					</>
				) : (
					<form className="login-api-form" onSubmit={submit}>
						<TextField className="login-field" value={apiKey} onChange={setApiKey}>
							<Label>CSTOA 个人密钥</Label>
							<div className="login-key-field">
								<Input
									className="login-input"
									type={showKey ? "text" : "password"}
									placeholder="输入 CSTOA APIKEY"
									autoComplete="off"
									spellCheck={false}
									disabled={loading}
								/>
								<button
									type="button"
									className="btn-icon xs-24 login-key-visibility"
									onClick={() => setShowKey((visible) => !visible)}
									aria-label={showKey ? "隐藏 APIKEY" : "显示 APIKEY"}
								>
									{showKey ? <EyeOff size={16} aria-hidden="true" /> : <Eye size={16} aria-hidden="true" />}
								</button>
							</div>
						</TextField>
						<div className="login-stack-group">
							<button
								type="submit"
								disabled={loading || !apiKey.trim()}
								className="tier-1-btn-primary login-action-btn"
							>
								验证密钥并登录
							</button>
							{hasCstoaOAuth && (
								<button
									type="button"
									className="tier-1-btn-outline login-action-btn"
									onClick={() => enterStep("cstoa")}
								>
									<QrCode size={14} aria-hidden="true" />
									改用手机扫码登录
								</button>
							)}
						</div>
					</form>
				)}
			</div>
		);
	}

	/* 视图 4：通用 OAuth 登录（300px 表单栈） */
	if (step === "oauth") {
		return (
			<div ref={stackRef} tabIndex={-1} className="login-stack" data-step="oauth">
				<header className="login-stack-head">
					<h1>OAuth 登录</h1>
					<p>选择支持 OAuth 的模型服务完成授权</p>
				</header>
				{picker}
				{error ? (
					<p className="login-message login-message--error login-message--inline" role="alert">
						{error}
					</p>
				) : null}
				{loading ? (
					<output className="login-message login-message--inline">正在读取登录服务…</output>
				) : loadFailed ? (
					retryButton
				) : selectedOAuth ? (
					<OAuthPanel
						key={oauthProvider}
						providerId={oauthProvider}
						providerName={selectedOAuth.name}
						onCancel={close}
						onSuccess={() => {
							if (!exited.current) void navigate(loginSuccessPath(oauthProvider, "oauth", destination));
						}}
					/>
				) : (
					<output className="login-message login-message--inline">暂无支持 OAuth 的模型服务。</output>
				)}
			</div>
		);
	}

	/* 视图 5：通用 APIKEY 登录（300px 表单栈） */
	return (
		<div ref={stackRef} tabIndex={-1} className="login-stack" data-step="api_key">
			<header className="login-stack-head">
				<h1>APIKEY 登录</h1>
				<p>选择模型服务并填写 APIKEY</p>
			</header>
			{picker}
			{error ? (
				<p className="login-message login-message--error login-message--inline" role="alert">
					{error}
				</p>
			) : null}
			{loadFailed ? (
				retryButton
			) : apiFlow ? (
				<OAuthPanel
					key={apiFlow.id}
					method="api_key"
					providerId={provider}
					providerName={selected?.name ?? provider}
					initialBody={apiFlow.body}
					onCancel={() => setApiFlow(null)}
					onSuccess={() => {
						if (!exited.current) void navigate(loginSuccessPath(provider, "api_key", destination));
					}}
				/>
			) : (
				<form className="login-api-form" onSubmit={submit}>
					<div className="login-form-fields">
						<Checkbox
							isSelected={customProvider}
							isDisabled={loading || selected?.id === CSTOA_PROVIDER}
							onChange={(checked) => {
								setCustomProvider(checked);
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
								<Label>自定义服务地址</Label>
							</Checkbox.Content>
						</Checkbox>
						{customProvider && (
							<TextField className="login-field" value={baseUrl} onChange={setBaseUrl}>
								<Label>Base URL</Label>
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
							<Label>APIKEY</Label>
							<div className="login-key-field">
								<Input
									className="login-input"
									type={showKey ? "text" : "password"}
									placeholder="输入 APIKEY"
									autoComplete="off"
									spellCheck={false}
									disabled={loading}
								/>
								<button
									type="button"
									className="btn-icon xs-24 login-key-visibility"
									onClick={() => setShowKey((visible) => !visible)}
									aria-label={showKey ? "隐藏 APIKEY" : "显示 APIKEY"}
								>
									{showKey ? <EyeOff size={16} aria-hidden="true" /> : <Eye size={16} aria-hidden="true" />}
								</button>
							</div>
						</TextField>
					</div>
					<div className="login-stack-group">
						<button
							type="submit"
							disabled={loading || !provider || (customProvider && !baseUrl.trim())}
							className="tier-1-btn-primary login-action-btn"
						>
							登录
						</button>
					</div>
				</form>
			)}
		</div>
	);
}
