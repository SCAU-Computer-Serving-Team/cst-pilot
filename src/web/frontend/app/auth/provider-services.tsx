import { Button } from "@heroui/react";
import { ChevronRight, CircleCheck, ExternalLink, Pencil, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import type { ProviderStatus } from "../data/api";
import { SettingsSegment } from "../settings/settings-segment";
import { LoginMethods } from "./login-methods";
import { configurationPath, type ProviderFilter, providerGroups } from "./providers";

const filters = [
	{ id: "all", label: "全部" },
	{ id: "oauth", label: "OAuth" },
	{ id: "api_key", label: "API KEY" },
];

function ProviderRow({
	provider,
	working,
	onLogout,
}: {
	provider: ProviderStatus;
	working: boolean;
	onLogout: (id: string) => void;
}) {
	const signedIn = !!provider.type && !provider.requiresLogin;
	const status = signedIn ? "已登录" : provider.requiresLogin ? "需要重新登录" : "未登录";
	return (
		<div className="settings-row settings-provider">
			<span className="settings-provider-name">{provider.name}</span>
			<LoginMethods provider={provider} />
			<span
				className={`settings-badge settings-status-icon ${signedIn ? "settings-badge-success" : "settings-badge-warning"}`}
				role="img"
				aria-label={status}
				title={status}
			>
				{signedIn ? <CircleCheck size={14} aria-hidden="true" /> : <TriangleAlert size={14} aria-hidden="true" />}
			</span>
			<span className="settings-spacer" />
			<div className="settings-provider-actions">
				{provider.supportsApiKey && (
					<Link
						className="settings-action settings-icon-action"
						to={configurationPath(provider.id, "api_key")}
						aria-label={`配置 ${provider.name} API KEY`}
						title="配置 API KEY"
						aria-disabled={working}
						onClick={(event) => {
							if (working) event.preventDefault();
						}}
					>
						<Pencil size={16} aria-hidden="true" />
					</Link>
				)}
				{provider.supportsOAuth && (
					<Link
						className="settings-action settings-icon-action"
						to={configurationPath(provider.id, "oauth")}
						aria-label={`通过 OAuth 登录 ${provider.name}`}
						title={provider.id === "cstoa" ? "CSTOA 扫码登录" : "OAuth 登录"}
						aria-disabled={working}
						onClick={(event) => {
							if (working) event.preventDefault();
						}}
					>
						<ExternalLink size={16} aria-hidden="true" />
					</Link>
				)}
				{provider.type && (
					<Button
						type="button"
						variant="ghost"
						className="settings-action settings-action-danger"
						isDisabled={working}
						aria-label={`退出 ${provider.name}`}
						onPress={() => onLogout(provider.id)}
					>
						退出
					</Button>
				)}
			</div>
		</div>
	);
}

export function ProviderServices({
	providers,
	working,
	onLogout,
}: {
	providers: ProviderStatus[];
	working: boolean;
	onLogout: (id: string) => void;
}) {
	const [open, setOpen] = useState(false);
	const [filter, setFilter] = useState<ProviderFilter>("all");
	const { primary, others } = providerGroups(providers, filter);
	return (
		<>
			<div className="settings-provider-head">
				<h3>登录状态</h3>
				<SettingsSegment
					label="筛选模型服务登录方式"
					value={filter}
					items={filters}
					onChange={(value) => {
						if (value === "all" || value === "oauth" || value === "api_key") {
							setFilter(value);
							setOpen(false);
						}
					}}
				/>
			</div>
			<div className="settings-card">
				{primary.map((provider) => (
					<ProviderRow key={provider.id} provider={provider} working={working} onLogout={onLogout} />
				))}
				{primary.length === 0 && others.length === 0 && <p className="settings-empty">没有匹配的模型服务</p>}
				{others.length > 0 && (
					<div className="settings-collapse t-acc" data-open={open}>
						<button
							type="button"
							className="t-acc-head settings-row"
							aria-expanded={open}
							aria-controls="other-model-services"
							onClick={() => setOpen((value) => !value)}
						>
							{open ? "收起模型服务" : "查看全部模型服务"}（{primary.length + others.length}）
							<ChevronRight size={14} className="settings-provider-chevron" aria-hidden="true" />
						</button>
						<div className="t-acc-panel" inert={!open} id="other-model-services">
							<div className="t-acc-panel-inner settings-other-providers">
								{others.map((provider) => (
									<ProviderRow key={provider.id} provider={provider} working={working} onLogout={onLogout} />
								))}
							</div>
						</div>
					</div>
				)}
			</div>
		</>
	);
}
