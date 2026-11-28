import { Checkbox, Input, Label, TextField } from "@heroui/react";
import { Eye, EyeOff, KeyRound, RefreshCw, ScanLine, Search, Trash2 } from "lucide-react";
import { type FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { OAuthPanel } from "../auth/oauth-panel";
import { initialLogin, type LoginMethod, returnPath } from "../auth/providers";
import type { ApiKeyStartBody, ProviderEndpointMode, ProviderStatus } from "../data/api";
import { apiJson, patchProviderEndpoint } from "../data/api";
import type { ModelSelectionPayload } from "../data/model-selection";
import type { Model } from "../data/web-state";
import { CredentialDialog } from "./credential-dialog";
import { toggleModel } from "./model-scope";
import { SettingsHeading, SettingsSection, useSettingsFeedback } from "./settings-frame";
import { SettingsSearch } from "./settings-search";
import { SettingsSegment } from "./settings-segment";
import { useSettingsResource } from "./use-settings-resource";

const methods = [
	{ id: "oauth", label: "账号授权", icon: ScanLine },
	{ id: "api_key", label: "API Key", icon: KeyRound },
];
const fullId = (model: Model) => `${model.provider}/${model.id}`;
const trimSlash = (url: string) => url.trim().replace(/\/+$/, "");

function inferEndpointMode(provider: ProviderStatus): ProviderEndpointMode {
	if (provider.endpointMode) return provider.endpointMode;
	if (provider.custom) return "custom";
	if (provider.id === "cstoa" || provider.id === "openai-codex" || provider.id === "github-copilot") {
		return "managed_oauth";
	}
	if (provider.id === "cloudflare-workers-ai" || provider.id === "cloudflare-ai-gateway") {
		return "cloudflare_template";
	}
	if (
		provider.id === "opencode-go" ||
		provider.id === "opencode" ||
		provider.id === "openrouter" ||
		provider.id === "fireworks"
	) {
		return "multi_endpoint";
	}
	return "single";
}

function validateCustomUrl(raw: string): boolean {
	try {
		const url = new URL(raw.trim());
		return (
			url.protocol === "https:" ||
			(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))
		);
	} catch {
		return false;
	}
}

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
	const endpointMode = inferEndpointMode(provider);
	const effectiveBaseUrl = provider.configuredBaseUrl ?? provider.defaultBaseUrl ?? "";
	const [apiKey, setApiKey] = useState("");
	const [baseUrl, setBaseUrl] = useState(effectiveBaseUrl);
	const [accountId, setAccountId] = useState(provider.cloudflareAccountId ?? "");
	const [gatewayId, setGatewayId] = useState(provider.cloudflareGatewayId ?? "");
	const [showKey, setShowKey] = useState(false);
	const [editingKey, setEditingKey] = useState(false);
	const [endpointBusy, setEndpointBusy] = useState(false);
	const [modelSearchOpen, setModelSearchOpen] = useState(false);
	const [modelQuery, setModelQuery] = useState("");
	const [catalogBusy, setCatalogBusy] = useState(false);
	const [catalogError, setCatalogError] = useState("");
	const [oauth, setOauth] = useState(
		() =>
			initialLogin([provider], provider.id, requestedMethod)?.method === "oauth" &&
			(!provider.type || provider.requiresLogin),
	);
	const navigate = useNavigate();
	const [flow, setFlow] = useState<{ id: string; body: ApiKeyStartBody } | null>(null);
	const [error, setError] = useState("");
	const [confirming, setConfirming] = useState(false);
	const [removing, setRemoving] = useState(false);
	const lock = useRef(false);
	const mounted = useRef(true);
	const modelsResource = useSettingsResource<ModelSelectionPayload>("/api/models");
	useEffect(() => {
		mounted.current = true;
		return () => {
			mounted.current = false;
		};
	}, []);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 切换 provider.id 时同步重置草稿
	useEffect(() => {
		setBaseUrl(provider.configuredBaseUrl ?? provider.defaultBaseUrl ?? "");
		setAccountId(provider.cloudflareAccountId ?? "");
		setGatewayId(provider.cloudflareGatewayId ?? "");
	}, [
		provider.id,
		provider.configuredBaseUrl,
		provider.defaultBaseUrl,
		provider.cloudflareAccountId,
		provider.cloudflareGatewayId,
	]);
	const feedback = useSettingsFeedback();
	const allowedMethods = methods.filter((item) =>
		item.id === "oauth" ? provider.supportsOAuth : provider.supportsApiKey,
	);
	const pending = oauth || !!flow;
	const isConnected = Boolean(provider.type || provider.configured) && !provider.requiresLogin;
	const canRemove = Boolean(provider.type || provider.requiresLogin || provider.configuredBaseUrl);

	const allModels = modelsResource.data?.models ?? [];
	const selection = modelsResource.data?.enabled ?? null;
	const allProviderModels = useMemo(
		() => allModels.filter((item) => item.provider === provider.id),
		[allModels, provider.id],
	);
	const enabledProviderCount = useMemo(
		() =>
			selection === null
				? allProviderModels.length
				: allProviderModels.filter((item) => selection.includes(fullId(item))).length,
		[allProviderModels, selection],
	);
	const providerModels = useMemo(() => {
		const needle = modelQuery.trim().toLocaleLowerCase();
		return allProviderModels.filter(
			(item) => !needle || `${item.name} ${item.id}`.toLocaleLowerCase().includes(needle),
		);
	}, [allProviderModels, modelQuery]);

	async function refreshCatalog(testLabel = false) {
		if (catalogBusy || removing) return;
		setCatalogBusy(true);
		setCatalogError("");
		try {
			await apiJson("/api/models/refresh", { method: "POST" });
			await modelsResource.refresh();
			feedback(testLabel ? "连接正常" : "目录已更新");
		} catch (cause) {
			setCatalogError(cause instanceof Error ? cause.message : "拉取模型目录失败，请检查网络连接与密钥");
		} finally {
			if (mounted.current) setCatalogBusy(false);
		}
	}

	async function toggleProviderModel(modelId: string) {
		if (!modelsResource.data) return;
		const ok = await modelsResource.mutateLatest(
			(current) => {
				const ids = current.models.map(fullId);
				return { ...current, enabled: toggleModel(current.enabled, ids, modelId) };
			},
			async (draft, confirmed) => {
				const patterns = draft.enabled ?? [];
				const result = await apiJson<{ enabled: string[] | null }>("/api/models/scoped", {
					method: "POST",
					body: { patterns, empty: draft.enabled !== null && draft.enabled.length === 0 },
				});
				return { ...confirmed, enabled: result.enabled, unavailableEnabled: [] };
			},
			{ coalesceKey: "scope" },
		);
		if (ok) feedback();
	}

	async function updateEndpoint(nextUrl: string | null) {
		if (endpointBusy || removing || pending) return;
		if (nextUrl !== null && !validateCustomUrl(nextUrl)) {
			setError("服务地址须使用 HTTPS 或本机 HTTP。");
			return;
		}
		setEndpointBusy(true);
		setError("");
		try {
			await patchProviderEndpoint(provider.id, nextUrl);
			if (mounted.current) {
				if (nextUrl === null) {
					setBaseUrl(provider.defaultBaseUrl ?? "");
				}
				feedback();
				await onRefresh();
				await modelsResource.refresh();
			}
		} catch (cause) {
			if (mounted.current) {
				setError(cause instanceof Error ? cause.message : "保存地址失败，请重试。");
			}
		} finally {
			if (mounted.current) setEndpointBusy(false);
		}
	}

	function saveKey(event?: FormEvent) {
		event?.preventDefault();
		if (pending || lock.current || !apiKey.trim()) return;
		const body: ApiKeyStartBody = { key: apiKey };
		if (endpointMode === "single") {
			const trimmed = baseUrl.trim();
			const defaultUrl = provider.defaultBaseUrl ?? "";
			const changedFromDefault = trimmed !== "" && trimSlash(trimmed) !== trimSlash(defaultUrl);
			if (changedFromDefault) {
				if (!validateCustomUrl(trimmed)) {
					setError("服务地址须使用 HTTPS 或本机 HTTP。");
					return;
				}
				body.baseUrl = trimmed;
			} else if (provider.configuredBaseUrl && defaultUrl) {
				body.baseUrl = defaultUrl;
			}
		} else if (endpointMode === "cloudflare_template") {
			if (accountId.trim()) body.accountId = accountId.trim();
			if (provider.id === "cloudflare-ai-gateway" && gatewayId.trim()) {
				body.gatewayId = gatewayId.trim();
			}
		}
		setError("");
		setFlow({ id: crypto.randomUUID(), body });
		setApiKey("");
		setShowKey(false);
	}
	async function success() {
		if (!mounted.current) return;
		setFlow(null);
		setOauth(false);
		setEditingKey(false);
		setApiKey("");
		setError("");
		feedback();
		await onRefresh();
		await modelsResource.refresh();
		if (mounted.current && returnTo) void navigate(returnPath(returnTo));
		else if (mounted.current && provider.id === "cstoa") void navigate("/account");
	}
	async function remove() {
		if (lock.current) return;
		lock.current = true;
		setRemoving(true);
		setError("");
		try {
			if (provider.type || provider.requiresLogin) {
				await apiJson(`/api/auth/${encodeURIComponent(provider.id)}/logout`, { method: "POST" });
			}
			if (provider.configuredBaseUrl) {
				await patchProviderEndpoint(provider.id, null);
			}
			if (mounted.current) {
				setConfirming(false);
				setFlow(null);
				setOauth(false);
				setEditingKey(false);
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
	const showKeyEditor = method === "api_key" && !flow && (!isConnected || editingKey);
	const defaultUrl = provider.defaultBaseUrl ?? "";
	const trimmedUrl = baseUrl.trim();
	const urlDiffersFromDefault =
		Boolean(provider.configuredBaseUrl) || (defaultUrl !== "" && trimSlash(trimmedUrl) !== trimSlash(defaultUrl));
	const urlDiffersFromEffective = trimSlash(trimmedUrl) !== trimSlash(effectiveBaseUrl);
	const cfPreview =
		provider.id === "cloudflare-ai-gateway"
			? `gateway.ai.cloudflare.com/v1/${accountId.trim() || "{accountId}"}/${gatewayId.trim() || "{gatewayId}"}/*`
			: `api.cloudflare.com/client/v4/accounts/${accountId.trim() || "{accountId}"}/ai/v1`;
	const readonlySummary =
		provider.configuredBaseUrl ??
		(endpointMode === "cloudflare_template" ? cfPreview : provider.endpointSummary) ??
		"官方默认";

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
				{endpointMode === "cloudflare_template" && method === "api_key" && (
					<>
						<div className="settings-flat-row">
							<span className="settings-custom-field-label">Account ID</span>
							<TextField
								className="settings-url-input"
								aria-label="Account ID"
								value={accountId}
								onChange={(value) => {
									setAccountId(value);
									setError("");
								}}
								isDisabled={removing || pending}
							>
								<Label className="visually-hidden">Account ID</Label>
								<Input placeholder="Account ID" autoComplete="off" spellCheck={false} />
							</TextField>
						</div>
						{provider.id === "cloudflare-ai-gateway" && (
							<div className="settings-flat-row">
								<span className="settings-custom-field-label">Gateway ID</span>
								<TextField
									className="settings-url-input"
									aria-label="Gateway ID"
									value={gatewayId}
									onChange={(value) => {
										setGatewayId(value);
										setError("");
									}}
									isDisabled={removing || pending}
								>
									<Label className="visually-hidden">Gateway ID</Label>
									<Input placeholder="Gateway ID" autoComplete="off" spellCheck={false} />
								</TextField>
							</div>
						)}
						<div className="settings-flat-row">
							<div className="settings-row-inline">
								<span className="settings-custom-field-label">服务地址</span>
								<span className="settings-description">{readonlySummary}</span>
							</div>
							{provider.configuredBaseUrl && (
								<button
									type="button"
									className="settings-inline-action"
									disabled={endpointBusy || removing || pending}
									onClick={() => void updateEndpoint(null)}
								>
									恢复默认
								</button>
							)}
						</div>
					</>
				)}
				{(endpointMode === "multi_endpoint" || endpointMode === "managed_oauth") && (
					<div className="settings-flat-row">
						<div className="settings-row-inline">
							<span className="settings-custom-field-label">服务地址</span>
							<span className="settings-description">{readonlySummary}</span>
						</div>
						{provider.configuredBaseUrl && (
							<button
								type="button"
								className="settings-inline-action"
								disabled={endpointBusy || removing || pending}
								onClick={() => void updateEndpoint(null)}
							>
								恢复默认
							</button>
						)}
					</div>
				)}
				{endpointMode === "single" && method === "api_key" && (
					<div className="settings-flat-row">
						<span className="settings-custom-field-label">服务地址</span>
						<TextField
							className="settings-url-input"
							aria-label="服务地址"
							value={baseUrl}
							onChange={(value) => {
								setBaseUrl(value);
								setError("");
							}}
							isDisabled={removing || pending || endpointBusy}
						>
							<Label className="visually-hidden">服务地址</Label>
							<div className="settings-input-with-action">
								<Input
									type="url"
									placeholder={defaultUrl || "https://"}
									autoComplete="off"
									spellCheck={false}
									onKeyDown={(event) => {
										if (
											event.key === "Enter" &&
											!event.nativeEvent.isComposing &&
											event.nativeEvent.keyCode !== 229 &&
											!apiKey.trim() &&
											urlDiffersFromEffective
										) {
											event.preventDefault();
											void updateEndpoint(trimmedUrl || null);
										}
									}}
								/>
								{urlDiffersFromDefault && (
									<button
										type="button"
										className="settings-inline-action"
										disabled={endpointBusy || removing || pending}
										onClick={() => {
											setBaseUrl(defaultUrl);
											setError("");
											if (provider.configuredBaseUrl) {
												void updateEndpoint(null);
											}
										}}
									>
										恢复默认
									</button>
								)}
								{urlDiffersFromEffective && !apiKey.trim() && (
									<button
										type="button"
										className="settings-inline-action settings-inline-action--accent"
										disabled={endpointBusy || removing || pending}
										onClick={() => void updateEndpoint(trimmedUrl || null)}
									>
										{endpointBusy ? "保存中…" : "保存地址"}
									</button>
								)}
							</div>
						</TextField>
					</div>
				)}
				{isConnected && method === "api_key" && !editingKey && !flow && (
					<div className="settings-flat-row">
						<div className="settings-row-inline">
							<span className="settings-custom-field-label">API 密钥</span>
							<span className="settings-description">
								{provider.credentialSource === "env" && !provider.type
									? "环境变量"
									: "已配置（••••••••••••••••）"}
							</span>
						</div>
						<div className="settings-section-actions">
							<button
								type="button"
								className="settings-inline-action"
								disabled={catalogBusy || removing}
								onClick={() => void refreshCatalog(true)}
							>
								{catalogBusy ? "检测中…" : "测试连接"}
							</button>
							<button
								type="button"
								className="settings-inline-action settings-inline-action--primary"
								disabled={removing}
								onClick={() => setEditingKey(true)}
							>
								更换密钥
							</button>
						</div>
					</div>
				)}
				{showKeyEditor && (
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
							>
								<Label className="visually-hidden">API 密钥</Label>
								<div className="settings-input-with-action">
									<Input
										type={showKey ? "text" : "password"}
										autoComplete="off"
										spellCheck={false}
										maxLength={8192}
										placeholder={provider.type === "api_key" ? "输入新的 API Key" : "输入 API Key"}
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
									{isConnected && (
										<button
											type="button"
											className="settings-inline-action"
											onClick={() => {
												setEditingKey(false);
												setApiKey("");
												setError("");
											}}
										>
											取消
										</button>
									)}
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
						<div className="settings-section-actions">
							{isConnected && (
								<button
									type="button"
									className="settings-inline-action"
									disabled={catalogBusy || removing}
									onClick={() => void refreshCatalog(true)}
								>
									{catalogBusy ? "检测中…" : "测试连接"}
								</button>
							)}
							<button
								type="button"
								className="settings-inline-action settings-inline-action--primary"
								onClick={() => setOauth(true)}
							>
								{provider.type === "oauth" ? "重新授权" : "开始授权"}
							</button>
						</div>
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
			{isConnected && (
				<SettingsSection
					title="可用模型"
					summary={`${enabledProviderCount} / ${allProviderModels.length}`}
					action={
						<div className="settings-section-actions">
							<button
								type="button"
								className="btn-icon sm-28"
								aria-label="搜索可用模型"
								aria-pressed={modelSearchOpen}
								onClick={() => {
									setModelSearchOpen((prev) => {
										if (prev) setModelQuery("");
										return !prev;
									});
								}}
							>
								<Search size={14} aria-hidden="true" />
							</button>
							<button
								type="button"
								className="btn-icon sm-28"
								aria-label="刷新可用模型"
								disabled={catalogBusy || removing}
								onClick={() => void refreshCatalog(false)}
							>
								<RefreshCw size={14} aria-hidden="true" />
							</button>
						</div>
					}
				>
					{modelSearchOpen && <SettingsSearch label="筛选可用模型" value={modelQuery} onChange={setModelQuery} />}
					{(catalogError || modelsResource.error) && (
						<div className="settings-load-error" role="alert">
							<span>{catalogError || modelsResource.error}</span>
							{catalogError && (
								<button
									type="button"
									className="settings-inline-action settings-inline-action--primary"
									onClick={() => void refreshCatalog(false)}
								>
									重试
								</button>
							)}
						</div>
					)}
					<div className="settings-flat-list settings-models" role="listbox" aria-label="可用模型列表">
						{providerModels.map((item) => {
							const itemKey = fullId(item);
							const enabled = selection === null || selection.includes(itemKey);
							return (
								<div
									role="option"
									aria-selected={enabled}
									tabIndex={0}
									className="settings-model"
									key={itemKey}
									data-enabled={enabled ? "true" : "false"}
									onClick={(event) => {
										if ((event.target as HTMLElement).closest(".settings-check-box, label, input, button"))
											return;
										void toggleProviderModel(itemKey);
									}}
									onKeyDown={(event) => {
										if (event.nativeEvent.isComposing || event.keyCode === 229) return;
										if (
											!event.altKey &&
											!event.ctrlKey &&
											!event.metaKey &&
											!event.shiftKey &&
											(event.key === "Enter" || event.key === " ")
										) {
											if ((event.target as HTMLElement).closest("button, input")) return;
											event.preventDefault();
											void toggleProviderModel(itemKey);
										}
									}}
								>
									<Checkbox
										isSelected={enabled}
										aria-label={`启用 ${item.name}`}
										className="settings-check-box"
										onChange={() => void toggleProviderModel(itemKey)}
									>
										<Checkbox.Content>
											<Checkbox.Control>
												<Checkbox.Indicator>
													<svg
														viewBox="0 0 24 24"
														fill="none"
														stroke="currentColor"
														strokeWidth="2"
														strokeLinecap="round"
														strokeLinejoin="round"
														aria-hidden="true"
													>
														<path d="M20 6 9 17 4 12" pathLength="1" />
													</svg>
												</Checkbox.Indicator>
											</Checkbox.Control>
										</Checkbox.Content>
									</Checkbox>
									<div className="settings-model-main">
										<span className="settings-model-name" title={item.name}>
											{item.name}
										</span>
										<span className="settings-model-provider" title={item.id}>
											{item.id}
										</span>
									</div>
								</div>
							);
						})}
						{!providerModels.length && (
							<p className="settings-empty">{modelQuery ? "没有匹配的模型" : "暂无可用模型"}</p>
						)}
					</div>
				</SettingsSection>
			)}
			{canRemove && (
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
