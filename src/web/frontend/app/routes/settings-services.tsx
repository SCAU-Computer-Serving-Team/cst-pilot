import { Plus } from "lucide-react";
import { Link } from "react-router";
import { ProviderServices } from "../auth/provider-services";
import type { ProviderStatus } from "../data/api";
import { settingsProviders } from "../settings/provider-list";
import { SettingsHeading, SettingsLoadState, SettingsSection } from "../settings/settings-frame";
import { useSettingsResource } from "../settings/use-settings-resource";
export default function ModelServices() {
	const state = useSettingsResource<{ providers: ProviderStatus[] }>("/api/auth");
	const configured = settingsProviders(state.data?.providers ?? []).configured;
	return (
		<>
			<SettingsHeading
				title="模型服务"
				action={
					<Link to="/settings/add" className="settings-action settings-add-action">
						<Plus size={14} aria-hidden="true" />
						添加服务
					</Link>
				}
			/>
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<SettingsSection title="已配置" summary={`${configured.length} 个服务`}>
					{configured.length ? (
						<ProviderServices providers={configured} />
					) : (
						<div className="settings-card">
							<p className="settings-empty">
								尚未配置模型服务。<Link to="/settings/add">添加服务</Link>
							</p>
						</div>
					)}
				</SettingsSection>
			)}
		</>
	);
}
