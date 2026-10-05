import { ToggleButton, ToggleButtonGroup } from "@heroui/react";
import { useCallback, useEffect, useLayoutEffect, useRef } from "react";

export function SettingsSegment({
	label,
	value,
	items,
	disabled = false,
	onChange,
}: {
	label: string;
	value: string;
	items: readonly { id: string; label: string }[];
	disabled?: boolean;
	onChange: (value: string) => void;
}) {
	const bar = useRef<HTMLDivElement>(null);
	const movePill = useCallback((animate: boolean) => {
		const pill = bar.current?.querySelector<HTMLElement>(".t-tabs-pill");
		const tab = bar.current?.querySelector<HTMLElement>('.t-tab[data-selected="true"]');
		if (!pill || !tab) return;
		const previous = pill.style.transition;
		if (!animate) pill.style.transition = "none";
		pill.style.transform = `translateX(${tab.offsetLeft}px)`;
		pill.style.width = `${tab.offsetWidth}px`;
		if (!animate) {
			void pill.offsetWidth;
			pill.style.transition = previous;
		}
	}, []);
	useLayoutEffect(() => movePill(false), [movePill]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: value 改变后按新选中项的位置移动高亮。
	useLayoutEffect(() => movePill(true), [value, movePill]);
	useEffect(() => {
		const element = bar.current;
		if (!element) return;
		const observer = new ResizeObserver(() => movePill(false));
		observer.observe(element);
		return () => observer.disconnect();
	}, [movePill]);
	return (
		<ToggleButtonGroup
			className="settings-segment t-tabs"
			aria-label={label}
			selectionMode="single"
			disallowEmptySelection
			selectedKeys={[value]}
			isDisabled={disabled}
			ref={bar}
			onSelectionChange={(keys) => {
				const key = [...keys][0];
				if (typeof key === "string") onChange(key);
			}}
		>
			<span className="t-tabs-pill" aria-hidden="true" />
			{items.map((item) => (
				<ToggleButton key={item.id} id={item.id} className="t-tab">
					{item.label}
				</ToggleButton>
			))}
		</ToggleButtonGroup>
	);
}
