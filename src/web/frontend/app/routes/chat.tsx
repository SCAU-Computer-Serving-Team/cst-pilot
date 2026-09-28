import { Button, Disclosure } from "@heroui/react";
import { ArrowUp, Brain, Copy, Download, GitFork, GripVertical, HardDrive, ListTree, Pencil, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { apiJson } from "../app/api";
import { Composer } from "../app/composer";
import { sampleSessions, Sidebar } from "../app/shell";
import { type ContentPart, type InboxSnapshot, type Message, type Question, useSessionDetail } from "../app/web-state";
import { ToolGroup } from "../cards/tool-card";
import { matchToolResults } from "../cards/match-results";

const textOf = (message: Message) => typeof message.content === "string" ? message.content : message.content.filter((part): part is Extract<ContentPart, { type: "text" }> => part.type === "text").map((part) => part.text).join("\n");
const base = (id: string) => `/api/sessions/${encodeURIComponent(id)}`;

function Markdown({ text }: { text: string }) {
  return <div className="markdown"><ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]} components={{
    a: ({ children, ...props }) => <a {...props} target="_blank" rel="noopener noreferrer">{children}</a>,
    img: () => null,
  }}>{text}</ReactMarkdown></div>;
}

function Conversation({ id, entries, streaming, running, refresh }: {
  id: string; entries: { id?: string; message: Message }[]; streaming?: Message; running: boolean; refresh: () => Promise<void>;
}) {
  const navigate = useNavigate();
  const scroll = useRef<HTMLDivElement>(null);
  const [following, setFollowing] = useState(true);
  const [error, setError] = useState("");
  const [visibleCount, setVisibleCount] = useState(80);
  const resultsByTurn = matchToolResults(entries);
  const history = entries.filter(({ message }) => message.role !== "toolResult");
  const display = history.slice(-visibleCount);
  if (streaming) {
    const active = display.findIndex(({ message }) => message.role === "assistant" && message.timestamp === streaming.timestamp);
    if (active >= 0) display[active] = { ...display[active], message: streaming };
    else display.push({ message: streaming });
  }
  useEffect(() => { if (following) scroll.current?.scrollTo({ top: scroll.current.scrollHeight }); }, [entries, streaming, following]);
  async function fork(entryId: string) {
    try { const created = await apiJson<{ id: string }>(`${base(id)}/fork`, { method: "POST", body: { entryId } }); navigate(`/s/${created.id}`); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "无法派生会话"); }
  }
  return <>
    <div className="conversation" ref={scroll} onScroll={() => {
      const el = scroll.current; if (el) setFollowing(el.scrollHeight - el.scrollTop - el.clientHeight < 100);
    }}>
      {history.length > visibleCount && <Button variant="ghost" className="show-earlier" onPress={() => { setVisibleCount((count) => count + 80); setFollowing(false); }}>显示更早的消息（剩余 {history.length - visibleCount} 条）</Button>}
      {display.map(({ id: entryId, message }, index) => message.role === "user" ?
        <div className="user-line" key={entryId ?? `user-${index}`}><div className="user-bubble">
          {textOf(message)}
          {Array.isArray(message.content) && message.content.filter((part) => part.type === "image").map((part, imageIndex) =>
            <img key={imageIndex} className="message-image" alt={`附图 ${imageIndex + 1}`} src={`data:${part.mimeType};base64,${part.data}`} />)}
        </div></div> :
        <article className="assistant-block" key={entryId ?? `assistant-${index}`}>
          {Array.isArray(message.content) && message.content.filter((part) => part.type === "thinking").map((part, thoughtIndex) => <Disclosure className="thinking-line" key={thoughtIndex} isDisabled={part.redacted}>
            <Disclosure.Heading><Disclosure.Trigger className="thinking-trigger"><Brain size={16} aria-hidden="true" /><span>已思考 · {part.redacted ? "内容不可用" : part.thinking.slice(0, 48)}</span></Disclosure.Trigger></Disclosure.Heading>
            <Disclosure.Content>{!part.redacted && <pre>{part.thinking}</pre>}</Disclosure.Content>
          </Disclosure>)}
          {Array.isArray(message.content) && message.content.some((part) => part.type === "toolCall") && <ToolGroup
            calls={message.content.filter((part): part is Extract<ContentPart, { type: "toolCall" }> => part.type === "toolCall")}
            results={resultsByTurn.get(entryId ?? `stamp:${message.timestamp}`) ?? new Map()} live={running && index === display.length - 1} startedAt={message.timestamp} />}
          {textOf(message) && <Markdown text={textOf(message)} />}
          {message.stopReason === "error" && <p className="message-error" role="alert">{message.errorMessage || "模型调用失败，请检查登录状态和模型设置。"}</p>}
          {message.stopReason === "aborted" && <p className="message-warning">已停止生成</p>}
          {entryId && message.stopReason != null && message.stopReason !== "toolUse" && <div className="answer-actions">
            <Button variant="ghost" isIconOnly aria-label="复制回答" onPress={() => { void navigator.clipboard.writeText(textOf(message)).catch(() => setError("无法复制，请检查浏览器权限。")); }}><Copy size={15} /></Button>
            <Button variant="ghost" isIconOnly aria-label="从此回答派生会话" onPress={() => void fork(entryId)}><GitFork size={15} /></Button>
            <a href={`${base(id)}/export`} download={`session-${id}.md`} aria-label="导出会话"><Download size={15} /></a>
            <span>{new Date(message.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}</span>
          </div>}
        </article>
      )}
      {error && <p role="alert" className="message-error">{error}<Button variant="ghost" onPress={() => { setError(""); void refresh(); }}>关闭</Button></p>}
    </div>
    {!following && <Button className="back-to-bottom" onPress={() => { scroll.current?.scrollTo({ top: scroll.current.scrollHeight, behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" }); setFollowing(true); }}>回到底部</Button>}
  </>;
}

function QueuePanel({ id, queue, refresh }: { id: string; queue: InboxSnapshot; refresh: () => Promise<void> }) {
  const [error, setError] = useState("");
  const [editing, setEditing] = useState("");
  const [text, setText] = useState("");
  const pending = queue.items.filter((item) => item.status === "pending" || item.status === "failed");
  const queuedIds = queue.items.filter((item) => item.status === "pending" && item.delivery === "queue").map((item) => item.id);
  if (!pending.length && !queue.paused) return null;
  async function change(path: string, method: "PATCH" | "DELETE" | "POST" | "PUT", body: object) {
    try { await apiJson(`${base(id)}/${path}`, { method, body: { version: queue.version, ...body } }); setError(""); setEditing(""); await refresh(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "队列更新失败"); await refresh(); }
  }
  return <section className="queue-panel" aria-label="排队消息">
    <strong>{queue.paused ? "排队已暂停 · 发送新消息后继续" : `排队 · ${pending.length} 条`}</strong>
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
          <Button variant="ghost" className="queue-steer" onPress={() => void change(`queue/${item.id}/steer`, "POST", {})}>插话</Button>
          <Button variant="ghost" isIconOnly aria-label="编辑排队消息" onPress={() => { setEditing(item.id); setText(item.text); }}><Pencil size={16} /></Button>
          <Button variant="ghost" isIconOnly aria-label="删除排队消息" onPress={() => void change(`queue/${item.id}`, "DELETE", {})}><Trash2 size={16} /></Button>
          <Button variant="ghost" isIconOnly aria-label="上移排队消息" isDisabled={queuedIds.indexOf(item.id) <= 0} onPress={() => {
            const ids = [...queuedIds];
            const at = ids.indexOf(item.id); if (at > 0) { [ids[at - 1], ids[at]] = [ids[at], ids[at - 1]]; void change("queue/order", "PUT", { ids }); }
          }}><ArrowUp size={16} /></Button>
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
  const [actionError, setActionError] = useState("");
  const queued = !!detail && (detail.queue.paused || detail.queue.items.some((item) => item.status === "pending" || item.status === "failed"));
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
    await apiJson(`${base(sessionId)}/messages`, { method: "POST", idempotencyKey: config.messageId, body: { id: config.messageId, text, delivery: "queue", images: config.images, ...(config.skill ? { skill: config.skill } : {}) } });
    await refresh();
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
  async function rename() {
    if (!sessionId) return;
    const name = window.prompt("会话名称", title);
    if (!name || !name.trim()) return;
    try { await apiJson(`${base(sessionId)}`, { method: "PATCH", body: { name: name.trim() } }); setTitle(name.trim()); }
    catch (cause) { setActionError(cause instanceof Error ? cause.message : "重命名失败"); }
  }
  return <div className="app-layout chat-layout">
    <Sidebar preview={!!example} />
    <main className="chat-main">
      <header className="chat-header"><HardDrive size={20} /><h1>{title}</h1>
        {!example && <><Button variant="ghost" onPress={() => void rename()}>重命名</Button><Link className="header-link" to={`/s/${sessionId}/tree`}><ListTree size={17} />分支树</Link></>}
        {example && <span className="preview-tag">设计预览 · 示例数据</span>}
      </header>
      {error && <div className="connection-banner" role="alert">{error}</div>}
      {actionError && <p className="message-error" role="alert">{actionError}</p>}
      {detail ? <Conversation id={detail.id} entries={detail.entries?.length ? detail.entries : detail.messages.map((message) => ({ message }))} streaming={streaming} running={!!detail.running} refresh={refresh} />
        : <div className="chat-empty">{example ? "此会话暂无对话示例。" : error || "正在读取会话…"}</div>}
      <div className="chat-composer-area">
        {detail?.ui.map((question) => <QuestionPanel key={question.requestId} id={detail.id} question={question} refresh={refresh} />)}
        {detail && <QueuePanel id={detail.id} queue={detail.queue} refresh={refresh} />}
        <Composer key={sessionId} sessionId={sessionId} queueActive={!example && queued} contextUsage={detail?.contextUsage} running={!!detail?.running || !!streaming} onSend={example ? undefined : send} onStop={example ? undefined : stop} onCommand={example ? undefined : command} />
        <div className="composer-footnote"><span>Enter 发送 · Shift + Enter 换行</span><span>诊断建议仅供参考，请结合设备实际情况核验。</span></div>
      </div>
    </main>
  </div>;
}
