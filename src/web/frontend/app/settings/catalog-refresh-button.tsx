import { RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { refreshModelCatalog } from "../data/api";
import { useSettingsFeedback } from "./settings-frame";
import { invalidateSetting, preloadSettings } from "./settings-resource-cache";

export function CatalogRefreshButton() {
	const feedback = useSettingsFeedback();
	const [working, setWorking] = useState(false);
	const pending = useRef(false);
	const mounted = useRef(false);
	useEffect(() => {
		mounted.current = true;
		return () => {
			mounted.current = false;
		};
	}, []);
	async function refresh() {
		if (pending.current) return;
		pending.current = true;
		setWorking(true);
		try {
			const result = await refreshModelCatalog();
			for (const path of ["/api/models", "/api/auth"]) invalidateSetting(path);
			void preloadSettings(["/api/models", "/api/auth"]);
			if (!mounted.current) return;
			if (result.aborted || result.failedProviders.length)
				feedback("部分模型目录未能更新，已保留可用目录。请稍后重试。", "warning");
			else feedback("模型目录已更新");
		} catch (cause) {
			if (mounted.current)
				feedback(cause instanceof Error ? cause.message : "模型目录更新失败，请重试。", "warning");
		} finally {
			pending.current = false;
			if (mounted.current) setWorking(false);
		}
	}
	return (
		<button
			type="button"
			className="settings-action settings-catalog-refresh"
			disabled={working}
			aria-busy={working}
			onClick={() => void refresh()}
		>
			<RefreshCw size={14} aria-hidden="true" />
			{working ? "更新中…" : "更新模型"}
		</button>
	);
}
