export type PaletteSessionItem = {
	id: string;
	title: string;
	updatedAt?: string;
};

export type PaletteMode = "commands" | "quick_open";

export type PaletteIcon =
	| "download"
	| "git-branch"
	| "minimize-2"
	| "git-fork"
	| "square-pen"
	| "history"
	| "panel-left"
	| "settings"
	| "message-square"
	| "image"
	| "file-code";

export type PaletteTarget =
	| { kind: "slash_export" }
	| { kind: "slash_tree" }
	| { kind: "slash_compact" }
	| { kind: "slash_fork" }
	| { kind: "new_session" }
	| { kind: "all_sessions" }
	| { kind: "toggle_sidebar" }
	| { kind: "open_settings" }
	| { kind: "open_session"; sessionId: string }
	| { kind: "open_export"; exportId: string };

export type RecentExportSummary = {
	id: string;
	sessionId: string;
	title: string;
	capturedAt: string;
	options?: {
		format?: string;
		theme?: string;
		includeThinking?: boolean;
		includeTools?: boolean;
	};
	status: string;
	directory?: string;
	files: { name: string; bytes: number; url: string }[];
};

export type PaletteEntry = {
	id: string;
	group: string;
	title: string;
	commandCol?: boolean;
	subtitle?: string;
	trailingKbd?: string;
	trailingMeta?: string;
	icon: PaletteIcon;
	disabled: boolean;
	target: PaletteTarget;
	haystack: string;
};

export function palettePlaceholder(mode: PaletteMode): string {
	return mode === "commands" ? "搜索命令或导航操作…" : "搜索历史会话或近期导出…";
}

export function paletteDialogLabel(mode: PaletteMode): string {
	return mode === "commands" ? "命令面板" : "快速打开面板";
}

export function paletteEmptyLabel(mode: PaletteMode): string {
	return mode === "commands" ? "未找到匹配的命令或导航操作" : "未找到匹配的会话或近期导出";
}

/** 从路由路径 `/s/:sessionId/...` 提取当前会话 ID。 */
export function extractActiveSessionId(pathname: string): string | undefined {
	const match = /^\/s\/([^/]+)/.exec(pathname);
	if (!match?.[1]) return undefined;
	try {
		return decodeURIComponent(match[1]);
	} catch {
		return match[1];
	}
}

export function formatExportBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
	const kb = 1024;
	const mb = kb * 1024;
	if (bytes >= mb) return `${(bytes / mb).toFixed(1)} MB`;
	if (bytes >= kb) return `${Math.round(bytes / kb)} KB`;
	return `${Math.round(bytes)} B`;
}

export function formatPaletteTime(raw: string | undefined, now: Date = new Date()): string | undefined {
	const trimmed = raw?.trim();
	if (!trimmed) return undefined;
	if (/^\d{4}-\d{2}-\d{2}$/.test(trimmed)) return trimmed.replaceAll("-", "/");
	const parsed = new Date(trimmed);
	if (!Number.isNaN(parsed.getTime())) {
		const sameDay =
			parsed.getFullYear() === now.getFullYear() &&
			parsed.getMonth() === now.getMonth() &&
			parsed.getDate() === now.getDate();
		if (sameDay) {
			const hh = String(parsed.getHours()).padStart(2, "0");
			const mm = String(parsed.getMinutes()).padStart(2, "0");
			return `${hh}:${mm}`;
		}
		const yyyy = String(parsed.getFullYear()).padStart(4, "0");
		const mo = String(parsed.getMonth() + 1).padStart(2, "0");
		const dd = String(parsed.getDate()).padStart(2, "0");
		return `${yyyy}/${mo}/${dd}`;
	}
	if (trimmed.length >= 10) return trimmed.slice(0, 10).replaceAll("-", "/");
	return trimmed;
}

export function buildCommandEntries(hasActiveSession: boolean): PaletteEntry[] {
	const slashDisabled = !hasActiveSession;
	const commands: {
		id: string;
		cmd: string;
		desc: string;
		keywords: string[];
		icon: PaletteIcon;
		target: PaletteTarget;
	}[] = [
		{
			id: "cmd-export",
			cmd: "export",
			desc: "导出当前诊断报告",
			keywords: ["/export", "export", "导出", "png", "html"],
			icon: "download",
			target: { kind: "slash_export" },
		},
		{
			id: "cmd-tree",
			cmd: "tree",
			desc: "查看会话分支树与节点总结",
			keywords: ["/tree", "tree", "分支树", "分支", "导航"],
			icon: "git-branch",
			target: { kind: "slash_tree" },
		},
		{
			id: "cmd-compact",
			cmd: "compact",
			desc: "压缩早期上下文并保留关键结论",
			keywords: ["/compact", "compact", "压缩", "上下文"],
			icon: "minimize-2",
			target: { kind: "slash_compact" },
		},
		{
			id: "cmd-fork",
			cmd: "fork",
			desc: "从当前对话派生新会话",
			keywords: ["/fork", "fork", "派生", "分支会话"],
			icon: "git-fork",
			target: { kind: "slash_fork" },
		},
	];

	const navigation: {
		id: string;
		title: string;
		kbd?: string;
		keywords: string[];
		icon: PaletteIcon;
		target: PaletteTarget;
	}[] = [
		{
			id: "nav-new",
			title: "新建对话",
			kbd: "Alt+N",
			keywords: ["new", "新建", "对话", "主页", "home", "alt+n"],
			icon: "square-pen",
			target: { kind: "new_session" },
		},
		{
			id: "nav-sessions",
			title: "打开全部会话",
			keywords: ["sessions", "all", "全部会话", "会话列表", "历史"],
			icon: "history",
			target: { kind: "all_sessions" },
		},
		{
			id: "nav-sidebar",
			title: "切换侧边栏显示",
			kbd: "Ctrl+B",
			keywords: ["sidebar", "侧边栏", "导航栏", "折叠", "展开", "ctrl+b"],
			icon: "panel-left",
			target: { kind: "toggle_sidebar" },
		},
		{
			id: "nav-settings",
			title: "打开设置",
			kbd: "Ctrl+,",
			keywords: ["settings", "设置", "常规", "模型", "提供商", "密钥", "ctrl+,"],
			icon: "settings",
			target: { kind: "open_settings" },
		},
	];

	return [
		...commands.map((item) => ({
			id: item.id,
			group: "当前会话命令",
			title: item.cmd,
			commandCol: true,
			subtitle: item.desc,
			icon: item.icon,
			disabled: slashDisabled,
			target: item.target,
			haystack: `${item.cmd} ${item.desc} ${item.keywords.join(" ")}`.toLowerCase(),
		})),
		...navigation.map((item) => ({
			id: item.id,
			group: "常见导航",
			title: item.title,
			trailingKbd: item.kbd,
			icon: item.icon,
			disabled: false,
			target: item.target,
			haystack: `${item.title} ${item.kbd ?? ""} ${item.keywords.join(" ")}`.toLowerCase(),
		})),
	];
}

export function buildQuickOpenEntries(
	sessions: readonly PaletteSessionItem[],
	activeSessionId: string | undefined,
	recentExports: readonly RecentExportSummary[],
	now: Date = new Date(),
): PaletteEntry[] {
	const sortedSessions = [...sessions].sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""));
	const sortedExports = recentExports
		.filter((item) => item.status === "completed")
		.sort((a, b) => (b.capturedAt ?? "").localeCompare(a.capturedAt ?? ""));

	const sessionEntries: PaletteEntry[] = sortedSessions.map((session) => {
		const title = session.title.trim() || "新对话";
		const isCurrent = activeSessionId !== undefined && activeSessionId === session.id;
		return {
			id: `sess-${session.id}`,
			group: "指定会话",
			title,
			subtitle: isCurrent ? "当前会话" : undefined,
			trailingMeta: formatPaletteTime(session.updatedAt, now),
			icon: "message-square",
			disabled: false,
			target: { kind: "open_session", sessionId: session.id },
			haystack: `${title} ${session.id} 会话 session`.toLowerCase(),
		};
	});

	const exportEntries: PaletteEntry[] = sortedExports.map((exp) => {
		const isPng = (exp.options?.format ?? "").toLowerCase() === "png";
		const title = exp.title.trim() || exp.files[0]?.name || "会话导出";
		const totalBytes = exp.files.reduce((sum, file) => sum + (file.bytes || 0), 0);
		const subtitle = isPng ? `PNG · ${Math.max(1, exp.files.length)} 页` : `HTML · ${formatExportBytes(totalBytes)}`;
		const fileNames = exp.files.map((file) => file.name).join(" ");
		return {
			id: `exp-${exp.id}`,
			group: "近期导出",
			title,
			subtitle,
			trailingMeta: formatPaletteTime(exp.capturedAt, now),
			icon: isPng ? "image" : "file-code",
			disabled: false,
			target: { kind: "open_export", exportId: exp.id },
			haystack: `${title} ${fileNames} ${isPng ? "png 图片" : "html 网页"} 导出 export`.toLowerCase(),
		};
	});

	return [...sessionEntries, ...exportEntries];
}

export function filterPaletteEntries(entries: readonly PaletteEntry[], query: string): PaletteEntry[] {
	const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
	if (!tokens.length) return [...entries];
	return entries.filter((entry) => tokens.every((token) => entry.haystack.includes(token)));
}

export function findFirstEnabledIndex(entries: readonly PaletteEntry[]): number {
	return entries.findIndex((entry) => !entry.disabled);
}

export function findLastEnabledIndex(entries: readonly PaletteEntry[]): number {
	for (let i = entries.length - 1; i >= 0; i--) {
		if (!entries[i]?.disabled) return i;
	}
	return -1;
}

export function stepEnabledIndex(entries: readonly PaletteEntry[], current: number, delta: 1 | -1): number {
	const len = entries.length;
	if (len === 0 || !entries.some((entry) => !entry.disabled)) return -1;
	let index = current >= 0 && current < len ? current : delta === 1 ? len - 1 : 0;
	for (let step = 0; step < len; step++) {
		index = (index + delta + len) % len;
		if (!entries[index]?.disabled) return index;
	}
	return -1;
}

/** 按稳定条目 ID 解析活动下标，列表后台重排时不偏移至其他项；ID 消失时回退首项可用条目。 */
export function resolveActiveIndex(entries: readonly PaletteEntry[], selectedId: string | null): number {
	if (!entries.length) return -1;
	if (selectedId) {
		const idx = entries.findIndex((entry) => entry.id === selectedId && !entry.disabled);
		if (idx >= 0) return idx;
	}
	return findFirstEnabledIndex(entries);
}

/** 判断面板操作是否实际改变当前路由；同页操作返回 false，关闭面板后归还原焦点。 */
export function willPaletteTargetChangeRoute(
	target: PaletteTarget,
	currentPathname: string,
	activeSessionId: string | undefined,
): boolean {
	switch (target.kind) {
		case "new_session":
			return currentPathname !== "/";
		case "all_sessions":
			return currentPathname !== "/sessions";
		case "open_settings":
			return currentPathname !== "/settings";
		case "open_session":
			return currentPathname !== `/s/${encodeURIComponent(target.sessionId)}`;
		case "slash_export":
			return Boolean(activeSessionId) && currentPathname !== `/s/${encodeURIComponent(activeSessionId!)}/export`;
		case "slash_tree":
			return Boolean(activeSessionId) && currentPathname !== `/s/${encodeURIComponent(activeSessionId!)}/tree`;
		case "slash_fork":
		case "slash_compact":
		case "toggle_sidebar":
		case "open_export":
			return false;
	}
}
