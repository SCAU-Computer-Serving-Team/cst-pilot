import { Checkbox, ListBox, Select } from "@heroui/react";
import { Check, ChevronDown, Eye, EyeOff, Plus, RefreshCw, Search, Trash2 } from "lucide-react";
import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { apiJson } from "../data/api";
import { commonOutputLimit, DEFAULT_CONTEXT_WINDOW, FALLBACK_MAX_TOKENS } from "../settings/custom-model-defaults";
import { SettingsHeading, SettingsSection, useSettingsFeedback } from "../settings/settings-frame";
import { SettingsSearch } from "../settings/settings-search";
import { useAnimatedHeight } from "../settings/use-animated-height";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { useWorkspaceShell } from "../shell/workspace-shell-context";

type ModelEntry = {
	clientId: string;
	id: string;
	name: string;
	contextWindow: number;
	maxTokens: number;
	reasoning: boolean;
	image: boolean;
};
type CustomService = {
	id: string;
	name: string;
	baseUrl: string;
	api: string;
	apiKeySet: boolean;
	models: Omit<ModelEntry, "clientId">[];
	revision: string;
};
type ModelCatalogEntry = { maxTokens?: number };
type ModelCatalog = { models: ModelCatalogEntry[] };
const emptyModel = (maxTokens = FALLBACK_MAX_TOKENS): ModelEntry => ({
	clientId: crypto.randomUUID(),
	id: "",
	name: "",
	contextWindow: DEFAULT_CONTEXT_WINDOW,
	maxTokens,
	reasoning: false,
	image: false,
});
const protocols = [
	["openai-completions", "OpenAI Chat Completions"],
	["openai-responses", "OpenAI Responses"],
	["anthropic-messages", "Anthropic Messages"],
	["google-generative-ai", "Google Gemini"],
] as const;

export default function CustomModelService() {
	const [params] = useSearchParams();
	const editing = params.get("provider");
	const navigate = useNavigate();
	const feedback = useSettingsFeedback();
	const [form, setForm] = useState(() => ({
		id: "",
		name: "",
		baseUrl: "",
		api: "openai-completions",
		models: [emptyModel()],
	}));
	const [defaultMaxTokens, setDefaultMaxTokens] = useState(FALLBACK_MAX_TOKENS);
	const [key, setKey] = useState("");
	const [keyPresent, setKeyPresent] = useState(false);
	const [showKey, setShowKey] = useState(false);
	const [revision, setRevision] = useState<string | undefined>();
	const [loading, setLoading] = useState(!!editing);
	const [working, setWorking] = useState(false);
	const [refreshingCatalog, setRefreshingCatalog] = useState(false);
	const [modelSearchOpen, setModelSearchOpen] = useState(false);
	const [modelQuery, setModelQuery] = useState("");
	const [error, setError] = useState("");
	const [confirming, setConfirming] = useState(false);
	const [protocolOpen, setProtocolOpen] = useState(false);
	const protocolTriggerRef = useRef<HTMLButtonElement>(null);
	const protocolListRef = useRef<HTMLFieldSetElement>(null);
	const protocolItemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const [protocolFocusedId, setProtocolFocusedId] = useState<string | null>(null);
	const [protocolKbdActive, setProtocolKbdActive] = useState(false);
	const clearProtocolKbd = useCallback(() => setProtocolKbdActive(false), []);
	const kbdStyle = useKeyboardFocusStyle();
	const removeTriggerRef = useRef<HTMLButtonElement>(null);
	const cancelConfirmRef = useRef<HTMLButtonElement>(null);
	const restoreRemoveFocusRef = useRef(false);
	const shell = useWorkspaceShell();

	const closeProtocolPopover = useCallback(() => {
		setProtocolOpen(false);
		setProtocolFocusedId(null);
		setProtocolKbdActive(false);
	}, []);
	const cancelRemoveConfirm = useCallback(() => {
		if (working) return;
		restoreRemoveFocusRef.current = true;
		setConfirming(false);
	}, [working]);

	// 统一注册进全局 LIFO 浮层栈：确认态与协议下拉严格按打开顺序消费 Esc。
	usePanelDismiss(confirming && !working, cancelRemoveConfirm, undefined, removeTriggerRef);
	usePanelDismiss(protocolOpen, closeProtocolPopover, undefined, protocolTriggerRef);
	usePointerMutex(protocolOpen && protocolKbdActive, clearProtocolKbd);

	useEffect(() => {
		if (shell?.appActive) closeProtocolPopover();
	}, [shell?.appActive, closeProtocolPopover]);

	// 打开行内删除确认时自动聚焦安全的「取消」钮；取消或失败关闭后将焦点归还「移除」触发钮。
	useEffect(() => {
		if (confirming) {
			cancelConfirmRef.current?.focus({ preventScroll: true });
		} else if (restoreRemoveFocusRef.current) {
			restoreRemoveFocusRef.current = false;
			removeTriggerRef.current?.focus({ preventScroll: true });
		}
	}, [confirming]);

	const editedOutputLimits = useRef(new Set<string>());
	const modelList = useAnimatedHeight(`${loading}:${form.models.length}:${modelSearchOpen}:${modelQuery}`);
	useEffect(() => {
		let active = true;
		void apiJson<ModelCatalog>("/api/models")
			.then((catalog) => {
				if (!active) return;
				const mode = commonOutputLimit(catalog.models);
				setDefaultMaxTokens(mode);
				if (!editing)
					setForm((current) => ({
						...current,
						models: current.models.map((model) =>
							editedOutputLimits.current.has(model.clientId) ? model : { ...model, maxTokens: mode },
						),
					}));
			})
			.catch(() => {});
		return () => {
			active = false;
		};
	}, [editing]);
	useEffect(() => {
		if (!editing) return;
		let active = true;
		void apiJson<CustomService>(`/api/auth/custom/${encodeURIComponent(editing)}`)
			.then((config) => {
				if (!active) return;
				setForm({
					id: config.id,
					name: config.name,
					baseUrl: config.baseUrl,
					api: config.api,
					models: config.models.map((model) => ({ ...model, clientId: crypto.randomUUID() })),
				});
				setKeyPresent(config.apiKeySet);
				setRevision(config.revision);
				setError("");
			})
			.catch((cause) => {
				if (active) setError(cause instanceof Error ? cause.message : "加载服务配置失败");
			})
			.finally(() => {
				if (active) setLoading(false);
			});
		return () => {
			active = false;
		};
	}, [editing]);

	function updateModel(index: number, patch: Partial<ModelEntry>) {
		if (patch.maxTokens !== undefined) editedOutputLimits.current.add(form.models[index].clientId);
		setForm((current) => ({
			...current,
			models: current.models.map((model, i) => (i === index ? { ...model, ...patch } : model)),
		}));
	}

	async function refreshCatalog() {
		if (refreshingCatalog || working) return;
		setRefreshingCatalog(true);
		setError("");
		try {
			await apiJson("/api/models/refresh", { method: "POST" });
			feedback("模型目录已刷新");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "拉取模型目录失败，请重试");
		} finally {
			setRefreshingCatalog(false);
		}
	}

	async function save(event: FormEvent) {
		event.preventDefault();
		if (working || loading) return;
		setWorking(true);
		setError("");
		try {
			await apiJson("/api/auth/custom", {
				method: "POST",
				body: {
					...form,
					models: form.models.map(({ clientId: _clientId, ...model }) => model),
					...(key.trim() ? { apiKey: key.trim() } : {}),
					...(revision ? { revision } : {}),
				},
			});
			setKey("");
			feedback(editing ? "提供商配置已更新" : "自定义提供商已添加");
			void navigate("/settings", { viewTransition: true });
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "保存失败，请重试");
		} finally {
			setWorking(false);
		}
	}

	async function remove() {
		if (!editing || !revision || working) return;
		setWorking(true);
		setError("");
		try {
			await apiJson(`/api/auth/custom/${encodeURIComponent(editing)}`, { method: "DELETE", body: { revision } });
			feedback("自定义提供商已移除");
			void navigate("/settings", { viewTransition: true });
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "移除失败，请重试");
			restoreRemoveFocusRef.current = true;
			setConfirming(false);
		} finally {
			setWorking(false);
		}
	}

	const modelNeedle = modelQuery.trim().toLocaleLowerCase();
	const indexedModels = form.models
		.map((model, index) => ({ model, index }))
		.filter(({ model }) => !modelNeedle || `${model.id} ${model.name}`.toLocaleLowerCase().includes(modelNeedle));

	return (
		<>
			<SettingsHeading title={editing ? form.name || editing : "自定义提供商"} back blockEscape={confirming} />
			{loading ? (
				<output className="settings-empty">正在读取配置…</output>
			) : (
				<form onSubmit={(event) => void save(event)} className="settings-custom-form">
					{error && (
						<p role="alert" className="settings-load-error">
							{error}
						</p>
					)}
					<SettingsSection title="服务连接">
						<div className="settings-custom-fields">
							<label className="settings-custom-field">
								<span className="settings-custom-field-label">服务名称</span>
								<span className="settings-custom-field-control">
									<input
										required
										maxLength={100}
										autoComplete="off"
										value={form.name}
										placeholder="如 本地网关"
										onChange={(event) => setForm({ ...form, name: event.target.value })}
									/>
								</span>
							</label>
							<label className="settings-custom-field">
								<span className="settings-custom-field-label">服务 ID</span>
								<span className="settings-custom-field-control">
									<input
										required
										pattern="[a-z][a-z0-9_-]{1,63}"
										title="以小写字母开头，使用小写字母、数字、连字符或下划线"
										maxLength={64}
										disabled={!!editing}
										autoComplete="off"
										value={form.id}
										placeholder="如 my-gateway"
										onChange={(event) => setForm({ ...form, id: event.target.value })}
									/>
								</span>
							</label>
							<div className="settings-custom-field settings-custom-protocol">
								<span className="settings-custom-field-label">接口协议</span>
								<div className="settings-custom-field-control">
									<Select
										aria-label="接口协议"
										className="settings-custom-select"
										isOpen={protocolOpen}
										onOpenChange={(next) => {
											if (next) setProtocolOpen(true);
											else closeProtocolPopover();
										}}
										selectedKey={form.api}
										onSelectionChange={(value) => {
											if (value) setForm({ ...form, api: String(value) });
											closeProtocolPopover();
										}}
									>
										<Select.Trigger ref={protocolTriggerRef}>
											<Select.Value>{protocols.find(([id]) => id === form.api)?.[1]}</Select.Value>
											<ChevronDown size={14} aria-hidden="true" />
										</Select.Trigger>
										<Select.Popover
											placement="bottom start"
											offset={6}
											className="tier-2-glass tier-2-menu settings-protocol-popover"
											data-kbd-active={protocolKbdActive && protocolFocusedId ? "true" : undefined}
										>
											<fieldset
												ref={protocolListRef}
												aria-label="接口协议列表"
												style={{ position: "relative", border: 0, padding: 0, margin: 0, minInlineSize: 0 }}
												onKeyDown={(event) => {
													if (event.nativeEvent.isComposing || event.keyCode === 229) return;
													if (["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) {
														setProtocolKbdActive(true);
														requestAnimationFrame(() => {
															const focused = protocolListRef.current?.querySelector<HTMLElement>(
																'[role="option"][data-focused="true"], [role="option"]:focus',
															);
															const fid = focused?.getAttribute("data-option-id") ?? null;
															if (fid) setProtocolFocusedId(fid);
														});
													}
												}}
											>
												<ListBox aria-label="接口协议" className="tier-2-menu-list">
													{protocols.map(([id, title]) => (
														<ListBox.Item
															key={id}
															id={id}
															textValue={title}
															className="menu-row login-provider-option"
															data-option-id={id}
															ref={(node: HTMLElement | null) => {
																if (node) protocolItemRefs.current.set(id, node);
																else protocolItemRefs.current.delete(id);
															}}
														>
															<span className="login-provider-option-name">{title}</span>
															<span className="login-provider-check">
																{form.api === id && <Check size={14} aria-hidden="true" />}
															</span>
														</ListBox.Item>
													))}
												</ListBox>
												<KbdFocusOverlay
													containerRef={protocolListRef}
													itemRefs={protocolItemRefs}
													targetId={protocolKbdActive && protocolFocusedId ? protocolFocusedId : null}
													style={kbdStyle}
													railPlacement="inside"
													bracketOutset={2}
												/>
											</fieldset>
										</Select.Popover>
									</Select>
								</div>
							</div>
							<label className="settings-custom-field">
								<span className="settings-custom-field-label">Base URL</span>
								<span className="settings-custom-field-control">
									<input
										required
										type="url"
										maxLength={2048}
										autoComplete="url"
										spellCheck={false}
										value={form.baseUrl}
										placeholder="https://api.example.com/v1"
										onChange={(event) => setForm({ ...form, baseUrl: event.target.value })}
									/>
								</span>
							</label>
							<label className="settings-custom-field">
								<span className="settings-custom-field-label">API Key</span>
								<span className="settings-custom-field-control">
									<span className="settings-custom-key">
										<input
											type={showKey ? "text" : "password"}
											autoComplete="new-password"
											maxLength={8192}
											spellCheck={false}
											value={key}
											placeholder={keyPresent ? "已保存 · 留空保持原密钥" : "输入 API Key"}
											onChange={(event) => setKey(event.target.value)}
										/>
										<button
											type="button"
											aria-label={showKey ? "隐藏密钥" : "显示密钥"}
											onClick={() => setShowKey(!showKey)}
										>
											{showKey ? (
												<EyeOff size={15} aria-hidden="true" />
											) : (
												<Eye size={15} aria-hidden="true" />
											)}
										</button>
									</span>
								</span>
							</label>
						</div>
					</SettingsSection>
					<SettingsSection
						title="模型列表"
						summary={form.models.length}
						action={
							<div className="settings-section-actions">
								<button
									type="button"
									className="btn-icon sm-28"
									aria-label="筛选模型"
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
									aria-label="刷新模型目录"
									disabled={refreshingCatalog || working}
									onClick={() => void refreshCatalog()}
								>
									<RefreshCw size={14} aria-hidden="true" />
								</button>
								<button
									type="button"
									className="btn-icon sm-28"
									aria-label="添加模型"
									disabled={form.models.length >= 32}
									onClick={() =>
										setForm((current) => ({
											...current,
											models: [...current.models, emptyModel(defaultMaxTokens)],
										}))
									}
								>
									<Plus size={15} aria-hidden="true" />
								</button>
							</div>
						}
					>
						<div className="settings-custom-models" ref={modelList}>
							{modelSearchOpen && (
								<SettingsSearch label="搜索自定义模型" value={modelQuery} onChange={setModelQuery} />
							)}
							<div className="settings-custom-table-head" aria-hidden="true">
								<span>模型 ID</span>
								<span>显示名称</span>
								<span>上下文</span>
								<span>最大输出</span>
								<span>思考</span>
								<span>图片</span>
								<span />
							</div>
							{indexedModels.map(({ model, index }) => (
								<div className="settings-custom-table-row" key={model.clientId}>
									<input
										required
										aria-label={`模型 ${index + 1} ID`}
										maxLength={128}
										spellCheck={false}
										value={model.id}
										placeholder="my-model"
										onChange={(event) => updateModel(index, { id: event.target.value })}
									/>
									<input
										required
										aria-label={`模型 ${index + 1} 显示名称`}
										maxLength={128}
										value={model.name}
										placeholder="My Model"
										onChange={(event) => updateModel(index, { name: event.target.value })}
									/>
									<input
										required
										aria-label={`模型 ${index + 1} 上下文窗口`}
										type="number"
										min={1024}
										max={100000000}
										value={model.contextWindow}
										onChange={(event) => updateModel(index, { contextWindow: Number(event.target.value) })}
									/>
									<input
										required
										aria-label={`模型 ${index + 1} 最大输出 Token`}
										type="number"
										min={1}
										max={model.contextWindow}
										value={model.maxTokens}
										onChange={(event) => updateModel(index, { maxTokens: Number(event.target.value) })}
									/>
									<div className="settings-table-check">
										<Checkbox
											isSelected={model.reasoning}
											aria-label={`模型 ${index + 1} 支持思考`}
											className="settings-check-box"
											onChange={(checked) => updateModel(index, { reasoning: checked })}
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
									</div>
									<div className="settings-table-check">
										<Checkbox
											isSelected={model.image}
											aria-label={`模型 ${index + 1} 支持图片输入`}
											className="settings-check-box"
											onChange={(checked) => updateModel(index, { image: checked })}
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
									</div>
									<div className="settings-table-remove">
										{form.models.length > 1 && (
											<button
												type="button"
												className="btn-icon sm-28"
												aria-label={`移除模型 ${index + 1}`}
												onClick={() =>
													setForm((current) => ({
														...current,
														models: current.models.filter((_, i) => i !== index),
													}))
												}
											>
												<Trash2 size={14} aria-hidden="true" />
											</button>
										)}
									</div>
								</div>
							))}
							{!indexedModels.length && <p className="settings-empty">没有匹配的模型</p>}
						</div>
					</SettingsSection>
					<div className="settings-custom-actions">
						<button
							type="button"
							className="tier-1-btn-outline"
							disabled={working}
							onClick={() => void navigate("/settings", { viewTransition: true })}
						>
							取消
						</button>
						<button type="submit" className="tier-1-btn-primary" disabled={working}>
							{working ? "正在保存…" : editing ? "保存修改" : "添加提供商"}
						</button>
					</div>
					{editing && (
						<div className="settings-custom-remove">
							<span>移除后将隐藏该服务下的关联模型</span>
							{confirming ? (
								<div className="settings-custom-confirm">
									<button
										ref={cancelConfirmRef}
										type="button"
										className="settings-inline-action"
										disabled={working}
										onClick={cancelRemoveConfirm}
									>
										取消
									</button>
									<button
										type="button"
										className="settings-inline-action settings-inline-action--danger"
										disabled={working}
										onClick={() => void remove()}
									>
										确认移除
									</button>
								</div>
							) : (
								<button
									ref={removeTriggerRef}
									type="button"
									className="settings-inline-action settings-inline-action--danger"
									onClick={() => setConfirming(true)}
								>
									<Trash2 size={14} aria-hidden="true" />
									移除
								</button>
							)}
						</div>
					)}
				</form>
			)}
		</>
	);
}
