import { useState } from "react";
import { ProviderServices } from "../auth/provider-services";
import type { ProviderStatus } from "../data/api";
import { settingsProviders } from "../settings/provider-list";
import { SettingsHeading, SettingsLoadState, SettingsSection } from "../settings/settings-frame";
import { SettingsSearch } from "../settings/settings-search";
import { useSettingsResource } from "../settings/use-settings-resource";
export default function AddModelService() {
	const state = useSettingsResource<{ providers: ProviderStatus[] }>("/api/auth");
	const [query, setQuery] = useState("");
	const available = settingsProviders(state.data?.providers ?? [], query).available;
	return (
		<>
			<SettingsHeading title="添加服务" back />
			<SettingsSearch label="搜索模型服务" value={query} onChange={setQuery} />
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<SettingsSection title="可添加的服务">
					{available.length ? (
						<ProviderServices providers={available} adding />
					) : (
						<div className="settings-card">
							<p className="settings-empty">
								{query.trim() ? "没有匹配的模型服务" : "所有支持连接的服务均已配置"}
							</p>
						</div>
					)}
				</SettingsSection>
			)}
		</>
	);
}
