import {
	Download,
	FileCode,
	GitBranch,
	GitFork,
	History,
	Image,
	MessageSquare,
	Minimize2,
	PanelLeft,
	Search,
	Settings,
	SquarePen,
} from "lucide-react";
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
	buildCommandEntries,
	buildQuickOpenEntries,
	filterPaletteEntries,
	findFirstEnabledIndex,
	findLastEnabledIndex,
	type PaletteEntry,
	type PaletteIcon,
	type PaletteMode,
	type PaletteSessionItem,
	type PaletteTarget,
	paletteDialogLabel,
	paletteEmptyLabel,
	palettePlaceholder,
	type RecentExportSummary,
	resolveActiveIndex,
	stepEnabledIndex,
} from "./palette-model";

const ICONS: Record<PaletteIcon, typeof Search> = {
	download: Download,
	"git-branch": GitBranch,
	"minimize-2": Minimize2,
	"git-fork": GitFork,
	"square-pen": SquarePen,
	history: History,
	"panel-left": PanelLeft,
	settings: Settings,
	"message-square": MessageSquare,
	image: Image,
	"file-code": FileCode,
};

export function CommandPalette({
	mode,
	sessions,
	activeSessionId,
	hasActiveSession,
	recentExports,
	onExecute,
	onDismiss,
}: {
	mode: PaletteMode;
	sessions: readonly PaletteSessionItem[];
	activeSessionId: string | undefined;
	hasActiveSession: boolean;
	recentExports: readonly RecentExportSummary[];
	onExecute: (target: PaletteTarget) => void;
	onDismiss: () => void;
}) {
	const [query, setQuery] = useState("");
	const [selectedId, setSelectedId] = useState<string | null>(null);
	const inputRef = useRef<HTMLInputElement>(null);
	const listRef = useRef<HTMLDivElement>(null);
	const rowRefs = useRef(new Map<string, HTMLButtonElement>());
	const scrollByKeyboardRef = useRef(false);
	const prevModeRef = useRef(mode);

	// 切换模式时在绘制前清空查询、重置滚动并保持搜索框聚焦，避免闪烁 1 帧旧过滤结果。
	useLayoutEffect(() => {
		if (prevModeRef.current === mode) return;
		prevModeRef.current = mode;
		setQuery("");
		setSelectedId(null);
		if (listRef.current) listRef.current.scrollTop = 0;
		inputRef.current?.focus({ preventScroll: true });
	}, [mode]);

	// 锁定焦点在搜索输入框（Tab / Shift+Tab 不逃出模态框）；关闭后的焦点归还由外壳统一管理，避免 StrictMode 重复清理丢失原焦点。
	useEffect(() => {
		inputRef.current?.focus({ preventScroll: true });
		const onTabCapture = (event: KeyboardEvent) => {
			if (
				event.key !== "Tab" ||
				event.ctrlKey ||
				event.altKey ||
				event.metaKey ||
				event.isComposing ||
				event.keyCode === 229
			)
				return;
			event.preventDefault();
			inputRef.current?.focus({ preventScroll: true });
		};
		window.addEventListener("keydown", onTabCapture, true);
		return () => {
			window.removeEventListener("keydown", onTabCapture, true);
		};
	}, []);

	const allEntries = useMemo(
		() =>
			mode === "commands"
				? buildCommandEntries(hasActiveSession)
				: buildQuickOpenEntries(sessions, activeSessionId, recentExports),
		[mode, hasActiveSession, sessions, activeSessionId, recentExports],
	);
	const entries = useMemo(() => filterPaletteEntries(allEntries, query), [allEntries, query]);

	const groups = useMemo(() => {
		const grouped: { group: string; items: { entry: PaletteEntry; index: number }[] }[] = [];
		for (let index = 0; index < entries.length; index++) {
			const entry = entries[index];
			if (!entry) continue;
			const last = grouped.at(-1);
			if (last && last.group === entry.group) {
				last.items.push({ entry, index });
			} else {
				grouped.push({ group: entry.group, items: [{ entry, index }] });
			}
		}
		return grouped;
	}, [entries]);

	const activeIdx = useMemo(() => resolveActiveIndex(entries, selectedId), [entries, selectedId]);
	const resolvedActiveId = activeIdx >= 0 ? (entries[activeIdx]?.id ?? null) : null;

	// 将初始默认高亮与活动项消失后的回退结果同步持久化为 ID，避免后台重排列表改变当前选中目标。
	useEffect(() => {
		if (resolvedActiveId !== null && selectedId !== resolvedActiveId) {
			setSelectedId(resolvedActiveId);
		}
	}, [resolvedActiveId, selectedId]);

	// 仅键盘移动选中行时滚动可见，鼠标悬停边缘行不抖动列表。
	useEffect(() => {
		if (!scrollByKeyboardRef.current) return;
		scrollByKeyboardRef.current = false;
		if (activeIdx < 0) return;
		const id = entries[activeIdx]?.id;
		if (id) rowRefs.current.get(id)?.scrollIntoView({ block: "nearest" });
	}, [activeIdx, entries]);

	function updateQuery(next: string) {
		setQuery(next);
		setSelectedId(null);
		if (listRef.current) listRef.current.scrollTop = 0;
	}

	function runEntry(target: PaletteTarget) {
		onExecute(target);
	}

	function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
		if (event.nativeEvent.isComposing || event.keyCode === 229) return;
		if (event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) return;
		switch (event.key) {
			case "ArrowDown": {
				event.preventDefault();
				const next = stepEnabledIndex(entries, activeIdx, 1);
				const entry = next >= 0 ? entries[next] : undefined;
				if (entry) {
					scrollByKeyboardRef.current = true;
					setSelectedId(entry.id);
				}
				break;
			}
			case "ArrowUp": {
				event.preventDefault();
				const next = stepEnabledIndex(entries, activeIdx, -1);
				const entry = next >= 0 ? entries[next] : undefined;
				if (entry) {
					scrollByKeyboardRef.current = true;
					setSelectedId(entry.id);
				}
				break;
			}
			case "Home": {
				event.preventDefault();
				const first = findFirstEnabledIndex(entries);
				const entry = first >= 0 ? entries[first] : undefined;
				if (entry) {
					scrollByKeyboardRef.current = true;
					setSelectedId(entry.id);
				}
				break;
			}
			case "End": {
				event.preventDefault();
				const last = findLastEnabledIndex(entries);
				const entry = last >= 0 ? entries[last] : undefined;
				if (entry) {
					scrollByKeyboardRef.current = true;
					setSelectedId(entry.id);
				}
				break;
			}
			case "Enter": {
				if (event.repeat) return;
				event.preventDefault();
				const entry = activeIdx >= 0 ? entries[activeIdx] : undefined;
				if (entry && !entry.disabled) runEntry(entry.target);
				break;
			}
			case "Escape": {
				if (event.repeat) return;
				event.preventDefault();
				event.stopPropagation();
				if (query) {
					updateQuery("");
				} else {
					onDismiss();
				}
				break;
			}
		}
	}

	const hasResults = groups.length > 0;
	const activeOptionId = activeIdx >= 0 && entries[activeIdx] ? `palette-opt-${entries[activeIdx].id}` : undefined;

	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: 点击遮罩关闭快捷面板，由 Esc 键同步覆盖。
		<div
			className="palette-scrim tier-3-scrim"
			onMouseDown={(event) => {
				if (event.target === event.currentTarget) onDismiss();
			}}
		>
			<div
				className="palette-modal tier-3-dialog"
				role="dialog"
				aria-modal="true"
				aria-label={paletteDialogLabel(mode)}
				data-mode={mode}
				tabIndex={-1}
				onKeyDown={handleKeyDown}
			>
				<div className="palette-header">
					<Search className="palette-search-icon" size={16} aria-hidden="true" />
					<input
						ref={inputRef}
						type="text"
						className="palette-input"
						role="combobox"
						aria-expanded={hasResults}
						aria-autocomplete="list"
						aria-controls={hasResults ? "command-palette-list" : undefined}
						aria-activedescendant={hasResults ? activeOptionId : undefined}
						aria-label={palettePlaceholder(mode)}
						placeholder={palettePlaceholder(mode)}
						value={query}
						onChange={(event) => updateQuery(event.target.value)}
					/>
					<kbd className="palette-kbd">Esc</kbd>
				</div>
				{hasResults ? (
					<div
						ref={listRef}
						id="command-palette-list"
						className="palette-list"
						role="listbox"
						aria-label={paletteDialogLabel(mode)}
					>
						{groups.map(({ group, items }, groupIndex) => {
							const groupTitleId = `palette-group-${mode}-${groupIndex}`;
							return (
								<Fragment key={group}>
									{groupIndex > 0 && <div className="palette-sep" role="presentation" />}
									{/* biome-ignore lint/a11y/useSemanticElements: WAI-ARIA listbox 分组规范要求 role="group"，而非表单 <fieldset> */}
									<div className="palette-group" role="group" aria-labelledby={groupTitleId}>
										<div id={groupTitleId} role="presentation" className="palette-group-title">
											{group}
										</div>
										{items.map(({ entry, index }) => {
											const isSelected = index === activeIdx && !entry.disabled;
											const Icon = ICONS[entry.icon];
											return (
												<button
													key={entry.id}
													ref={(node) => {
														if (node) rowRefs.current.set(entry.id, node);
														else rowRefs.current.delete(entry.id);
													}}
													id={`palette-opt-${entry.id}`}
													type="button"
													role="option"
													tabIndex={-1}
													className="palette-row"
													aria-selected={isSelected}
													aria-disabled={entry.disabled || undefined}
													data-selected={isSelected ? "true" : undefined}
													onMouseDown={(event) => event.preventDefault()}
													onMouseMove={() => {
														if (!entry.disabled && selectedId !== entry.id) setSelectedId(entry.id);
													}}
													onClick={() => {
														if (!entry.disabled) runEntry(entry.target);
													}}
												>
													<span className="palette-row-left">
														<Icon className="palette-row-icon" size={14} aria-hidden="true" />
														<span
															className={`palette-row-title${entry.commandCol ? " palette-row-title--cmd" : ""}`}
														>
															{entry.title}
														</span>
														{entry.subtitle && (
															<span className="palette-row-subtitle">{entry.subtitle}</span>
														)}
													</span>
													{entry.trailingKbd ? (
														<kbd className="palette-kbd">{entry.trailingKbd}</kbd>
													) : entry.trailingMeta ? (
														<span className="palette-row-meta">{entry.trailingMeta}</span>
													) : null}
												</button>
											);
										})}
									</div>
								</Fragment>
							);
						})}
					</div>
				) : (
					<output className="palette-empty">{paletteEmptyLabel(mode)}</output>
				)}
			</div>
		</div>
	);
}
