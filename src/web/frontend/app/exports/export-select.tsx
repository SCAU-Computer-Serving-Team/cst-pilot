import { Label, ListBox, Select } from "@heroui/react";
import { ChevronDown } from "lucide-react";
import { useState } from "react";
import { usePanelDismiss } from "../shell/panel-dismiss";

export function ExportSelect<T extends string>({
	label,
	value,
	items,
	disabled,
	onChange,
}: {
	label: string;
	value: T;
	items: readonly { id: T; label: string }[];
	disabled: boolean;
	onChange: (value: T) => void;
}) {
	const [open, setOpen] = useState(false);
	usePanelDismiss(open, () => setOpen(false));
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
			<Label>{label}</Label>
			<Select.Trigger className="export-select-trigger">
				<Select.Value>{items.find((item) => item.id === value)?.label}</Select.Value>
				<ChevronDown size={16} aria-hidden="true" />
			</Select.Trigger>
			<Select.Popover className="export-select-popover" placement="bottom start" offset={8}>
				<ListBox aria-label={label}>
					{items.map((item) => (
						<ListBox.Item key={item.id} id={item.id} textValue={item.label}>
							{item.label}
						</ListBox.Item>
					))}
				</ListBox>
			</Select.Popover>
		</Select>
	);
}
