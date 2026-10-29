import "../settings/settings.css";
import { Settings as SettingsIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { Outlet, useLocation } from "react-router";
import { useGlobalEvents } from "../data/web-state";
import { SettingsFeedback, SettingsLink } from "../settings/settings-frame";
import { invalidateSetting, preloadSettings, settingsPaths } from "../settings/settings-resource-cache";

const tabs = [
	{ to: "/settings/general", label: "常规" },
	{ to: "/settings", label: "模型服务" },
	{ to: "/settings/models", label: "模型偏好" },
	{ to: "/settings/shortcuts", label: "快捷键" },
];
export default function Settings() {
	const { pathname } = useLocation();
	const activePath = ["/settings/add", "/settings/provider", "/settings/custom"].includes(pathname)
		? "/settings"
		: pathname;
	const index = tabs.findIndex((tab) => tab.to === activePath);
	const [navigation, setNavigation] = useState({ pathname, index, direction: 0 });
	if (navigation.pathname !== pathname) {
		setNavigation({
			pathname,
			index,
			direction: index < 0 || navigation.index < 0 ? 0 : Math.sign(index - navigation.index),
		});
	}
	useEffect(() => {
		void preloadSettings();
		return () => {
			// TUI 可能在 Web 设置页关闭期间更改共享文件；下次进入重新读取。
			for (const path of settingsPaths) invalidateSetting(path);
		};
	}, []);
	useGlobalEvents((name, raw) => {
		let paths: readonly string[] = settingsPaths;
		if (name === "state") {
			try {
				const type = JSON.parse(raw?.data ?? "null")?.type;
				if (type === "models_changed") paths = ["/api/models"];
				else if (type !== "auth_changed" && type !== "settings_changed") return;
			} catch {
				return;
			}
		} else if (name !== "open" && name !== "reset") return;
		for (const path of paths) invalidateSetting(path);
		void preloadSettings(paths);
	});
	return (
		<main
			className="chat-main settings-main settings-workspace"
			data-settings-slide={
				navigation.direction === 0 ? undefined : navigation.direction > 0 ? "forward" : "backward"
			}
		>
			<header className="chat-header settings-header">
				<SettingsIcon size={22} aria-hidden="true" />
				<h1>设置</h1>
			</header>
			<SettingsFeedback>
				<div className="settings-scroll">
					<div className="settings-content settings-pages">
						<nav className="settings-nav" aria-label="设置分类">
							{tabs.map((tab) => (
								<SettingsLink
									key={tab.to}
									to={tab.to}
									aria-current={activePath === tab.to ? "page" : undefined}
									className={`settings-nav-link${activePath === tab.to ? " is-current" : ""}`}
									prefetch="render"
								>
									{tab.label}
									{activePath === tab.to && <span className="settings-nav-indicator" aria-hidden="true" />}
								</SettingsLink>
							))}
						</nav>
						<div className="settings-route" key={pathname}>
							<Outlet />
						</div>
					</div>
				</div>
			</SettingsFeedback>
		</main>
	);
}
