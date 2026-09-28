import { Button } from "@heroui/react";
import { GitFork, ListTree } from "lucide-react";
import { useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { apiJson } from "../app/api";
import { Sidebar } from "../app/shell";

type Entry = { id: string; type: string; timestamp: string; message?: { role: string; toolName?: string; content: string | { type: string; text?: string }[] } };
type Node = { entry: Entry; children: Node[]; label?: string };
function title(node: Node): string {
  const { entry } = node;
  if (node.label) return node.label;
  if (entry.type !== "message") return ({ compaction: "会话压缩", model_change: "模型切换", thinking_level_change: "思考强度调整", branch_summary: "分支摘要" } as Record<string, string>)[entry.type] ?? entry.type;
  if (entry.message?.role === "toolResult") return `工具 · ${entry.message.toolName ?? "调用"} · 返回结果`;
  const content = entry.message?.content;
  const text = typeof content === "string" ? content : content?.filter((part) => part.type === "text").map((part) => part.text).join(" ") ?? "";
  return `${entry.message?.role === "user" ? "提问" : entry.message?.role === "assistant" ? "回答" : "工具"} · ${text.slice(0, 90) || "无文字"}`;
}
function TreeNode({ node, navigateTo, fork, busy }: { node: Node; navigateTo: (id: string) => void; fork: (id: string) => void; busy: boolean }) {
  return <li><div className="tree-row"><span title={title(node)}>{title(node)}</span><time>{new Date(node.entry.timestamp).toLocaleString("zh-CN")}</time>
    <Button variant="ghost" isDisabled={busy} onPress={() => navigateTo(node.entry.id)}>切换至此</Button>
    {node.entry.type === "message" && node.entry.message?.role !== "toolResult" && <Button variant="ghost" isIconOnly isDisabled={busy} aria-label={`从${title(node)}派生会话`} onPress={() => fork(node.entry.id)}><GitFork size={16} /></Button>}
  </div>{node.children.length > 0 && <ul className={node.children.length === 1 ? "tree-continue" : "tree-branches"}>{node.children.map((child) => <TreeNode node={child} key={child.entry.id} navigateTo={navigateTo} fork={fork} busy={busy} />)}</ul>}</li>;
}
export default function Tree() {
  const { sessionId } = useParams();
  const navigate = useNavigate();
  const [tree, setTree] = useState<Node[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!sessionId) return;
    let active = true;
    apiJson<{ tree: Node[] }>(`/api/sessions/${encodeURIComponent(sessionId)}/tree`).then((data) => { if (active) setTree(data.tree); })
      .catch((cause: unknown) => { if (active) setError(cause instanceof Error ? cause.message : "无法读取分支树"); });
    return () => { active = false; };
  }, [sessionId]);
  async function change(entryId: string) {
    if (!sessionId) return;
    setBusy(true);
    try {
      const result = await apiJson<{ editorText: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/tree/navigate`, { method: "POST", body: { entryId } });
      if (result.editorText) sessionStorage.setItem(`cst-draft:${sessionId}`, result.editorText);
      navigate(`/s/${sessionId}`);
    } catch (cause) { setError(cause instanceof Error ? cause.message : "无法切换分支"); setBusy(false); }
  }
  async function fork(entryId: string) {
    if (!sessionId) return;
    setBusy(true);
    try { const result = await apiJson<{ id: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/fork`, { method: "POST", body: { entryId } }); navigate(`/s/${result.id}`); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "无法派生会话"); setBusy(false); }
  }
  return <div className="app-layout chat-layout"><Sidebar /><main className="chat-main tree-main">
    <header className="chat-header"><ListTree size={20} /><h1>分支树</h1><Link className="header-link" to={`/s/${sessionId}`}>返回会话</Link></header>
    <div className="tree-content">{error && <p role="alert" className="message-error">{error}</p>}
      {tree.length ? <ul>{tree.map((node) => <TreeNode key={node.entry.id} node={node} navigateTo={(id) => void change(id)} fork={(id) => void fork(id)} busy={busy} />)}</ul> : <p>暂无分支记录</p>}
    </div>
  </main></div>;
}
