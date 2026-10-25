import { Monitor, Moon, Sun } from "lucide-react";
import { apiJson } from "../data/api";
import { SettingsHeading, SettingsLoadState, SettingsSection, useSettingsFeedback } from "../settings/settings-frame";
import { SettingsSegment } from "../settings/settings-segment";
import { useSettingsResource } from "../settings/use-settings-resource";
import { currentTheme, type Theme } from "../shell/theme";

const themes = [
	{ id: "system", label: "跟随系统", icon: Monitor },
	{ id: "light", label: "浅色", icon: Sun },
	{ id: "dark", label: "深色", icon: Moon },
];
export default function GeneralSettings() {
	const state = useSettingsResource<{ theme: Theme }>("/api/settings");
	const feedback = useSettingsFeedback();
	async function update(value: string) {
		if (!state.data || state.working || !themes.some((item) => item.id === value) || value === state.data.theme)
			return;
		const next = value as Theme,
			previous = currentTheme();
		document.documentElement.dataset.theme = next;
		const ok = await state.mutate(
			async () => {
				await apiJson("/api/settings", { method: "PATCH", body: { theme: next } });
				return { theme: next };
			},
			{ theme: next },
		);
		if (ok) feedback();
		else document.documentElement.dataset.theme = previous;
	}
	return (
		<>
			<SettingsHeading title="常规" />
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<SettingsSection title="外观">
					<div className="settings-card">
						<div className="settings-flat-row">
							<div className="settings-row-copy">
								<span>界面主题</span>
							</div>
							<SettingsSegment
								label="页面主题"
								value={state.data.theme}
								items={themes}
								disabled={state.working}
								onChange={(value) => void update(value)}
							/>
						</div>
					</div>
				</SettingsSection>
			)}
		</>
	);
}
