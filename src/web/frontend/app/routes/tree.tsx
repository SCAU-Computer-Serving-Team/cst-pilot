import { Label, ListBox, Select } from "@heroui/react";
import {
	Bot,
	Check,
	ChevronDown,
	ChevronsDownUp,
	ChevronsUpDown,
	CornerUpLeft,
	GitBranch,
	HardDrive,
	Info,
	Minimize2,
	PencilLine,
	Search,
	Sparkles,
	User,
	Wrench,
	X,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams, useSearchParams } from "react-router";
import { draftKey } from "../composer/drafts";
import { apiJson, sessionPath } from "../data/api";
import { useSessionTitle, useSessionTree } from "../data/web-state";
import { focusableElements, resolveDialogTabTarget } from "../shell/dialog-focus";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { isNavigationKeyEvent, scrollRowIntoView } from "../shell/keyboard-navigation";
import { Notification } from "../shell/notification";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { SidebarHeaderToggle, useWorkspaceShell } from "../shell/workspace-shell-context";
import { branchSummaryHref } from "../tree/branch-summary";
import {
	applyFold,
	buildVisibleTree,
	collectToolCalls,
	foldable,
	layoutTree,
	loadViewFilter,
	matchesSearch,
	type RowText,
	rowText,
	saveViewFilter,
	slotMark,
	type TreeRow,
	VIEW_FILTERS,
	type ViewFilter,
	visibleTreeCursor,
} from "../tree/branch-tree";
import { sampleTree } from "../tree/branch-tree-sample";

const kindIcon = { user: User, assistant: Bot, tool: Wrench, info: Minimize2 } as const;

function RowIcon({ kind, summary }: { kind: RowText["kind"]; summary: boolean }) {
	const Icon = summary ? GitBranch : kindIcon[kind];
	return <Icon size={16} aria-hidden="true" className={`tree-icon tree-icon-${summary ? "summary" : kind}`} />;
}

/** 行前缀：每个缩进列一格，连接符列画方块，其余按 gutter 画竖线或留空。 */
function RowPrefix({ row }: { row: TreeRow }) {
	return (
		<>
			{Array.from({ length: row.displayIndent }, (_, column) => {
				const mark = slotMark(row, column);
				return (
					<span key={`${column}-${mark.kind}`} className="tree-slot" aria-hidden="true">
						{mark.kind === "square" && <span className="tree-square" />}
						{mark.kind === "line" && <span className="tree-line" />}
					</span>
				);
			})}
		</>
	);
}

export default function SessionTree() {
	const { sessionId } = useParams();
	return <SessionTreeView key={sessionId} sessionId={sessionId} />;
}

function SessionTreeView({ sessionId }: { sessionId?: string }) {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const preview = params.get("preview") === "1";
	const focusParam = params.get("focus")?.trim() || null;
	const focusKey = useLocation().key;
	const sample = useMemo(() => (preview ? sampleTree() : undefined), [preview]);
	const live = useSessionTree(preview ? undefined : sessionId);
	const tree = sample?.tree ?? live.tree;
	const leafId = sample?.leafId ?? live.leafId;
	const error = live.error;
	const refresh = live.refresh;
	const title = useSessionTitle(sessionId, preview ? "checkpoint4 UI 精修" : "会话", !preview);
	const [query, setQuery] = useState("");
	const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());
	const [filter, setFilter] = useState<ViewFilter>(() => (focusParam ? "all" : loadViewFilter()));
	// 视图过滤下拉弹层打开中：Esc 先关弹层，不退出树页。
	const [filterOpen, setFilterOpen] = useState(false);
	const filterTriggerRef = useRef<HTMLButtonElement>(null);
	usePanelDismiss(filterOpen, () => setFilterOpen(false), undefined, filterTriggerRef);
	const [choice, setChoice] = useState("");
	// 键盘光标在三个选项间的位置；对话框打开时重置。
	const [choiceIndex, setChoiceIndex] = useState(0);
	const choiceDialogRef = useRef<HTMLDivElement>(null);
	const shell = useWorkspaceShell();
	useEffect(() => {
		if (!shell?.appActive) return;
		setChoice("");
		setFilterOpen(false);
	}, [shell?.appActive]);
	const [toast, setToast] = useState("");
	const [actionError, setActionError] = useState("");
	// 未指定条目时跟随当前叶；带 ?focus= 时自动定位目标节点。
	const [cursorId, setCursorId] = useState<string | null>(() => focusParam);
	const [kbdActive, setKbdActive] = useState(() => Boolean(focusParam));
	const appliedFocusRef = useRef<string | null>(null);
	useEffect(() => {
		if (!focusParam || appliedFocusRef.current === focusKey) return;
		appliedFocusRef.current = focusKey;
		setQuery("");
		setFilter("all");
		setFolded(new Set());
		setCursorId(focusParam);
		setKbdActive(true);
	}, [focusKey, focusParam]);
	const clearKeyboard = useCallback(() => setKbdActive(false), []);
	const kbdStyle = useKeyboardFocusStyle();
	const listRef = useRef<HTMLUListElement>(null);
	const rowRefs = useRef(new Map<string, HTMLLIElement>());
	const mounted = useRef(false);
	const initialLeafScrolled = useRef(false);
	useLayoutEffect(() => {
		mounted.current = true;
		if (!preview) listRef.current?.focus({ preventScroll: true });
		return () => {
			mounted.current = false;
		};
	}, [preview]);
	// 选中一行：当前叶位置不变时只给消息弹窗，否则打开选择对话框。
	const selectRow = useCallback(
		(id: string) => {
			if (preview) return;
			if (id === leafId) setToast("你已经在本消息处");
			else setChoice(id);
		},
		[preview, leafId],
	);
	const jump = useCallback(
		async (entryId: string) => {
			if (!sessionId || preview) return;
			setActionError("");
			try {
				const result = await apiJson<{ editorText?: string }>(sessionPath(sessionId, "/tree/navigate"), {
					method: "POST",
					body: { entryId },
				});
				if (result.editorText) sessionStorage.setItem(draftKey(sessionId), result.editorText);
				if (mounted.current) navigate(`/s/${sessionId}`, { viewTransition: true });
			} catch (cause) {
				if (!mounted.current) return;
				setActionError(cause instanceof Error ? cause.message : "分支导航未完成");
				await refresh();
			}
		},
		[sessionId, preview, navigate, refresh],
	);
	/** 三种选择：不总结在这里直接导航；总结与自定义提示词把目标条目交给聊天页，导航与总结必须同一次调用。 */
	const choose = useCallback(
		async (mode: "none" | "summarize" | "custom") => {
			if (!sessionId || !choice) return;
			const entryId = choice;
			setChoice("");
			if (mode === "none") await jump(entryId);
			else navigate(branchSummaryHref(sessionId, entryId, mode), { viewTransition: true });
		},
		[sessionId, choice, jump, navigate],
	);
	useEffect(() => {
		if (choice) setChoiceIndex(0);
	}, [choice]);
	// 选择对话框打开时记录焦点并在关闭后归还；箭头键移动光标不重置焦点。
	const choiceIndexRef = useRef(choiceIndex);
	choiceIndexRef.current = choiceIndex;
	const chooseRef = useRef(choose);
	chooseRef.current = choose;
	useEffect(() => {
		if (!choice) return;
		const previous = document.activeElement;
		const list = listRef.current;
		const frame = requestAnimationFrame(() => {
			choiceDialogRef.current?.querySelector<HTMLElement>(".summary-option")?.focus({ preventScroll: true });
		});
		const onKey = (event: KeyboardEvent) => {
			const dialog = choiceDialogRef.current;
			if (
				!dialog ||
				dialog.closest("[inert]") ||
				event.defaultPrevented ||
				event.isComposing ||
				event.keyCode === 229
			)
				return;
			if (event.key === "Escape") {
				event.preventDefault();
				setChoice("");
				return;
			}
			if (event.key === "Tab" && !event.ctrlKey && !event.altKey && !event.metaKey) {
				const items = focusableElements(dialog);
				const active =
					document.activeElement instanceof HTMLElement && dialog.contains(document.activeElement)
						? document.activeElement
						: null;
				const target = resolveDialogTabTarget(items, active, event.shiftKey);
				if (target) {
					event.preventDefault();
					if (target === "dialog") dialog.focus();
					else target.focus();
				}
				return;
			}
			if (!isNavigationKeyEvent(event)) return;
			if (event.key === "ArrowUp" || event.key === "ArrowDown") {
				const options = [...dialog.querySelectorAll<HTMLButtonElement>(".summary-option")];
				if (options.length === 0) return;
				event.preventDefault();
				const active = document.activeElement;
				const focused = active instanceof HTMLButtonElement ? options.indexOf(active) : -1;
				const from = focused >= 0 ? focused : choiceIndexRef.current;
				const step = event.key === "ArrowUp" ? -1 : 1;
				const next = (from + step + options.length) % options.length;
				setChoiceIndex(next);
				options[next]?.focus({ preventScroll: true });
				return;
			}
			if (event.key === "Enter") {
				if (document.activeElement instanceof HTMLButtonElement && dialog.contains(document.activeElement)) {
					return;
				}
				event.preventDefault();
				const mode = (["none", "summarize", "custom"] as const)[choiceIndexRef.current];
				void chooseRef.current(mode);
			}
		};
		window.addEventListener("keydown", onKey, true);
		return () => {
			cancelAnimationFrame(frame);
			window.removeEventListener("keydown", onKey, true);
			if (document.querySelector(".standby-scrim")) return;
			if (
				previous instanceof HTMLElement &&
				previous !== document.body &&
				previous.isConnected &&
				!previous.closest("[inert]")
			) {
				previous.focus({ preventScroll: true });
			} else if (list?.isConnected && !list.closest("[inert]")) {
				list.focus({ preventScroll: true });
			}
		};
	}, [choice]);
	const toolCalls = useMemo(() => collectToolCalls(tree ?? []), [tree]);
	const allRows = useMemo(
		() => layoutTree(buildVisibleTree(tree ?? [], leafId, filter), leafId),
		[tree, leafId, filter],
	);
	// 行文案预计算：搜索过滤与渲染复用，避免每次输入都重算整棵树的文案。
	const rowTexts = useMemo(
		() => new Map(allRows.map((row) => [row.node.entry.id, rowText(row.node, toolCalls)])),
		[allRows, toolCalls],
	);
	const foldResult = useMemo(() => applyFold(allRows, folded), [allRows, folded]);
	const rows = useMemo(() => {
		if (!query.trim()) return foldResult.visible;
		return allRows.filter((row) => matchesSearch(row.node, rowTexts.get(row.node.entry.id)!, query));
	}, [allRows, foldResult, query, rowTexts]);
	const leafIndex = useMemo(() => rows.findIndex((row) => row.node.entry.id === leafId), [rows, leafId]);
	const resolvedCursor = visibleTreeCursor(allRows, rows, cursorId ?? leafId);
	const cursorIndex = resolvedCursor >= 0 ? resolvedCursor : leafIndex >= 0 ? leafIndex : rows.length - 1;
	const overlayBlocked = !!choice || filterOpen || !!shell?.appActive || !!shell?.overlay;
	const targetId = kbdActive && !overlayBlocked ? (rows[cursorIndex]?.node.entry.id ?? null) : null;
	usePointerMutex(targetId !== null, clearKeyboard);
	const [narrowViewport, setNarrowViewport] = useState(
		() =>
			typeof window !== "undefined" &&
			typeof window.matchMedia === "function" &&
			window.matchMedia("(max-width: 960px)").matches,
	);
	useEffect(() => {
		if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
		const mq = window.matchMedia("(max-width: 960px)");
		const sync = () => setNarrowViewport(mq.matches);
		sync();
		mq.addEventListener("change", sync);
		return () => mq.removeEventListener("change", sync);
	}, []);

	const moveTo = useCallback((row: TreeRow) => {
		listRef.current?.focus({ preventScroll: true });
		setCursorId(row.node.entry.id);
		setKbdActive(true);
	}, []);

	// Esc 返回会话；方向键、Home/End 与 Enter 收敛在树列表内部，避免拦截按钮、弹层或待机层。
	useEffect(() => {
		if (preview || overlayBlocked) return;
		const onKey = (event: KeyboardEvent) => {
			if (
				event.defaultPrevented ||
				event.isComposing ||
				event.keyCode === 229 ||
				document.querySelector(
					'.palette-scrim, .standby-scrim, .summary-scrim, dialog[open], .session-menu, .popover:not([data-exiting="true"]), .select__popover:not([data-exiting="true"]), [data-slot="popover-dialog"]',
				)
			) {
				return;
			}
			const target = event.target;
			const list = listRef.current;
			if (!(target instanceof HTMLElement) || !list) return;
			if (target.isContentEditable || target.matches("input, textarea, select")) return;
			if (event.key === "Escape") {
				event.preventDefault();
				navigate(`/s/${sessionId}`, { viewTransition: true });
				return;
			}
			const row = rows[cursorIndex];
			const fromList =
				target === document.body || target === list || (list.contains(target) && target.matches(".tree-row"));
			if (!fromList || !row || !isNavigationKeyEvent(event)) return;
			const id = row.node.entry.id;
			switch (event.key) {
				case "ArrowUp":
				case "ArrowDown": {
					if (!kbdActive) {
						const initial = cursorId === null && leafIndex < 0 ? 0 : cursorIndex;
						moveTo(rows[Math.max(0, Math.min(rows.length - 1, initial))]);
						break;
					}
					const step = event.key === "ArrowUp" ? -1 : 1;
					moveTo(rows[Math.max(0, Math.min(rows.length - 1, cursorIndex + step))]);
					break;
				}
				case "Home":
					moveTo(rows[0]);
					break;
				case "End":
					moveTo(rows[rows.length - 1]);
					break;
				case "ArrowLeft": {
					if (query.trim()) return;
					if (foldable(row) && !folded.has(id)) {
						moveTo(row);
						setFolded((current) => new Set(current).add(id));
					} else {
						const parent = allRows[row.parentIndex];
						const index = visibleTreeCursor(allRows, rows, parent?.node.entry.id ?? null);
						moveTo(rows[index] ?? row);
					}
					break;
				}
				case "ArrowRight": {
					if (query.trim()) return;
					if (folded.has(id) && foldable(row)) {
						moveTo(row);
						setFolded((current) => {
							const next = new Set(current);
							next.delete(id);
							return next;
						});
					} else {
						const child = rows[cursorIndex + 1];
						moveTo(child?.parentIndex === row.index ? child : row);
					}
					break;
				}
				case "Enter":
					if (event.repeat) return;
					moveTo(row);
					selectRow(id);
					break;
				default:
					return;
			}
			event.preventDefault();
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [
		preview,
		overlayBlocked,
		rows,
		allRows,
		folded,
		query,
		leafIndex,
		cursorId,
		cursorIndex,
		kbdActive,
		moveTo,
		selectRow,
		navigate,
		sessionId,
	]);

	useLayoutEffect(() => {
		const list = listRef.current;
		if (!list) return;
		if (cursorIndex >= 0 && targetId) {
			const row = rowRefs.current.get(targetId);
			if (row) scrollRowIntoView(list, row);
			initialLeafScrolled.current = true;
		} else if (!initialLeafScrolled.current && leafId && leafIndex >= 0) {
			const leaf = rowRefs.current.get(leafId);
			if (leaf) {
				scrollRowIntoView(list, leaf);
				initialLeafScrolled.current = true;
			}
		}
	}, [targetId, cursorIndex, leafId, leafIndex]);

	function toggleFold(row: TreeRow) {
		const id = row.node.entry.id;
		setFolded((current) => {
			const next = new Set(current);
			if (next.has(id)) next.delete(id);
			else next.add(id);
			return next;
		});
	}
	const foldableRows = useMemo(() => allRows.filter(foldable), [allRows]);

	return (
		<main className="chat-main tree-main">
			<header className="chat-header">
				<SidebarHeaderToggle />
				<HardDrive size={16} aria-hidden="true" />
				<h1>
					{title}
					<span className="tree-suffix">· 分支树</span>
				</h1>
				{preview && <span className="preview-tag">设计预览 · 示例数据</span>}
				<button
					type="button"
					className="btn-icon sm-28 tree-close"
					aria-label="关闭分支树，返回会话"
					onClick={() => navigate(`/s/${sessionId}${preview ? "?preview=1" : ""}`, { viewTransition: true })}
				>
					<X size={16} aria-hidden="true" />
				</button>
			</header>
			{(error || actionError) && (
				<div className="connection-banner" role="alert">
					{actionError || error}
				</div>
			)}
			<div className="tree-toolbar">
				<div className="tree-search">
					<Search size={16} aria-hidden="true" />
					<input
						aria-label="搜索条目"
						placeholder="搜索条目"
						value={query}
						onChange={(event) => setQuery(event.target.value)}
						onKeyDown={(event) => {
							if (event.nativeEvent.isComposing || event.keyCode === 229) return;
							if (event.key === "Escape" && query) {
								event.preventDefault();
								event.stopPropagation();
								setQuery("");
								return;
							}
							if (preview || !isNavigationKeyEvent(event.nativeEvent)) return;
							if ((event.key === "ArrowDown" || event.key === "Enter") && rows[0]) {
								event.preventDefault();
								event.stopPropagation();
								moveTo(rows[0]);
							}
						}}
					/>
				</div>
				<Select
					aria-label="视图过滤"
					className="tree-filter"
					selectedKey={filter}
					isOpen={filterOpen}
					onOpenChange={setFilterOpen}
					onSelectionChange={(key) => {
						setFilter(key as ViewFilter);
						saveViewFilter(key as ViewFilter);
						setFolded(new Set());
					}}
				>
					<Label className="visually-hidden">视图过滤</Label>
					<Select.Trigger ref={filterTriggerRef} className="tree-filter-trigger">
						<span>{VIEW_FILTERS.find((item) => item.id === filter)?.label}</span>
						<ChevronDown size={14} aria-hidden="true" />
					</Select.Trigger>
					<Select.Popover
						className="tier-2-glass tier-2-menu tree-filter-popover"
						placement="bottom start"
						offset={6}
					>
						<ListBox className="tier-2-menu-list">
							{VIEW_FILTERS.map((item) => (
								<ListBox.Item
									key={item.id}
									id={item.id}
									textValue={item.label}
									className="menu-row tree-filter-option"
								>
									<span>{item.label}</span>
									{filter === item.id && <Check size={14} className="tree-filter-check" aria-hidden="true" />}
								</ListBox.Item>
							))}
						</ListBox>
					</Select.Popover>
				</Select>
				<span className="tree-toolbar-spacer" />
				<button
					type="button"
					className="btn-ghost tree-toolbar-button"
					title="全部展开"
					onClick={() => setFolded(new Set())}
				>
					<ChevronsUpDown size={16} aria-hidden="true" />
					全部展开
				</button>
				<button
					type="button"
					className="btn-ghost tree-toolbar-button"
					title="全部折叠"
					onClick={() => setFolded(new Set(foldableRows.map((row) => row.node.entry.id)))}
				>
					<ChevronsDownUp size={16} aria-hidden="true" />
					全部折叠
				</button>
				<span className="tree-counter">
					{leafIndex >= 0 ? leafIndex + 1 : "–"} / {rows.length} 条
				</span>
			</div>
			<ul
				className="tree-list kbd-focus-scope"
				ref={listRef}
				tabIndex={-1}
				aria-label="分支树条目"
				data-kbd-active={targetId !== null || undefined}
				onBlur={(event) => {
					if (!event.currentTarget.contains(event.relatedTarget) && !overlayBlocked) clearKeyboard();
				}}
			>
				{tree === undefined ? (
					<li className="tree-empty">正在读取分支树…</li>
				) : rows.length === 0 ? (
					<li className="tree-empty">{query ? "没有匹配的条目" : "此会话还没有条目"}</li>
				) : (
					rows.map((row, index) => {
						const id = row.node.entry.id;
						const text = rowTexts.get(id)!;
						const summary = row.node.entry.type === "branch_summary";
						const isFolded = folded.has(id) && foldable(row);
						return (
							<li
								key={id}
								ref={(el) => {
									if (el) rowRefs.current.set(id, el);
									else rowRefs.current.delete(id);
								}}
								tabIndex={index === cursorIndex ? 0 : -1}
								className={`tree-row tree-row-${text.kind} ${id === leafId ? "tree-row-current" : ""}`}
								onFocus={(event) => {
									if (event.target !== event.currentTarget) {
										clearKeyboard();
										return;
									}
									setCursorId(id);
									setKbdActive(event.currentTarget.matches(":focus-visible"));
								}}
								onClick={() => {
									clearKeyboard();
									setCursorId(id);
									selectRow(id);
								}}
								onKeyDown={(event) => {
									if (
										event.target !== event.currentTarget ||
										event.key !== "Enter" ||
										event.repeat ||
										!isNavigationKeyEvent(event.nativeEvent) ||
										preview ||
										overlayBlocked
									)
										return;
									event.preventDefault();
									selectRow(id);
								}}
							>
								<RowPrefix row={row} />
								{row.showConnector && row.childCount > 0 && (
									<span className="tree-branch-offset" aria-hidden="true" />
								)}
								<RowIcon kind={text.kind} summary={summary} />
								{text.role && <span className="tree-role">{text.role}</span>}
								<span
									className={`tree-text ${text.kind !== "user" && text.kind !== "assistant" ? "tree-text-muted" : ""}`}
								>
									{text.text}
								</span>
								{foldable(row) && (
									<button
										type="button"
										className="tree-fold"
										aria-label={isFolded ? "展开分支" : "折叠分支"}
										onClick={(event) => {
											event.stopPropagation();
											toggleFold(row);
										}}
									>
										{isFolded ? (
											<>
												▸
												<span className="tree-fold-count">
													已隐藏 {foldResult.hiddenCounts.get(id) ?? 0} 条
												</span>
											</>
										) : (
											"▾"
										)}
									</button>
								)}
								{id === leafId && <span className="tree-current">当前位置</span>}
							</li>
						);
					})
				)}
				<KbdFocusOverlay
					containerRef={listRef}
					itemRefs={rowRefs}
					targetId={targetId}
					as="li"
					style={kbdStyle}
					insetX={narrowViewport ? 12 : 20}
					insetY={1}
				/>
			</ul>
			{!!choice && (
				<div className="summary-scrim tier-3-scrim" role="none" onMouseDown={() => setChoice("")}>
					<div
						ref={choiceDialogRef}
						tabIndex={-1}
						className="summary-dialog tier-3-dialog"
						role="dialog"
						aria-modal="true"
						aria-label="选择"
						onMouseDown={(event) => event.stopPropagation()}
					>
						<div className="summary-dialog-head">
							<h2>选择</h2>
							<button type="button" className="btn-icon sm-28" aria-label="关闭" onClick={() => setChoice("")}>
								<X size={16} aria-hidden="true" />
							</button>
						</div>
						<div className="summary-dialog-options">
							{[
								{ mode: "none" as const, icon: CornerUpLeft, label: "不总结" },
								{ mode: "summarize" as const, icon: Sparkles, label: "总结" },
								{ mode: "custom" as const, icon: PencilLine, label: "用自定义提示词总结" },
							].map((option, index) => (
								<button
									type="button"
									key={option.mode}
									className={`menu-row summary-option ${index === choiceIndex ? "summary-option-active" : ""}`}
									onFocus={() => setChoiceIndex(index)}
									onMouseEnter={() => setChoiceIndex(index)}
									onClick={() => void choose(option.mode)}
								>
									<option.icon size={16} aria-hidden="true" />
									<span>{option.label}</span>
								</button>
							))}
						</div>
					</div>
				</div>
			)}
			{toast && (
				<Notification
					key={toast}
					className="tree-toast"
					label="消息提示"
					duration={2600}
					onDismiss={() => setToast("")}
				>
					<Info size={16} aria-hidden="true" />
					<span className="notification-message">{toast}</span>
					<button
						type="button"
						className="btn-icon xs-24 notification-close"
						aria-label="关闭消息提示"
						onClick={() => setToast("")}
					>
						<X size={14} aria-hidden="true" />
					</button>
				</Notification>
			)}
		</main>
	);
}
