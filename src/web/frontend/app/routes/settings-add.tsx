import { ChevronRight, SlidersHorizontal } from "lucide-react";
import { useState } from "react";
import { ProviderServices } from "../auth/provider-services";
import type { ProviderStatus } from "../data/api";
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
			<SettingsHeading title="添加服务" back />
			<SettingsSearch label="搜索模型服务" value={query} onChange={setQuery} />
			<SettingsLoadState {...state} onRetry={() => void state.refresh()} />
			{state.data && (
				<>
					{!query.trim() && (
						<SettingsSection title="自定义服务">
							<div className="settings-card settings-services">
								<div className="settings-flat-row settings-service-row">
									<div className="settings-row-copy">
										<span className="settings-service-name">连接兼容接口</span>
										<span className="settings-description">填写服务地址、接口协议、API key 和模型信息</span>
									</div>
									<SettingsLink to="/settings/custom" className="settings-action">
										<SlidersHorizontal size={14} aria-hidden="true" /> 配置
									</SettingsLink>
								</div>
							</div>
						</SettingsSection>
					)}
					<SettingsSection title="可添加的服务" summary={`${available.length} 个服务`}>
						{available.length ? (
							<div
								className="settings-card settings-service-card"
								ref={list}
								data-expanded={expanded || !!query.trim()}
							>
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
										{expanded || query.trim() ? "收起服务列表" : "查看全部服务"}（{listed.total}）
										<ChevronRight size={14} aria-hidden="true" />
									</button>
								)}
							</div>
						) : (
							<div className="settings-card">
								<p className="settings-empty">
									{query.trim() ? "没有匹配的模型服务" : "所有支持连接的服务均已配置"}
								</p>
							</div>
						)}
					</SettingsSection>
				</>
			)}
		</>
	);
}
