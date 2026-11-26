import { Monitor, Moon, Scan, Sun, Tally1 } from "lucide-react";
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

	async function updateTheme(value: string) {
		if (!state.data || state.working || !themes.some((item) => item.id === value) || value === state.data.theme)
			return;
		const next = value as Theme;
		const previous = currentTheme();
		const kbdStyle = currentKbdStyle;
		document.documentElement.dataset.theme = next;
		const ok = await state.mutate(
			async () => {
				const saved = await apiJson<GeneralSettingsData>("/api/settings", {
					method: "PATCH",
					body: { theme: next },
				});
				return {
					theme: saved.theme,
					keyboardFocusStyle: normalizeKeyboardFocusStyle(saved.keyboardFocusStyle ?? kbdStyle),
				};
			},
			{ theme: next, keyboardFocusStyle: kbdStyle },
		);
		if (ok) feedback();
		else document.documentElement.dataset.theme = previous;
	}

	async function updateKbdStyle(value: string) {
		if (
			!state.data ||
			state.working ||
			!kbdFocusStyles.some((item) => item.id === value) ||
			value === currentKbdStyle
		)
			return;
		const next = normalizeKeyboardFocusStyle(value);
		const theme = state.data.theme;
		const ok = await state.mutate(
			async () => {
				const saved = await apiJson<GeneralSettingsData>("/api/settings", {
					method: "PATCH",
					body: { keyboardFocusStyle: next },
				});
				const confirmed = normalizeKeyboardFocusStyle(saved.keyboardFocusStyle ?? next);
				applyKeyboardFocusStyle(confirmed);
				return { theme: saved.theme, keyboardFocusStyle: confirmed };
			},
			{ theme, keyboardFocusStyle: next },
		);
		if (ok) feedback();
	}

	return (
		<>
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<section className="settings-flat-list" aria-label="常规设置">
					<div className="settings-flat-row">
						<div className="settings-row-copy">
							<span>界面主题</span>
						</div>
						<SettingsSegment
							label="界面主题"
							value={state.data.theme}
							items={themes}
							disabled={state.working}
							onChange={(value) => void updateTheme(value)}
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
							disabled={state.working}
							onChange={(value) => void updateKbdStyle(value)}
						/>
					</div>
				</section>
			)}
		</>
	);
}
