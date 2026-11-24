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
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { draftKey } from "../composer/drafts";
import { apiJson, sessionPath } from "../data/api";
import { useSessionTitle, useSessionTree } from "../data/web-state";
import { focusableElements, resolveDialogTabTarget } from "../shell/dialog-focus";
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
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const preview = params.get("preview") === "1";
	const sample = useMemo(() => (preview ? sampleTree() : undefined), [preview]);
	const live = useSessionTree(preview ? undefined : sessionId);
	const tree = sample?.tree ?? live.tree;
	const leafId = sample?.leafId ?? live.leafId;
	const error = live.error;
	const refresh = live.refresh;
	const title = useSessionTitle(sessionId, preview ? "checkpoint4 UI 精修" : "会话", !preview);
	const [query, setQuery] = useState("");
	const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());
	const [filter, setFilter] = useState<ViewFilter>(loadViewFilter);
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
	// 键盘游标：null 表示跟随当前叶（leafIndex）。
	const [cursor, setCursor] = useState<number | null>(null);
	const rowRefs = useRef(new Map<string, HTMLElement>());
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
				navigate(`/s/${sessionId}`, { viewTransition: true });
			} catch (cause) {
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
		const frame = requestAnimationFrame(() => {
			choiceDialogRef.current?.querySelector<HTMLElement>(".summary-option")?.focus({ preventScroll: true });
		});
		const onKey = (event: KeyboardEvent) => {
			if (event.isComposing || event.keyCode === 229) return;
			if (event.key === "Escape") {
				event.preventDefault();
				setChoice("");
				return;
			}
			if (event.key === "Tab" && choiceDialogRef.current) {
				const items = focusableElements(choiceDialogRef.current);
				const active =
					document.activeElement instanceof HTMLElement && choiceDialogRef.current.contains(document.activeElement)
						? document.activeElement
						: null;
				const target = resolveDialogTabTarget(items, active, event.shiftKey);
				if (target) {
					event.preventDefault();
					if (target === "dialog") choiceDialogRef.current.focus();
					else target.focus();
				}
				return;
			}
			if (event.key === "ArrowUp" || event.key === "ArrowDown") {
				event.preventDefault();
				const next = (choiceIndexRef.current + (event.key === "ArrowUp" ? 2 : 1)) % 3;
				setChoiceIndex(next);
				const options = choiceDialogRef.current?.querySelectorAll<HTMLButtonElement>(".summary-option");
				options?.[next]?.focus({ preventScroll: true });
				return;
			}
			if (event.key === "Enter") {
				if (
					document.activeElement instanceof HTMLButtonElement &&
					choiceDialogRef.current?.contains(document.activeElement)
				) {
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
			if (
				!document.querySelector(".standby-scrim") &&
				previous instanceof HTMLElement &&
				previous !== document.body &&
				previous.isConnected &&
				!previous.closest("[inert]")
			) {
				previous.focus({ preventScroll: true });
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
	const rawCursorIndex = cursor ?? (leafIndex >= 0 ? leafIndex : rows.length - 1);
	const cursorIndex = rows.length ? Math.max(0, Math.min(rawCursorIndex, rows.length - 1)) : -1;

	// 全局仅处理 Esc 返回会话（方向键与 Enter 收敛在树列表内部，避免拦截按钮或待机层）。
	useEffect(() => {
		if (preview || shell?.appActive || shell?.overlay) return;
		const onKey = (event: KeyboardEvent) => {
			if (
				event.key !== "Escape" ||
				choice ||
				filterOpen ||
				event.defaultPrevented ||
				event.isComposing ||
				event.keyCode === 229 ||
				document.querySelector(
					'.palette-scrim, .standby-scrim, .summary-scrim, dialog[open], .session-menu, .popover:not([data-exiting="true"]), .select__popover:not([data-exiting="true"]), [data-slot="popover-dialog"]',
				)
			) {
				return;
			}
			const target = event.target as HTMLElement | null;
			if (target && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
			event.preventDefault();
			navigate(`/s/${sessionId}`, { viewTransition: true });
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [preview, shell?.appActive, shell?.overlay, choice, filterOpen, navigate, sessionId]);

	// 游标行滚动到可见区域。
	useEffect(() => {
		if (cursorIndex < 0 || cursorIndex >= rows.length) return;
		rowRefs.current.get(rows[cursorIndex].node.entry.id)?.scrollIntoView({ block: "nearest" });
	}, [cursorIndex, rows]);

	function moveTreeCursor(nextIndex: number) {
		if (!rows.length) return;
		const clamped = Math.max(0, Math.min(nextIndex, rows.length - 1));
		setCursor(clamped);
		const nextId = rows[clamped]?.node.entry.id;
		if (nextId) {
			rowRefs.current.get(nextId)?.focus({ preventScroll: true });
		}
	}

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
			<ul className="tree-list" aria-label="分支树条目">
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
								className={`tree-row tree-row-${text.kind} ${id === leafId ? "tree-row-current" : ""} ${
									index === cursorIndex ? "tree-row-cursor" : ""
								}`}
								onFocus={() => setCursor(index)}
								onClick={() => {
									setCursor(index);
									selectRow(id);
								}}
								onKeyDown={(event) => {
									if (event.nativeEvent.isComposing || event.keyCode === 229) return;
									if (event.target !== event.currentTarget) return;
									if (event.key === "ArrowUp") {
										event.preventDefault();
										moveTreeCursor(index - 1);
									} else if (event.key === "ArrowDown") {
										event.preventDefault();
										moveTreeCursor(index + 1);
									} else if (event.key === "Home") {
										event.preventDefault();
										moveTreeCursor(0);
									} else if (event.key === "End") {
										event.preventDefault();
										moveTreeCursor(rows.length - 1);
									} else if (event.key === "Enter") {
										event.preventDefault();
										selectRow(id);
									}
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
										onKeyDown={(event) => event.stopPropagation()}
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
