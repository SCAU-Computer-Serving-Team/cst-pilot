import { Button, Disclosure, Popover } from "@heroui/react";
import { ArrowDown, ArrowUp, Brain, ChevronDown, Copy, Download, EllipsisVertical, GitBranch, GitFork, GripVertical, HardDrive, ListTree, Pencil, Trash2, X } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { apiJson } from "../app/api";
import { Composer } from "../app/composer";
import { activeTurnIndex, formatDuration, groupTurns, splitTurn, visibleQueueItems, type Turn, type TurnEntry } from "../app/conversation-parts";
import { branchSummaryChoice, summaryBody, summaryTagText, type NavigateResult } from "../app/branch-summary";
import { rehypeStreamWords, StreamWords } from "../app/stream-words";
import { splitStableText } from "../app/markdown-split";
import { sampleSessions } from "../app/shell";
import { type ContentPart, type InboxSnapshot, type Message, type Question, useSessionDetail } from "../app/web-state";
import { ToolGroup } from "../cards/tool-card";
import { matchToolResults } from "../cards/match-results";

const textOf = (message: Message) => typeof message.content === "string" ? message.content : message.content.filter((part): part is Extract<ContentPart, { type: "text" }> => part.type === "text").map((part) => part.text).join("\n");
const base = (id: string) => `/api/sessions/${encodeURIComponent(id)}`;

// 插件数组与组件映射提到模块级：react-markdown 按引用比较依赖，内联字面量会让每次渲染重建解析器、重新解析全文。
const markdownRemarkPlugins = [remarkGfm];
const markdownRehypePlugins = [rehypeHighlight];
// 流式期间跳过代码高亮：highlight.js 对代码块的重跑是单帧最贵成本，定稿后一次性高亮。
const markdownRehypeLivePlugins = [rehypeStreamWords];
const markdownComponents: Components = {
	a: ({ children, ...props }) => <a {...props} target="_blank" rel="noopener noreferrer">{children}</a>,
	img: () => null,
};
const Markdown = memo(function Markdown({ text, live }: { text: string; live?: boolean }) {
  return <div className="markdown"><ReactMarkdown remarkPlugins={markdownRemarkPlugins} rehypePlugins={live ? markdownRehypeLivePlugins : markdownRehypePlugins} components={markdownComponents}>{text}</ReactMarkdown></div>;
});
// 流式正文分段：已定稿前缀 memo 缓存（含高亮），每帧只重解析尾段。
const LiveMarkdown = memo(function LiveMarkdown({ text }: { text: string }) {
  const { stable, tail } = splitStableText(text);
  return <div className="markdown">{stable && <ReactMarkdown remarkPlugins={markdownRemarkPlugins} rehypePlugins={markdownRehypePlugins} components={markdownComponents}>{stable}</ReactMarkdown>}<ReactMarkdown remarkPlugins={markdownRemarkPlugins} rehypePlugins={markdownRehypeLivePlugins} components={markdownComponents}>{tail}</ReactMarkdown></div>;
});

function AssistantTurnImpl({ id, turn, live, resultsByTurn, fork, onError }: {
  id: string;
  turn: Turn;
  live: boolean;
  resultsByTurn: Map<string, Map<string, Message>>;
  fork: (entryId: string) => Promise<void>;
  onError: (text: string) => void;
}) {
  const { process, finalText } = splitTurn(turn.assistants);
  const last = turn.assistants.at(-1);
  // 折叠状态：运行中展开、结束后自动折叠；用户手动操作过后不再跟随。
  const [open, setOpen] = useState(live);
  const [touched, setTouched] = useState(false);
  useEffect(() => { if (!touched) setOpen(live); }, [live, touched]);
  // 计时：运行中每秒推进，结束时冻结；历史轮次取轮内最后一个时间戳（助手消息的时间戳是消息开始时刻，为近似值）。
  const wasLive = useRef(false);
  const [frozenAt, setFrozenAt] = useState<number>();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [live]);
  useEffect(() => {
    if (live) wasLive.current = true;
    else if (wasLive.current && frozenAt === undefined) setFrozenAt(Date.now());
  }, [live, frozenAt]);
  const startedAt = turn.user?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? now;
  const historyEnd = Math.max(startedAt, ...turn.assistants.flatMap((entry) => [
    entry.message.timestamp,
    ...[...(resultsByTurn.get(entry.id ?? `stamp:${entry.message.timestamp}`)?.values() ?? [])].map((result) => result.timestamp),
  ]));
  const elapsed = formatDuration(Math.max((live ? now : frozenAt ?? historyEnd) - startedAt, live ? 1000 : 0));
  return <article className="assistant-block assistant-turn" data-live={live || undefined}>
    {(live || process.length > 0) && <Disclosure className="turn-process" isExpanded={open} onExpandedChange={(value) => { setTouched(true); setOpen(value); }}>
      <Disclosure.Heading><Disclosure.Trigger className="turn-process-head">
        <span>{live ? `工作中 ${elapsed}` : `已工作 ${elapsed}`}</span><Disclosure.Indicator />
      </Disclosure.Trigger></Disclosure.Heading>
      <Disclosure.Content><div className="turn-process-body">
        {process.map(({ entry, final, part }, partIndex) => {
          const key = `part-${partIndex}`;
          if (part.kind === "thinking") return <Disclosure className="thinking-line" key={key} isDisabled={part.part.redacted}>
            <Disclosure.Heading><Disclosure.Trigger className="thinking-trigger"><Brain size={16} aria-hidden="true" /><span>已思考{part.part.redacted ? " · 内容不可用" : ""}</span><Disclosure.Indicator /></Disclosure.Trigger></Disclosure.Heading>
            <Disclosure.Content>{!part.part.redacted && <pre>{live && final ? <StreamWords text={part.part.thinking} /> : part.part.thinking}</pre>}</Disclosure.Content>
          </Disclosure>;if (part.kind === "tools") return <ToolGroup key={key} calls={part.calls}
            results={resultsByTurn.get(entry.id ?? `stamp:${entry.message.timestamp}`) ?? new Map()}
            live={live && final} startedAt={entry.message.timestamp} />;
          return <Markdown key={key} text={part.text} />;
        })}
        {live && process.length === 0 && !finalText && <span className="t-shimmer" data-text="......">......</span>}
      </div></Disclosure.Content>
    </Disclosure>}
    {finalText && (live ? <LiveMarkdown text={finalText} /> : <Markdown text={finalText} />)}
    {last?.message.stopReason === "error" && <p className="message-error" role="alert">{last.message.errorMessage || "模型调用失败，请检查登录状态和模型设置。"}</p>}
    {last?.message.stopReason === "aborted" && <p className="message-warning">已停止生成</p>}
    {last?.id && last.message.stopReason != null && last.message.stopReason !== "toolUse" && <div className="answer-actions">
      <Button variant="ghost" isIconOnly aria-label="复制回答" onPress={() => { void navigator.clipboard.writeText(textOf(last.message)).catch(() => onError("无法复制，请检查浏览器权限。")); }}><Copy size={15} /></Button>
      <Button variant="ghost" isIconOnly aria-label="从此回答派生会话" onPress={() => void fork(last.id!)}><GitFork size={15} /></Button>
      <a href={`${base(id)}/export`} download={`session-${id}.md`} aria-label="导出会话"><Download size={15} /></a>
      <span>{new Date(last.message.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}</span>
    </div>}
  </article>;
}
// 轮次按条目引用比较：流式刷新只替换正在生成的条目，历史轮次整体跳过重渲染（含 Markdown 重解析）。
const AssistantTurn = memo(AssistantTurnImpl, (prev, next) =>
  prev.live === next.live &&
  prev.id === next.id &&
  prev.resultsByTurn === next.resultsByTurn &&
  prev.fork === next.fork &&
  prev.onError === next.onError &&
  prev.turn.user === next.turn.user &&
  prev.turn.assistants.length === next.turn.assistants.length &&
  prev.turn.assistants.every((entry, index) => entry === next.turn.assistants[index]));

/** 分支总结消息：标题行是折叠触发器（分支图标 + 标题 + 时间 + 展开箭头，箭头在最右端），展开后正文收进灰底原文块，与工具调用原文块同一规范；总结输出按原文呈现，不在聊天流里直接渲染。 */
const SummaryEntry = memo(function SummaryEntry({ entry }: { entry: TurnEntry }) {
  const [open, setOpen] = useState(false);
  return <article className="assistant-block summary-block">
    <Disclosure isExpanded={open} onExpandedChange={setOpen}>
      <Disclosure.Heading><Disclosure.Trigger className="summary-head">
        <GitBranch size={18} aria-hidden="true" />
        <span className="summary-title">分支已总结</span>
        <span className="summary-time">{new Date(entry.message.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}</span>
        <Disclosure.Indicator><ChevronDown size={14} aria-hidden="true" /></Disclosure.Indicator>
      </Disclosure.Trigger></Disclosure.Heading>
      <Disclosure.Content><section className="tool-text"><pre className="tool-raw">{entry.message.summary ?? ""}</pre></section></Disclosure.Content>
    </Disclosure>
  </article>;
});

function Conversation({ id, entries, streaming, running, summarizing, refresh, onCancelSummary }: {
  id: string; entries: { id?: string; message: Message }[]; streaming?: Message; running: boolean; summarizing: boolean; refresh: () => Promise<void>; onCancelSummary: () => Promise<void>;
}) {
  const navigate = useNavigate();
  const scroll = useRef<HTMLDivElement>(null);
  const [following, setFollowing] = useState(true);
  const [error, setError] = useState("");
  const [visibleCount, setVisibleCount] = useState(80);
  const [mountedAt] = useState(() => Date.now());
  const resultsByTurn = useMemo(() => matchToolResults(entries), [entries]);
  const history = useMemo(() => entries.filter(({ message }) => message.role !== "toolResult"), [entries]);
  const display = useMemo(() => {
    const shown = history.slice(-visibleCount);
    if (!streaming) return shown;
    const active = shown.findIndex(({ message }) => message.role === "assistant" && message.timestamp === streaming.timestamp);
    if (active >= 0) {
      const copy = shown.slice();
      copy[active] = { ...copy[active], message: streaming };
      return copy;
    }
    return [...shown, { message: streaming }];
  }, [history, visibleCount, streaming]);
  const turns = useMemo(() => groupTurns(display), [display]);
  const active = activeTurnIndex(turns, running);
  useEffect(() => { if (following) scroll.current?.scrollTo({ top: scroll.current.scrollHeight }); }, [entries, streaming, following]);
  const fork = useCallback(async (entryId: string) => {
    try { const created = await apiJson<{ id: string }>(`${base(id)}/fork`, { method: "POST", body: { entryId } }); navigate(`/s/${created.id}`); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "无法派生会话"); }
  }, [id, navigate]);
  return <>
    <div className="conversation" ref={scroll} onScroll={() => {
      const el = scroll.current; if (el) setFollowing(el.scrollHeight - el.scrollTop - el.clientHeight < 100);
    }}>
      {history.length > visibleCount && <Button variant="ghost" className="show-earlier" onPress={() => { setVisibleCount((count) => count + 80); setFollowing(false); }}>显示更早的消息（剩余 {history.length - visibleCount} 条）</Button>}
      {turns.map((turn, turnIndex) => {
        const turnKey = turn.user?.id ?? turn.summary?.id ?? turn.assistants[0]?.id ?? `turn-${turn.user?.message.timestamp ?? turn.summary?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? turnIndex}`;
        if (turn.summary) return <SummaryEntry key={turnKey} entry={turn.summary} />;
        const live = turnIndex === active;
        return <div className="turn" key={turnKey}>
          {turn.user && <div className="user-line" data-fresh={turn.user.message.timestamp >= mountedAt - 2000 || undefined}><div className="user-bubble">
            {textOf(turn.user.message)}
            {Array.isArray(turn.user.message.content) && turn.user.message.content.filter((part) => part.type === "image").map((part, imageIndex) =>
              <img key={imageIndex} className="message-image" alt={`附图 ${imageIndex + 1}`} src={`data:${part.mimeType};base64,${part.data}`} />)}
          </div></div>}
          {(turn.assistants.length > 0 || live) && <AssistantTurn id={id} turn={turn} live={live} resultsByTurn={resultsByTurn} fork={fork} onError={setError} />}
        </div>;
      })}
      {summarizing && <div className="summary-pending" role="status">
        <GitBranch size={18} aria-hidden="true" />
        <span>正在总结所选分支…</span>
        <Button variant="ghost" isIconOnly className="summary-cancel" aria-label="取消分支总结" onPress={() => void onCancelSummary()}><X size={18} /></Button>
      </div>}
      {error && <p role="alert" className="message-error">{error}<Button variant="ghost" onPress={() => { setError(""); void refresh(); }}>关闭</Button></p>}
    </div>
    {!following && <Button className="back-to-bottom" onPress={() => { scroll.current?.scrollTo({ top: scroll.current.scrollHeight, behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" }); setFollowing(true); }}><ArrowDown size={14} aria-hidden="true" />回到底部</Button>}
  </>;
}

function QueuePanel({ id, queue, refresh }: { id: string; queue: InboxSnapshot; refresh: () => Promise<void> }) {
  const [error, setError] = useState("");
  const [editing, setEditing] = useState("");
  const [text, setText] = useState("");
  const pending = visibleQueueItems(queue.items);
  const queuedIds = pending.filter((item) => item.status === "pending" && item.delivery === "queue").map((item) => item.id);
  if (!pending.length) return null;
  async function change(path: string, method: "PATCH" | "DELETE" | "POST" | "PUT", body: object) {
    try { await apiJson(`${base(id)}/${path}`, { method, body: { version: queue.version, ...body } }); setError(""); setEditing(""); await refresh(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "队列更新失败"); await refresh(); }
  }
  return <section className="queue-panel" aria-label="排队消息">
    <strong>{queue.paused ? "排队已暂停 · 发送新消息后继续" : pending.every((item) => item.status === "failed") ? "投递失败" : `排队 · ${pending.length} 条`}</strong>
    {pending.map((item) => <div className="queue-item" key={item.id} draggable={item.status === "pending" && item.delivery === "queue"}
      onDragStart={(event) => { event.dataTransfer.setData("text/plain", item.id); event.dataTransfer.effectAllowed = "move"; }}
      onDragOver={(event) => { if (item.status === "pending" && item.delivery === "queue") event.preventDefault(); }}
      onDrop={(event) => {
        event.preventDefault();
        const source = event.dataTransfer.getData("text/plain");
        const ids = [...queuedIds];
        const from = ids.indexOf(source); const to = ids.indexOf(item.id);
        if (from < 0 || to < 0 || from === to) return;
        ids.splice(from, 1); ids.splice(to, 0, source); void change("queue/order", "PUT", { ids });
      }}>
      {editing === item.id ? <><input aria-label="编辑排队消息" value={text} onChange={(event) => setText(event.target.value)} /><Button onPress={() => void change(`queue/${item.id}`, "PATCH", { text })}>保存</Button><Button onPress={() => setEditing("")}>取消</Button></> : <>
        <GripVertical size={16} aria-hidden="true" className="queue-grip" />
        <span>{item.status === "failed" ? "投递失败 · " : ""}{item.text || "图片消息"}</span>
        {item.status === "pending" && <>
          <Button variant="ghost" className="queue-steer" onPress={() => void change(`queue/${item.id}/steer`, "POST", {})}><ArrowUp size={14} aria-hidden="true" />立即</Button>
          <Button variant="ghost" isIconOnly aria-label="编辑排队消息" onPress={() => { setEditing(item.id); setText(item.text); }}><Pencil size={16} /></Button>
          <Button variant="ghost" isIconOnly aria-label="删除排队消息" onPress={() => void change(`queue/${item.id}`, "DELETE", {})}><Trash2 size={16} /></Button>
        </>}
      </>}
    </div>)}
    {error && <p role="alert">{error} · 已重新读取队列，请重试。</p>}
  </section>;
}

function QuestionPanel({ id, question, refresh }: { id: string; question: Question; refresh: () => Promise<void> }) {
  const [value, setValue] = useState(question.prefill ?? "");
  const [error, setError] = useState("");
  async function respond(answer: string | boolean | null) {
    try { await apiJson(`${base(id)}/ui/${encodeURIComponent(question.requestId)}/response`, { method: "POST", body: { value: answer } }); setError(""); await refresh(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "回答未送达"); await refresh(); }
  }
  return <section className="question-panel" aria-label="扩展提问" key={question.requestId}>
    <strong>{question.title}</strong>{question.message && <p>{question.message}</p>}
    {question.kind === "confirm" ? <div><Button onPress={() => void respond(true)}>确认</Button><Button variant="ghost" onPress={() => void respond(false)}>取消</Button></div> : <>
      {question.kind === "select" ? <select aria-label="选择答案" value={value} onChange={(event) => setValue(event.target.value)}><option value="">请选择</option>{question.options?.map((option) => <option key={option} value={option}>{option}</option>)}</select>
        : question.kind === "editor" ? <textarea aria-label="输入回答" value={value} onChange={(event) => setValue(event.target.value)} />
        : <input aria-label="输入回答" value={value} onChange={(event) => setValue(event.target.value)} />}
      <Button isDisabled={question.kind === "select" && !value} onPress={() => void respond(value)}>提交</Button><Button variant="ghost" onPress={() => void respond(null)}>跳过</Button>
    </>}
    {error && <p role="alert">{error}</p>}
  </section>;
}

export default function Chat() {
  const { sessionId } = useParams();
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const example = params.get("preview") === "1" ? sampleSessions.find((session) => session.id === sessionId) : undefined;
  const { detail, streaming, error, refresh } = useSessionDetail(example ? undefined : sessionId);
  const [title, setTitle] = useState(example?.title ?? "会话");
  const [summarizing, setSummarizing] = useState(false);
  const [summaryNote, setSummaryNote] = useState("");
  const startedSummary = useRef("");
  const choice = useMemo(() => (example ? undefined : branchSummaryChoice(params)), [example, params]);
  const clearChoice = useCallback(() => { if (sessionId) navigate(`/s/${sessionId}`, { replace: true }); }, [navigate, sessionId]);
  const runSummary = useCallback(async (entryId: string, customInstructions?: string) => {
    if (!sessionId) return;
    setSummarizing(true); setSummaryNote("");
    try {
      const result = await apiJson<NavigateResult>(`${base(sessionId)}/tree/navigate`, {
        method: "POST", idempotencyKey: crypto.randomUUID(), body: summaryBody(entryId, customInstructions),
      });
      if (result.cancelled) setSummaryNote("已取消分支总结，会话位置没有变化。");
    } catch (cause) {
      setSummaryNote(cause instanceof Error ? cause.message : "分支总结未完成");
    } finally {
      setSummarizing(false);
      await refresh();
    }
  }, [sessionId, refresh]);
  // 选「总结」时树页只留下地址参数，真正的导航请求在这里发起：叶指针未动之前，那段分支还在。
  useEffect(() => {
    if (!choice || choice.mode !== "summarize" || startedSummary.current === choice.entryId) return;
    startedSummary.current = choice.entryId;
    void (async () => { await runSummary(choice.entryId); clearChoice(); })();
  }, [choice, runSummary, clearChoice]);
  async function cancelSummary() {
    if (!sessionId) return;
    try { await apiJson(`${base(sessionId)}/tree/abort`, { method: "POST" }); }
    catch { /* 总结可能刚好结束，不是错误 */ }
  }
  // 依赖 detail 而非每次渲染新建：流式 tick 间 entries 引用稳定，下游 memo 才能跳过历史轮次。
  const entries = useMemo(() => detail
    ? detail.entries?.length ? detail.entries : detail.messages.map((message) => ({ message }))
    : [], [detail]);
  const queued = !!detail && visibleQueueItems(detail.queue.items).some((item) => item.status === "pending");
  useEffect(() => {
    if (!sessionId || example) return;
    let active = true;
    apiJson<{ sessions: { id: string; title: string }[] }>("/api/sessions").then((data) => {
      if (active) setTitle(data.sessions.find((item) => item.id === sessionId)?.title ?? "新对话");
    }).catch(() => undefined);
    return () => { active = false; };
  }, [sessionId, example]);
  async function send(text: string, config: { messageId: string; images: { mimeType: string; data: string }[]; skill?: string }) {
    if (!sessionId) return;
    // 提交成功即返回（立即清空输入框、复位发送按钮）；队列与气泡由 SSE 事件驱动的刷新补齐，
    // 避免落盘窗口内 GET 偶发变慢时按钮长时间置灰。
    await apiJson(`${base(sessionId)}/messages`, { method: "POST", idempotencyKey: config.messageId, body: { id: config.messageId, text, delivery: "queue", images: config.images, ...(config.skill ? { skill: config.skill } : {}) } });
    void refresh();
  }
  async function stop() {
    if (!sessionId) return;
    await apiJson(`${base(sessionId)}/abort`, { method: "POST" });
    await refresh();
  }
  async function command(name: "compact" | "fork", text: string) {
    if (!sessionId) return;
    if (name === "compact") await apiJson(`${base(sessionId)}/compact`, { method: "POST", body: { instructions: text } });
    else {
      const entryId = [...(detail?.entries ?? [])].reverse().find((entry) => entry.message.role === "assistant")?.id;
      if (!entryId) throw new Error("当前会话还没有可派生的消息");
      const created = await apiJson<{ id: string }>(`${base(sessionId)}/fork`, { method: "POST", body: { entryId } });
      navigate(`/s/${created.id}`);
    }
    await refresh();
  }
  return <main className="chat-main">
      <header className="chat-header"><HardDrive size={20} /><h1>{title}</h1>
        {example && <span className="preview-tag">设计预览 · 示例数据</span>}
        {!example && <Popover>
          <Button variant="ghost" isIconOnly className="chat-menu-trigger" aria-label="消息菜单"><EllipsisVertical size={20} /></Button>
          <Popover.Content placement="bottom end" className="sidebar-account-popover">
            <Popover.Dialog aria-label="消息菜单" className="sidebar-account-menu">
              <Link className="sidebar-account-item" to={`/s/${sessionId}/tree`}><ListTree size={18} aria-hidden="true" />分支树</Link>
            </Popover.Dialog>
          </Popover.Content>
        </Popover>}
      </header>
      {(error || summaryNote) && <div className="connection-banner" role="alert">{summaryNote || error}</div>}
      {detail ? <Conversation id={detail.id} entries={entries} streaming={streaming} running={!!detail.running} summarizing={summarizing} refresh={refresh} onCancelSummary={cancelSummary} />
        : <div className="chat-empty">{example ? "此会话暂无对话示例。" : error || "正在读取会话…"}</div>}
      <div className="chat-composer-area">
        {detail?.ui.map((question) => <QuestionPanel key={question.requestId} id={detail.id} question={question} refresh={refresh} />)}
        {detail && <QueuePanel id={detail.id} queue={detail.queue} refresh={refresh} />}
        <Composer key={sessionId} sessionId={sessionId} queueActive={!example && queued} contextUsage={detail?.contextUsage} running={!!detail?.running || !!streaming}
          summaryTag={choice?.mode === "custom" ? { label: summaryTagText, onCancel: clearChoice } : undefined}
          onSummary={choice?.mode === "custom" ? async (instructions) => { await runSummary(choice.entryId, instructions); clearChoice(); } : undefined}
          onSend={example ? undefined : send} onStop={example ? undefined : stop} onCommand={example ? undefined : command} />
      </div>
    </main>;
}
