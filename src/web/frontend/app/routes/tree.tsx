import { Button, Label, ListBox, Select } from "@heroui/react";
import { Bot, ChevronDown, ChevronsDownUp, ChevronsUpDown, CornerUpLeft, GitBranch, HardDrive, Minimize2, PencilLine, Search, Sparkles, User, Wrench, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { apiJson } from "../app/api";
import { branchSummaryHref } from "../app/branch-summary";
import { sampleTree } from "../app/branch-tree-sample";
import {
  applyFold,
  buildVisibleTree,
  collectToolCalls,
  foldable,
  layoutTree,
  matchesSearch,
  rowText,
  slotMark,
  type RowText,
  type TreeRow,
} from "../app/branch-tree";
import { useSessionTree } from "../app/web-state";

const kindIcon = { user: User, assistant: Bot, tool: Wrench, info: Minimize2 } as const;

function RowIcon({ kind, summary }: { kind: RowText["kind"]; summary: boolean }) {
  const Icon = summary ? GitBranch : kindIcon[kind];
  return <Icon size={16} aria-hidden="true" className={`tree-icon tree-icon-${summary ? "summary" : kind}`} />;
}

/** 行前缀：每个缩进列一格，连接符列画方块，其余按 gutter 画竖线或留空。 */
function RowPrefix({ row }: { row: TreeRow }) {
  return <>
    {Array.from({ length: row.displayIndent }, (_, column) => {
      const mark = slotMark(row, column);
      return <span key={column} className="tree-slot" aria-hidden="true">
        {mark.kind === "square" && <span className="tree-square" />}
        {mark.kind === "line" && <span className="tree-line" />}
      </span>;
    })}
  </>;
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
  const [title, setTitle] = useState(preview ? "checkpoint4 UI 精修" : "会话");
  const [query, setQuery] = useState("");
  const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());
  const [choice, setChoice] = useState("");
  const [actionError, setActionError] = useState("");
  useEffect(() => {
    if (!choice) return;
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setChoice(""); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [choice]);
  useEffect(() => {
    if (!sessionId || preview) return;
    let active = true;
    apiJson<{ sessions: { id: string; title: string }[] }>("/api/sessions").then((data) => {
      if (active) setTitle(data.sessions.find((item) => item.id === sessionId)?.title ?? "新对话");
    }).catch(() => undefined);
    return () => { active = false; };
  }, [sessionId, preview]);

  const toolCalls = useMemo(() => collectToolCalls(tree ?? []), [tree]);
  const allRows = useMemo(() => layoutTree(buildVisibleTree(tree ?? [], leafId), leafId), [tree, leafId]);
  // 行文案预计算：搜索过滤与渲染复用，避免每次输入都重算整棵树的文案。
  const rowTexts = useMemo(() => new Map(allRows.map((row) => [row.node.entry.id, rowText(row.node, toolCalls)])), [allRows, toolCalls]);
  const foldResult = useMemo(() => applyFold(allRows, folded), [allRows, folded]);
  const rows = useMemo(() => {
    if (!query.trim()) return foldResult.visible;
    return allRows.filter((row) => matchesSearch(row.node, rowTexts.get(row.node.entry.id)!, query));
  }, [allRows, foldResult, query, rowTexts]);
  const leafIndex = useMemo(() => rows.findIndex((row) => row.node.entry.id === leafId), [rows, leafId]);

  async function jump(entryId: string) {
    if (!sessionId || preview) return;
    setActionError("");
    try {
      const result = await apiJson<{ editorText?: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/tree/navigate`, { method: "POST", body: { entryId } });
      if (result.editorText) sessionStorage.setItem(`cst-draft:${sessionId}`, result.editorText);
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

  return <main className="chat-main tree-main">
    <header className="chat-header">
      <HardDrive size={20} /><h1>{title}<span className="tree-suffix">· 分支树</span></h1>
      {preview && <span className="preview-tag">设计预览 · 示例数据</span>}
      <Button variant="ghost" isIconOnly className="tree-close" aria-label="关闭分支树，返回会话" onPress={() => navigate(`/s/${sessionId}${preview ? "?preview=1" : ""}`)}><X size={18} /></Button>
    </header>
    {(error || actionError) && <div className="connection-banner" role="alert">{actionError || error}</div>}
    <div className="tree-toolbar">
      <div className="tree-search"><Search size={16} aria-hidden="true" /><input aria-label="搜索条目" placeholder="搜索条目" value={query} onChange={(event) => setQuery(event.target.value)} /></div>
      <Select aria-label="视图过滤" selectedKey="default" isDisabled className="tree-filter">
        <Label className="visually-hidden">视图过滤</Label>
        <Select.Trigger className="tree-filter-trigger"><span>默认视图</span><ChevronDown size={14} /></Select.Trigger>
        <Select.Popover><ListBox><ListBox.Item id="default" textValue="默认视图">默认视图</ListBox.Item></ListBox></Select.Popover>
      </Select>
      <span className="tree-toolbar-spacer" />
      <Button variant="ghost" className="tree-toolbar-button" onPress={() => setFolded(new Set())}><ChevronsUpDown size={16} aria-hidden="true" />全部展开</Button>
      <Button variant="ghost" className="tree-toolbar-button" onPress={() => setFolded(new Set(foldableRows.map((row) => row.node.entry.id)))}><ChevronsDownUp size={16} aria-hidden="true" />全部折叠</Button>
      <span className="tree-counter">{leafIndex >= 0 ? leafIndex + 1 : "–"} / {rows.length} 条</span>
    </div>
    <div className="tree-list" role="list" aria-label="分支树条目">
      {tree === undefined ? <p className="tree-empty">正在读取分支树…</p>
        : rows.length === 0 ? <p className="tree-empty">{query ? "没有匹配的条目" : "此会话还没有条目"}</p>
        : rows.map((row) => {
          const id = row.node.entry.id;
          const text = rowTexts.get(id)!;
          const summary = row.node.entry.type === "branch_summary";
          const isFolded = folded.has(id) && foldable(row);
          return <div key={id} role="listitem" tabIndex={0}
            className={`tree-row tree-row-${text.kind} ${id === leafId ? "tree-row-current" : ""}`}
            onClick={() => { if (!preview) setChoice(id); }}
            onKeyDown={(event) => { if (!preview && event.key === "Enter" && event.target === event.currentTarget) setChoice(id); }}>
            <RowPrefix row={row} />
            <RowIcon kind={text.kind} summary={summary} />
            {text.role && <span className="tree-role">{text.role}</span>}
            <span className={`tree-text ${text.kind !== "user" && text.kind !== "assistant" ? "tree-text-muted" : ""}`}>{text.text}</span>
            {foldable(row) && <button type="button" className="tree-fold" aria-label={isFolded ? "展开分支" : "折叠分支"}
              onClick={(event) => { event.stopPropagation(); toggleFold(row); }}
              onKeyDown={(event) => event.stopPropagation()}>
              {isFolded ? <>▸<span className="tree-fold-count">已隐藏 {foldResult.hiddenCounts.get(id) ?? 0} 条</span></> : "▾"}
            </button>}
            {id === leafId && <span className="tree-current">当前位置</span>}
          </div>;
        })}
    </div>
    {!!choice && <div className="summary-scrim" onMouseDown={() => setChoice("")}>
      <div className="summary-dialog" role="dialog" aria-modal="true" aria-label="选择" onMouseDown={(event) => event.stopPropagation()}>
        <div className="summary-dialog-head">
          <h2>选择</h2>
          <Button variant="ghost" isIconOnly aria-label="关闭" onPress={() => setChoice("")}><X size={18} /></Button>
        </div>
        <div className="summary-dialog-options">
          <button type="button" className="summary-option" onClick={() => void choose("none")}><CornerUpLeft size={18} aria-hidden="true" /><span>不总结</span></button>
          <button type="button" className="summary-option" onClick={() => void choose("summarize")}><Sparkles size={18} aria-hidden="true" /><span>总结</span></button>
          <button type="button" className="summary-option" onClick={() => void choose("custom")}><PencilLine size={18} aria-hidden="true" /><span>用自定义提示词总结</span></button>
        </div>
      </div>
    </div>}
  </main>;
}
