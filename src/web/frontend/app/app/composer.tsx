import { Button, Label, ListBox, Select, TextArea } from "@heroui/react";
import {
	ArrowUp,
	Brain,
	Check,
	ChevronDown,
	Command,
	FileText,
	PencilLine,
	Plus,
	Sparkles,
	Square,
	X,
} from "lucide-react";
import { type ClipboardEvent, type KeyboardEvent, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { apiJson } from "./api";
import { ContextMeter } from "./context-meter";
import { addImage, type DraftImage, encodeImages, loadImages } from "./draft-images";

type Model = { id: string; provider: string; name: string; reasoning: boolean };
type Selection = { provider: string; id: string; thinkingLevel?: string } | null;
// 档位与 pi 的 ThinkingLevel 一一对应（off/minimal/low/medium/high/xhigh/max），面板内用英文短名。
const thinkingOptions = [
	{ id: "off", label: "Off" },
	{ id: "minimal", label: "Minimal" },
	{ id: "low", label: "Low" },
	{ id: "medium", label: "Medium" },
	{ id: "high", label: "High" },
	{ id: "xhigh", label: "XHigh" },
	{ id: "max", label: "Max" },
];
export type ComposerConfig = {
	provider?: string;
	modelId?: string;
	thinkingLevel?: string;
	messageId: string;
	images: { mimeType: string; data: string }[];
	skill?: string;
};
const commandLabels: Record<string, string> = {
	"/compact": "压缩会话",
	"/fork": "派生会话",
	"/tree": "打开分支树",
	"/skill:disk": "磁盘诊断",
	"/skill:driver": "驱动检查",
	"/skill:eventlog": "事件日志",
	"/skill:ls": "目录占用",
	"/skill:runbook": "命令清单",
	"/skill:startup": "开机自启",
	"/skill:sys": "系统状态",
};

function ImagePreview({ src, onClose }: { src: string; onClose: () => void }) {
	const dialog = useRef<HTMLDialogElement>(null);
	useEffect(() => {
		dialog.current?.showModal();
	}, []);
	return (
		// biome-ignore lint/a11y/useKeyWithClickEvents: 键盘关闭由 dialog 原生 Escape 触发 onClose，这里的点击只处理鼠标点背景关闭
		<dialog
			ref={dialog}
			className="image-preview"
			onClose={onClose}
			onClick={(event) => {
				if (event.target === dialog.current) dialog.current?.close();
			}}
		>
			<button type="button" aria-label="关闭图片预览" onClick={() => dialog.current?.close()}>
				关闭
			</button>
			<img src={src} alt="待发送图片的大图预览" />
		</dialog>
	);
}

type SessionUsage = {
	tokens: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number };
	cacheHitRate: number | null;
};
export function Composer({
	home = false,
	sessionId,
	running = false,
	queueActive = false,
	contextUsage,
	summaryTag,
	onSend,
	onStop,
	onCommand,
	onSummary,
}: {
	home?: boolean;
	sessionId?: string;
	running?: boolean;
	queueActive?: boolean;
	contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null } | null;
	/** 待填的自定义总结提示词：标记块在输入框内，回车走总结而不是发消息。 */
	summaryTag?: { label: string; onCancel: () => void };
	onSend?: (text: string, config: ComposerConfig) => Promise<void>;
	onStop?: () => Promise<void>;
	onCommand?: (name: "compact" | "fork", text: string) => Promise<void>;
	onSummary?: (instructions: string) => Promise<void>;
}) {
	const draftKey = `cst-draft:${sessionId ?? "new"}`;
	const [text, setText] = useState(() =>
		typeof window !== "undefined" ? (sessionStorage.getItem(draftKey) ?? "") : "",
	);
	const [messageId, setMessageId] = useState(() =>
		typeof window !== "undefined" ? (sessionStorage.getItem(`${draftKey}:id`) ?? crypto.randomUUID()) : "",
	);
	const [images, setImages] = useState<DraftImage[]>([]);
	const [previewImage, setPreviewImage] = useState<string | null>(null);
	const imagesRef = useRef(images);
	imagesRef.current = images;
	const fileInput = useRef<HTMLInputElement>(null);
	const [models, setModels] = useState<Model[]>([]);
	const [enabled, setEnabled] = useState<string[] | null>(null);
	const [modelQuery, setModelQuery] = useState("");
	const [loadedContext, setLoadedContext] = useState<{
		tokens: number | null;
		contextWindow: number;
		percent: number | null;
	} | null>(null);
	const [usageStats, setUsageStats] = useState<SessionUsage | null>(null);
	const [selected, setSelected] = useState<Selection>(null);
	const [thinking, setThinking] = useState("off");
	const [thinkingLevels, setThinkingLevels] = useState<string[]>([]);
	const [sending, setSending] = useState(false);
	const [commands, setCommands] = useState<string[]>([]);
	const [command, setCommand] = useState("");
	const [commandIndex, setCommandIndex] = useState(0);
	const [suggestionsVisible, setSuggestionsVisible] = useState(true);
	const [files, setFiles] = useState<{ path: string; dir: string }[]>([]);
	const [fileIndex, setFileIndex] = useState(0);
	const [filesVisible, setFilesVisible] = useState(true);
	const suggestions =
		!command && suggestionsVisible && /^\/[\w:-]*$/.test(text)
			? commands.filter((item) => item.startsWith(text))
			: [];
	// `@` 文件引用：光标位于行尾的 @query 时列出项目文件。
	const fileMatch = filesVisible ? /(?:^|\s)@([^\s@]*)$/.exec(text) : null;
	const fileSuggestions = fileMatch
		? files.filter((file) => file.path.toLowerCase().includes((fileMatch[1] ?? "").toLowerCase())).slice(0, 40)
		: [];
	const filesOpen = !!fileMatch;
	useEffect(() => {
		if (!filesOpen || files.length) return;
		let active = true;
		apiJson<{ files: { path: string; dir: string }[] }>("/api/files")
			.then((data) => {
				if (active) setFiles(data.files);
			})
			.catch((cause: unknown) => {
				if (active) setError(cause instanceof Error ? cause.message : "文件列表加载失败");
			});
		return () => {
			active = false;
		};
	}, [filesOpen, files.length]);
	// 视图类命令（/tree）在补全面板里选中即跳转，不折叠成标记，也不等回车。
	function selectCommand(name: string) {
		if (name === "/tree") {
			setSuggestionsVisible(false);
			setCommandIndex(0);
			if (!sessionId) {
				setCommand("");
				setText("");
				setError("请先进入会话，再使用此命令。");
				return;
			}
			setCommand("");
			setText("");
			navigate(`/s/${sessionId}/tree`);
			return;
		}
		setCommand(name);
		setText("");
		setCommandIndex(0);
	}
	function selectFile(path: string) {
		const at = text.lastIndexOf("@");
		setText(`${at >= 0 ? text.slice(0, at) : ""}@${path} `);
		setFileIndex(0);
		setFilesVisible(false);
	}
	const [error, setError] = useState("");
	const navigate = useNavigate();

	useEffect(() => {
		setText(sessionStorage.getItem(draftKey) ?? "");
		setMessageId(sessionStorage.getItem(`${draftKey}:id`) ?? crypto.randomUUID());
	}, [draftKey]);
	useEffect(() => {
		sessionStorage.setItem(draftKey, text);
		sessionStorage.setItem(`${draftKey}:id`, messageId);
	}, [draftKey, text, messageId]);
	useEffect(() => {
		let active = true;
		const stored = JSON.parse(sessionStorage.getItem(`${draftKey}:images`) ?? "[]") as string[];
		void loadImages(stored)
			.then((loaded) => {
				if (active) setImages(loaded);
				else
					loaded.forEach((image) => {
						URL.revokeObjectURL(image.url);
					});
			})
			.catch(() => {
				if (active) setError("无法恢复未发送的图片");
			});
		return () => {
			active = false;
			imagesRef.current.forEach((image) => {
				URL.revokeObjectURL(image.url);
			});
		};
	}, [draftKey]);
	async function attach(files: File[]) {
		const added: DraftImage[] = [];
		try {
			let total = imagesRef.current.reduce((sum, image) => sum + image.blob.size, 0);
			for (const file of files) {
				const image = await addImage(file, total);
				total += file.size;
				added.push(image);
			}
			const next = [...imagesRef.current, ...added];
			sessionStorage.setItem(`${draftKey}:images`, JSON.stringify(next.map((image) => image.hash)));
			setImages(next);
			setMessageId(crypto.randomUUID());
			setError("");
		} catch (cause) {
			added.forEach((image) => {
				URL.revokeObjectURL(image.url);
			});
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
		setImages(next);
		setMessageId(crypto.randomUUID());
	}
	function paste(event: ClipboardEvent<HTMLTextAreaElement>) {
		const files = [...event.clipboardData.items]
			.filter((item) => item.kind === "file")
			.map((item) => item.getAsFile())
			.filter((file): file is File => !!file);
		if (files.length) {
			event.preventDefault();
			void attach(files);
		}
	}
	useEffect(() => {
		let active = true;
		apiJson<{ commands: string[] }>("/api/state")
			.then((state) => {
				if (active) setCommands(state.commands.filter((name) => name in commandLabels));
			})
			.catch(() => {
				if (active) setCommands(["/compact", "/fork"]);
			});
		return () => {
			active = false;
		};
	}, []);
	useEffect(() => {
		let active = true;
		apiJson<{
			models: Model[];
			enabled: string[] | null;
			selected: Selection & { thinkingLevel?: string };
			thinkingLevels?: string[];
			contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null };
			usage?: SessionUsage | null;
		}>(`/api/models${sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : ""}`)
			.then((data) => {
				if (active) {
					setModels(data.models);
					setEnabled(data.enabled?.length ? data.enabled : null);
					setSelected(data.selected);
					setThinking(data.selected?.thinkingLevel ?? "off");
					setThinkingLevels(data.thinkingLevels ?? []);
					setLoadedContext(data.contextUsage ?? null);
					setUsageStats(data.usage ?? null);
				}
			})
			.catch((cause: unknown) => {
				if (active) setError(cause instanceof Error ? cause.message : "模型列表加载失败");
			});
		return () => {
			active = false;
		};
	}, [sessionId]);

	const usage = contextUsage ?? loadedContext;
	const cacheHitRate = usageStats?.cacheHitRate ?? null;
	const modelKey = selected ? `${selected.provider}/${selected.id}` : "auto";
	const availableThinking = thinkingLevels.length
		? thinkingOptions.filter((option) => thinkingLevels.includes(option.id))
		: thinkingOptions;
	const scoped =
		enabled === null
			? models
			: models.filter(
					(item) =>
						enabled.includes(`${item.provider}/${item.id}`) ||
						(selected && item.provider === selected.provider && item.id === selected.id),
				);
	const modelMatches = scoped.filter((item) =>
		`${item.provider} ${item.name} ${item.id}`.toLowerCase().includes(modelQuery.toLowerCase()),
	);
	async function chooseModel(key: string) {
		if (!sessionId && key === "auto") {
			setSelected(null);
			setModelQuery("");
			return;
		}
		const choice = models.find((item) => `${item.provider}/${item.id}` === key);
		if (!choice) return;
		try {
			if (sessionId) {
				const changed = await apiJson<{ thinkingLevel?: string; thinkingLevels?: string[] }>("/api/models/select", {
					method: "POST",
					body: { sessionId, provider: choice.provider, modelId: choice.id },
				});
				if (changed.thinkingLevel) setThinking(changed.thinkingLevel);
				if (changed.thinkingLevels) setThinkingLevels(changed.thinkingLevels);
			}
			setSelected({ provider: choice.provider, id: choice.id });
			setModelQuery("");
			setError("");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "无法切换模型");
		}
	}
	async function chooseThinking(level: string) {
		try {
			if (sessionId) await apiJson("/api/models/thinking", { method: "POST", body: { sessionId, level } });
			setThinking(level);
			setError("");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "无法调整思考强度");
		}
	}
	async function submit() {
		if (sending) return;
		// 自定义总结提示词：回车不是发消息，而是带着这段提示词去发起导航与总结。
		if (summaryTag) {
			if (!text.trim() || !onSummary) return;
			setSending(true);
			setError("");
			try {
				await onSummary(text.trim());
				setText("");
				setMessageId(crypto.randomUUID());
				sessionStorage.removeItem(draftKey);
				sessionStorage.removeItem(`${draftKey}:id`);
			} catch (cause) {
				setError(cause instanceof Error ? cause.message : "分支总结未开始，请重试");
			} finally {
				setSending(false);
			}
			return;
		}
		if (!text.trim() && !images.length && !command && running) {
			if (onStop) {
				setSending(true);
				try {
					await onStop();
					setError("");
				} catch (cause) {
					setError(cause instanceof Error ? cause.message : "停止失败");
				} finally {
					setSending(false);
				}
			}
			return;
		}
		if ((!text.trim() && !images.length && !command) || (!onSend && !onCommand)) return;
		setSending(true);
		setError("");
		try {
			if (command === "/tree") {
				if (!sessionId) throw new Error("请先进入会话，再使用此命令。");
				navigate(`/s/${sessionId}/tree`);
			} else if (command === "/compact" || command === "/fork") {
				if (!onCommand) throw new Error("请先进入会话，再使用此命令。");
				await onCommand(command.slice(1) as "compact" | "fork", text);
			} else {
				if (!onSend) throw new Error("当前无法提交消息");
				await onSend(text, {
					provider: selected?.provider,
					modelId: selected?.id,
					thinkingLevel: thinking,
					messageId,
					images: await encodeImages(images),
					skill: command.startsWith("/skill:") ? command.slice(7) : undefined,
				});
			}
			setCommand("");
			setPreviewImage(null);
			images.forEach((image) => {
				URL.revokeObjectURL(image.url);
			});
			setImages([]);
			sessionStorage.removeItem(`${draftKey}:images`);
			setText("");
			setMessageId(crypto.randomUUID());
			sessionStorage.removeItem(draftKey);
			sessionStorage.removeItem(`${draftKey}:id`);
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "消息未被接受，请重试");
		} finally {
			setSending(false);
		}
	}
	function keyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
		if (event.nativeEvent.isComposing || event.keyCode === 229) return;
		if (fileSuggestions.length && ["ArrowUp", "ArrowDown", "Tab", "Enter", "Escape"].includes(event.key)) {
			event.preventDefault();
			if (event.key === "ArrowUp")
				setFileIndex((value) => (value - 1 + fileSuggestions.length) % fileSuggestions.length);
			else if (event.key === "ArrowDown") setFileIndex((value) => (value + 1) % fileSuggestions.length);
			else if (event.key === "Escape") setFilesVisible(false);
			else selectFile((fileSuggestions[fileIndex] ?? fileSuggestions[0]).path);
			return;
		}
		if (suggestions.length && ["ArrowUp", "ArrowDown", "Tab", "Enter", "Escape"].includes(event.key)) {
			event.preventDefault();
			if (event.key === "ArrowUp") setCommandIndex((value) => (value - 1 + suggestions.length) % suggestions.length);
			else if (event.key === "ArrowDown") setCommandIndex((value) => (value + 1) % suggestions.length);
			else if (event.key === "Escape") setSuggestionsVisible(false);
			else selectCommand(suggestions[commandIndex] ?? suggestions[0]);
			return;
		}
		if (event.key === "Escape" && command) {
			event.preventDefault();
			setCommand("");
			return;
		}
		if (event.key === "Escape" && running) {
			event.preventDefault();
			void onStop?.();
			return;
		}
		if (event.key === "Enter" && !event.shiftKey) {
			event.preventDefault();
			void submit();
		}
	}
	return (
		<div className={`composer-shell ${home ? "composer-shell--home" : "composer-shell--chat"}`}>
			{!!images.length && (
				<ul className="composer-attachments" aria-label="待发送图片">
					{images.map((image, index) => (
						<li className="draft-image" key={`${image.hash}-${index}`}>
							<button
								className="draft-image-open"
								type="button"
								aria-label={`查看图片 ${index + 1}`}
								onClick={() => setPreviewImage(image.url)}
							>
								<img src={image.url} alt={`待发送图片 ${index + 1}`} />
							</button>
							<button
								className="draft-image-remove"
								type="button"
								aria-label={`移除图片 ${index + 1}`}
								onClick={() => removeImage(index)}
							>
								<X size={12} aria-hidden="true" />
							</button>
						</li>
					))}
				</ul>
			)}
			<section className={`composer ${home ? "composer--home" : "composer--chat"}`} aria-label="消息编辑器">
				{summaryTag && (
					<div className="composer-command composer-summary-tag">
						<PencilLine size={16} aria-hidden="true" />
						<span>{summaryTag.label}</span>
						<Button variant="ghost" isIconOnly aria-label="取消自定义总结提示词" onPress={summaryTag.onCancel}>
							<X size={14} aria-hidden="true" />
						</Button>
					</div>
				)}
				{command && (
					<div className="composer-command">
						<span>{command}</span>
						<Button variant="ghost" isIconOnly aria-label="移除命令" onPress={() => setCommand("")}>
							×
						</Button>
					</div>
				)}
				<TextArea
					aria-label="消息"
					className="composer-input"
					value={text}
					onChange={(event) => {
						setText(event.target.value);
						setCommandIndex(0);
						setFileIndex(0);
						setSuggestionsVisible(true);
						setFilesVisible(true);
						setMessageId(crypto.randomUUID());
					}}
					onKeyDown={keyDown}
					onPaste={paste}
					placeholder={
						home
							? "描述这台电脑遇到的问题…"
							: summaryTag
								? "写下总结时要保留什么，回车开始总结…"
								: queueActive
									? "继续输入以排队后续修改"
									: "继续提问，或补充这台电脑的情况…"
					}
					rows={home ? 1 : 2}
				/>
				{!!suggestions.length && (
					<fieldset className="command-suggestions" aria-label="命令补全">
						{([false, true] as const).map((skill) => {
							const group = suggestions.filter((item) => item.startsWith("/skill:") === skill);
							return group.length ? (
								<div key={String(skill)} className="command-group">
									<span className="command-group-title">{skill ? "技能" : "命令"}</span>
									{group.map((item) => (
										<Button
											key={item}
											className={suggestions.indexOf(item) === commandIndex ? "command-active" : ""}
											variant="ghost"
											onPress={() => selectCommand(item)}
										>
											{skill ? <Sparkles size={16} /> : <Command size={16} />}
											<span>{item}</span>
											<small>{commandLabels[item]}</small>
										</Button>
									))}
								</div>
							) : null;
						})}
					</fieldset>
				)}
				{!!fileSuggestions.length && (
					<fieldset className="command-suggestions file-suggestions" aria-label="文件引用补全">
						<div className="command-group">
							<span className="command-group-title">文件引用</span>
							{fileSuggestions.map((file, index) => (
								<Button
									key={file.path}
									className={index === fileIndex ? "command-active" : ""}
									variant="ghost"
									onPress={() => selectFile(file.path)}
								>
									<FileText size={16} />
									<span>{file.path.split("/").pop()}</span>
									<small>{file.dir}</small>
								</Button>
							))}
						</div>
					</fieldset>
				)}
				{error && (
					<p className="composer-error" role="alert">
						{error}
					</p>
				)}
				<div className="composer-actions">
					<input
						ref={fileInput}
						className="visually-hidden"
						type="file"
						accept="image/png,image/jpeg,image/gif,image/webp"
						multiple
						aria-label="选择图片"
						onChange={(event) => {
							void attach([...(event.target.files ?? [])]);
							event.target.value = "";
						}}
					/>
					<Button
						variant="ghost"
						isIconOnly
						className="composer-icon"
						aria-label="添加图片"
						onPress={() => fileInput.current?.click()}
					>
						<Plus size={20} />
					</Button>
					<span className="composer-spacer" />
					{!home && <ContextMeter usage={usage} cacheHitRate={cacheHitRate} provider={selected?.provider ?? ""} />}
					<Select
						aria-label="模型"
						selectedKey={modelKey}
						onSelectionChange={(key) => {
							if (key != null) void chooseModel(String(key));
						}}
						className="model-select"
					>
						<Label className="visually-hidden">模型</Label>
						<Select.Trigger className="composer-model">
							<span className="model-label">
								{models.find((item) => `${item.provider}/${item.id}` === modelKey)?.name ?? "自动选择"}
								<ChevronDown size={13} />
							</span>
						</Select.Trigger>
						<Select.Popover className="composer-popover model-popover" placement="top end">
							<div className="model-search">
								<input
									aria-label="搜索模型"
									value={modelQuery}
									onChange={(event) => setModelQuery(event.target.value)}
									placeholder="搜索 Provider 或模型名称"
								/>
								<small>
									显示 {Math.min(modelMatches.length, 80)} / {modelMatches.length}
								</small>
							</div>
							<ListBox>
								{!sessionId && (
									<ListBox.Item id="auto" textValue="自动选择">
										<Check className="model-option-check" size={16} aria-hidden="true" />
										<span>自动选择</span>
									</ListBox.Item>
								)}
								{modelMatches.slice(0, 80).map((model) => (
									<ListBox.Item
										key={`${model.provider}/${model.id}`}
										id={`${model.provider}/${model.id}`}
										textValue={`${model.provider} ${model.name}`}
									>
										<Check className="model-option-check" size={16} aria-hidden="true" />
										<span title={`${model.provider} · ${model.name}`}>{model.name}</span>
									</ListBox.Item>
								))}
							</ListBox>
						</Select.Popover>
					</Select>
					{!home && (
						<Select
							aria-label="思考强度"
							selectedKey={thinking}
							onSelectionChange={(key) => {
								if (key != null) void chooseThinking(String(key));
							}}
							className="thinking-select"
						>
							<Label className="visually-hidden">思考强度</Label>
							<Select.Trigger className="composer-thinking">
								<Brain size={16} aria-hidden="true" />
								{thinking === "off"
									? "Off"
									: (thinkingOptions.find((option) => option.id === thinking)?.label ?? thinking)}
								<ChevronDown size={13} />
							</Select.Trigger>
							<Select.Popover className="composer-popover thinking-popover" placement="top end">
								<ListBox>
									{availableThinking.map((option) => (
										<ListBox.Item id={option.id} key={option.id} textValue={option.label}>
											<span className="thinking-option-copy">
												<span>{option.label}</span>
											</span>
											<Check className="model-option-check" size={16} aria-hidden="true" />
										</ListBox.Item>
									))}
								</ListBox>
							</Select.Popover>
						</Select>
					)}
					<Button
						isIconOnly
						isDisabled={
							sending ||
							(summaryTag
								? !text.trim()
								: (!text.trim() && !images.length && !command && !running) ||
									(!onSend && !onStop && !onCommand))
						}
						className="composer-send"
						aria-label={
							summaryTag
								? "开始分支总结"
								: running && !text.trim() && !images.length && !command
									? "停止生成"
									: "发送消息"
						}
						onPress={() => void submit()}
					>
						{running && !text.trim() && !images.length && !command ? (
							<Square size={12} fill="currentColor" />
						) : (
							<ArrowUp size={16} />
						)}
					</Button>
				</div>
			</section>
			{previewImage && <ImagePreview src={previewImage} onClose={() => setPreviewImage(null)} />}
		</div>
	);
}
