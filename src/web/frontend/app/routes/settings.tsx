import { Button, Checkbox, Kbd, SearchField, ToggleButton, ToggleButtonGroup } from "@heroui/react";
import { Check, GripVertical, Search, Settings as SettingsIcon } from "lucide-react";
import { type DragEvent, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { apiJson, type ProviderStatus } from "../data/api";
import type { Model } from "../data/web-state";
import type { Theme } from "../shell/theme";

type ModelsPayload = { models: Model[]; enabled: string[] | null; unavailableEnabled: string[] };

const fullId = (model: Model) => `${model.provider}/${model.id}`;

/** 与 TUI /scoped-models 相同：全启用存为 null；覆盖全部可用模型的选择归一为 null */
function normalize(ids: string[], allIds: string[]): string[] | null {
	return ids.length === allIds.length && ids.every((id) => allIds.includes(id)) ? null : ids;
}
function toggle(selection: string[] | null, allIds: string[], id: string): string[] | null {
	if (selection === null) return allIds.filter((modelId) => modelId !== id);
	const index = selection.indexOf(id);
	const next = index >= 0 ? selection.filter((modelId) => modelId !== id) : [...selection, id];
	return normalize(next, allIds);
}

const shortcuts: { command: string; keys: string }[] = [
	{ command: "打开命令补全", keys: "/" },
	{ command: "打开文件引用补全", keys: "@" },
	{ command: "补全命令", keys: "Tab" },
	{ command: "在补全面板中移动选项", keys: "↑ ↓" },
	{ command: "选中补全项；发送消息", keys: "Enter" },
	{ command: "在输入框中换行", keys: "Shift + Enter" },
	{ command: "关闭补全面板；停止当前会话的执行", keys: "Esc" },
];

export default function Settings() {
	const [theme, setTheme] = useState<Theme>("system");
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const [models, setModels] = useState<Model[]>([]);
	const [unavailable, setUnavailable] = useState<string[]>([]);
	const [saved, setSaved] = useState<string[] | null>(null);
	const [selection, setSelection] = useState<string[] | null>(null);
	const [query, setQuery] = useState("");
	const [dragId, setDragId] = useState<string | null>(null);
	const [providersOpen, setProvidersOpen] = useState(false);
	const [savedFlash, setSavedFlash] = useState(false);
	const [error, setError] = useState("");
	const [working, setWorking] = useState(false);
	const segmentRef = useRef<HTMLDivElement>(null);

	// t-tabs：pill 的位移与宽度由 JS 测量写入，首绘与 resize 不动画
	const movePill = useCallback((animate: boolean) => {
		const bar = segmentRef.current;
		const pill = bar?.querySelector<HTMLElement>(".t-tabs-pill");
		const tab =
			bar?.querySelector<HTMLElement>('.t-tab[data-selected="true"]') ?? bar?.querySelector<HTMLElement>(".t-tab");
		if (!pill || !tab) return;
		if (!animate) {
			const previous = pill.style.transition;
			pill.style.transition = "none";
			pill.style.transform = `translateX(${tab.offsetLeft}px)`;
			pill.style.width = `${tab.offsetWidth}px`;
			void pill.offsetWidth;
			pill.style.transition = previous;
			return;
		}
		pill.style.transform = `translateX(${tab.offsetLeft}px)`;
		pill.style.width = `${tab.offsetWidth}px`;
	}, []);
	useLayoutEffect(() => {
		movePill(false);
	}, [movePill]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 切换主题后 pill 要按新布局重新测量，theme 只作触发信号
	useLayoutEffect(() => {
		movePill(true);
	}, [theme, movePill]);
	useEffect(() => {
		const onResize = () => movePill(false);
		window.addEventListener("resize", onResize);
		return () => window.removeEventListener("resize", onResize);
	}, [movePill]);

	const refresh = useCallback(async () => {
		try {
			const [settings, auth, payload] = await Promise.all([
				apiJson<{ theme: Theme }>("/api/settings"),
				apiJson<{ providers: ProviderStatus[] }>("/api/auth"),
				apiJson<ModelsPayload>("/api/models"),
			]);
			const enabled = payload.enabled?.length ? payload.enabled : null;
			setTheme(settings.theme);
			setProviders(auth.providers);
			setModels(payload.models);
			setUnavailable(payload.unavailableEnabled);
			setSaved(enabled);
			setSelection(enabled);
			setError("");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "设置加载失败");
		}
	}, []);
	useEffect(() => {
		void refresh();
	}, [refresh]);

	const allIds = useMemo(() => models.map(fullId), [models]);
	const modelById = useMemo(() => new Map(models.map((model) => [fullId(model), model])), [models]);
	const sortedIds = useMemo(() => {
		if (selection === null) return allIds;
		const enabledSet = new Set(selection);
		return [...selection.filter((id) => allIds.includes(id)), ...allIds.filter((id) => !enabledSet.has(id))];
	}, [selection, allIds]);
	const visibleIds = useMemo(() => {
		const needle = query.trim().toLowerCase();
		const matches = (id: string) => {
			if (!needle) return true;
			const model = modelById.get(id);
			return `${model?.provider ?? ""} ${model?.name ?? ""} ${id}`.toLowerCase().includes(needle);
		};
		return {
			ids: sortedIds.filter(matches),
			unavailable: unavailable.filter((pattern) => pattern.toLowerCase().includes(needle)),
		};
	}, [sortedIds, unavailable, query, modelById]);

	const dirty = JSON.stringify(selection) !== JSON.stringify(saved);
	const enabledCount = selection === null ? allIds.length : selection.filter((id) => allIds.includes(id)).length;

	async function updateTheme(value: Theme) {
		setWorking(true);
		try {
			await apiJson("/api/settings", { method: "PATCH", body: { theme: value } });
			setTheme(value);
			document.documentElement.dataset.theme = value;
			setError("");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "主题未保存");
		} finally {
			setWorking(false);
		}
	}
	async function saveScope() {
		if (selection === null || working) return;
		setWorking(true);
		try {
			await apiJson("/api/models/scoped", { method: "POST", body: { patterns: selection } });
			setQuery("");
			await refresh();
			setSavedFlash(true);
			setTimeout(() => setSavedFlash(false), 1600);
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "启用模型未保存");
		} finally {
			setWorking(false);
		}
	}
	async function logout(providerId: string) {
		setWorking(true);
		try {
			await apiJson(`/api/auth/${encodeURIComponent(providerId)}/logout`, { method: "POST" });
			await refresh();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "退出登录失败");
		} finally {
			setWorking(false);
		}
	}

	function selectAll() {
		const targets = visibleIds.ids;
		if (selection === null) return;
		setSelection(normalize([...new Set([...selection, ...targets])], allIds));
	}
	function clearAll() {
		const targets = new Set(visibleIds.ids);
		setSelection(
			selection === null ? allIds.filter((id) => !targets.has(id)) : selection.filter((id) => !targets.has(id)),
		);
	}
	function dropOn(targetId: string) {
		if (!dragId || selection === null || dragId === targetId) return;
		if (!selection.includes(dragId) || !selection.includes(targetId)) return;
		const next = selection.filter((id) => id !== dragId);
		next.splice(next.indexOf(targetId), 0, dragId);
		setSelection(next);
		setDragId(null);
	}
	function dragOver(event: DragEvent<HTMLElement>, id: string) {
		if (dragId && selection?.includes(dragId) && selection.includes(id)) event.preventDefault();
	}

	const signedIn = providers.filter((provider) => provider.type || provider.requiresLogin);
	const signedOut = providers.filter((provider) => !provider.type && !provider.requiresLogin);

	return (
		<main className="chat-main settings-main">
			<header className="chat-header settings-header">
				<SettingsIcon size={20} aria-hidden="true" />
				<h1>设置</h1>
			</header>
			<div className="settings-content">
				{error && (
					<p role="alert" className="message-error">
						{error}
					</p>
				)}
				<section className="settings-group">
					<header className="settings-group-head">
						<h2>外观</h2>
						<p>主题对本机全部页面生效，选择后立即保存。</p>
					</header>
					<div className="settings-card">
						<div className="settings-row">
							<div className="settings-label">
								<span>界面主题</span>
								<small>选择浅色、深色或跟随系统。</small>
							</div>
							<ToggleButtonGroup
								className="settings-segment t-tabs"
								aria-label="页面主题"
								selectionMode="single"
								disallowEmptySelection
								selectedKeys={[theme]}
								isDisabled={working}
								onSelectionChange={(keys) => {
									const key = [...keys][0];
									if (typeof key === "string") void updateTheme(key as Theme);
								}}
								ref={segmentRef}
							>
								<span className="t-tabs-pill" aria-hidden="true" />
								{(["system", "light", "dark"] as const).map((value) => (
									<ToggleButton key={value} id={value} className="t-tab">
										{{ system: "跟随系统", light: "浅色", dark: "深色" }[value]}
									</ToggleButton>
								))}
							</ToggleButtonGroup>
						</div>
					</div>
				</section>
				<section className="settings-group" id="accounts">
					<header className="settings-group-head">
						<h2>模型服务</h2>
						<p>凭据只保存在本机后端，浏览器不保存。需要登录或更新 API KEY 时前往登录页。</p>
					</header>
					<div className="settings-subgroup">
						<h3>登录状态</h3>
						<div className="settings-card">
							{signedIn.map((provider) => (
								<div key={provider.id} className="settings-row settings-provider">
									<span className="settings-provider-name">{provider.name}</span>
									<span
										className={`settings-badge ${provider.requiresLogin ? "settings-badge-warning" : "settings-badge-success"}`}
									>
										{provider.requiresLogin ? "需要重新登录" : "已登录"}
									</span>
									<span className="settings-spacer" />
									{provider.requiresLogin ? (
										<a
											className="settings-action"
											href={`/login?provider=${encodeURIComponent(provider.id)}`}
										>
											配置 API KEY
										</a>
									) : (
										provider.type && (
											<Button
												type="button"
												variant="ghost"
												className="settings-action settings-action-danger"
												isDisabled={working}
												onPress={() => void logout(provider.id)}
											>
												退出
											</Button>
										)
									)}
								</div>
							))}
							<div className="settings-collapse t-acc" data-open={providersOpen}>
								<button
									type="button"
									className="t-acc-head settings-row"
									aria-expanded={providersOpen}
									onClick={() => setProvidersOpen((open) => !open)}
								>
									查看全部模型服务（{providers.length}）
									<span className="t-acc-chevron">
										<svg
											viewBox="0 0 16 16"
											width="14"
											height="14"
											fill="none"
											stroke="currentColor"
											strokeWidth="2"
											strokeLinecap="round"
											strokeLinejoin="round"
											aria-hidden="true"
										>
											<path d="M4 6.5L8 10.5L12 6.5" />
										</svg>
									</span>
								</button>
								<div className="t-acc-panel">
									<div className="t-acc-panel-inner">
										{signedOut.map((provider) => (
											<div key={provider.id} className="settings-row settings-provider">
												<span className="settings-provider-name">{provider.name}</span>
												<span className="settings-spacer" />
												<a
													className="settings-action"
													href={`/login?provider=${encodeURIComponent(provider.id)}`}
												>
													配置 API KEY
												</a>
											</div>
										))}
									</div>
								</div>
							</div>
						</div>
					</div>
					<div className="settings-subgroup">
						<div className="settings-scope-head">
							<h3>启用模型</h3>
							<span className="settings-spacer" />
							<span className="settings-count">
								{enabledCount} / {allIds.length} 已启用
								{unavailable.length ? ` · ${unavailable.length} 不可用` : ""}
							</span>
							{dirty && <span className="settings-dirty">（未保存）</span>}
						</div>
						<p className="settings-note">
							启用的模型出现在模型菜单并参与循环切换，列表顺序即循环顺序。改动在保存后写入设置，与 TUI 的
							/scoped-models 一致。
						</p>
						<div className="settings-toolbar">
							<SearchField aria-label="搜索模型" className="settings-search" value={query} onChange={setQuery}>
								<SearchField.Group>
									<Search size={16} aria-hidden="true" />
									<SearchField.Input placeholder="搜索模型…" type="search" />
								</SearchField.Group>
							</SearchField>
							<Button
								type="button"
								variant="ghost"
								className="settings-tool-button"
								isDisabled={working || selection === null}
								onPress={selectAll}
							>
								全选
							</Button>
							<Button
								type="button"
								variant="ghost"
								className="settings-tool-button"
								isDisabled={working || enabledCount === 0}
								onPress={clearAll}
							>
								清空
							</Button>
							<Button
								type="button"
								variant="ghost"
								className="settings-tool-button settings-save"
								isDisabled={working || !dirty || selection === null}
								onPress={() => void saveScope()}
							>
								<span className="settings-save-label" key={savedFlash ? "saved" : "idle"}>
									{savedFlash ? (
										<>
											<Check size={14} aria-hidden="true" />
											已保存
										</>
									) : (
										"保存"
									)}
								</span>
							</Button>
						</div>
						<ul className="settings-card settings-models">
							{visibleIds.ids.map((id) => {
								const model = modelById.get(id);
								if (!model) return null;
								const enabled = selection === null || selection.includes(id);
								return (
									<li
										key={id}
										className="settings-model"
										onDragOver={(event) => dragOver(event, id)}
										onDrop={() => dropOn(id)}
									>
										<Checkbox
											isSelected={enabled}
											isDisabled={working}
											aria-label={`启用 ${model.name}`}
											className="settings-check-box"
											onChange={() => setSelection(toggle(selection, allIds, id))}
										>
											<Checkbox.Content>
												<Checkbox.Control>
													<Checkbox.Indicator>
														<svg
															viewBox="0 0 10.1668 10.1668"
															fill="none"
															stroke="#fff"
															strokeWidth="1.6"
															strokeLinecap="round"
															strokeLinejoin="round"
															aria-hidden="true"
														>
															<path d="M1 5.52L3.92 9.17L9.17 1" />
														</svg>
													</Checkbox.Indicator>
												</Checkbox.Control>
											</Checkbox.Content>
										</Checkbox>
										<span className="settings-model-name">{model.name}</span>
										<span className="settings-model-provider">[{model.provider}]</span>
										<span className="settings-spacer" />
										{enabled && (
											<button
												type="button"
												className="settings-grip"
												draggable
												aria-label={`调整 ${model.name} 的顺序`}
												onDragStart={() => setDragId(id)}
												onDragEnd={() => setDragId(null)}
											>
												<GripVertical size={16} aria-hidden="true" />
											</button>
										)}
									</li>
								);
							})}
							{visibleIds.unavailable.map((pattern) => (
								<li key={pattern} className="settings-model settings-model-unavailable">
									<span className="settings-check settings-check-disabled" aria-hidden="true" />
									<span className="settings-model-name">{pattern}</span>
									<span className="settings-badge settings-badge-warning">不可用</span>
									<span className="settings-spacer" />
								</li>
							))}
							{visibleIds.ids.length === 0 && visibleIds.unavailable.length === 0 && (
								<li className="settings-empty">没有匹配的模型</li>
							)}
						</ul>
					</div>
				</section>
				<section className="settings-group">
					<header className="settings-group-head">
						<h2>快捷键</h2>
						<p>会话页输入框内的键盘行为。</p>
					</header>
					<div className="settings-card settings-keys">
						<div className="settings-key settings-key-head">
							<span>命令</span>
							<span>按键绑定</span>
						</div>
						{shortcuts.map((shortcut) => (
							<div key={shortcut.keys} className="settings-key">
								<span>{shortcut.command}</span>
								<Kbd className="settings-key-kbd">{shortcut.keys}</Kbd>
							</div>
						))}
					</div>
				</section>
			</div>
		</main>
	);
}
