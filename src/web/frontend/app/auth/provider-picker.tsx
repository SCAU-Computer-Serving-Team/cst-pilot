import { Label, ListBox, Select } from "@heroui/react";
import { Check, ChevronDown } from "lucide-react";
import { useRef, useState } from "react";
import type { ProviderStatus } from "../data/api";
import { usePanelDismiss } from "../shell/panel-dismiss";

export function ProviderPicker({
	providers,
	selected,
	standalone = false,
	disabled,
	onChange,
}: {
	providers: ProviderStatus[];
	selected: string;
	standalone?: boolean;
	disabled: boolean;
	onChange: (id: string) => void;
}) {
	const [open, setOpen] = useState(false);
	const trigger = useRef<HTMLButtonElement>(null);
	usePanelDismiss(open, () => setOpen(false), undefined, trigger);
	return (
		<Select
			isOpen={open}
			onOpenChange={setOpen}
			className="login-field login-provider login-provider-picker"
			selectedKey={selected || null}
			onSelectionChange={(key) => {
				if (key != null) onChange(String(key));
			}}
			isDisabled={disabled || !providers.length}
		>
			<Label>模型服务</Label>
			<Select.Trigger ref={trigger} className="login-input">
				<Select.Value>
					{providers.find((provider) => provider.id === selected)?.name ?? "选择模型服务"}
				</Select.Value>
				<ChevronDown size={14} aria-hidden="true" />
			</Select.Trigger>
			<Select.Popover
				className={`tier-2-glass tier-2-menu login-provider-popover${standalone ? " provider-config-popover" : ""}`}
				placement="bottom start"
				offset={6}
			>
				<ListBox className="tier-2-menu-list login-provider-list">
					{providers.map((provider) => (
						<ListBox.Item
							key={provider.id}
							id={provider.id}
							textValue={provider.name}
							className="menu-row login-provider-option"
						>
							<span className="login-provider-option-name" title={provider.name}>
								{provider.name}
							</span>
							<span className="login-provider-check">
								<Check size={14} aria-hidden="true" />
							</span>
						</ListBox.Item>
					))}
				</ListBox>
			</Select.Popover>
		</Select>
	);
}
