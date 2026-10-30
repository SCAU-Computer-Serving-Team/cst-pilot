import { Link, useSearchParams } from "react-router";
import type { ProviderStatus } from "../data/api";
import { ProviderConfigurationForm } from "../settings/provider-configuration";
import { SettingsHeading, SettingsLoadState } from "../settings/settings-frame";
import { useSettingsResource } from "../settings/use-settings-resource";

export default function ProviderConfiguration() {
	const [params] = useSearchParams();
	const state = useSettingsResource<{ providers: ProviderStatus[] }>("/api/auth");
	const providerId = params.get("provider");
	const provider = state.data?.providers.find((item) => item.id === providerId);
	return (
		<>
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{provider ? (
				<ProviderConfigurationForm
					key={provider.id}
					provider={provider}
					requestedMethod={params.get("method")}
					returnTo={params.get("from") === "model-key" ? params.get("returnTo") : null}
					onRefresh={state.refresh}
				/>
			) : !state.loading && !state.error ? (
				<>
					<SettingsHeading title="模型服务不存在" back />
					<p className="settings-empty">
						请返回<Link to="/settings/add">添加服务</Link>重新选择。
					</p>
				</>
			) : null}
		</>
	);
}
