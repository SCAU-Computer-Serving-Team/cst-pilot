import type { LucideIcon } from "lucide-react";
import { type KeyboardEvent as ReactKeyboardEvent, useCallback, useRef, useState } from "react";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { resolveToolbarNavKey } from "../shell/keyboard-navigation";

export function SettingsSegment({
	label,
	value,
	items,
	disabled = false,
	busy = false,
	onChange,
	onVerticalNav,
}: {
	label: string;
	value: string;
	items: readonly { id: string; label: string; icon?: LucideIcon }[];
	disabled?: boolean;
	busy?: boolean;
	onChange: (value: string) => void;
	onVerticalNav?: (direction: -1 | 1) => boolean | undefined;
}) {
	const groupRef = useRef<HTMLDivElement>(null);
	const itemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const kbdStyle = useKeyboardFocusStyle();
	const [focusedId, setFocusedId] = useState<string | null>(null);
	const [kbdActive, setKbdActive] = useState(false);
	const clearKbd = useCallback(() => setKbdActive(false), []);
	usePointerMutex(kbdActive, clearKbd);

	function handleKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
		if (event.defaultPrevented || event.nativeEvent.isComposing || event.keyCode === 229) return;
		if (
			onVerticalNav &&
			!event.altKey &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey &&
			(event.key === "ArrowUp" || event.key === "ArrowDown")
		) {
			event.preventDefault();
			const moved = onVerticalNav(event.key === "ArrowUp" ? -1 : 1);
			if (moved !== false) setKbdActive(false);
			return;
		}
		if (disabled || busy) return;
		const buttons = [...(groupRef.current?.querySelectorAll<HTMLElement>(".settings-chip") ?? [])];
		if (!buttons.length) return;
		const cur = Math.max(0, buttons.indexOf(document.activeElement as HTMLElement));
		const nextIdx = resolveToolbarNavKey(
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
			cur,
			buttons.length,
		);
		if (nextIdx !== null) {
			event.preventDefault();
			setKbdActive(true);
			const item = items[nextIdx];
			if (item) {
				setFocusedId(item.id);
				buttons[nextIdx]?.focus();
				if (item.id !== value) onChange(item.id);
			}
		}
	}

	return (
		<div
			ref={groupRef}
			role="toolbar"
			aria-label={label}
			aria-busy={busy || undefined}
			className="settings-chip-group"
			data-kbd-active={kbdActive && focusedId !== null ? "true" : undefined}
			onKeyDown={handleKeyDown}
			onBlur={(event) => {
				if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
					setFocusedId(null);
					setKbdActive(false);
				}
			}}
		>
			{items.map((item) => {
				const selected = item.id === value;
				const IconComp = item.icon;
				return (
					<button
						key={item.id}
						ref={(node) => {
							if (node) itemRefs.current.set(item.id, node);
							else itemRefs.current.delete(item.id);
						}}
						type="button"
						aria-pressed={selected}
						aria-disabled={disabled || busy || undefined}
						tabIndex={selected ? 0 : -1}
						disabled={disabled}
						className="settings-chip"
						data-selected={selected || undefined}
						onFocus={(event) => {
							setFocusedId(item.id);
							if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
						}}
						onClick={() => {
							if (disabled || busy) return;
							setKbdActive(false);
							if (!selected) onChange(item.id);
						}}
					>
						{IconComp && <IconComp size={14} aria-hidden="true" />}
						<span>{item.label}</span>
					</button>
				);
			})}
			<KbdFocusOverlay
				containerRef={groupRef}
				itemRefs={itemRefs}
				targetId={kbdActive ? focusedId : null}
				style={kbdStyle}
				railPlacement="bottom"
				bracketOutset={3}
			/>
		</div>
	);
}
