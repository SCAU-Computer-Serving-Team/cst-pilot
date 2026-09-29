import { Button, Card, Disclosure } from "@heroui/react";
import { Braces, CheckCheck, ChevronDown, CircleCheck, CirclePause, CircleX, Copy, Globe, Info, LoaderCircle, TriangleAlert } from "lucide-react";
import { useEffect, useState } from "react";
import type { ContentPart, Message } from "../app/web-state";
import { type Block, mapTool } from "./map-tool";
import { isStandaloneTool, splitToolCalls, toolState, type ToolState } from "./tool-state";

type Call = Extract<ContentPart, { type: "toolCall" }>;
const summary = (args: unknown) => typeof args === "string" ? args : Array.isArray(args) ? args.join(" · ") : "未提供关键词";
const stateLabels: Record<ToolState, string> = { running: "执行中", interrupted: "未完成或已中断", error: "执行失败", degraded: "部分结果", success: "已完成" };

function StatusIcon({ state, size = 16 }: { state: ToolState; size?: number }) {
  const Icon = state === "running" ? LoaderCircle : state === "interrupted" ? CirclePause : state === "success" ? CircleCheck : state === "degraded" ? TriangleAlert : CircleX;
  return <Icon aria-hidden="true" className={`tool-status-icon tool-status-icon--${state === "degraded" ? "warning" : state}`} size={size} />;
}

function Listing({ block }: { block: Extract<Block, { kind: "listing" }> }) {
  const [visible, setVisible] = useState(40);
  return <section aria-label={block.id} className="tool-listing">
    <ol>{block.items.slice(0, visible).map((item, index) => <li key={index}><strong>{item.name}</strong><span title={item.metrics.map((metric) => `${metric.label}：${metric.value}`).join(" · ")}>{item.metrics.map((metric) => metric.value).join(" · ")}</span></li>)}</ol>
    {visible < block.items.length && <Button variant="ghost" onPress={() => setVisible((value) => value + 40)}>继续显示（剩余 {block.items.length - visible} 项）</Button>}
  </section>;
}
function Notice({ block }: { block: Extract<Block, { kind: "notice" }> }) {
  const Icon = block.status === "danger" ? CircleX : block.status === "warning" ? TriangleAlert : Info;
  return <p className={`tool-notice tool-notice--${block.status}`}><Icon className="tool-notice-icon" size={14} aria-hidden="true" /><span>{block.text}</span></p>;
}
function renderBlock(block: Block) {
  if (block.kind === "fields") return <section aria-label={block.title ?? block.id} className="tool-fields">{block.title && <h4>{block.title}</h4>}<dl>{block.rows.map((row, index) => <div key={index} className="tool-field"><dt>{row.label}</dt><dd>{row.value}</dd></div>)}</dl></section>;
  if (block.kind === "listing") return <Listing block={block} />;
  if (block.kind === "notice") return <Notice block={block} />;
  if (block.kind === "text") return <section className="tool-text"><pre className="tool-raw">{block.text}</pre></section>;
  if (block.kind === "image") return ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(block.mimeType) ? <img className="tool-image" src={`data:${block.mimeType};base64,${block.data}`} alt="工具返回的图片" /> : <p>图片格式不支持显示</p>;
  return <section className="tool-commands"><ol>{block.items.map((item, index) => <li key={index}>
    <span className="tool-command-meta">{item.shell} · {item.admin ? "管理员" : "普通权限"} ｜ {item.summary}</span>
    <div className="tool-command"><pre>{item.command}</pre><Button variant="ghost" isIconOnly aria-label="复制命令" onPress={() => { void navigator.clipboard.writeText(item.command); }}><Copy size={14} /></Button></div>
  </li>)}</ol></section>;
}

function SearchRow({ call, result, live, state, failed }: { call: Call; result?: Message; live?: boolean; state: ToolState; failed: boolean }) {
  const details = result && "details" in result ? (result as Message & { details?: unknown }).details : undefined;
  const data = details && typeof details === "object" ? details as Record<string, unknown> : {};
  const status = !result ? (live ? "running" : "interrupted") : failed ? "error" : state;
  const label = status === "running" ? "正在联网检索…" : status === "interrupted" ? "联网检索未完成" : status === "error" ? "联网检索失败" : status === "degraded" ? "部分检索结果" : "已联网检索";
  return <Disclosure className="search-tool"><Disclosure.Heading><Disclosure.Trigger className="search-trigger">
    <Globe className="search-tool-icon" size={18} aria-hidden="true" />
    <span className={`search-status search-status--${status}`}>{label}</span>
    {status === "running" ? <LoaderCircle className="tool-status-icon tool-status-icon--running" size={16} aria-hidden="true" /> : status !== "interrupted" && <StatusIcon state={status} />}
    <Disclosure.Indicator />
  </Disclosure.Trigger></Disclosure.Heading><Disclosure.Content>
    <small className={failed ? "search-query search-query--error" : "search-query"}>{summary(call.arguments.query ?? call.arguments.queries)}{failed && ` · ${String(data.error ?? "请检查网络或检索配置")}`}</small>
  </Disclosure.Content></Disclosure>;
}

export function ToolCard({ call, result, live, startedAt }: { call: Call; result?: Message; live?: boolean; startedAt?: number }) {
  const special = isStandaloneTool(call, result);
  const [open, setOpen] = useState(special || !!live);
  const [touched, setTouched] = useState(false);
  useEffect(() => { if (!touched && (live || special)) setOpen(true); }, [live, special, touched]);
  const [raw, setRaw] = useState(false);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (result || !live || !startedAt) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [result, live, startedAt]);
  const view = result ? mapTool(call, result) : undefined;
  const details = result && "details" in result ? (result as Message & { details?: unknown }).details : undefined;
  const state = toolState(result, view?.status, live);
  if (call.name === "web_search") {
    const data = details && typeof details === "object" ? details as Record<string, unknown> : {};
    const failed = !!result && (view?.status === "error" || typeof data.successfulQueries === "number" && data.successfulQueries === 0);
    return <SearchRow call={call} result={result} live={live} state={state} failed={failed} />;
  }
  const status = stateLabels[state];
  const scope = typeof call.arguments.scope === "string" ? call.arguments.scope : undefined;
  const invocation = `${call.name}${scope ? `(${scope})` : ""}`;
  const argumentSummary = Object.fromEntries(Object.entries(call.arguments).filter(([key]) => key !== "items" && key !== "data"));
  // 画布把参数摘要写成 key: value 风格。
  const callText = `${call.name}(${Object.entries(argumentSummary).map(([key, value]) => `${key}: ${JSON.stringify(value)}`).join(", ")}${Array.isArray(call.arguments.items) ? `${Object.keys(argumentSummary).length ? ", " : ""}items: ${call.arguments.items.length}` : ""})`;
  const elapsed = startedAt && (result?.timestamp ?? (live ? now : 0)) >= startedAt
    ? `${(((result?.timestamp ?? now) - startedAt) / 1000).toFixed(1)}s` : undefined;
  const heading = <><StatusIcon state={state} />
    <span>{state === "running" ? `正在调用 ${call.name} 工具` : `${invocation} · ${status}`}</span>
    {elapsed && <time className="tool-elapsed">{elapsed}</time>}</>;
  const content = <div className="tool-card-detail">
    <Card.Content><div className="tool-card-body">
      <code className="tool-invocation">{callText}</code>
      {!view ? (state === "running" ? null : <p>本次调用没有返回结果。</p>) : view.blocks.map((block) => <div key={block.id}>{renderBlock(block)}</div>)}
    </div></Card.Content>
    {result && <Disclosure isExpanded={raw} onExpandedChange={setRaw}>
      <Disclosure.Heading><Disclosure.Trigger className="tool-raw-toggle"><Braces size={12} aria-hidden="true" /><span>原始数据</span><span className="tool-raw-spacer" /><Disclosure.Indicator><ChevronDown size={12} /></Disclosure.Indicator></Disclosure.Trigger></Disclosure.Heading>
      <Disclosure.Content><pre className="tool-raw">{(view?.raw || JSON.stringify(details) || "无原始结果").slice(0, 100000)}{(view?.raw.length ?? 0) > 100000 && "\n……显示已截断"}</pre></Disclosure.Content>
    </Disclosure>}
  </div>;
  return <Card variant="transparent" className={`tool-card ${special ? "tool-card--special" : ""} ${state === "error" ? "tool-card--error" : ""}`}>
    {special ? <><Card.Header><div className="tool-card-heading tool-static-heading">{heading}</div></Card.Header>{content}</> :
      <Disclosure isExpanded={open} onExpandedChange={(value) => { setTouched(true); setOpen(value); }}>
        <Card.Header><Disclosure.Heading><Disclosure.Trigger className="tool-card-heading">{heading}</Disclosure.Trigger></Disclosure.Heading></Card.Header>
        <Disclosure.Content>{content}</Disclosure.Content>
      </Disclosure>}
  </Card>;
}

function CollapsedToolGroup({ calls, results, live, startedAt }: { calls: Call[]; results: Map<string, Message>; live?: boolean; startedAt?: number }) {
  const [open, setOpen] = useState(false);
  const states = calls.map((call) => {
      const result = results.get(call.id);
      return result ? mapTool(call, result).status : undefined;
    });
    const failed = states.includes("error");
    const degraded = states.includes("degraded");
    const finished = states.every((state) => state != null);
    return <Disclosure className="tool-group" isExpanded={open} onExpandedChange={setOpen}>
      <Disclosure.Heading><Disclosure.Trigger className="conversation-status tool-group-heading">
        {!finished ? <StatusIcon state={live ? "running" : "interrupted"} size={18} /> : failed ? <StatusIcon state="error" size={18} /> : degraded ? <StatusIcon state="degraded" size={18} /> : <CheckCheck size={18} className="success-icon" aria-hidden="true" />}
        <span className={!finished && live ? "tool-group-state--running" : undefined}>{!finished ? (live ? "正在调用多个工具…" : "检查未完成") : failed ? "部分检查失败" : degraded ? "检查返回部分结果" : "已完成检查"}</span>
        <small>{calls.length} 次工具调用</small><Disclosure.Indicator />
      </Disclosure.Trigger></Disclosure.Heading>
      <Disclosure.Content>{calls.map((call) => <ToolCard key={call.id} call={call} result={results.get(call.id)} live={live} startedAt={startedAt} />)}</Disclosure.Content>
    </Disclosure>;
}

export function ToolGroup({ calls, results, live, startedAt }: { calls: Call[]; results: Map<string, Message>; live?: boolean; startedAt?: number }) {
  return <>{splitToolCalls(calls, results).map((segment, index) => segment.kind === "standalone"
    ? <ToolCard key={`${index}-${segment.calls[0].id}`} call={segment.calls[0]} result={results.get(segment.calls[0].id)} live={live} startedAt={startedAt} />
    : <CollapsedToolGroup key={`${index}-${segment.calls[0].id}`} calls={segment.calls} results={results} live={live} startedAt={startedAt} />)}</>;
}
