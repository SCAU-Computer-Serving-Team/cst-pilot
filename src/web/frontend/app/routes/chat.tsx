import { Button, Popover } from "@heroui/react";
import { CheckCircle2, CircleAlert, Download, EllipsisVertical, HardDrive, ListTree, Share2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useParams, useSearchParams, useViewTransitionState } from "react-router";
import { Composer, type ComposerConfig } from "../composer/composer";
import { addImage, type DraftImage, releaseImageHolds } from "../composer/draft-images";
import { Conversation } from "../conversation/conversation";
import { visibleQueueItems } from "../conversation/conversation-parts";
import { QuestionPanel } from "../conversation/question-panel";
import { QueuePanel } from "../conversation/queue-panel";
import type { UserImagePart } from "../conversation/user-message";
import { apiJson, sessionPath } from "../data/api";
import type { PendingBitLocker } from "../data/bitlocker-state";
import { presentError } from "../data/error-presenter";
import { outgoingFor, useMessageOutbox } from "../data/use-message-outbox";
import { type SessionDetail, useSessionDetail, useSessionTitle } from "../data/web-state";
import { ErrorNotice } from "../shell/error-notice";
import { readDurationMs } from "../shell/motion-tokens";
import { Notification } from "../shell/notification";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { sampleSessions } from "../shell/sample-sessions";
import { SidebarHeaderToggle, useWorkspaceShell } from "../shell/workspace-shell-context";
import { branchSummaryChoice, type NavigateResult, summaryBody, summaryTagText } from "../tree/branch-summary";

const EMPTY_REQUESTS: PendingBitLocker[] = [];
const NOOP_FORK = () => {};

type ChatToast = {
	kind: "success" | "warning" | "danger";
	message: string;
	duration: number;
	key: number;
};

function imagePartToFile(part: UserImagePart, index: number): File {
	try {
		const raw = atob(part.data.trim());
		const bytes = new Uint8Array(raw.length);
		for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
		const ext = part.mimeType.split("/")[1] || "png";
		return new File([bytes], `turn-image-${index}.${ext}`, { type: part.mimeType });
	} catch {
		throw new Error("无法读取历史消息的附图。");
	}
}

export default function Chat() {
	const { sessionId } = useParams();
	return <SessionChat key={sessionId} sessionId={sessionId} />;
}

function SessionChat({ sessionId }: { sessionId: string | undefined }) {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const example =
		params.get("preview") === "1" ? sampleSessions.find((session) => session.id === sessionId) : undefined;
	const { detail: loadedDetail, streaming, error, refresh } = useSessionDetail(example ? undefined : sessionId);
	const { outbox, records } = useMessageOutbox();
	const outgoing = useMemo(() => (example ? [] : outgoingFor(records, sessionId)), [records, sessionId, example]);
	const pendingDetail = useMemo<SessionDetail | undefined>(
		() =>
			sessionId
				? {
						id: sessionId,
						entries: [],
						messages: [],
						running: false,
						queue: { version: 0, paused: false, items: [] },
						ui: [],
					}
				: undefined,
		[sessionId],
	);
	const detail = loadedDetail ?? (outgoing.length ? pendingDetail : undefined);
	useEffect(() => {
		if (!detail || example) return;
		try {
			outbox.observe(
				detail.id,
				detail.queue.items,
				detail.entries.flatMap(({ message }) => (message.clientMessageId ? [message.clientMessageId] : [])),
			);
		} catch {
			/* 本地清理失败时保留备份，下一次同步重试。 */
		}
	}, [detail, example, outbox]);
	// 共享分支、重新生成与普通提交操作锁，通过 ref 保持传给 Conversation 的回调引用稳定，流式期间不破坏历史轮次 memo。
	const turnActionBusyRef = useRef(false);
	const [turnActionBusy, setTurnActionBusy] = useState(false);
	const composerSubmittingRef = useRef(false);
	const [composerSubmitting, setComposerSubmitting] = useState(false);
	const handleComposerSubmittingChange = useCallback((submitting: boolean) => {
		composerSubmittingRef.current = submitting;
		setComposerSubmitting(submitting);
	}, []);
	const sessionMutationBusy = turnActionBusy || composerSubmitting;
	const runningNow = Boolean(detail?.running || streaming);
	const runningRef = useRef(runningNow);
	runningRef.current = runningNow;
	const outgoingCountRef = useRef(outgoing.length);
	outgoingCountRef.current = outgoing.length;
	const loadedEntriesRef = useRef(loadedDetail?.entries);
	loadedEntriesRef.current = loadedDetail?.entries;
	const active = useRef(false);
	useEffect(() => {
		active.current = true;
		return () => {
			active.current = false;
		};
	}, []);
	const [messageMenuOpen, setMessageMenuOpen] = useState(false);
	const messageMenuTrigger = useRef<HTMLButtonElement>(null);
	const shell = useWorkspaceShell();
	const appActiveRef = useRef(Boolean(shell?.appActive));
	appActiveRef.current = Boolean(shell?.appActive);
	usePanelDismiss(messageMenuOpen, () => setMessageMenuOpen(false), messageMenuTrigger, messageMenuTrigger);
	useEffect(() => {
		if (shell?.appActive) setMessageMenuOpen(false);
	}, [shell?.appActive]);
	const location = useLocation();
	const [toast, setToast] = useState<ChatToast | null>(null);
	const showToast = useCallback((kind: ChatToast["kind"], message: string, duration = 2600) => {
		setToast((current) => ({ kind, message, duration, key: (current?.key ?? 0) + 1 }));
	}, []);
	const showSuccess = useCallback((message: string) => showToast("success", message, 2200), [showToast]);
	// 分支会话直接跳转后弹出顶部成功提示（H3YwBW / ZVIJj），并清掉 history state 防止刷新重放。
	useEffect(() => {
		const state = location.state as { forkNotice?: boolean } | null;
		if (!state?.forkNotice) return;
		showToast("success", "已从当前节点分支新会话", 2600);
		navigate(location.pathname + location.search, { replace: true, state: null });
	}, [location.pathname, location.search, location.state, navigate, showToast]);
	const transitioning = useViewTransitionState(location.pathname);
	const entrance = useRef<{ id: string; snapshot: boolean } | null>(null);
	if (entrance.current?.id !== sessionId) entrance.current = null;
	if (detail && entrance.current?.id !== detail.id) {
		const captured = document
			.getAnimations()
			.some(
				(animation) =>
					animation.effect instanceof KeyframeEffect &&
					animation.effect.pseudoElement?.startsWith("::view-transition-"),
			);
		entrance.current = { id: detail.id, snapshot: transitioning && !captured };
	}
	const [linearEntering, setLinearEntering] = useState(true);
	const entranceId = detail?.id;
	useEffect(() => {
		if (!entranceId) return;
		const duration = readDurationMs(document.documentElement, "--fast", 150);
		const timer = window.setTimeout(() => setLinearEntering(false), duration);
		return () => window.clearTimeout(timer);
	}, [entranceId]);
	const title = useSessionTitle(sessionId, example?.title ?? "会话", !example);
	const [summarizing, setSummarizing] = useState(false);
	const startedSummary = useRef("");
	const choice = useMemo(() => (example ? undefined : branchSummaryChoice(params)), [example, params]);
	const clearChoice = useCallback(() => {
		if (sessionId) navigate(`/s/${sessionId}`, { replace: true });
	}, [navigate, sessionId]);
	const runSummary = useCallback(
		async (entryId: string, customInstructions?: string) => {
			if (!sessionId) return;
			setSummarizing(true);
			try {
				const result = await apiJson<NavigateResult>(sessionPath(sessionId, "/tree/navigate"), {
					method: "POST",
					idempotencyKey: crypto.randomUUID(),
					body: summaryBody(entryId, customInstructions),
				});
				if (result.cancelled) showToast("warning", "已取消分支总结，会话位置没有变化。", 3500);
			} catch (cause) {
				showToast("danger", cause instanceof Error ? cause.message : "分支总结未完成", 4500);
			} finally {
				setSummarizing(false);
				await refresh();
			}
		},
		[sessionId, refresh, showToast],
	);
	// 选「总结」时树页只留下地址参数，真正的导航请求在这里发起：叶指针未动之前，那段分支还在。
	useEffect(() => {
		if (choice?.mode !== "summarize" || startedSummary.current === choice.entryId) return;
		startedSummary.current = choice.entryId;
		void (async () => {
			await runSummary(choice.entryId);
			clearChoice();
		})();
	}, [choice, runSummary, clearChoice]);
	async function cancelSummary() {
		if (!sessionId) return;
		try {
			await apiJson(sessionPath(sessionId, "/tree/abort"), { method: "POST" });
		} catch {
			/* 总结可能刚好结束，不是错误 */
		}
	}
	// 依赖 detail 而非每次渲染新建：流式 tick 间 entries 引用稳定，下游 memo 才能跳过历史轮次。
	const entries = useMemo(
		() => (detail ? (detail.entries?.length ? detail.entries : detail.messages.map((message) => ({ message }))) : []),
		[detail],
	);
	const queued = !!detail && visibleQueueItems(detail.queue.items).some((item) => item.status === "pending");
	async function send(text: string, config: ComposerConfig) {
		if (!sessionId) throw new Error("会话尚未就绪");
		if (turnActionBusyRef.current) throw new Error("正在切换分支，请稍后重试");
		const { messageId, ...payload } = config;
		outbox.enqueue({ ...payload, id: messageId, sessionId, text });
	}
	async function stop() {
		if (!sessionId) return;
		await apiJson(sessionPath(sessionId, "/abort"), { method: "POST" });
		await refresh();
	}
	async function command(name: "compact", text: string) {
		if (!sessionId) return;
		if (name === "compact")
			await apiJson(sessionPath(sessionId, "/compact"), { method: "POST", body: { instructions: text } });
		await refresh();
	}
	const forkDirectly = useCallback(
		async (explicitEntryId?: string) => {
			if (!sessionId || turnActionBusyRef.current || composerSubmittingRef.current) return;
			if (runningRef.current) {
				showToast("warning", "请等待当前回复完成后再分支新会话。", 3200);
				return;
			}
			const targetEntryId =
				explicitEntryId ??
				[...(loadedEntriesRef.current ?? [])]
					.reverse()
					.find(
						(entry) =>
							entry.id &&
							entry.message.role === "assistant" &&
							entry.message.stopReason != null &&
							entry.message.stopReason !== "toolUse",
					)?.id;
			if (!targetEntryId) {
				showToast("warning", "当前会话尚无可用于分支的已完成回复。", 3200);
				return;
			}
			turnActionBusyRef.current = true;
			setTurnActionBusy(true);
			try {
				const created = await apiJson<{ id: string }>(sessionPath(sessionId, "/fork"), {
					method: "POST",
					body: { entryId: targetEntryId },
				});
				if (!created?.id) throw new Error("后台未返回新会话 ID。");
				if (active.current && !appActiveRef.current)
					navigate(`/s/${encodeURIComponent(created.id)}`, { state: { forkNotice: true } });
			} catch (cause) {
				if (active.current)
					showToast("danger", cause instanceof Error ? cause.message : "分支会话失败，请重试", 4500);
			} finally {
				turnActionBusyRef.current = false;
				if (active.current) setTurnActionBusy(false);
			}
		},
		[sessionId, navigate, showToast],
	);
	const handleConversationFork = useCallback(
		(entryId: string) => {
			void forkDirectly(entryId);
		},
		[forkDirectly],
	);
	// 修改用户消息或重新生成回复（对齐桌面端 retry_user_turn 与 PaRHN）：先校验发件箱可写，再定位节点并确保重发入箱。
	const retryUserTurn = useCallback(
		async (userEntryId: string, nextText: string, userImages: UserImagePart[]): Promise<boolean> => {
			if (!sessionId || example || turnActionBusyRef.current || composerSubmittingRef.current) return false;
			if (runningRef.current) {
				showToast("warning", "请等待当前回复完成后再重新生成。", 3200);
				return false;
			}
			if (outgoingCountRef.current > 0) {
				showToast("warning", "请先等待待发送消息完成或处理后再重新生成。", 3200);
				return false;
			}
			if (!nextText.trim() && userImages.length === 0) return false;
			turnActionBusyRef.current = true;
			setTurnActionBusy(true);
			const held: DraftImage[] = [];
			try {
				let totalBytes = 0;
				for (let index = 0; index < userImages.length; index++) {
					const file = imagePartToFile(userImages[index], index);
					held.push(await addImage(file, totalBytes));
					totalBytes += file.size;
				}
				const imageHashes = held.map((image) => image.hash);
				const resendRecord = {
					id: crypto.randomUUID(),
					sessionId,
					text: nextText,
					serialized: nextText,
					images: imageHashes,
				};
				// 在修改服务端会话叶节点前按完整待入箱记录尺寸预检本地发件箱可写性，避免服务端回退后本地落盘失败。
				outbox.verifyWritable(resendRecord);
				const nav = await apiJson<NavigateResult>(sessionPath(sessionId, "/tree/navigate"), {
					method: "POST",
					idempotencyKey: crypto.randomUUID(),
					body: { entryId: userEntryId, summarize: false },
				});
				if (nav?.cancelled) return false;
				// 服务端已回退叶节点：无论当前路由是否仍挂载，都必须立即将新消息送入全局发件箱继续投递。
				outbox.enqueue(resendRecord);
				if (active.current) void refresh().catch(() => {});
				return true;
			} catch (cause) {
				if (active.current)
					showToast("danger", cause instanceof Error ? cause.message : "重新生成失败，请重试", 4500);
				return false;
			} finally {
				turnActionBusyRef.current = false;
				if (active.current) setTurnActionBusy(false);
				for (const image of held) URL.revokeObjectURL(image.url);
				if (held.length) void releaseImageHolds(held).catch(() => {});
			}
		},
		[sessionId, example, refresh, outbox, showToast],
	);
	return (
		<main className="chat-main">
			{toast && (
				<Notification
					className="chat-toast"
					label={toast.message}
					kind={toast.kind}
					duration={toast.duration}
					lifetimeKey={toast.key}
					onDismiss={() => setToast(null)}
				>
					{toast.kind === "success" ? (
						<CheckCircle2 size={16} aria-hidden="true" />
					) : (
						<CircleAlert size={16} aria-hidden="true" />
					)}
					<span className="notification-message">{toast.message}</span>
					{toast.kind !== "success" && (
						<button
							type="button"
							className="btn-icon xs-24 notification-close"
							aria-label="关闭通知"
							onClick={() => setToast(null)}
						>
							<X size={14} aria-hidden="true" />
						</button>
					)}
				</Notification>
			)}
			<header className="chat-header">
				<SidebarHeaderToggle />
				<HardDrive size={16} aria-hidden="true" />
				<h1>{title}</h1>
				{example && <span className="preview-tag">设计预览 · 示例数据</span>}
				{!example && (
					<Popover isOpen={messageMenuOpen} onOpenChange={setMessageMenuOpen}>
						<Button
							ref={messageMenuTrigger}
							variant="ghost"
							isIconOnly
							className="btn-icon sm-28 chat-menu-trigger"
							aria-label="消息菜单"
							data-open={messageMenuOpen || undefined}
						>
							<EllipsisVertical size={16} aria-hidden="true" />
						</Button>
						<Popover.Content
							placement="bottom end"
							offset={6}
							className="tier-2-glass tier-2-menu chat-more-popover"
						>
							<Popover.Dialog aria-label="消息菜单" className="tier-2-menu-list">
								<Link
									className="menu-row"
									to={`/s/${sessionId}/tree`}
									viewTransition
									onClick={() => setMessageMenuOpen(false)}
								>
									<ListTree size={16} aria-hidden="true" />
									<span>分支树</span>
								</Link>
								<Link
									className="menu-row"
									to={`/s/${sessionId}/export`}
									viewTransition
									onClick={() => setMessageMenuOpen(false)}
								>
									<Download size={16} aria-hidden="true" />
									<span>导出会话</span>
								</Link>
								<button type="button" className="menu-row" disabled title="计划中">
									<Share2 size={16} aria-hidden="true" />
									<span>分享</span>
								</button>
							</Popover.Dialog>
						</Popover.Content>
					</Popover>
				)}
			</header>
			{error && detail && presentError(error).kind !== "connection" && (
				<ErrorNotice message={error} onRetry={() => void refresh()} />
			)}
			<div className="chat-message-surface">
				{detail ? (
					<div
						key={detail.id}
						className="chat-loaded"
						data-session-id={detail.id}
						data-snapshot-entered={entrance.current?.snapshot || undefined}
						data-linear-text-enter={
							linearEntering && !entrance.current?.snapshot && !detail.running && !streaming ? "true" : undefined
						}
					>
						<Conversation
							id={detail.id}
							entries={entries}
							outgoing={outgoing}
							requests={detail.bitlocker?.requests ?? EMPTY_REQUESTS}
							streaming={streaming}
							running={runningNow}
							actionBusy={sessionMutationBusy}
							summarizing={summarizing}
							refresh={refresh}
							onCancelSummary={cancelSummary}
							onFork={example ? NOOP_FORK : handleConversationFork}
							onRetryTurn={example ? undefined : retryUserTurn}
							onNotice={showSuccess}
						/>
					</div>
				) : (
					<div className="chat-empty">
						{example ? (
							"此会话暂无对话示例。"
						) : error ? (
							<ErrorNotice message={error} onRetry={() => void refresh()} />
						) : (
							<output className="session-pending" aria-label="会话加载中" />
						)}
					</div>
				)}
			</div>
			<div className="chat-composer-area">
				{detail?.ui.map((question) => (
					<QuestionPanel key={question.requestId} id={detail.id} question={question} refresh={refresh} />
				))}
				{detail && (
					<QueuePanel id={detail.id} queue={detail.queue} disabled={sessionMutationBusy} refresh={refresh} />
				)}
				<Composer
					key={sessionId}
					sessionId={sessionId}
					busy={turnActionBusy}
					onSubmittingChange={handleComposerSubmittingChange}
					queueActive={!example && queued}
					contextUsage={detail?.contextUsage}
					running={runningNow}
					summaryTag={choice?.mode === "custom" ? { label: summaryTagText, onCancel: clearChoice } : undefined}
					onSummary={
						choice?.mode === "custom"
							? async (instructions) => {
									await runSummary(choice.entryId, instructions);
									clearChoice();
								}
							: undefined
					}
					onSend={example ? undefined : send}
					onStop={example ? undefined : stop}
					onCommand={example ? undefined : command}
					onFork={example ? undefined : () => void forkDirectly()}
				/>
			</div>
		</main>
	);
}
