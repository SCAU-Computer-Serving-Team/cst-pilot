import { SearchField } from "@heroui/react";
import { Search } from "lucide-react";
export function SettingsSearch({
	label,
	value,
	onChange,
}: {
	label: string;
	value: string;
	onChange: (value: string) => void;
}) {
	return (
		<SearchField aria-label={label} className="settings-search" value={value} onChange={onChange}>
			<SearchField.Group>
				<Search size={16} aria-hidden="true" />
				<SearchField.Input placeholder={`${label}…`} type="search" />
			</SearchField.Group>
		</SearchField>
	);
}
