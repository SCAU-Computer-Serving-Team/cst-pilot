import { Monitor, Moon, Scan, Sun, Tally1 } from "lucide-react";
import { useRef } from "react";
import { apiJson } from "../data/api";
import { SettingsLoadState, useSettingsFeedback } from "../settings/settings-frame";
import { SettingsSegment } from "../settings/settings-segment";
import { useSettingsResource } from "../settings/use-settings-resource";
import { applyKeyboardFocusStyle, type KeyboardFocusStyle, normalizeKeyboardFocusStyle } from "../shell/kbd-focus";
import { currentTheme, type Theme } from "../shell/theme";

const themes = [
	{ id: "system", label: "跟随系统", icon: Monitor },
	{ id: "light", label: "浅色", icon: Sun },
	{ id: "dark", label: "深色", icon: Moon },
];
const kbdFocusStyles = [
	{ id: "brackets", label: "四角", icon: Scan },
	{ id: "rail", label: "游标", icon: Tally1 },
];

type GeneralSettingsData = {
	theme: Theme;
	keyboardFocusStyle?: KeyboardFocusStyle;
};

export default function GeneralSettings() {
	const state = useSettingsResource<GeneralSettingsData>("/api/settings");
	const feedback = useSettingsFeedback();
	const currentKbdStyle = normalizeKeyboardFocusStyle(state.data?.keyboardFocusStyle);
	const listRef = useRef<HTMLElement>(null);

	function focusRow(rowIdx: number) {
		if (rowIdx < 0) {
			document.querySelector<HTMLElement>(".settings-nav-link.is-current")?.focus();
			return;
		}
		const rows = listRef.current?.querySelectorAll<HTMLElement>(".settings-flat-row");
		const target = rows?.[rowIdx]?.querySelector<HTMLElement>('.settings-chip[tabindex="0"], .settings-chip');
		target?.focus();
	}

	async function updateTheme(value: string) {
		if (!state.data || !themes.some((item) => item.id === value)) return;
		const next = value as Theme;
		document.documentElement.dataset.theme = next;
		const ok = await state.mutateLatest(
			(current) => ({
				theme: next,
				keyboardFocusStyle: normalizeKeyboardFocusStyle(current.keyboardFocusStyle),
			}),
			async (draft, confirmed) => {
				const saved = await apiJson<GeneralSettingsData>("/api/settings", {
					method: "PATCH",
					body: { theme: draft.theme },
				});
				return {
					theme: saved.theme,
					keyboardFocusStyle: normalizeKeyboardFocusStyle(
						saved.keyboardFocusStyle ?? confirmed.keyboardFocusStyle,
					),
				};
			},
			{
				coalesceKey: "theme",
				onRollback: (confirmed) => {
					document.documentElement.dataset.theme = confirmed.theme || currentTheme();
				},
			},
		);
		if (ok) feedback();
	}

	async function updateKbdStyle(value: string) {
		if (!state.data || !kbdFocusStyles.some((item) => item.id === value)) return;
		const next = normalizeKeyboardFocusStyle(value);
		applyKeyboardFocusStyle(next);
		const ok = await state.mutateLatest(
			(current) => ({ theme: current.theme, keyboardFocusStyle: next }),
			async (draft, confirmed) => {
				const target = normalizeKeyboardFocusStyle(draft.keyboardFocusStyle);
				const saved = await apiJson<GeneralSettingsData>("/api/settings", {
					method: "PATCH",
					body: { keyboardFocusStyle: target },
				});
				const resolved = normalizeKeyboardFocusStyle(saved.keyboardFocusStyle ?? target);
				return { theme: confirmed.theme, keyboardFocusStyle: resolved };
			},
			{
				coalesceKey: "kbd",
				onRollback: (confirmed) => {
					applyKeyboardFocusStyle(normalizeKeyboardFocusStyle(confirmed.keyboardFocusStyle));
				},
			},
		);
		if (ok) feedback();
	}

	return (
		<>
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<section ref={listRef} className="settings-flat-list" aria-label="常规设置">
					<div className="settings-flat-row">
						<div className="settings-row-copy">
							<span>界面主题</span>
						</div>
						<SettingsSegment
							label="界面主题"
							value={state.data.theme}
							items={themes}
							busy={false}
							onChange={(value) => void updateTheme(value)}
							onVerticalNav={(dir) => focusRow(dir < 0 ? -1 : 1)}
						/>
					</div>
					<div className="settings-flat-row">
						<div className="settings-row-copy">
							<span>方向键选中样式</span>
						</div>
						<SettingsSegment
							label="方向键选中样式"
							value={currentKbdStyle}
							items={kbdFocusStyles}
							busy={false}
							onChange={(value) => void updateKbdStyle(value)}
							onVerticalNav={(dir) => {
								if (dir < 0) focusRow(0);
							}}
						/>
					</div>
				</section>
			)}
		</>
	);
}
