import { Plus } from "lucide-react";

import { ProviderServices } from "../auth/provider-services";
import type { ProviderStatus } from "../data/api";
import { settingsProviders } from "../settings/provider-list";
import { SettingsLink, SettingsLoadState, SettingsSection } from "../settings/settings-frame";
import { useSettingsResource } from "../settings/use-settings-resource";

export default function ModelServices() {
	const state = useSettingsResource<{ providers: ProviderStatus[] }>("/api/auth");
	const configured = settingsProviders(state.data?.providers ?? []).configured;
	return (
		<>
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<SettingsSection
					title="已配置"
					action={
						<SettingsLink to="/settings/add" className="settings-inline-action settings-inline-action--primary">
							<Plus size={14} aria-hidden="true" />
							添加提供商
						</SettingsLink>
					}
				>
					{configured.length ? (
						<ProviderServices providers={configured} unframed />
					) : (
						<p className="settings-empty">
							尚未配置提供商。<SettingsLink to="/settings/add">添加提供商</SettingsLink>
						</p>
					)}
				</SettingsSection>
			)}
		</>
	);
}
