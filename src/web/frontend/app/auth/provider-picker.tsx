import { Label, ListBox, Select } from "@heroui/react";
import { Check, ChevronDown } from "lucide-react";
import { useState } from "react";
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
	usePanelDismiss(open, () => setOpen(false));
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
			<Label>{standalone ? "模型服务" : "Provider"}</Label>
			<Select.Trigger className="login-input">
				<Select.Value>
					{providers.find((provider) => provider.id === selected)?.name ?? "选择模型服务"}
				</Select.Value>
				<ChevronDown size={16} aria-hidden="true" />
			</Select.Trigger>
			<Select.Popover
				className={`login-provider-popover${standalone ? " provider-config-popover" : ""}`}
				placement="bottom start"
			>
				<ListBox className="login-provider-list">
					{providers.map((provider) => (
						<ListBox.Item
							key={provider.id}
							id={provider.id}
							textValue={provider.name}
							className="login-provider-option"
						>
							<span className="login-provider-option-name" title={provider.name}>
								{provider.name}
							</span>
							<span className="login-provider-check">
								<Check size={16} aria-hidden="true" />
							</span>
						</ListBox.Item>
					))}
				</ListBox>
			</Select.Popover>
		</Select>
	);
}
