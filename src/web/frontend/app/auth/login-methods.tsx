import type { ProviderStatus } from "../data/api";
import { type LoginMethod, loginMethods } from "./providers";

export function LoginMethods({ provider, current }: { provider: ProviderStatus; current?: LoginMethod }) {
	return (
		<span className="provider-methods">
			{loginMethods(provider).map((method) => {
				const active = method === (current ?? provider.type);
				const label = method === "oauth" ? "OAuth" : "API KEY";
				return (
					<span
						key={method}
						className="provider-method-tag"
						data-active={active}
						title={active ? `当前使用：${label}` : `支持 ${label}`}
					>
						{label}
					</span>
				);
			})}
		</span>
	);
}
