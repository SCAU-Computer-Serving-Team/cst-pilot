import { Checkbox, ListBox, Select } from "@heroui/react";
import { Check, ChevronDown, Eye, EyeOff, Plus, Trash2 } from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { apiJson } from "../data/api";
import { commonOutputLimit, DEFAULT_CONTEXT_WINDOW, FALLBACK_MAX_TOKENS } from "../settings/custom-model-defaults";
import { SettingsHeading, SettingsSection, useSettingsFeedback } from "../settings/settings-frame";
import { useAnimatedHeight } from "../settings/use-animated-height";
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
	const [error, setError] = useState("");
	const [confirming, setConfirming] = useState(false);
	const [protocolOpen, setProtocolOpen] = useState(false);
	const protocolTriggerRef = useRef<HTMLButtonElement>(null);
	const shell = useWorkspaceShell();
	usePanelDismiss(protocolOpen, () => setProtocolOpen(false), undefined, protocolTriggerRef);
	useEffect(() => {
		if (shell?.appActive) setProtocolOpen(false);
	}, [shell?.appActive]);
	// Esc closes inline delete confirmation BEFORE navigating back to providers list (Level 4 -> Level 6).
	useEffect(() => {
		if (!confirming) return;
		const onEscape = (event: KeyboardEvent) => {
			if (event.key !== "Escape" || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
			event.preventDefault();
			setConfirming(false);
		};
		window.addEventListener("keydown", onEscape);
		return () => window.removeEventListener("keydown", onEscape);
	}, [confirming]);
	const editedOutputLimits = useRef(new Set<string>());
	const modelList = useAnimatedHeight(`${loading}:${form.models.length}`);
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
		} finally {
			setWorking(false);
			setConfirming(false);
		}
	}
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
										onOpenChange={setProtocolOpen}
										selectedKey={form.api}
										onSelectionChange={(value) => {
											if (value) setForm({ ...form, api: String(value) });
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
										>
											<ListBox aria-label="接口协议" className="tier-2-menu-list">
												{protocols.map(([id, title]) => (
													<ListBox.Item
														key={id}
														id={id}
														textValue={title}
														className="menu-row login-provider-option"
													>
														<span className="login-provider-option-name">{title}</span>
														<span className="login-provider-check">
															<Check size={14} aria-hidden="true" />
														</span>
													</ListBox.Item>
												))}
											</ListBox>
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
						title="模型"
						summary={form.models.length}
						action={
							<button
								type="button"
								className="settings-inline-action settings-inline-action--primary"
								disabled={form.models.length >= 32}
								onClick={() =>
									setForm((current) => ({
										...current,
										models: [...current.models, emptyModel(defaultMaxTokens)],
									}))
								}
							>
								<Plus size={14} aria-hidden="true" /> 添加模型
							</button>
						}
					>
						<div className="settings-custom-models" ref={modelList}>
							{form.models.map((model, index) => (
								<div className="settings-custom-model" key={model.clientId}>
									<div className="settings-custom-model-title">
										<strong>模型 {index + 1}</strong>
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
												<Trash2 size={15} aria-hidden="true" />
											</button>
										)}
									</div>
									<div className="settings-custom-model-grid">
										<label className="settings-custom-field">
											<span>模型 ID</span>
											<span className="settings-custom-field-control">
												<input
													required
													maxLength={128}
													spellCheck={false}
													value={model.id}
													placeholder="例如：my-model"
													onChange={(event) => updateModel(index, { id: event.target.value })}
												/>
											</span>
										</label>
										<label className="settings-custom-field">
											<span>显示名称</span>
											<span className="settings-custom-field-control">
												<input
													required
													maxLength={128}
													value={model.name}
													placeholder="例如：My Model"
													onChange={(event) => updateModel(index, { name: event.target.value })}
												/>
											</span>
										</label>
										<label className="settings-custom-field">
											<span>上下文窗口</span>
											<span className="settings-custom-field-control">
												<input
													required
													type="number"
													min={1024}
													max={100000000}
													value={model.contextWindow}
													onChange={(event) =>
														updateModel(index, { contextWindow: Number(event.target.value) })
													}
												/>
											</span>
										</label>
										<label className="settings-custom-field">
											<span>最大输出 Token</span>
											<span className="settings-custom-field-control">
												<input
													required
													type="number"
													min={1}
													max={model.contextWindow}
													value={model.maxTokens}
													onChange={(event) =>
														updateModel(index, { maxTokens: Number(event.target.value) })
													}
												/>
											</span>
										</label>
									</div>
									<div className="settings-custom-capabilities">
										{(
											[
												{ key: "reasoning", label: "支持思考" },
												{ key: "image", label: "支持图片输入" },
											] as const
										).map(({ key: capKey, label }) => (
											<div className="settings-capability-check" key={capKey}>
												<Checkbox
													isSelected={model[capKey]}
													aria-label={label}
													className="settings-check-box"
													onChange={(checked) => updateModel(index, { [capKey]: checked })}
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
												<span>{label}</span>
											</div>
										))}
									</div>
								</div>
							))}
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
										type="button"
										className="settings-inline-action"
										disabled={working}
										onClick={() => setConfirming(false)}
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
