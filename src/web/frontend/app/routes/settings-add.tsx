import { ChevronRight } from "lucide-react";
import { useState } from "react";
import { ProviderServices } from "../auth/provider-services";
import type { ProviderStatus } from "../data/api";
import { CatalogRefreshButton } from "../settings/catalog-refresh-button";
import { modelListRows } from "../settings/model-list";
import { settingsProviders } from "../settings/provider-list";
import { SettingsHeading, SettingsLink, SettingsLoadState, SettingsSection } from "../settings/settings-frame";
import { SettingsSearch } from "../settings/settings-search";
import { useAnimatedHeight } from "../settings/use-animated-height";
import { useSettingsResource } from "../settings/use-settings-resource";

export default function AddModelService() {
	const state = useSettingsResource<{ providers: ProviderStatus[] }>("/api/auth");
	const [query, setQuery] = useState("");
	const [expanded, setExpanded] = useState(false);
	const available = settingsProviders(state.data?.providers ?? [], query).available;
	const listed = modelListRows(
		available.map((provider) => provider.id),
		[],
		expanded || !!query.trim(),
	);
	const visible = available.filter((provider) => listed.ids.includes(provider.id));
	const list = useAnimatedHeight(`${expanded}:${query}:${listed.total}`);
	return (
		<>
			<SettingsHeading title="添加提供商" back action={<CatalogRefreshButton />} />
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<>
					{!query.trim() && (
						<div className="settings-flat-list">
							<div className="settings-flat-row">
								<div className="settings-row-inline">
									<span className="settings-service-name">自定义提供商</span>
									<span className="settings-description">兼容 OpenAI / Anthropic / Gemini API</span>
								</div>
								<SettingsLink to="/settings/custom" className="settings-inline-action">
									配置
									<ChevronRight size={14} aria-hidden="true" />
								</SettingsLink>
							</div>
						</div>
					)}
					<SettingsSection title="可添加的提供商">
						<SettingsSearch label="搜索提供商" value={query} onChange={setQuery} />
						{available.length ? (
							<div className="settings-service-card" ref={list} data-expanded={expanded || !!query.trim()}>
								<ProviderServices providers={visible} adding unframed />
								{listed.hasMore && (
									<button
										type="button"
										className="settings-models-toggle"
										aria-expanded={expanded || !!query.trim()}
										onClick={() => {
											setExpanded(!(expanded || !!query.trim()));
											setQuery("");
										}}
									>
										{expanded || query.trim() ? "收起列表" : "查看全部提供商"}（{listed.total}）
										<ChevronRight size={14} className="settings-provider-chevron" aria-hidden="true" />
									</button>
								)}
							</div>
						) : (
							<p className="settings-empty">
								{query.trim() ? "没有匹配的提供商" : "所有支持连接的提供商均已配置"}
							</p>
						)}
					</SettingsSection>
				</>
			)}
		</>
	);
}
