import { Button } from "@heroui/react";
import { ArrowUp, Command, FileText, PencilLine, Plus, Sparkles, Square, X } from "lucide-react";
import { type ClipboardEvent, type DragEvent, type KeyboardEvent, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ModelKeyDialog, type ModelKeyDialogReason } from "../auth/model-key-dialog";
import { readModelKeyPromptTime, shouldAutoPrompt, writeModelKeyPromptTime } from "../auth/model-key-prompt";
import { EDIT_OUTGOING_EVENT } from "../conversation/outgoing-message";
import { ApiFailure, apiJson } from "../data/api";
import { type ModelChoice, type ModelSelectionPayload, refreshManualSelection } from "../data/model-selection";
import { messageOutbox } from "../data/use-message-outbox";
import { type Model, useGlobalEvents } from "../data/web-state";
import { PreviewableImage } from "../images/image-preview";
import { ConfirmDialog } from "../shell/app-dialogs";
import { ErrorNotice } from "../shell/error-notice";
import { chipCaretSpace, editorHtml, extractPayload, parseEditor, replaceTrailingFileQuery } from "./composer-editor";
import { ContextMeter } from "./context-meter";
import { parseImageHashes } from "./draft-image-references";
import { addImage, collectDraftImages, type DraftImage, loadImages, releaseImageHolds } from "./draft-images";
import { draftKey as makeDraftKey, readSavedDrafts, type SavedDraft } from "./drafts";
import { ModelPicker } from "./model-picker";
import { scopedModels } from "./scoped-models";

type Selection = ModelChoice | null;
export type ComposerConfig = {
	provider?: string;
	modelId?: string;
	thinkingLevel?: string;
	messageId: string;
	images: string[];
	serialized: string;
	skill?: string;
};
const commandLabels: Record<string, string> = {
	"/compact": "压缩上下文",
	"/fork": "派生当前会话",
	"/tree": "打开分支树",
	"/skill:bitlocker": "BitLocker 状态、解锁与解密",
	"/skill:disk": "磁盘容量、型号与健康",
	"/skill:driver": "设备识别与驱动状态",
	"/skill:eventlog": "崩溃、蓝屏与服务历史",
	"/skill:ls": "目录索引与文件列表",
	"/skill:runbook": "手动修复命令清单",
	"/skill:startup": "开机启动项",
	"/skill:sys": "系统概览",
};
const knownCommands = Object.keys(commandLabels);

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
	onFork,
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
	onCommand?: (name: "compact", text: string) => Promise<void>;
	onFork?: () => void;
	onSummary?: (instructions: string) => Promise<void>;
}) {
	const draftKey = makeDraftKey(sessionId);
	const [text, setText] = useState(() =>
		typeof window !== "undefined" ? (sessionStorage.getItem(draftKey) ?? "") : "",
	);
	const [messageId, setMessageId] = useState(() =>
		typeof window !== "undefined" ? (sessionStorage.getItem(`${draftKey}:id`) ?? crypto.randomUUID()) : "",
	);
	const [images, setImages] = useState<DraftImage[]>([]);
	const [imagesLoading, setImagesLoading] = useState(true);
	const [attaching, setAttaching] = useState(false);
	const [dragging, setDragging] = useState(false);
	const dragDepth = useRef(0);
	const [pendingEditId, setPendingEditId] = useState<string | null>(null);
	const attachmentBusy = useRef(false);
	const imageGeneration = useRef(0);
	const imagesRef = useRef(images);
	imagesRef.current = images;
	const fileInput = useRef<HTMLInputElement>(null);
	const [models, setModels] = useState<Model[]>([]);
	const [enabled, setEnabled] = useState<string[] | null>(null);
	const [loadedContext, setLoadedContext] = useState<{
		tokens: number | null;
		contextWindow: number;
		percent: number | null;
	} | null>(null);
	const [usageStats, setUsageStats] = useState<SessionUsage | null>(null);
	const [selected, setSelected] = useState<Selection>(null);
	const manualHomeModel = useRef(false);
	const initialKeyPrompt = useRef(false);
	const [missingKey, setMissingKey] = useState<{ model: ModelChoice; reason: ModelKeyDialogReason } | null>(null);
	const [thinking, setThinking] = useState("off");
	const [thinkingLevels, setThinkingLevels] = useState<string[]>([]);
	const [sending, setSending] = useState(false);
	const submitLock = useRef(false);
	const editorRevision = useRef(0);
	const restoring = useRef(false);
	const [savedCount, setSavedCount] = useState(() => {
		try {
			return readSavedDrafts(sessionStorage, draftKey).length;
		} catch {
			return 0;
		}
	});
	const [commands, setCommands] = useState<string[]>([]);
	const [commandIndex, setCommandIndex] = useState(0);
	const [suggestionsVisible, setSuggestionsVisible] = useState(true);
	const [files, setFiles] = useState<{ path: string; dir: string }[]>([]);
	const [fileScope, setFileScope] = useState<string | undefined>();
	const [fileIndex, setFileIndex] = useState(0);
	const [filesVisible, setFilesVisible] = useState(true);
	const [chipCount, setChipCount] = useState(0);
	const suggestions =
		chipCount === 0 && suggestionsVisible && /^\/[\w:-]*$/.test(text)
			? commands.filter((item) => item.startsWith(text))
			: [];
	// `@` 文件引用：光标位于行尾的 @query 时列出项目文件。
	const fileMatch = filesVisible ? /(?:^|\s)@([^\s@]*)$/.exec(text) : null;
	const editorRef = useRef<HTMLDivElement>(null);
	// 读回编辑器内容：withChips=false 只要纯文本，true 时 chip 还原成它的 token。
	function readEditor(withChips: boolean): string {
		const root = editorRef.current;
		if (!root) return "";
		let out = "";
		const walk = (node: Node) => {
			if (node.nodeType === Node.TEXT_NODE) {
				const value = node.textContent ?? "";
				const afterChip =
					node.previousSibling instanceof HTMLElement && node.previousSibling.classList.contains("composer-chip");
				out += afterChip && value.startsWith(chipCaretSpace) ? value.slice(chipCaretSpace.length) : value;
				return;
			}
			if (node.nodeType !== Node.ELEMENT_NODE) return;
			const el = node as HTMLElement;
			if (el.classList.contains("composer-chip")) {
				if (withChips) out += `${el.dataset.token ?? ""} `;
				return;
			}
			if (el.tagName === "BR") {
				out += "\n";
				return;
			}
			el.childNodes.forEach(walk);
			if (el.tagName === "DIV" || el.tagName === "P") out += "\n";
		};
		root.childNodes.forEach(walk);
		return out.replace(/\n+$/u, "");
	}
	// 编辑器 DOM 是事实源：同步纯文本状态、草稿和空态。
	function syncFromEditor() {
		editorRevision.current++;
		const root = editorRef.current;
		if (!root) return;
		const chips = root.querySelectorAll(".composer-chip").length;
		if (chips === 0 && !readEditor(false).trim() && root.innerHTML !== "") root.innerHTML = "";
		setChipCount(chips);
		setText(readEditor(false));
		setMessageId(crypto.randomUUID());
		setCommandIndex(0);
		setFileIndex(0);
		setSuggestionsVisible(true);
		setFilesVisible(true);
		sessionStorage.setItem(draftKey, readEditor(true));
	}
	function caretToEnd() {
		const root = editorRef.current;
		if (!root) return;
		root.focus();
		const range = document.createRange();
		range.selectNodeContents(root);
		range.collapse(false);
		const selection = window.getSelection();
		selection?.removeAllRanges();
		selection?.addRange(range);
	}
	function renderEditor(serialized: string) {
		const root = editorRef.current;
		if (!root) return;
		root.innerHTML = editorHtml(parseEditor(serialized, knownCommands));
		syncFromEditor();
		caretToEnd();
	}
	function removeChip(button: HTMLElement) {
		const chip = button.closest(".composer-chip");
		const parent = chip?.parentNode;
		if (!chip || !parent) return;
		const at = [...parent.childNodes].indexOf(chip);
		const separator = chip.nextSibling;
		if (separator?.nodeType === Node.TEXT_NODE && separator.textContent?.startsWith(chipCaretSpace)) {
			separator.textContent = separator.textContent.slice(chipCaretSpace.length);
		}
		chip.remove();
		const range = document.createRange();
		range.setStart(parent, at);
		range.collapse(true);
		const selection = window.getSelection();
		selection?.removeAllRanges();
		selection?.addRange(range);
		syncFromEditor();
	}
	const fileSuggestions =
		fileMatch && fileScope === sessionId
			? files.filter((file) => file.path.toLowerCase().includes((fileMatch[1] ?? "").toLowerCase())).slice(0, 40)
			: [];
	const filesOpen = !!fileMatch;
	useEffect(() => {
		if (!filesOpen || (files.length && fileScope === sessionId)) return;
		let active = true;
		apiJson<{ files: { path: string; dir: string }[] }>(
			`/api/files${sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : ""}`,
		)
			.then((data) => {
				if (active) {
					setFiles(data.files);
					setFileScope(sessionId);
				}
			})
			.catch((cause: unknown) => {
				if (active) setError(cause instanceof Error ? cause.message : "文件列表加载失败");
			});
		return () => {
			active = false;
		};
	}, [filesOpen, files.length, fileScope, sessionId]);
	// 视图类命令（/tree、/fork）在补全面板里选中即执行，不折叠成标记，也不等回车。
	function selectCommand(name: string) {
		setSuggestionsVisible(false);
		setCommandIndex(0);
		if (name === "/fork" || name === "/tree") {
			if (!sessionId) {
				renderEditor("");
				setError("请先进入会话，再使用此命令。");
				return;
			}
			renderEditor("");
			if (name === "/tree") navigate(`/s/${sessionId}/tree`);
			else onFork?.();
			return;
		}
		renderEditor(`${name} `);
	}
	function selectFile(path: string) {
		renderEditor(replaceTrailingFileQuery(readEditor(true), path));
		setFileIndex(0);
		setFilesVisible(false);
	}
	const [error, setError] = useState("");
	const navigate = useNavigate();

	useEffect(() => {
		const stored = sessionStorage.getItem(draftKey) ?? "";
		const root = editorRef.current;
		if (root) {
			root.innerHTML = editorHtml(parseEditor(stored, knownCommands));
			setChipCount(root.querySelectorAll(".composer-chip").length);
		}
		setText(
			parseEditor(stored, knownCommands)
				.filter((segment) => segment.kind === "text")
				.map((segment) => (segment.kind === "text" ? segment.text : ""))
				.join(""),
		);
		setMessageId(sessionStorage.getItem(`${draftKey}:id`) ?? crypto.randomUUID());
	}, [draftKey]);
	useEffect(() => {
		sessionStorage.setItem(`${draftKey}:id`, messageId);
	}, [draftKey, messageId]);
	useEffect(() => {
		let active = true;
		imageGeneration.current++;
		imagesRef.current = [];
		setImages([]);
		setImagesLoading(true);
		void Promise.resolve()
			.then(() => parseImageHashes(sessionStorage.getItem(`${draftKey}:images`)))
			.then(loadImages)
			.then((loaded) => {
				if (active) {
					imagesRef.current = loaded;
					setImages(loaded);
				} else
					loaded.forEach((image) => {
						URL.revokeObjectURL(image.url);
					});
			})
			.catch(() => {
				if (active) setError("无法恢复未发送的图片");
			})
			.finally(() => {
				if (active) setImagesLoading(false);
			});
		return () => {
			active = false;
			imageGeneration.current++;
			imagesRef.current.forEach((image) => {
				URL.revokeObjectURL(image.url);
			});
		};
	}, [draftKey]);
	async function attach(files: File[]) {
		if (attachmentBusy.current || imagesLoading || sending || restoring.current) return;
		attachmentBusy.current = true;
		setAttaching(true);
		const generation = imageGeneration.current;
		const added: DraftImage[] = [];
		try {
			let total = imagesRef.current.reduce((sum, image) => sum + image.blob.size, 0);
			for (const file of files) {
				const image = await addImage(file, total);
				total += file.size;
				added.push(image);
			}
			if (generation !== imageGeneration.current) {
				added.forEach((image) => {
					URL.revokeObjectURL(image.url);
				});
				return;
			}
			const next = [...imagesRef.current, ...added];
			sessionStorage.setItem(`${draftKey}:images`, JSON.stringify(next.map((image) => image.hash)));
			imagesRef.current = next;
			setImages(next);
			setMessageId(crypto.randomUUID());
			setError("");
		} catch (cause) {
			added.forEach((image) => {
				URL.revokeObjectURL(image.url);
			});
			if (generation === imageGeneration.current) setError(cause instanceof Error ? cause.message : "无法添加图片");
		} finally {
			await releaseImageHolds(added).catch(() => {});
			attachmentBusy.current = false;
			setAttaching(false);
		}
	}
	function removeImage(index: number) {
		if (sending || attaching || restoring.current) return;
		const removed = imagesRef.current[index];
		if (!removed) return;
		URL.revokeObjectURL(removed.url);
		const next = imagesRef.current.filter((_, at) => at !== index);
		sessionStorage.setItem(`${draftKey}:images`, JSON.stringify(next.map((image) => image.hash)));
		imagesRef.current = next;
		setImages(next);
		void collectDraftImages().catch(() => {});
		setMessageId(crypto.randomUUID());
	}
	function hasDraggedFiles(event: DragEvent<HTMLElement>): boolean {
		return (
			Array.from(event.dataTransfer?.types ?? []).includes("Files") || (event.dataTransfer?.files?.length ?? 0) > 0
		);
	}
	function dragEnter(event: DragEvent<HTMLElement>) {
		if (!hasDraggedFiles(event)) return;
		event.preventDefault();
		dragDepth.current++;
		setDragging(true);
	}
	function dragOver(event: DragEvent<HTMLElement>) {
		if (!hasDraggedFiles(event)) return;
		event.preventDefault();
		event.dataTransfer.dropEffect = "copy";
		if (!dragging) setDragging(true);
	}
	function dragLeave(event: DragEvent<HTMLElement>) {
		if (!hasDraggedFiles(event) && !dragging) return;
		dragDepth.current = Math.max(0, dragDepth.current - 1);
		if (dragDepth.current === 0) setDragging(false);
	}
	function dropFiles(event: DragEvent<HTMLElement>) {
		dragDepth.current = 0;
		setDragging(false);
		const files = [...(event.dataTransfer?.files ?? [])];
		if (!files.length) return;
		event.preventDefault();
		event.stopPropagation();
		void attach(files);
	}
	function paste(event: ClipboardEvent<HTMLDivElement>) {
		const files = [...event.clipboardData.items]
			.filter((item) => item.kind === "file")
			.map((item) => item.getAsFile())
			.filter((file): file is File => !!file);
		if (files.length) {
			event.preventDefault();
			void attach(files);
			return;
		}
		const plain = event.clipboardData.getData("text/plain");
		if (plain) {
			event.preventDefault();
			document.execCommand("insertText", false, plain);
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
	const [modelRevision, setModelRevision] = useState(0);
	useGlobalEvents((name, event) => {
		if (name === "open" || name === "reset") setModelRevision((value) => value + 1);
		if (name !== "state") return;
		try {
			const data = JSON.parse(event?.data ?? "null");
			if (
				data?.type === "auth_changed" ||
				data?.type === "models_changed" ||
				data?.type === "settings_changed" ||
				(data?.type === "usage_changed" && data.sessionId === sessionId)
			)
				setModelRevision((value) => value + 1);
		} catch {
			/* 后续重连重新读取。 */
		}
	}, !!onSend);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 全局登录和 scoped 事件通过版本号重新读取同一接口
	useEffect(() => {
		const controller = new AbortController();
		let active = true;
		apiJson<
			ModelSelectionPayload & {
				thinkingLevels?: string[];
				contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null };
				usage?: SessionUsage | null;
			}
		>(`/api/models${sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : ""}`, { signal: controller.signal })
			.then((data) => {
				if (active) {
					setModels(data.models);
					setEnabled(data.enabled);
					setSelected((current) => refreshManualSelection(current, data, !sessionId && manualHomeModel.current));
					if (!initialKeyPrompt.current && onSend && data.selected) {
						initialKeyPrompt.current = true;
						// 只在完全没有配置模型时自动提示，且 24 小时内只提示一次。
						const now = Date.now();
						if (
							data.selected.catalogPresent &&
							shouldAutoPrompt({
								catalog: data.catalog,
								lastPromptAt: readModelKeyPromptTime(),
								now,
							})
						) {
							writeModelKeyPromptTime(undefined, now);
							setMissingKey({ model: data.selected, reason: "initial" });
						}
					}
					setMissingKey((current) =>
						current &&
						data.catalog.some(
							(model) =>
								model.provider === current.model.provider &&
								model.id === current.model.id &&
								model.credentialReady,
						)
							? null
							: current,
					);
					if (sessionId || !manualHomeModel.current) {
						const levels = data.thinkingLevels?.length ? data.thinkingLevels : ["off"];
						setThinking(
							levels.includes(data.selected?.thinkingLevel ?? "")
								? (data.selected?.thinkingLevel ?? levels[0])
								: levels[0],
						);
						setThinkingLevels(levels);
					}
					setLoadedContext(data.contextUsage ?? null);
					setUsageStats(data.usage ?? null);
				}
			})
			.catch((cause: unknown) => {
				if (active && !controller.signal.aborted)
					setError(cause instanceof Error ? cause.message : "模型列表加载失败");
			});
		return () => {
			active = false;
			controller.abort();
		};
	}, [sessionId, modelRevision]);

	const usage = contextUsage ?? loadedContext;
	const cacheHitRate = usageStats?.cacheHitRate ?? null;
	const availableThinking = thinkingLevels.length ? thinkingLevels : ["off"];
	const scoped = scopedModels(models, enabled);
	const choices =
		selected && !scoped.some((model) => model.provider === selected.provider && model.id === selected.id)
			? [selected, ...scoped]
			: scoped;
	async function chooseModel(key: string) {
		const choice = scoped.find((item) => `${item.provider}/${item.id}` === key);
		if (!choice) return false;
		try {
			const changed = await apiJson<ModelChoice & { thinkingLevels?: string[] }>("/api/models/select", {
				method: "POST",
				body: { ...(sessionId ? { sessionId } : {}), provider: choice.provider, modelId: choice.id },
			});
			const levels = changed.thinkingLevels?.length ? changed.thinkingLevels : ["off"];
			setThinking(levels.includes(changed.thinkingLevel ?? "") ? (changed.thinkingLevel ?? levels[0]) : levels[0]);
			setThinkingLevels(levels);
			manualHomeModel.current = !sessionId;
			setSelected(changed);
			setError("");
			return true;
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "无法切换模型");
			return false;
		}
	}
	async function chooseThinking(level: string) {
		try {
			if (sessionId) await apiJson("/api/models/thinking", { method: "POST", body: { sessionId, level } });
			setThinking(level);
			if (!sessionId) manualHomeModel.current = true;
			setError("");
			return true;
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "无法调整思考强度");
			return false;
		}
	}
	async function ensureCredentials(): Promise<ModelChoice | null> {
		const data = await apiJson<ModelSelectionPayload>(
			`/api/models${sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : ""}`,
		);
		const model = refreshManualSelection(selected, data, !sessionId && manualHomeModel.current);
		setSelected(model);
		if (!model?.catalogPresent) throw new Error("所选模型不在目录中，请手动选择模型。");
		if (!model.credentialReady) {
			setMissingKey({ model, reason: "unsent" });
			return null;
		}
		return model;
	}
	async function submit() {
		if (submitLock.current || sending || attaching || imagesLoading || restoring.current) return;
		// 自定义总结提示词：回车不是发消息，而是带着这段提示词去发起导航与总结。
		if (summaryTag) {
			if (!text.trim() || !onSummary) return;
			setSending(true);
			setError("");
			try {
				if (!(await ensureCredentials())) return;
				await onSummary(text.trim());
				renderEditor("");
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
		const serialized = readEditor(true);
		const { command, body } = extractPayload(serialized, knownCommands);
		if (!body.trim() && !images.length && !command && running) {
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
		if ((!body.trim() && !images.length && !command) || (!onSend && !onCommand)) return;
		submitLock.current = true;
		const revision = editorRevision.current;
		const submittedImages = [...images];
		setSending(true);
		setError("");
		try {
			if (command === "/tree") {
				if (!sessionId) throw new Error("请先进入会话，再使用此命令。");
				navigate(`/s/${sessionId}/tree`);
			} else if (command === "/fork") {
				if (!sessionId || !onFork) throw new Error("请先进入会话，再使用此命令。");
				onFork();
			} else if (command === "/compact") {
				if (!onCommand) throw new Error("请先进入会话，再使用此命令。");
				if (!(await ensureCredentials())) return;
				await onCommand("compact", body);
			} else {
				if (!onSend) throw new Error("当前无法提交消息");
				const model = selected?.catalogPresent && selected.credentialReady ? selected : await ensureCredentials();
				if (!model) return;
				if (editorRevision.current !== revision) return;
				if (body.length > 100_000) throw new Error("消息不能超过 100000 个字符");
				await onSend(body, {
					provider: model.provider,
					modelId: model.id,
					thinkingLevel: thinking,
					messageId,
					images: submittedImages.map((image) => image.hash),
					serialized,
					skill: command.startsWith("/skill:") ? command.slice(7) : undefined,
				});
			}
			// 等待配置或本地保存期间的新输入归下一条草稿，不能被迟到完成清空。
			if (editorRevision.current !== revision) return;
			submittedImages.forEach((image) => {
				URL.revokeObjectURL(image.url);
			});
			imagesRef.current = [];
			setImages([]);
			sessionStorage.removeItem(`${draftKey}:images`);
			void collectDraftImages().catch(() => {});
			renderEditor("");
			setMessageId(crypto.randomUUID());
			sessionStorage.removeItem(draftKey);
			sessionStorage.removeItem(`${draftKey}:id`);
		} catch (cause) {
			if (cause instanceof ApiFailure && cause.code === "credentials_required" && selected)
				setMissingKey({ model: selected, reason: "unsent" });
			else setError(cause instanceof Error ? cause.message : "消息未被接受，请重试");
		} finally {
			submitLock.current = false;
			setSending(false);
		}
	}
	async function replaceDraft(draft: SavedDraft, saveCurrent: boolean) {
		if (restoring.current || submitLock.current || imagesLoading || attaching) throw new Error("请等待当前操作完成");
		restoring.current = true;
		const revision = editorRevision.current;
		const generation = imageGeneration.current;
		let loaded: DraftImage[] = [];
		try {
			loaded = await loadImages(draft.images);
			if (loaded.length !== draft.images.length) throw new Error("图片不完整，请保留发送记录并重新添加图片。");
			if (revision !== editorRevision.current || generation !== imageGeneration.current)
				throw new Error("草稿已更新，请重新点击编辑。");
			const saved = readSavedDrafts(sessionStorage, draftKey);
			if (!saveCurrent) saved.pop();
			const current = { text: readEditor(true), images: imagesRef.current.map((image) => image.hash) };
			if (current.text || current.images.length) saved.push(current);
			sessionStorage.setItem(`${draftKey}:saved`, JSON.stringify(saved));
			sessionStorage.setItem(draftKey, draft.text);
			sessionStorage.setItem(`${draftKey}:images`, JSON.stringify(draft.images));
			for (const image of imagesRef.current) URL.revokeObjectURL(image.url);
			imagesRef.current = loaded;
			setImages(loaded);
			loaded = [];
			renderEditor(draft.text);
			setSavedCount(saved.length);
			setError("");
			void collectDraftImages().catch(() => {});
		} finally {
			restoring.current = false;
			for (const image of loaded) URL.revokeObjectURL(image.url);
		}
	}
	function performOutgoingEdit(targetId: string) {
		const outbox = messageOutbox();
		const item = outbox.getSnapshot().find((entry) => entry.id === targetId);
		if (!item || (sessionId ? item.sessionId !== sessionId : !item.fromHome)) return;
		if (item.state !== "failed" && !(item.state === "waiting" && !item.attempted)) return;
		try {
			outbox.beginEdit(item.id);
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "消息状态已变化");
			return;
		}
		void replaceDraft({ text: item.serialized, images: item.images }, true)
			.then(() => outbox.completeEdit(item.id))
			.catch((cause: unknown) => setError(cause instanceof Error ? cause.message : "无法恢复消息"))
			.finally(() => outbox.endEdit(item.id));
	}
	// biome-ignore lint/correctness/useExhaustiveDependencies: 事件读写当前会话 DOM 和 refs，不随输入内容重新订阅。
	useEffect(() => {
		const edit = (event: Event) => {
			if (!(event instanceof CustomEvent) || typeof event.detail !== "string") return;
			const outbox = messageOutbox();
			const item = outbox.getSnapshot().find((entry) => entry.id === event.detail);
			if (!item || (sessionId ? item.sessionId !== sessionId : !item.fromHome)) return;
			if (item.state !== "failed" && !(item.state === "waiting" && !item.attempted)) return;
			if (readEditor(true) || imagesRef.current.length) {
				setPendingEditId(item.id);
				return;
			}
			performOutgoingEdit(item.id);
		};
		window.addEventListener(EDIT_OUTGOING_EVENT, edit);
		return () => window.removeEventListener(EDIT_OUTGOING_EVENT, edit);
	}, [draftKey, sessionId, imagesLoading, attaching]);
	function keyDown(event: KeyboardEvent<HTMLDivElement>) {
		if (event.nativeEvent.isComposing || event.keyCode === 229) return;
		if (event.key === "Enter" && event.target instanceof HTMLElement && event.target.closest(".chip-remove")) return;
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
	// biome-ignore lint/correctness/useExhaustiveDependencies: 选项索引变化后滚动已渲染的高亮行。
	useEffect(() => {
		const panel = editorRef.current?.parentElement;
		panel?.querySelector<HTMLElement>(".command-active")?.scrollIntoView({ block: "nearest" });
	}, [commandIndex, fileIndex]);
	const nothingToSend = !text.trim() && !images.length && chipCount === 0;

	return (
		<div className={`composer-shell ${home ? "composer-shell--home" : "composer-shell--chat"}`}>
			{missingKey && (
				<ModelKeyDialog model={missingKey.model} reason={missingKey.reason} onClose={() => setMissingKey(null)} />
			)}
			{pendingEditId && (
				<ConfirmDialog
					title="继续编辑这条消息？"
					description="当前草稿将保留，可通过「恢复保留草稿」取回。"
					confirmLabel="继续编辑"
					onCancel={() => setPendingEditId(null)}
					onConfirm={() => {
						const id = pendingEditId;
						setPendingEditId(null);
						performOutgoingEdit(id);
					}}
				/>
			)}
			{savedCount > 0 && (
				<Button
					variant="ghost"
					className="composer-saved-draft"
					onPress={() => {
						void Promise.resolve()
							.then(() => {
								const saved = readSavedDrafts(sessionStorage, draftKey).at(-1);
								if (saved) return replaceDraft(saved, false);
							})
							.catch((cause: unknown) => setError(cause instanceof Error ? cause.message : "草稿恢复失败"));
					}}
				>
					恢复保留草稿（{savedCount}）
				</Button>
			)}
			{!!images.length && (
				<ul className="composer-attachments" aria-label="待发送图片">
					{images.map((image, index) => (
						<li className="draft-image" key={`${image.hash}-${index}`}>
							<PreviewableImage
								buttonClassName="draft-image-open"
								label={`查看图片 ${index + 1}`}
								src={image.url}
								alt={`待发送图片 ${index + 1}`}
							/>
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
			<section
				className={`composer ${home ? "composer--home" : "composer--chat"}`}
				aria-label="消息编辑器"
				data-dragging={dragging || undefined}
				onDragEnter={dragEnter}
				onDragOver={dragOver}
				onDragLeave={dragLeave}
				onDrop={dropFiles}
			>
				{summaryTag && (
					<div className="composer-command composer-summary-tag">
						<PencilLine size={16} aria-hidden="true" />
						<span>{summaryTag.label}</span>
						<Button variant="ghost" isIconOnly aria-label="取消自定义总结提示词" onPress={summaryTag.onCancel}>
							<X size={14} aria-hidden="true" />
						</Button>
					</div>
				)}
				{/* biome-ignore lint/a11y/useSemanticElements: 多行富文本输入没有等价的原生元素，contenteditable + textbox 是标准模式 */}
				<div
					ref={editorRef}
					className="composer-input"
					role="textbox"
					aria-multiline="true"
					aria-label="消息"
					tabIndex={0}
					contentEditable
					suppressContentEditableWarning
					data-placeholder={
						home
							? "描述这台电脑遇到的问题…"
							: summaryTag
								? "写下总结时要保留什么，回车开始总结…"
								: queueActive
									? "继续输入以排队后续修改"
									: "继续提问，或补充这台电脑的情况…"
					}
					onInput={syncFromEditor}
					onKeyDown={keyDown}
					onPaste={paste}
					onClick={(event) => {
						const button = (event.target as HTMLElement).closest<HTMLElement>(".chip-remove");
						if (button) removeChip(button);
					}}
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
											{skill ? <Sparkles size={14} /> : <Command size={14} />}
											<span>{skill ? item.slice("/skill:".length) : item.slice(1)}</span>
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
							<span className="command-group-title">最近文件</span>
							{fileSuggestions.map((file, index) => (
								<Button
									key={file.path}
									className={index === fileIndex ? "command-active" : ""}
									variant="ghost"
									onPress={() => selectFile(file.path)}
								>
									<FileText size={14} />
									<span>{file.path.split("/").pop()}</span>
									<small>{file.dir}</small>
								</Button>
							))}
						</div>
					</fieldset>
				)}
				{error && <ErrorNotice key={error} input message={error} onDismiss={() => setError("")} />}
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
						isDisabled={sending || attaching || imagesLoading}
						onPress={() => fileInput.current?.click()}
					>
						<Plus size={20} />
					</Button>
					<span className="composer-spacer" />
					<div className="composer-model-controls">
						<ContextMeter usage={usage} cacheHitRate={cacheHitRate} provider={selected?.provider ?? ""} />
						<ModelPicker
							selected={selected}
							models={choices}
							thinking={thinking}
							levels={availableThinking}
							onOpen={() => setModelRevision((value) => value + 1)}
							onSelect={chooseModel}
							onThinking={chooseThinking}
						/>
					</div>
					<Button
						isIconOnly
						isDisabled={
							sending ||
							attaching ||
							imagesLoading ||
							(summaryTag ? !text.trim() : (nothingToSend && !running) || (!onSend && !onStop && !onCommand))
						}
						className="composer-send"
						aria-label={summaryTag ? "开始分支总结" : running && nothingToSend ? "停止生成" : "发送消息"}
						onPress={() => void submit()}
					>
						{running && nothingToSend ? <Square size={12} fill="currentColor" /> : <ArrowUp size={16} />}
					</Button>
				</div>
			</section>
		</div>
	);
}
