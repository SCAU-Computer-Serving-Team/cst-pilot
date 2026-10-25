import "../settings/settings.css";
import { Settings as SettingsIcon } from "lucide-react";
import { Link, Outlet, useLocation } from "react-router";
import { SettingsFeedback } from "../settings/settings-frame";

const tabs = [
	{ to: "/settings/general", label: "常规" },
	{ to: "/settings", label: "模型服务" },
	{ to: "/settings/models", label: "模型偏好" },
	{ to: "/settings/shortcuts", label: "快捷键" },
];
export default function Settings() {
	const { pathname } = useLocation();
	const activePath = ["/settings/add", "/settings/provider"].includes(pathname) ? "/settings" : pathname;
	return (
		<main className="chat-main settings-main settings-workspace">
			<header className="chat-header settings-header">
				<SettingsIcon size={22} aria-hidden="true" />
				<h1>设置</h1>
			</header>
			<SettingsFeedback>
				<div className="settings-scroll">
					<div className="settings-content settings-pages">
						<nav className="settings-nav" aria-label="设置分类">
							{tabs.map((tab) => (
								<Link
									key={tab.to}
									to={tab.to}
									aria-current={activePath === tab.to ? "page" : undefined}
									className={`settings-nav-link${activePath === tab.to ? " is-current" : ""}`}
								>
									{tab.label}
								</Link>
							))}
						</nav>
						<Outlet />
					</div>
				</div>
			</SettingsFeedback>
		</main>
	);
}
