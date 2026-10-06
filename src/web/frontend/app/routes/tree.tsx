import { Button, Label, ListBox, Select } from "@heroui/react";
import {
	Bot,
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
	const [choice, setChoice] = useState("");
	// 键盘光标在三个选项间的位置；对话框打开时重置。
	const [choiceIndex, setChoiceIndex] = useState(0);
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
	useEffect(() => {
		if (!toast) return;
		const timer = window.setTimeout(() => setToast(""), 2600);
		return () => window.clearTimeout(timer);
	}, [toast]);
	useEffect(() => {
		if (choice) setChoiceIndex(0);
	}, [choice]);
	// 选择对话框的键盘操作：方向键移动光标，Enter 确认，Esc 关闭。
	useEffect(() => {
		if (!choice) return;
		const onKey = (event: KeyboardEvent) => {
			if (event.key === "Escape") {
				setChoice("");
				return;
			}
			if (event.key === "ArrowUp" || event.key === "ArrowDown") {
				event.preventDefault();
				setChoiceIndex((index) => (event.key === "ArrowUp" ? (index + 2) % 3 : (index + 1) % 3));
				return;
			}
			if (event.key === "Enter") {
				event.preventDefault();
				const mode = (["none", "summarize", "custom"] as const)[choiceIndex];
				void choose(mode);
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	});
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
	const cursorIndex = cursor ?? (leafIndex >= 0 ? leafIndex : rows.length - 1);

	// 方向键移动游标，Enter 选中行；输入框、下拉与选择对话框打开时不接管。
	// Ctrl+O 循环切换视图过滤（Shift 反向），与 TUI 一致；换档时清空折叠集合。
	const cycleFilter = useCallback(
		(step: number) => {
			const index = VIEW_FILTERS.findIndex((item) => item.id === filter);
			const next = VIEW_FILTERS[(index + step + VIEW_FILTERS.length) % VIEW_FILTERS.length];
			setFilter(next.id);
			saveViewFilter(next.id);
			setFolded(new Set());
		},
		[filter],
	);
	useEffect(() => {
		if (preview) return;
		const onKey = (event: KeyboardEvent) => {
			if (choice) return;
			const target = event.target as HTMLElement | null;
			if (target && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
			// 行自身聚焦时 Enter 由行的按键处理，避免同一按键选中两次。
			if (target?.classList.contains("tree-row")) return;
			if (event.key === "ArrowUp" || event.key === "ArrowDown") {
				event.preventDefault();
				setCursor((value) => {
					const base = value ?? leafIndex;
					const next = event.key === "ArrowUp" ? Math.max(0, base - 1) : Math.min(rows.length - 1, base + 1);
					return rows.length ? next : null;
				});
				return;
			}
			if (event.key === "Enter" && cursorIndex >= 0 && cursorIndex < rows.length) {
				event.preventDefault();
				selectRow(rows[cursorIndex].node.entry.id);
				return;
			}
			if (event.key === "Escape") {
				if (filterOpen) return;
				event.preventDefault();
				navigate(`/s/${sessionId}`);
				return;
			}
			if (event.key.toLowerCase() === "o" && event.ctrlKey && !event.altKey && !event.metaKey) {
				event.preventDefault();
				cycleFilter(event.shiftKey ? -1 : 1);
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [preview, choice, rows, leafIndex, cursorIndex, selectRow, cycleFilter, filterOpen, navigate, sessionId]);

	// 游标行滚动到可见区域。
	useEffect(() => {
		if (cursorIndex < 0 || cursorIndex >= rows.length) return;
		rowRefs.current.get(rows[cursorIndex].node.entry.id)?.scrollIntoView({ block: "nearest" });
	}, [cursorIndex, rows]);

	async function jump(entryId: string) {
		if (!sessionId || preview) return;
		setActionError("");
		try {
			const result = await apiJson<{ editorText?: string }>(sessionPath(sessionId, "/tree/navigate"), {
				method: "POST",
				body: { entryId },
			});
			if (result.editorText) sessionStorage.setItem(draftKey(sessionId), result.editorText);
			navigate(`/s/${sessionId}`);
		} catch (cause) {
			setActionError(cause instanceof Error ? cause.message : "分支导航未完成");
			await refresh();
		}
	}
	/** 三种选择：不总结在这里直接导航；总结与自定义提示词把目标条目交给聊天页，导航与总结必须同一次调用。 */
	async function choose(mode: "none" | "summarize" | "custom") {
		if (!sessionId || !choice) return;
		const entryId = choice;
		setChoice("");
		if (mode === "none") await jump(entryId);
		else navigate(branchSummaryHref(sessionId, entryId, mode));
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
				<HardDrive size={22} />
				<h1>
					{title}
					<span className="tree-suffix">· 分支树</span>
				</h1>
				{preview && <span className="preview-tag">设计预览 · 示例数据</span>}
				<Button
					variant="ghost"
					isIconOnly
					className="tree-close"
					aria-label="关闭分支树，返回会话"
					onPress={() => navigate(`/s/${sessionId}${preview ? "?preview=1" : ""}`)}
				>
					<X size={18} />
				</Button>
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
					/>
				</div>
				<Select
					aria-label="视图过滤"
					className="tree-filter"
					selectedKey={filter}
					onOpenChange={setFilterOpen}
					onSelectionChange={(key) => {
						setFilter(key as ViewFilter);
						saveViewFilter(key as ViewFilter);
						setFolded(new Set());
					}}
				>
					<Label className="visually-hidden">视图过滤</Label>
					<Select.Trigger className="tree-filter-trigger">
						<span>{VIEW_FILTERS.find((item) => item.id === filter)?.label}</span>
						<ChevronDown size={14} />
					</Select.Trigger>
					<Select.Popover>
						<ListBox>
							{VIEW_FILTERS.map((item) => (
								<ListBox.Item key={item.id} id={item.id} textValue={item.label}>
									{item.label}
								</ListBox.Item>
							))}
						</ListBox>
					</Select.Popover>
				</Select>
				<span className="tree-toolbar-spacer" />
				<Button variant="ghost" className="tree-toolbar-button" onPress={() => setFolded(new Set())}>
					<ChevronsUpDown size={16} aria-hidden="true" />
					全部展开
				</Button>
				<Button
					variant="ghost"
					className="tree-toolbar-button"
					onPress={() => setFolded(new Set(foldableRows.map((row) => row.node.entry.id)))}
				>
					<ChevronsDownUp size={16} aria-hidden="true" />
					全部折叠
				</Button>
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
								// biome-ignore lint/a11y/noNoninteractiveTabindex: 行要能 Tab 聚焦并用 Enter 选择；分支树的键盘跳段落地后改为 roving tabindex
								tabIndex={0}
								className={`tree-row tree-row-${text.kind} ${id === leafId ? "tree-row-current" : ""} ${
									index === cursorIndex ? "tree-row-cursor" : ""
								}`}
								onClick={() => selectRow(id)}
								onKeyDown={(event) => {
									if (event.key === "Enter" && event.target === event.currentTarget) selectRow(id);
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
				<div className="summary-scrim" role="none" onMouseDown={() => setChoice("")}>
					<div
						className="summary-dialog"
						role="dialog"
						aria-modal="true"
						aria-label="选择"
						onMouseDown={(event) => event.stopPropagation()}
					>
						<div className="summary-dialog-head">
							<h2>选择</h2>
							<Button variant="ghost" isIconOnly aria-label="关闭" onPress={() => setChoice("")}>
								<X size={18} />
							</Button>
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
									className={`summary-option ${index === choiceIndex ? "summary-option-active" : ""}`}
									onMouseEnter={() => setChoiceIndex(index)}
									onClick={() => void choose(option.mode)}
								>
									<option.icon size={18} aria-hidden="true" />
									<span>{option.label}</span>
								</button>
							))}
						</div>
					</div>
				</div>
			)}
			{toast && (
				<output className="tree-toast">
					<Info size={18} aria-hidden="true" />
					<span>{toast}</span>
				</output>
			)}
		</main>
	);
}
