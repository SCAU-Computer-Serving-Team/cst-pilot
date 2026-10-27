import { Button, ListBox, Select } from "@heroui/react";
import { Check, ChevronDown, Eye, EyeOff, Plus, Trash2 } from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { apiJson } from "../data/api";
import { commonOutputLimit, DEFAULT_CONTEXT_WINDOW, FALLBACK_MAX_TOKENS } from "../settings/custom-model-defaults";
import { SettingsHeading, SettingsSection, useSettingsFeedback } from "../settings/settings-frame";
import { useAnimatedHeight } from "../settings/use-animated-height";

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
	["google-generative-ai", "Google Generative AI"],
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
			feedback(editing ? "服务配置已更新" : "自定义服务已添加");
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
			feedback("自定义服务已移除");
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
			<SettingsHeading
				title={editing ? `管理 ${form.name || editing}` : "自定义模型服务"}
				description="连接兼容 OpenAI、Anthropic 或 Google 接口的服务。"
				back
			/>
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
						<div className="settings-card settings-custom-fields">
							<label className="settings-custom-field">
								服务名称{" "}
								<input
									required
									maxLength={100}
									autoComplete="off"
									value={form.name}
									placeholder="例如：团队网关"
									onChange={(event) => setForm({ ...form, name: event.target.value })}
								/>
							</label>
							<label className="settings-custom-field">
								服务 ID{" "}
								<input
									required
									pattern="[a-z][a-z0-9_-]{1,63}"
									title="以小写字母开头，使用小写字母、数字、连字符或下划线"
									maxLength={64}
									disabled={!!editing}
									autoComplete="off"
									value={form.id}
									placeholder="例如：team-gateway"
									onChange={(event) => setForm({ ...form, id: event.target.value })}
								/>
								<small>Pi 中以此 ID 引用模型。保存后不可修改。</small>
							</label>
							<div className="settings-custom-field settings-custom-protocol">
								<span>接口协议</span>
								<Select
									aria-label="接口协议"
									className="settings-custom-select"
									selectedKey={form.api}
									onSelectionChange={(value) => {
										if (value) setForm({ ...form, api: String(value) });
									}}
								>
									<Select.Trigger>
										<Select.Value>{protocols.find(([id]) => id === form.api)?.[1]}</Select.Value>
										<ChevronDown size={14} aria-hidden="true" />
									</Select.Trigger>
									<Select.Popover
										placement="bottom start"
										className="login-provider-popover settings-protocol-popover"
									>
										<ListBox aria-label="接口协议">
											{protocols.map(([id, title]) => (
												<ListBox.Item key={id} id={id} textValue={title} className="login-provider-option">
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
							<label className="settings-custom-field">
								Base URL{" "}
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
								<small>使用 HTTPS；本机服务可使用 HTTP。</small>
							</label>
							<label className="settings-custom-field">
								API key{" "}
								<span className="settings-custom-key">
									<input
										type={showKey ? "text" : "password"}
										autoComplete="new-password"
										maxLength={8192}
										spellCheck={false}
										value={key}
										placeholder={keyPresent ? "已保存 · 留空保持原密钥" : "填写服务密钥"}
										onChange={(event) => setKey(event.target.value)}
									/>
									<button
										type="button"
										aria-label={showKey ? "隐藏密钥" : "显示密钥"}
										onClick={() => setShowKey(!showKey)}
									>
										{showKey ? <EyeOff size={16} /> : <Eye size={16} />}
									</button>
								</span>
								<small>密钥写入 Pi 的 models.json；界面不会回显。</small>
							</label>
						</div>
					</SettingsSection>
					<SettingsSection title="模型" summary={`${form.models.length} 个模型`}>
						<div className="settings-card settings-custom-models" ref={modelList}>
							{form.models.map((model, index) => (
								<div className="settings-custom-model" key={model.clientId}>
									<div className="settings-custom-model-title">
										<strong>模型 {index + 1}</strong>
										{form.models.length > 1 && (
											<button
												type="button"
												aria-label={`移除模型 ${index + 1}`}
												onClick={() =>
													setForm((current) => ({
														...current,
														models: current.models.filter((_, i) => i !== index),
													}))
												}
											>
												<Trash2 size={16} />
											</button>
										)}
									</div>
									<div className="settings-custom-model-grid">
										<label className="settings-custom-field">
											模型 ID{" "}
											<input
												required
												maxLength={128}
												spellCheck={false}
												value={model.id}
												placeholder="例如：my-model"
												onChange={(event) => updateModel(index, { id: event.target.value })}
											/>
										</label>
										<label className="settings-custom-field">
											显示名称{" "}
											<input
												required
												maxLength={128}
												value={model.name}
												placeholder="例如：My Model"
												onChange={(event) => updateModel(index, { name: event.target.value })}
											/>
										</label>
										<label className="settings-custom-field">
											上下文窗口{" "}
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
										</label>
										<label className="settings-custom-field">
											最大输出 Token{" "}
											<input
												required
												type="number"
												min={1}
												max={model.contextWindow}
												value={model.maxTokens}
												onChange={(event) => updateModel(index, { maxTokens: Number(event.target.value) })}
											/>
										</label>
									</div>
									<div className="settings-custom-capabilities">
										{(
											[
												{ key: "reasoning", label: "支持思考" },
												{ key: "image", label: "支持图片输入" },
											] as const
										).map(({ key, label }) => (
											<div className="settings-capability-row" key={key}>
												<span id={`${model.clientId}-${key}`}>{label}</span>
												<button
													type="button"
													className="settings-action settings-capability-button"
													role="switch"
													aria-labelledby={`${model.clientId}-${key}`}
													aria-checked={model[key]}
													data-selected={model[key] || undefined}
													onClick={() => updateModel(index, { [key]: !model[key] })}
												>
													{model[key] ? "已启用" : "启用"}
												</button>
											</div>
										))}
									</div>
								</div>
							))}
							<Button
								type="button"
								variant="ghost"
								className="settings-action"
								isDisabled={form.models.length >= 32}
								onPress={() =>
									setForm((current) => ({
										...current,
										models: [...current.models, emptyModel(defaultMaxTokens)],
									}))
								}
							>
								<Plus size={14} /> 添加模型
							</Button>
						</div>
					</SettingsSection>
					<div className="settings-custom-actions">
						<Button type="submit" className="settings-add-action" isDisabled={working}>
							{working ? "正在保存…" : editing ? "保存修改" : "添加服务"}
						</Button>
					</div>
					{editing && (
						<SettingsSection title="移除服务">
							<div className="settings-card settings-custom-remove">
								<span>从 Pi 的 models.json 移除此服务和模型。</span>
								{confirming ? (
									<div className="settings-custom-confirm">
										<Button type="button" variant="ghost" onPress={() => setConfirming(false)}>
											取消
										</Button>
										<Button type="button" variant="danger" isDisabled={working} onPress={() => void remove()}>
											确认移除
										</Button>
									</div>
								) : (
									<Button type="button" variant="ghost" onPress={() => setConfirming(true)}>
										移除服务
									</Button>
								)}
							</div>
						</SettingsSection>
					)}
				</form>
			)}
		</>
	);
}
