import { Button, ListBox, Popover } from "@heroui/react";
import { Check, ChevronDown, KeyRound } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { ModelKeyDialog } from "../auth/model-key-dialog";
import type { ModelChoice } from "../data/model-selection";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import { SettingsSection } from "./settings-frame";
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
	const shell = useWorkspaceShell();
	const closePopover = useCallback(() => {
		setOpen(false);
		setQuery("");
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
		<SettingsSection title="新会话默认模型">
			<div className="settings-card settings-default-model">
				<div className="settings-flat-row">
					<div className="settings-row-copy">
						<span>默认模型</span>
						<span className="settings-description">用于新会话，与 TUI 同步</span>
					</div>
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
							<span>
								<strong>{model.name}</strong>
							</span>
							<ChevronDown size={14} aria-hidden="true" />
						</Button>
						<Popover.Content className="tier-2-glass settings-default-popover" placement="bottom end" offset={6}>
							<Popover.Dialog aria-label="选择新会话默认模型">
								<SettingsSearch label="搜索默认模型" value={query} onChange={setQuery} />
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
									{shown.map((item) => (
										<ListBox.Item
											key={fullId(item)}
											id={fullId(item)}
											textValue={`${item.name} ${item.provider}`}
											className="settings-default-option"
										>
											<span>
												<strong>{item.name}</strong>
												<small>
													{item.providerName ?? item.provider} · {item.id}
												</small>
											</span>
											{fullId(model) === fullId(item) && (
												<Check size={14} className="settings-select-check" aria-hidden="true" />
											)}
										</ListBox.Item>
									))}
								</ListBox>
								{!shown.length && <p className="settings-empty">没有匹配的模型</p>}
								{matches.length > shown.length && <p className="settings-default-hint">输入名称查找更多模型</p>}
							</Popover.Dialog>
						</Popover.Content>
					</Popover>
				</div>
				{!model.credentialReady && (
					<div className="settings-default-status">
						<span>
							<KeyRound size={16} aria-hidden="true" />
							尚未配置 {model.providerName ?? model.provider} 密钥
						</span>
						<button
							type="button"
							className="tier-1-btn-outline settings-action"
							onClick={() => setMissingKey(model)}
						>
							配置密钥
						</button>
					</div>
				)}
			</div>
			{missingKey && <ModelKeyDialog model={missingKey} onClose={() => setMissingKey(null)} />}
		</SettingsSection>
	);
}
