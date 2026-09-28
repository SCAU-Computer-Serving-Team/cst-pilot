import { Button, Label, ListBox, Select, TextArea } from "@heroui/react";
import { ArrowUp, Check, ChevronDown, Command, Plus, Sparkles, Square } from "lucide-react";
import { type ClipboardEvent, type CSSProperties, type KeyboardEvent, useEffect, useRef, useState } from "react";
import { apiJson } from "./api";
import { addImage, encodeImages, loadImages, type DraftImage } from "./draft-images";

type Model = { id: string; provider: string; name: string; reasoning: boolean };
type Selection = { provider: string; id: string; thinkingLevel?: string } | null;
const thinkingOptions = [
  { id: "off", label: "关闭", description: "直接回答，不展示思考过程" },
  { id: "minimal", label: "极低", description: "尽量减少推理" },
  { id: "low", label: "低", description: "少量推理，响应更快" },
  { id: "medium", label: "中", description: "适度推理，平衡速度与质量" },
  { id: "high", label: "高", description: "充分推理，复杂问题优先" },
  { id: "xhigh", label: "最高", description: "尽可能深入推理" },
];
export type ComposerConfig = { provider?: string; modelId?: string; thinkingLevel?: string; messageId: string; images: { mimeType: string; data: string }[]; skill?: string };
const commandLabels: Record<string, string> = { "/compact": "压缩会话", "/fork": "派生会话", "/skill:disk": "磁盘诊断", "/skill:driver": "驱动检查", "/skill:eventlog": "事件日志", "/skill:ls": "目录占用", "/skill:runbook": "命令清单", "/skill:startup": "开机自启", "/skill:sys": "系统状态" };

function ImagePreview({ src, onClose }: { src: string; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  return <dialog ref={dialog} className="image-preview" onClose={onClose} onClick={(event) => { if (event.target === dialog.current) dialog.current?.close(); }}>
    <button type="button" aria-label="关闭图片预览" onClick={() => dialog.current?.close()}>关闭</button>
    <img src={src} alt="待发送图片的大图预览" />
  </dialog>;
}

export function Composer({ home = false, sessionId, running = false, queueActive = false, contextUsage, onSend, onStop, onCommand }: {
  home?: boolean;
  sessionId?: string;
  running?: boolean;
  queueActive?: boolean;
  contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null } | null;
  onSend?: (text: string, config: ComposerConfig) => Promise<void>;
  onStop?: () => Promise<void>;
  onCommand?: (name: "compact" | "fork", text: string) => Promise<void>;
}) {
  const draftKey = `cst-draft:${sessionId ?? "new"}`;
  const [text, setText] = useState(() => typeof window !== "undefined" ? sessionStorage.getItem(draftKey) ?? "" : "");
  const [messageId, setMessageId] = useState(() => typeof window !== "undefined" ? sessionStorage.getItem(`${draftKey}:id`) ?? crypto.randomUUID() : "");
  const [images, setImages] = useState<DraftImage[]>([]);
  const [previewImage, setPreviewImage] = useState<string | null>(null);
  const imagesRef = useRef(images);
  imagesRef.current = images;
  const fileInput = useRef<HTMLInputElement>(null);
  const [models, setModels] = useState<Model[]>([]);
  const [modelQuery, setModelQuery] = useState("");
  const [loadedContext, setLoadedContext] = useState<{ tokens: number | null; contextWindow: number; percent: number | null } | null>(null);
  const [selected, setSelected] = useState<Selection>(null);
  const [thinking, setThinking] = useState("off");
  const [sending, setSending] = useState(false);
  const [commands, setCommands] = useState<string[]>([]);
  const [command, setCommand] = useState("");
  const [commandIndex, setCommandIndex] = useState(0);
  const [suggestionsVisible, setSuggestionsVisible] = useState(true);
  const suggestions = !command && suggestionsVisible && /^\/[\w:-]*$/.test(text) ? commands.filter((item) => item.startsWith(text)) : [];
  function selectCommand(name: string) { setCommand(name); setText(""); setCommandIndex(0); }
  const [error, setError] = useState("");

  useEffect(() => { setText(sessionStorage.getItem(draftKey) ?? ""); setMessageId(sessionStorage.getItem(`${draftKey}:id`) ?? crypto.randomUUID()); }, [draftKey]);
  useEffect(() => { sessionStorage.setItem(draftKey, text); sessionStorage.setItem(`${draftKey}:id`, messageId); }, [draftKey, text, messageId]);
  useEffect(() => {
    let active = true;
    const stored = JSON.parse(sessionStorage.getItem(`${draftKey}:images`) ?? "[]") as string[];
    void loadImages(stored).then((loaded) => { if (active) setImages(loaded); else loaded.forEach((image) => URL.revokeObjectURL(image.url)); })
      .catch(() => { if (active) setError("无法恢复未发送的图片"); });
    return () => { active = false; imagesRef.current.forEach((image) => URL.revokeObjectURL(image.url)); };
  }, [draftKey]);
  async function attach(files: File[]) {
    const added: DraftImage[] = [];
    try {
      let total = imagesRef.current.reduce((sum, image) => sum + image.blob.size, 0);
      for (const file of files) { const image = await addImage(file, total); total += file.size; added.push(image); }
      const next = [...imagesRef.current, ...added];
      sessionStorage.setItem(`${draftKey}:images`, JSON.stringify(next.map((image) => image.hash)));
      setImages(next); setMessageId(crypto.randomUUID()); setError("");
    } catch (cause) {
      added.forEach((image) => URL.revokeObjectURL(image.url));
      setError(cause instanceof Error ? cause.message : "无法添加图片");
    }
  }
  function removeImage(index: number) {
    const removed = imagesRef.current[index];
    if (!removed) return;
    if (previewImage === removed.url) setPreviewImage(null);
    URL.revokeObjectURL(removed.url);
    const next = imagesRef.current.filter((_, at) => at !== index);
    sessionStorage.setItem(`${draftKey}:images`, JSON.stringify(next.map((image) => image.hash)));
    setImages(next); setMessageId(crypto.randomUUID());
  }
  function paste(event: ClipboardEvent<HTMLTextAreaElement>) {
    const files = [...event.clipboardData.items].filter((item) => item.kind === "file").map((item) => item.getAsFile()).filter((file): file is File => !!file);
    if (files.length) { event.preventDefault(); void attach(files); }
  }
  useEffect(() => {
    let active = true;
    apiJson<{ commands: string[] }>("/api/state").then((state) => { if (active) setCommands(state.commands.filter((name) => name in commandLabels)); })
      .catch(() => { if (active) setCommands(["/compact", "/fork"]); });
    return () => { active = false; };
  }, []);
  useEffect(() => {
    let active = true;
    apiJson<{ models: Model[]; selected: (Selection & { thinkingLevel?: string }); contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null } }>(`/api/models${sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : ""}`)
      .then((data) => { if (active) { setModels(data.models); setSelected(data.selected); setThinking(data.selected?.thinkingLevel ?? "off"); setLoadedContext(data.contextUsage ?? null); } })
      .catch((cause: unknown) => { if (active) setError(cause instanceof Error ? cause.message : "模型列表加载失败"); });
    return () => { active = false; };
  }, [sessionId]);

  const usage = contextUsage ?? loadedContext;
  const usagePercent = usage?.percent ?? 0;
  const modelKey = selected ? `${selected.provider}/${selected.id}` : "auto";
  const modelMatches = models.filter((item) => `${item.provider} ${item.name} ${item.id}`.toLowerCase().includes(modelQuery.toLowerCase()));
  async function chooseModel(key: string) {
    if (!sessionId && key === "auto") { setSelected(null); setModelQuery(""); return; }
    const choice = models.find((item) => `${item.provider}/${item.id}` === key);
    if (!choice) return;
    try {
      if (sessionId) await apiJson("/api/models/select", { method: "POST", body: { sessionId, provider: choice.provider, modelId: choice.id } });
      setSelected({ provider: choice.provider, id: choice.id }); setModelQuery(""); setError("");
    } catch (cause) { setError(cause instanceof Error ? cause.message : "无法切换模型"); }
  }
  async function chooseThinking(level: string) {
    try {
      if (sessionId) await apiJson("/api/models/thinking", { method: "POST", body: { sessionId, level } });
      setThinking(level); setError("");
    } catch (cause) { setError(cause instanceof Error ? cause.message : "无法调整思考强度"); }
  }
  async function submit() {
    if (sending) return;
    if (!text.trim() && !images.length && !command && running) { if (onStop) { setSending(true); try { await onStop(); setError(""); } catch (cause) { setError(cause instanceof Error ? cause.message : "停止失败"); } finally { setSending(false); } } return; }
    if ((!text.trim() && !images.length && !command) || (!onSend && !onCommand)) return;
    setSending(true); setError("");
    try {
      if (command === "/compact" || command === "/fork") {
        if (!onCommand) throw new Error("请先进入会话，再使用此命令。");
        await onCommand(command.slice(1) as "compact" | "fork", text);
      } else {
        if (!onSend) throw new Error("当前无法提交消息");
        await onSend(text, { provider: selected?.provider, modelId: selected?.id, thinkingLevel: thinking, messageId, images: await encodeImages(images), skill: command.startsWith("/skill:") ? command.slice(7) : undefined });
      }
      setCommand(""); setPreviewImage(null);
      images.forEach((image) => URL.revokeObjectURL(image.url)); setImages([]); sessionStorage.removeItem(`${draftKey}:images`);
      setText(""); setMessageId(crypto.randomUUID()); sessionStorage.removeItem(draftKey); sessionStorage.removeItem(`${draftKey}:id`);
    } catch (cause) { setError(cause instanceof Error ? cause.message : "消息未被接受，请重试"); }
    finally { setSending(false); }
  }
  function keyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (suggestions.length && ["ArrowUp", "ArrowDown", "Tab", "Enter", "Escape"].includes(event.key)) {
      event.preventDefault();
      if (event.key === "ArrowUp") setCommandIndex((value) => (value - 1 + suggestions.length) % suggestions.length);
      else if (event.key === "ArrowDown") setCommandIndex((value) => (value + 1) % suggestions.length);
      else if (event.key === "Escape") setSuggestionsVisible(false);
      else selectCommand(suggestions[commandIndex] ?? suggestions[0]);
      return;
    }
    if (event.key === "Escape" && command) { event.preventDefault(); setCommand(""); return; }
    if (event.key === "Escape" && running) { event.preventDefault(); void onStop?.(); return; }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault(); void submit();
    }
  }
  return (
    <div className={`composer ${home ? "composer--home" : "composer--chat"}`} aria-label="消息编辑器">
      {command && <div className="composer-command"><span>{command}</span><Button variant="ghost" isIconOnly aria-label="移除命令" onPress={() => setCommand("")}>×</Button></div>}
      <TextArea aria-label="消息" className="composer-input" value={text} onChange={(event) => { setText(event.target.value); setCommandIndex(0); setSuggestionsVisible(true); setMessageId(crypto.randomUUID()); }} onKeyDown={keyDown} onPaste={paste}
        placeholder={home ? "描述这台电脑遇到的问题…" : queueActive ? "继续输入以排队后续修改" : "继续提问，或补充这台电脑的情况…"} rows={home ? 1 : 2} />
      {!!suggestions.length && <div className="command-suggestions" role="group" aria-label="命令补全">{([false, true] as const).map((skill) => {
        const group = suggestions.filter((item) => item.startsWith("/skill:") === skill);
        return group.length ? <div key={String(skill)} className="command-group"><span className="command-group-title">{skill ? "技能" : "命令"}</span>{group.map((item) => <Button key={item} className={suggestions.indexOf(item) === commandIndex ? "command-active" : ""} variant="ghost" onPress={() => selectCommand(item)}>{skill ? <Sparkles size={16} /> : <Command size={16} />}<span>{item}</span><small>{commandLabels[item]}</small></Button>)}</div> : null;
      })}</div>}
      {!!images.length && <div className="draft-images" aria-label="待发送图片">{images.map((image, index) => <div className="draft-image" key={`${image.hash}-${index}`}>
        <button className="draft-image-open" type="button" aria-label={`查看图片 ${index + 1}`} onClick={() => setPreviewImage(image.url)}><img src={image.url} alt={`待发送图片 ${index + 1}`} /></button><Button variant="ghost" isIconOnly aria-label={`移除图片 ${index + 1}`} onPress={() => removeImage(index)}>×</Button>
      </div>)}</div>}
      {previewImage && <ImagePreview src={previewImage} onClose={() => setPreviewImage(null)} />}
      {error && <p className="composer-error" role="alert">{error}</p>}
      <div className="composer-actions">
        <input ref={fileInput} className="visually-hidden" type="file" accept="image/png,image/jpeg,image/gif,image/webp" multiple aria-label="选择图片" onChange={(event) => { void attach([...event.target.files ?? []]); event.target.value = ""; }} />
        <Button variant="ghost" isIconOnly className="composer-icon" aria-label="添加图片" onPress={() => fileInput.current?.click()}><Plus size={20} /></Button>
        <span className="composer-spacer" />
        <Select aria-label="模型" selectedKey={modelKey} onSelectionChange={(key) => { if (key != null) void chooseModel(String(key)); }} className="model-select">
          <Label className="visually-hidden">模型</Label>
          <Select.Trigger className="composer-model">{!home && <span className="context-meter"><span className="context-ring" style={{ "--context-progress": `${Math.max(0, Math.min(100, usagePercent))}%` } as CSSProperties} data-level={usagePercent >= 90 ? "danger" : usagePercent >= 75 ? "warning" : "normal"} aria-label={usage?.percent == null ? "上下文用量未知" : `上下文用量 ${Math.round(usagePercent)}%`} /><span className="context-panel" role="tooltip"><strong>上下文容量</strong><span>{usage?.tokens == null ? "用量未知" : `${usage.tokens.toLocaleString("zh-CN")} / ${usage.contextWindow.toLocaleString("zh-CN")}（${usage.percent == null ? "占比未知" : `${usage.percent.toFixed(1)}%`}）`}</span><span className="context-progress" aria-hidden="true"><span style={{ width: `${Math.max(0, Math.min(100, usagePercent))}%` }} /></span></span></span>}{models.find((item) => `${item.provider}/${item.id}` === modelKey)?.name ?? "自动选择"}<ChevronDown size={13} /></Select.Trigger>
          <Select.Popover className="composer-popover model-popover" placement="top end"><div className="model-search"><input aria-label="搜索模型" value={modelQuery} onChange={(event) => setModelQuery(event.target.value)} placeholder="搜索 Provider 或模型名称" /><small>显示 {Math.min(modelMatches.length, 80)} / {modelMatches.length}</small></div><ListBox>
            {!sessionId && <ListBox.Item id="auto" textValue="自动选择"><Check className="model-option-check" size={16} aria-hidden="true" /><span>自动选择</span></ListBox.Item>}
            {modelMatches.slice(0, 80).map((model) => <ListBox.Item key={`${model.provider}/${model.id}`} id={`${model.provider}/${model.id}`} textValue={`${model.provider} ${model.name}`}><Check className="model-option-check" size={16} aria-hidden="true" /><span title={`${model.provider} · ${model.name}`}>{model.name}</span></ListBox.Item>)}
          </ListBox></Select.Popover>
        </Select>
        {!home && <Select aria-label="思考强度" selectedKey={thinking} onSelectionChange={(key) => { if (key != null) void chooseThinking(String(key)); }} className="thinking-select">
          <Label className="visually-hidden">思考强度</Label>
          <Select.Trigger className="composer-thinking">{thinking === "off" ? "思考关闭" : `思考 · ${thinkingOptions.find((option) => option.id === thinking)?.label ?? thinking}`}<ChevronDown size={12} /></Select.Trigger>
          <Select.Popover className="composer-popover thinking-popover" placement="top end"><ListBox>{thinkingOptions.map((option) => <ListBox.Item id={option.id} key={option.id} textValue={option.label}><span className="thinking-option-copy"><span>{option.label}</span><small>{option.description}</small></span><Check className="model-option-check" size={16} aria-hidden="true" /></ListBox.Item>)}</ListBox></Select.Popover>
        </Select>}
        <Button isIconOnly isDisabled={sending || (!text.trim() && !images.length && !command && !running) || (!onSend && !onStop && !onCommand)} className="composer-send"
          aria-label={running && !text.trim() && !images.length && !command ? "停止生成" : "发送消息"} onPress={() => void submit()}>{running && !text.trim() && !images.length && !command ? <Square size={15} /> : <ArrowUp size={16} />}</Button>
      </div>
    </div>
  );
}
