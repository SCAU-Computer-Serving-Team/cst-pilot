import { Button, Checkbox, ListBox, Select } from "@heroui/react";
import { ChevronDown, ChevronRight, GripVertical } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { apiJson } from "../data/api";
import type { Model } from "../data/web-state";
import { modelListRows } from "../settings/model-list";
import { moveModel, normalizeScope, toggleModel } from "../settings/model-scope";
import { SettingsHeading, SettingsLoadState, SettingsSection, useSettingsFeedback } from "../settings/settings-frame";
import { SettingsSearch } from "../settings/settings-search";
import { useAnimatedHeight } from "../settings/use-animated-height";
import { useSettingsResource } from "../settings/use-settings-resource";

type ModelsPayload = { models: Model[]; enabled: string[] | null; unavailableEnabled: string[] };
const fullId = (model: Model) => `${model.provider}/${model.id}`;
export default function ModelPreferences() {
	const state = useSettingsResource<ModelsPayload>("/api/models");
	const feedback = useSettingsFeedback();
	const [query, setQuery] = useState("");
	const [provider, setProvider] = useState("all");
	const [expanded, setExpanded] = useState(false);
	const [dragId, setDragId] = useState<string | null>(null);
	const models = state.data?.models ?? [];
	const selection = state.data?.enabled ?? null;
	const allIds = models.map(fullId);
	const byId = useMemo(() => new Map(models.map((model) => [fullId(model), model])), [models]);
	const providers = [...new Set(models.map((model) => model.provider))];
	// 勾选变化不重新排列可见行；取消勾选的模型留在原位。
	const displayOrder = useRef<string[]>([]);
	if (displayOrder.current.length !== allIds.length || displayOrder.current.some((id) => !byId.has(id))) {
		displayOrder.current =
			selection === null
				? allIds
				: [...selection.filter((id) => byId.has(id)), ...allIds.filter((id) => !selection.includes(id))];
	}
	const sorted = displayOrder.current;
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
	const enabledCount = selection === null ? models.length : selection.filter((id) => byId.has(id)).length;
	async function save(next: string[] | null) {
		if (!state.data || state.working) return;
		const optimistic = { ...state.data, enabled: next };
		const ok = await state.mutate(async (current) => {
			const result = await apiJson<{ enabled: string[] | null }>("/api/models/scoped", {
				method: "POST",
				body: { patterns: next ?? [], empty: next !== null && next.length === 0 },
			});
			return { ...current, enabled: result.enabled, unavailableEnabled: [] };
		}, optimistic);
		if (ok) feedback();
	}
	function drop(target: string) {
		if (!dragId || !selection) return;
		const next = moveModel(selection, dragId, target);
		setDragId(null);
		if (next !== selection) {
			displayOrder.current = [...next!, ...sorted.filter((id) => !next?.includes(id))];
			void save(next);
		}
	}
	return (
		<>
			<SettingsHeading title="模型偏好" description="控制聊天输入框中显示的模型。" />
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<>
					<div className="settings-model-filters">
						<SettingsSearch label="搜索模型" value={query} onChange={setQuery} />
						<Select
							aria-label="按服务筛选模型"
							className="settings-provider-select"
							selectedKey={provider}
							onSelectionChange={(key) => setProvider(String(key ?? "all"))}
						>
							<Select.Trigger>
								<Select.Value /> <ChevronDown size={14} aria-hidden="true" />
							</Select.Trigger>
							<Select.Popover
								className="login-provider-popover settings-provider-popover"
								placement="bottom end"
							>
								<ListBox aria-label="模型服务">
									<ListBox.Item id="all" textValue="全部">
										全部
									</ListBox.Item>
									{providers.map((id) => (
										<ListBox.Item id={id} key={id} textValue={id}>
											{id}
										</ListBox.Item>
									))}
								</ListBox>
							</Select.Popover>
						</Select>
					</div>
					<SettingsSection title="启用模型" summary={`${enabledCount} 个已启用`}>
						<div className="settings-model-bulk">
							<Button
								size="sm"
								variant="ghost"
								isDisabled={state.working || !visible.length || selection === null}
								onPress={() =>
									void save(normalizeScope([...new Set([...(selection ?? allIds), ...visible])], allIds))
								}
							>
								全选
							</Button>
							<Button
								size="sm"
								variant="ghost"
								isDisabled={
									state.working || !visible.some((id) => selection === null || selection.includes(id))
								}
								onPress={() =>
									void save(
										normalizeScope(
											(selection ?? allIds).filter((id) => !visible.includes(id)),
											allIds,
										),
									)
								}
							>
								清空
							</Button>
							<Button
								size="sm"
								variant="ghost"
								isDisabled={state.working || (selection === null && !unavailable.length)}
								onPress={() => void save(null)}
							>
								恢复默认
							</Button>
							{state.working && <output>正在保存…</output>}
						</div>
						<div
							className="settings-card settings-model-card"
							ref={list}
							data-expanded={expanded}
							aria-busy={state.working}
						>
							<ul className="settings-models" id="enabled-model-list">
								{listed.ids.map((id) => {
									const model = byId.get(id)!;
									const enabled = selection === null || selection.includes(id);
									return (
										<li
											key={id}
											className="settings-model"
											data-dragging={dragId === id || undefined}
											onDragOver={(event) => {
												if (dragId && selection?.includes(id)) event.preventDefault();
											}}
											onDrop={(event) => {
												event.preventDefault();
												drop(id);
											}}
										>
											<Checkbox
												isSelected={enabled}
												isDisabled={state.working}
												aria-label={`启用 ${model.name}`}
												className="settings-check-box"
												onChange={() => void save(toggleModel(selection, allIds, id))}
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
											<span className="settings-model-name" title={model.name}>
												{model.name}
											</span>
											<span className="settings-model-provider" title={model.provider}>
												{model.provider}
											</span>
											{enabled && selection !== null ? (
												<button
													type="button"
													className="settings-grip"
													draggable={!state.working}
													disabled={state.working}
													aria-label={`调整 ${model.name} 的顺序`}
													title="拖动排序；Alt + ↑ / ↓ 调整顺序"
													onDragStart={(event) => {
														event.dataTransfer.setData("text/plain", id);
														event.dataTransfer.effectAllowed = "move";
														setDragId(id);
													}}
													onDragEnd={() => setDragId(null)}
													onKeyDown={(event) => {
														if (!event.altKey || !["ArrowUp", "ArrowDown"].includes(event.key)) return;
														event.preventDefault();
														const target =
															selection[selection.indexOf(id) + (event.key === "ArrowUp" ? -1 : 1)];
														if (target) {
															displayOrder.current = moveModel(sorted, id, target) ?? sorted;
															void save(moveModel(selection, id, target));
														}
													}}
												>
													<GripVertical size={16} aria-hidden="true" />
												</button>
											) : (
												<span className="settings-grip-space" aria-hidden="true" />
											)}
										</li>
									);
								})}
								{listed.unavailable.map((id) => (
									<li key={id} className="settings-model settings-model-unavailable">
										<span className="settings-check settings-check-disabled" aria-hidden="true" />
										<span className="settings-model-name">{id}</span>
										<span className="settings-model-provider">不可用</span>
										<span className="settings-grip-space" aria-hidden="true" />
									</li>
								))}
								{!listed.total && (
									<li className="settings-empty">
										{models.length ? "没有匹配的模型" : "连接模型服务后，可在此选择模型。"}
									</li>
								)}
							</ul>
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
									<ChevronRight size={14} aria-hidden="true" />
								</button>
							)}
						</div>
					</SettingsSection>
				</>
			)}
		</>
	);
}
