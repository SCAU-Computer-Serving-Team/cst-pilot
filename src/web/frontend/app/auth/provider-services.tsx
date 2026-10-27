import { TriangleAlert } from "lucide-react";
import { Link } from "react-router";
import { describeAccount } from "../account/format";
import { useAccount } from "../account/use-account";
import type { ProviderStatus } from "../data/api";
import { providerDescription } from "../settings/provider-list";
import { configurationPath, initialLogin } from "./providers";

export function ProviderServices({ providers, adding = false }: { providers: ProviderStatus[]; adding?: boolean }) {
	const { account } = useAccount();
	const identity = describeAccount(account);
	return (
		<div className="settings-card settings-services">
			{providers.map((item) => {
				const provider = item.id === "cstoa" && account?.requiresLogin ? { ...item, requiresLogin: true } : item;
				const initial = initialLogin([provider], provider.id, provider.type);
				const status = providerDescription(provider);
				return (
					<div className="settings-flat-row settings-service-row" key={provider.id} data-provider={provider.id}>
						<div className="settings-row-copy">
							<span className="settings-service-name">{provider.name}</span>
							<span className="settings-description" data-warning={provider.requiresLogin || undefined}>
								{provider.requiresLogin && <TriangleAlert size={14} aria-hidden="true" />}
								{provider.id === "cstoa" && identity && !provider.requiresLogin
									? `${identity.identity} · ${status}`
									: status}
							</span>
						</div>
						<Link
							className="settings-action"
							to={configurationPath(provider.id, initial?.method ?? "api_key")}
							aria-label={`${adding ? "连接" : provider.requiresLogin ? "重新登录" : "管理"} ${provider.name}`}
						>
							{adding ? "连接" : provider.requiresLogin ? "重新登录" : "管理"}
						</Link>
					</div>
				);
			})}
		</div>
	);
}
