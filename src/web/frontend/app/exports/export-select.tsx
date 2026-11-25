import { Label, ListBox, Select } from "@heroui/react";
import { Check, ChevronDown } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { useWorkspaceShell } from "../shell/workspace-shell-context";

export function ExportSelect<T extends string>({
	label,
	value,
	items,
	disabled,
	onChange,
	open: controlledOpen,
	onOpenChange,
	triggerRef,
	kbdHint = false,
}: {
	label: string;
	value: T;
	items: readonly { id: T; label: string }[];
	disabled: boolean;
	onChange: (value: T) => void;
	open?: boolean;
	onOpenChange?: (open: boolean) => void;
	triggerRef?: (el: HTMLButtonElement | null) => void;
	kbdHint?: boolean;
}) {
	const [uncontrolledOpen, setUncontrolledOpen] = useState(false);
	const open = controlledOpen ?? uncontrolledOpen;
	const setOpen = useCallback(
		(next: boolean) => {
			if (controlledOpen === undefined) setUncontrolledOpen(next);
			onOpenChange?.(next);
		},
		[controlledOpen, onOpenChange],
	);
	const trigger = useRef<HTMLButtonElement>(null);
	const shell = useWorkspaceShell();
	usePanelDismiss(open, () => setOpen(false), undefined, trigger);
	useEffect(() => {
		if (shell?.appActive) setOpen(false);
	}, [shell?.appActive, setOpen]);
	return (
		<Select
			className="export-select"
			isOpen={open}
			onOpenChange={setOpen}
			isDisabled={disabled}
			selectedKey={value}
			onSelectionChange={(key) => {
				const item = items.find((item) => item.id === key);
				if (item) onChange(item.id);
			}}
		>
			<div className="export-label-row">
				<Label>{label}</Label>
				{kbdHint && <span className="export-kbd-hint">← → 切换</span>}
			</div>
			<Select.Trigger
				ref={(node) => {
					trigger.current = node;
					triggerRef?.(node);
				}}
				className="export-select-trigger"
			>
				<Select.Value>{items.find((item) => item.id === value)?.label}</Select.Value>
				<ChevronDown size={14} aria-hidden="true" />
			</Select.Trigger>
			<Select.Popover className="tier-2-glass tier-2-menu export-select-popover" placement="bottom start" offset={6}>
				<ListBox aria-label={label} className="tier-2-menu-list">
					{items.map((item) => (
						<ListBox.Item
							key={item.id}
							id={item.id}
							textValue={item.label}
							className="menu-row export-select-option"
						>
							<span>{item.label}</span>
							{item.id === value && <Check size={14} className="export-select-check" aria-hidden="true" />}
						</ListBox.Item>
					))}
				</ListBox>
			</Select.Popover>
		</Select>
	);
}
