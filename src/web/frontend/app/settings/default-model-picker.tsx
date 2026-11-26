import { Button, ListBox, Popover } from "@heroui/react";
import { Check, ChevronDown, KeyRound } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { ModelKeyDialog } from "../auth/model-key-dialog";
import type { ModelChoice } from "../data/model-selection";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import { SettingsSearch } from "./settings-search";

const fullId = (model: ModelChoice) => `${model.provider}/${model.id}`;
export function DefaultModelPicker({
	model,
	models,
	busy,
	onSelect,
}: {
	model: ModelChoice;
	models: ModelChoice[];
	busy: boolean;
	onSelect: (model: ModelChoice) => Promise<boolean>;
}) {
	const [open, setOpen] = useState(false);
	const [query, setQuery] = useState("");
	const [missingKey, setMissingKey] = useState<ModelChoice | null>(null);
	const triggerRef = useRef<HTMLButtonElement>(null);
	const listRef = useRef<HTMLFieldSetElement>(null);
	const itemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const kbdStyle = useKeyboardFocusStyle();
	const [focusedId, setFocusedId] = useState<string | null>(null);
	const [kbdActive, setKbdActive] = useState(false);
	const clearKbd = useCallback(() => setKbdActive(false), []);
	usePointerMutex(kbdActive && open, clearKbd);
	const shell = useWorkspaceShell();
	const closePopover = useCallback(() => {
		setOpen(false);
		setQuery("");
		setFocusedId(null);
		setKbdActive(false);
	}, []);
	usePanelDismiss(open, closePopover, undefined, triggerRef);
	useEffect(() => {
		if (!shell?.appActive) return;
		closePopover();
		setMissingKey(null);
	}, [shell?.appActive, closePopover]);
	const needle = query.trim().toLocaleLowerCase();
	const ordered = [
		model,
		...models.filter((item) => fullId(item) !== fullId(model) && item.credentialReady),
		...models.filter((item) => fullId(item) !== fullId(model) && !item.credentialReady),
	];
	const matches = ordered.filter((item) =>
		`${item.name} ${item.providerName ?? item.provider} ${fullId(item)}`.toLocaleLowerCase().includes(needle),
	);
	const shown = matches.slice(0, 50);
	async function select(key: string) {
		const selected = models.find((item) => fullId(item) === key);
		if (!selected || busy) return;
		closePopover();
		if (await onSelect(selected)) {
			if (!selected.credentialReady) setMissingKey(selected);
		}
	}
	return (
		<div className="settings-default-model">
			<div className="settings-default-row">
				<span className="settings-default-label">新会话默认模型</span>
				<Popover
					isOpen={open}
					onOpenChange={(next) => {
						if (next) setOpen(true);
						else closePopover();
					}}
				>
					<Button
						ref={triggerRef}
						className="settings-default-trigger"
						aria-label="选择新会话默认模型"
						isDisabled={busy}
					>
						<span className="settings-default-trigger-main">
							<strong>{model.name}</strong>
							<span className="settings-default-trigger-provider">{model.provider}</span>
						</span>
						<ChevronDown size={13} aria-hidden="true" />
					</Button>
					<Popover.Content
						className="tier-2-glass settings-default-popover"
						placement="bottom end"
						offset={6}
						data-kbd-active={kbdActive && focusedId ? "true" : undefined}
					>
						<Popover.Dialog aria-label="选择新会话默认模型">
							<SettingsSearch label="搜索默认模型" value={query} onChange={setQuery} />
							<fieldset
								ref={listRef}
								aria-label="默认模型候选列表"
								style={{ position: "relative", border: 0, padding: 0, margin: 0, minInlineSize: 0 }}
								onKeyDown={(event) => {
									if (["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) {
										setKbdActive(true);
										requestAnimationFrame(() => {
											const focused = listRef.current?.querySelector<HTMLElement>(
												'[role="option"][data-focused="true"], [role="option"]:focus',
											);
											const fid = focused?.dataset.key ?? focused?.getAttribute("data-option-id") ?? null;
											if (fid) setFocusedId(fid);
										});
									}
								}}
							>
								<ListBox
									aria-label="默认模型列表"
									selectionMode="single"
									selectedKeys={[fullId(model)]}
									onSelectionChange={(keys) => {
										if (keys !== "all") {
											const key = [...keys][0];
											if (key != null) void select(String(key));
										}
									}}
								>
									{shown.map((item) => {
										const itemKey = fullId(item);
										return (
											<ListBox.Item
												key={itemKey}
												id={itemKey}
												textValue={`${item.name} ${item.provider}`}
												className="settings-default-option"
												data-option-id={itemKey}
												ref={(node: HTMLElement | null) => {
													if (node) itemRefs.current.set(itemKey, node);
													else itemRefs.current.delete(itemKey);
												}}
											>
												<span>
													<strong>{item.name}</strong>
													<small>
														{item.providerName ?? item.provider} · {item.id}
													</small>
												</span>
												{fullId(model) === itemKey && (
													<Check size={14} className="settings-select-check" aria-hidden="true" />
												)}
											</ListBox.Item>
										);
									})}
								</ListBox>
								<KbdFocusOverlay
									containerRef={listRef}
									itemRefs={itemRefs}
									targetId={kbdActive && focusedId ? focusedId : null}
									style={kbdStyle}
									railPlacement="inside"
									bracketOutset={2}
								/>
							</fieldset>
							{!shown.length && <p className="settings-empty">没有匹配的模型</p>}
							{matches.length > shown.length && <p className="settings-default-hint">输入名称查找更多模型</p>}
						</Popover.Dialog>
					</Popover.Content>
				</Popover>
			</div>
			{!model.credentialReady && (
				<div className="settings-default-status">
					<span>
						<KeyRound size={15} aria-hidden="true" />
						尚未配置 {model.providerName ?? model.provider} 密钥
					</span>
					<button
						type="button"
						className="settings-inline-action settings-inline-action--primary"
						onClick={() => setMissingKey(model)}
					>
						配置密钥
					</button>
				</div>
			)}
			{missingKey && <ModelKeyDialog model={missingKey} onClose={() => setMissingKey(null)} />}
		</div>
	);
}
