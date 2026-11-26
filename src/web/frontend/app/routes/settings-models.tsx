import { Checkbox, ListBox, Select } from "@heroui/react";
import { ArrowDown, ArrowUp, Check, ChevronDown, ChevronRight } from "lucide-react";
import {
	type KeyboardEvent as ReactKeyboardEvent,
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { apiJson } from "../data/api";
import type { ModelChoice, ModelSelectionPayload } from "../data/model-selection";
import type { Model } from "../data/web-state";
import { DefaultModelPicker } from "../settings/default-model-picker";
import { modelListRows } from "../settings/model-list";
import { moveModel, normalizeScope, toggleModel } from "../settings/model-scope";
import { SettingsLoadState, SettingsSection, useSettingsFeedback } from "../settings/settings-frame";
import { SettingsSearch } from "../settings/settings-search";
import { useAnimatedHeight } from "../settings/use-animated-height";
import { useSettingsResource } from "../settings/use-settings-resource";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { resolveListNavKey } from "../shell/keyboard-navigation";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { useWorkspaceShell } from "../shell/workspace-shell-context";

type ModelsPayload = ModelSelectionPayload;
const fullId = (model: Model) => `${model.provider}/${model.id}`;

export default function ModelPreferences() {
	const state = useSettingsResource<ModelsPayload>("/api/models");
	const feedback = useSettingsFeedback();
	const [query, setQuery] = useState("");
	const [provider, setProvider] = useState("all");
	const [providerOpen, setProviderOpen] = useState(false);
	const providerTriggerRef = useRef<HTMLButtonElement>(null);
	const providerListRef = useRef<HTMLFieldSetElement>(null);
	const providerItemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const [providerFocusedId, setProviderFocusedId] = useState<string | null>(null);
	const [providerKbdActive, setProviderKbdActive] = useState(false);
	const clearProviderKbd = useCallback(() => setProviderKbdActive(false), []);
	const shell = useWorkspaceShell();
	const closeProviderPopover = useCallback(() => {
		setProviderOpen(false);
		setProviderFocusedId(null);
		setProviderKbdActive(false);
	}, []);
	usePanelDismiss(providerOpen, closeProviderPopover, undefined, providerTriggerRef);
	usePointerMutex(providerOpen && providerKbdActive, clearProviderKbd);
	useEffect(() => {
		if (shell?.appActive) closeProviderPopover();
	}, [shell?.appActive, closeProviderPopover]);
	const [expanded, setExpanded] = useState(false);
	const [dragId, setDragId] = useState<string | null>(null);
	const keyboardFocusId = useRef<string | null>(null);
	const listRoot = useRef<HTMLDivElement>(null);
	const itemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const kbdStyle = useKeyboardFocusStyle();
	const [focusedRowId, setFocusedRowId] = useState<string | null>(null);
	const [kbdActive, setKbdActive] = useState(false);
	const clearKbd = useCallback(() => setKbdActive(false), []);
	usePointerMutex(kbdActive, clearKbd);

	const models = state.data?.models ?? [];
	const selection = state.data?.enabled ?? null;
	const allIds = useMemo(() => models.map(fullId), [models]);
	const byId = useMemo(() => new Map(models.map((model) => [fullId(model), model])), [models]);
	const providers = [...new Set(models.map((model) => model.provider))];
	if (state.data && provider !== "all" && !providers.includes(provider)) setProvider("all");
	// 确认顺序与乐观重排纳入同一事务：外部刷新或保存失败时直接回滚到已确认顺序，取消勾选保留原展示位。
	const canonicalOrder = useMemo(
		() =>
			selection === null
				? allIds
				: [...selection.filter((id) => byId.has(id)), ...allIds.filter((id) => !selection.includes(id))],
		[allIds, byId, selection],
	);
	const [displayOrderState, setDisplayOrderState] = useState<{
		baseSelection: string[] | null;
		baseCatalogKey: string;
		order: string[];
	} | null>(null);
	const catalogKey = allIds.join("\0");
	const sorted = useMemo(() => {
		if (
			displayOrderState &&
			displayOrderState.baseCatalogKey === catalogKey &&
			displayOrderState.order.length === allIds.length &&
			displayOrderState.order.every((id) => byId.has(id))
		) {
			return displayOrderState.order;
		}
		return canonicalOrder;
	}, [allIds.length, byId, canonicalOrder, catalogKey, displayOrderState]);
	// 外部数据源变更（如刷新、其他端修改或保存失败回滚）时，同步对账到最新确认顺序。
	const lastSelectionRef = useRef<string[] | null | undefined>(undefined);
	const pendingReorderRef = useRef(false);
	useEffect(() => {
		if (pendingReorderRef.current) return;
		if (lastSelectionRef.current !== selection) {
			lastSelectionRef.current = selection;
			setDisplayOrderState((prev) => {
				if (!prev || prev.baseCatalogKey !== catalogKey) return null;
				// 仅在启用集合相同但顺序不同、或外部刷新时重置展示顺序；单次勾选保留当前行位置。
				const prevEnabled = prev.baseSelection;
				const sameOrder =
					selection === null
						? prevEnabled === null
						: prevEnabled !== null &&
							selection.length === prevEnabled.length &&
							selection.every((id, i) => prevEnabled[i] === id);
				if (sameOrder) return prev;
				// 如果启用集长度相同但顺序改变（或外部重置为 null），按服务端确认顺序重排
				const prevLen = prevEnabled === null ? allIds.length : prevEnabled.length;
				const nextLen = selection === null ? allIds.length : selection.length;
				if (prevLen === nextLen) return null;
				return { baseSelection: selection, baseCatalogKey: catalogKey, order: prev.order };
			});
		}
	}, [allIds.length, catalogKey, selection]);
	const needle = query.trim().toLocaleLowerCase();
	const matches = (id: string) =>
		(provider === "all" || byId.get(id)?.provider === provider) &&
		(!needle || `${byId.get(id)?.name ?? ""} ${id}`.toLocaleLowerCase().includes(needle));
	const visible = sorted.filter(matches);
	const unavailable = (state.data?.unavailableEnabled ?? []).filter(
		(id) => (provider === "all" || id.startsWith(`${provider}/`)) && id.toLocaleLowerCase().includes(needle),
	);
	const listed = modelListRows(visible, unavailable, expanded || !!needle);
	const list = useAnimatedHeight(`${expanded}:${query}:${provider}:${listed.total}`);

	// Effective ordered enabled list (materializes allIds when selection === null so Alt+↑↓ always works!)
	const effectiveEnabledOrder = selection === null ? sorted : selection.filter((id) => byId.has(id));
	const visibleEnabledOrder = effectiveEnabledOrder.filter((id) => listed.ids.includes(id));

	useLayoutEffect(() => {
		const targetId = keyboardFocusId.current;
		if (!targetId) return;
		const row = listRoot.current?.querySelector<HTMLElement>(
			`.settings-model[data-model-id="${CSS.escape(targetId)}"]`,
		);
		const active = document.activeElement;
		if (active !== document.body && active !== row) {
			keyboardFocusId.current = null;
			return;
		}
		if (row && active === document.body) row.focus({ preventScroll: true });
		if (!state.working) keyboardFocusId.current = null;
	});

	const enabledCount = selection === null ? models.length : selection.filter((id) => byId.has(id)).length;
	const allVisibleEnabled = visible.length > 0 && visible.every((id) => selection === null || selection.includes(id));

	async function save(
		nextOrFn: string[] | null | ((current: ModelsPayload) => string[] | null),
		nextDisplayOrder?: string[],
	) {
		if (!state.data) return;
		const previousOrder = displayOrderState;
		if (nextDisplayOrder) {
			pendingReorderRef.current = true;
			const resolvedNext = typeof nextOrFn === "function" ? nextOrFn(state.data) : nextOrFn;
			setDisplayOrderState({ baseSelection: resolvedNext, baseCatalogKey: catalogKey, order: nextDisplayOrder });
		}
		const ok = await state.mutateLatest(
			(current) => {
				const resolved = typeof nextOrFn === "function" ? nextOrFn(current) : nextOrFn;
				return { ...current, enabled: resolved };
			},
			async (next) => {
				const result = await apiJson<{ enabled: string[] | null }>("/api/models/scoped", {
					method: "POST",
					body: { patterns: next.enabled ?? [], empty: next.enabled !== null && next.enabled.length === 0 },
				});
				return { ...next, enabled: result.enabled, unavailableEnabled: [] };
			},
			{
				onRollback: () => {
					if (nextDisplayOrder) setDisplayOrderState(previousOrder);
				},
			},
		);
		pendingReorderRef.current = false;
		if (ok) feedback();
	}

	async function saveDefault(model: ModelChoice) {
		const ok = await state.mutateLatest(
			(current) => ({ ...current, defaultModel: model, selected: model }),
			async (next) => {
				await apiJson("/api/models/default", {
					method: "POST",
					body: { provider: model.provider, modelId: model.id },
				});
				return { ...next, defaultModel: model, selected: model };
			},
		);
		if (ok) feedback("已保存默认模型");
		return ok;
	}

	function reorderByDelta(id: string, delta: -1 | 1) {
		const baseOrder = selection === null ? [...sorted] : [...selection];
		const visPos = visibleEnabledOrder.indexOf(id);
		if (visPos < 0) return;
		const targetId = visibleEnabledOrder[visPos + delta];
		if (!targetId) return;
		const nextOrder = moveModel(baseOrder, id, targetId);
		if (!nextOrder || nextOrder === baseOrder) return;
		keyboardFocusId.current = id;
		const nextDisplay = [...nextOrder, ...sorted.filter((item) => !nextOrder.includes(item))];
		void save(nextOrder, nextDisplay);
	}

	function drop(target: string) {
		if (!dragId) return;
		const baseOrder = selection === null ? [...sorted] : [...selection];
		const next = moveModel(baseOrder, dragId, target);
		setDragId(null);
		if (next && next !== baseOrder) {
			const nextDisplay = [...next, ...sorted.filter((id) => !next.includes(id))];
			void save(next, nextDisplay);
		}
	}

	function handleRowKeyDown(event: ReactKeyboardEvent<HTMLDivElement>, id: string, idx: number, enabled: boolean) {
		if (event.nativeEvent.isComposing || event.keyCode === 229) return;
		if (
			enabled &&
			event.altKey &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey &&
			(event.key === "ArrowUp" || event.key === "ArrowDown")
		) {
			event.preventDefault();
			setKbdActive(true);
			reorderByDelta(id, event.key === "ArrowUp" ? -1 : 1);
			return;
		}
		if (
			!event.altKey &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey &&
			(event.key === "Enter" || event.key === " ")
		) {
			if ((event.target as HTMLElement)?.closest("button")) return;
			event.preventDefault();
			setKbdActive(true);
			void save((current) => toggleModel(current.enabled, current.models.map(fullId), id));
			return;
		}
		const nextIdx = resolveListNavKey(
			{
				key: event.key,
				defaultPrevented: event.defaultPrevented,
				isComposing: event.nativeEvent.isComposing,
				keyCode: event.keyCode,
				ctrlKey: event.ctrlKey,
				metaKey: event.metaKey,
				altKey: event.altKey,
				shiftKey: event.shiftKey,
			},
			idx,
			listed.ids.length,
		);
		if (nextIdx !== null) {
			event.preventDefault();
			setKbdActive(true);
			const nextId = listed.ids[nextIdx];
			if (nextId) {
				setFocusedRowId(nextId);
				listRoot.current
					?.querySelector<HTMLElement>(`.settings-model[data-model-id="${CSS.escape(nextId)}"]`)
					?.focus();
			}
		}
	}

	return (
		<>
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<>
					<DefaultModelPicker
						model={state.data.defaultModel}
						models={state.data.catalog}
						busy={false}
						onSelect={saveDefault}
					/>
					<SettingsSection
						title="启用模型"
						summary={`${enabledCount} / ${models.length}`}
						action={
							<div className="settings-section-actions">
								<button
									type="button"
									className="settings-inline-action"
									disabled={!visible.length}
									onClick={() => {
										if (allVisibleEnabled) {
											void save((current) => {
												const ids = current.models.map(fullId);
												return normalizeScope(
													(current.enabled ?? ids).filter((id) => !visible.includes(id)),
													ids,
												);
											});
										} else {
											void save((current) => {
												const ids = current.models.map(fullId);
												const merged = sorted.filter(
													(id) => (current.enabled ?? ids).includes(id) || visible.includes(id),
												);
												return normalizeScope(merged, ids);
											});
										}
									}}
								>
									{allVisibleEnabled ? "清空" : "全选"}
								</button>
								<button
									type="button"
									className="settings-inline-action"
									disabled={state.loading}
									onClick={() => void state.refresh()}
								>
									刷新
								</button>
							</div>
						}
					>
						<div className="settings-model-filters">
							<SettingsSearch label="搜索模型" value={query} onChange={setQuery} />
							<Select
								aria-label="按提供商筛选模型"
								className="settings-provider-select"
								isOpen={providerOpen}
								onOpenChange={(next) => {
									if (next) setProviderOpen(true);
									else closeProviderPopover();
								}}
								selectedKey={provider}
								onSelectionChange={(key) => {
									setProvider(String(key ?? "all"));
									closeProviderPopover();
								}}
							>
								<Select.Trigger ref={providerTriggerRef}>
									<Select.Value>{provider === "all" ? "全部提供商" : provider}</Select.Value>{" "}
									<ChevronDown size={13} aria-hidden="true" />
								</Select.Trigger>
								<Select.Popover
									className="tier-2-glass tier-2-menu settings-provider-popover"
									placement="bottom end"
									offset={6}
									data-kbd-active={providerKbdActive && providerFocusedId ? "true" : undefined}
								>
									<fieldset
										ref={providerListRef}
										aria-label="提供商筛选列表"
										style={{ position: "relative", border: 0, padding: 0, margin: 0, minInlineSize: 0 }}
										onKeyDown={(event) => {
											if (event.nativeEvent.isComposing || event.keyCode === 229) return;
											if (["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) {
												setProviderKbdActive(true);
												requestAnimationFrame(() => {
													const focused = providerListRef.current?.querySelector<HTMLElement>(
														'[role="option"][data-focused="true"], [role="option"]:focus',
													);
													const fid = focused?.getAttribute("data-option-id") ?? null;
													if (fid) setProviderFocusedId(fid);
												});
											}
										}}
									>
										<ListBox aria-label="提供商筛选" className="tier-2-menu-list">
											<ListBox.Item
												id="all"
												textValue="全部提供商"
												className="menu-row settings-select-option"
												data-option-id="all"
												ref={(node: HTMLElement | null) => {
													if (node) providerItemRefs.current.set("all", node);
													else providerItemRefs.current.delete("all");
												}}
											>
												<span>全部提供商</span>
												{provider === "all" && (
													<Check size={14} className="settings-select-check" aria-hidden="true" />
												)}
											</ListBox.Item>
											{providers.map((id) => (
												<ListBox.Item
													id={id}
													key={id}
													textValue={id}
													className="menu-row settings-select-option"
													data-option-id={id}
													ref={(node: HTMLElement | null) => {
														if (node) providerItemRefs.current.set(id, node);
														else providerItemRefs.current.delete(id);
													}}
												>
													<span>{id}</span>
													{provider === id && (
														<Check size={14} className="settings-select-check" aria-hidden="true" />
													)}
												</ListBox.Item>
											))}
										</ListBox>
										<KbdFocusOverlay
											containerRef={providerListRef}
											itemRefs={providerItemRefs}
											targetId={providerKbdActive && providerFocusedId ? providerFocusedId : null}
											style={kbdStyle}
											railPlacement="inside"
											bracketOutset={2}
										/>
									</fieldset>
								</Select.Popover>
							</Select>
						</div>
						<div
							className="settings-model-card"
							ref={list}
							data-expanded={expanded || !!needle}
							aria-busy={state.working}
						>
							<div
								className="settings-models"
								id="enabled-model-list"
								role="listbox"
								aria-label="启用模型列表"
								ref={listRoot}
								data-kbd-active={kbdActive && focusedRowId !== null ? "true" : undefined}
								onBlur={(event) => {
									if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
										setFocusedRowId(null);
										setKbdActive(false);
									}
								}}
							>
								{listed.ids.map((id, idx) => {
									const model = byId.get(id)!;
									const enabled = selection === null || selection.includes(id);
									const enabledPos = enabled ? visibleEnabledOrder.indexOf(id) : -1;
									const canMoveUp = enabledPos > 0;
									const canMoveDown = enabledPos >= 0 && enabledPos + 1 < visibleEnabledOrder.length;
									return (
										<div
											key={id}
											ref={(node) => {
												if (node) itemRefs.current.set(id, node);
												else itemRefs.current.delete(id);
											}}
											role="option"
											aria-selected={enabled}
											tabIndex={0}
											data-model-id={id}
											data-enabled={enabled ? "true" : "false"}
											className="settings-model"
											data-dragging={dragId === id || undefined}
											draggable={enabled}
											onClick={(event) => {
												if (
													(event.target as HTMLElement).closest(
														".settings-check-box, label, input, button",
													)
												)
													return;
												void save((current) =>
													toggleModel(current.enabled, current.models.map(fullId), id),
												);
											}}
											onFocus={(event) => {
												setFocusedRowId(id);
												if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
											}}
											onKeyDown={(event) => handleRowKeyDown(event, id, idx, enabled)}
											onDragStart={(event) => {
												if (!enabled) {
													event.preventDefault();
													return;
												}
												event.dataTransfer.setData("text/plain", id);
												event.dataTransfer.effectAllowed = "move";
												setDragId(id);
											}}
											onDragEnd={() => setDragId(null)}
											onDragOver={(event) => {
												if (dragId && enabled) event.preventDefault();
											}}
											onDrop={(event) => {
												event.preventDefault();
												drop(id);
											}}
										>
											<Checkbox
												isSelected={enabled}
												aria-label={`启用 ${model.name}`}
												className="settings-check-box"
												onChange={() =>
													void save((current) =>
														toggleModel(current.enabled, current.models.map(fullId), id),
													)
												}
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
												<span className="settings-model-name" title={model.name}>
													{model.name}
												</span>
												<span className="settings-model-provider" title={model.provider}>
													{model.provider}
												</span>
											</div>
											{enabled && (
												<div className="settings-model-reorder">
													<button
														type="button"
														className="settings-model-arrow"
														disabled={!canMoveUp}
														aria-label={`上移 ${model.name}`}
														title="上移（Alt + ↑）"
														onClick={(event) => {
															event.stopPropagation();
															reorderByDelta(id, -1);
														}}
													>
														<ArrowUp size={13} aria-hidden="true" />
													</button>
													<button
														type="button"
														className="settings-model-arrow"
														disabled={!canMoveDown}
														aria-label={`下移 ${model.name}`}
														title="下移（Alt + ↓）"
														onClick={(event) => {
															event.stopPropagation();
															reorderByDelta(id, 1);
														}}
													>
														<ArrowDown size={13} aria-hidden="true" />
													</button>
												</div>
											)}
										</div>
									);
								})}
								{listed.unavailable.map((id) => (
									<div key={id} className="settings-model settings-model-unavailable">
										<span className="settings-check settings-check-disabled" aria-hidden="true" />
										<div className="settings-model-main">
											<span className="settings-model-name">{id}</span>
											<span className="settings-model-provider">不可用</span>
										</div>
									</div>
								))}
								{!listed.total && (
									<div className="settings-empty">
										{models.length ? "没有匹配的模型" : "连接提供商后，可在此选择模型。"}
									</div>
								)}
								<KbdFocusOverlay
									containerRef={listRoot}
									itemRefs={itemRefs}
									targetId={
										kbdActive && focusedRowId && listed.ids.includes(focusedRowId) ? focusedRowId : null
									}
									style={kbdStyle}
									railPlacement="inside"
									bracketOutset={2}
								/>
							</div>
							{listed.hasMore && (
								<button
									type="button"
									className="settings-models-toggle"
									aria-expanded={expanded || !!needle}
									aria-controls="enabled-model-list"
									onClick={() => {
										setQuery("");
										setExpanded(!(expanded || !!needle));
									}}
								>
									{expanded || needle ? "收起模型列表" : "查看全部模型"}（{listed.total}）
									<ChevronRight size={14} className="settings-provider-chevron" aria-hidden="true" />
								</button>
							)}
						</div>
					</SettingsSection>
				</>
			)}
		</>
	);
}
