import { Button, Popover } from "@heroui/react";
import { Download, EllipsisVertical, HardDrive, ListTree, Share2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useParams, useSearchParams, useViewTransitionState } from "react-router";
import { Composer, type ComposerConfig } from "../composer/composer";
import { Conversation } from "../conversation/conversation";
import { visibleQueueItems } from "../conversation/conversation-parts";
import { QuestionPanel } from "../conversation/question-panel";
import { QueuePanel } from "../conversation/queue-panel";
import { apiJson, sessionPath } from "../data/api";
import type { PendingBitLocker } from "../data/bitlocker-state";
import { presentError } from "../data/error-presenter";
import { outgoingFor, useMessageOutbox } from "../data/use-message-outbox";
import { type SessionDetail, useSessionDetail, useSessionTitle } from "../data/web-state";
import { ErrorNotice } from "../shell/error-notice";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { sampleSessions } from "../shell/sample-sessions";
import { branchSummaryChoice, type NavigateResult, summaryBody, summaryTagText } from "../tree/branch-summary";

const EMPTY_REQUESTS: PendingBitLocker[] = [];

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
	const forkBusy = useRef(false);
	const active = useRef(false);
	useEffect(() => {
		active.current = true;
		return () => {
			active.current = false;
		};
	}, []);
	const [messageMenuOpen, setMessageMenuOpen] = useState(false);
	const messageMenuTrigger = useRef<HTMLButtonElement>(null);
	usePanelDismiss(messageMenuOpen, () => setMessageMenuOpen(false), undefined, messageMenuTrigger);
	const location = useLocation();
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
		const token = getComputedStyle(document.documentElement).getPropertyValue("--duration-quick").trim();
		const duration = Number.parseFloat(token) * (token.endsWith("ms") ? 1 : 1000);
		const timer = window.setTimeout(() => setLinearEntering(false), Number.isFinite(duration) ? duration : 150);
		return () => window.clearTimeout(timer);
	}, [entranceId]);
	const title = useSessionTitle(sessionId, example?.title ?? "会话", !example);
	const [summarizing, setSummarizing] = useState(false);
	const [summaryNote, setSummaryNote] = useState("");
	const startedSummary = useRef("");
	const choice = useMemo(() => (example ? undefined : branchSummaryChoice(params)), [example, params]);
	const clearChoice = useCallback(() => {
		if (sessionId) navigate(`/s/${sessionId}`, { replace: true });
	}, [navigate, sessionId]);
	const runSummary = useCallback(
		async (entryId: string, customInstructions?: string) => {
			if (!sessionId) return;
			setSummarizing(true);
			setSummaryNote("");
			try {
				const result = await apiJson<NavigateResult>(sessionPath(sessionId, "/tree/navigate"), {
					method: "POST",
					idempotencyKey: crypto.randomUUID(),
					body: summaryBody(entryId, customInstructions),
				});
				if (result.cancelled) setSummaryNote("已取消分支总结，会话位置没有变化。");
			} catch (cause) {
				setSummaryNote(cause instanceof Error ? cause.message : "分支总结未完成");
			} finally {
				setSummarizing(false);
				await refresh();
			}
		},
		[sessionId, refresh],
	);
	// 选「总结」时树页只留下地址参数，真正的导航请求在这里发起：叶指针未动之前，那段分支还在。
	useEffect(() => {
		if (!choice || choice.mode !== "summarize" || startedSummary.current === choice.entryId) return;
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
	async function forkDirectly(explicitEntryId?: string) {
		if (!sessionId || forkBusy.current) return;
		if (detail?.running || streaming) {
			setSummaryNote("请等待当前回复完成后再分支新会话。");
			return;
		}
		const targetEntryId =
			explicitEntryId ??
			[...(loadedDetail?.entries ?? [])]
				.reverse()
				.find(
					(entry) =>
						entry.id &&
						entry.message.role === "assistant" &&
						entry.message.stopReason != null &&
						entry.message.stopReason !== "toolUse",
				)?.id;
		if (!targetEntryId) {
			setSummaryNote("当前会话尚无可用于分支的已完成回复。");
			return;
		}
		forkBusy.current = true;
		setSummaryNote("");
		try {
			const created = await apiJson<{ id: string }>(sessionPath(sessionId, "/fork"), {
				method: "POST",
				body: { entryId: targetEntryId },
			});
			if (!created?.id) throw new Error("后台未返回新会话 ID。");
			if (active.current) navigate(`/s/${encodeURIComponent(created.id)}`);
		} catch (cause) {
			setSummaryNote(cause instanceof Error ? cause.message : "分支会话失败，请重试");
		} finally {
			forkBusy.current = false;
		}
	}
	return (
		<main className="chat-main">
			<header className="chat-header">
				<HardDrive size={22} />
				<h1>{title}</h1>
				{example && <span className="preview-tag">设计预览 · 示例数据</span>}
				{!example && (
					<Popover isOpen={messageMenuOpen} onOpenChange={setMessageMenuOpen}>
						<Button
							ref={messageMenuTrigger}
							variant="ghost"
							isIconOnly
							className="chat-menu-trigger"
							aria-label="消息菜单"
						>
							<EllipsisVertical size={22} />
						</Button>
						<Popover.Content placement="bottom end" className="sidebar-account-popover">
							<Popover.Dialog aria-label="消息菜单" className="sidebar-account-menu">
								<Link className="sidebar-account-item" to={`/s/${sessionId}/tree`} viewTransition>
									<ListTree size={18} aria-hidden="true" />
									分支树
								</Link>
								<Link
									className="sidebar-account-item"
									to={`/s/${sessionId}/export`}
									viewTransition
									onClick={() => setMessageMenuOpen(false)}
								>
									<Download size={18} aria-hidden="true" />
									导出会话
								</Link>
								<button type="button" className="sidebar-account-item" disabled title="计划中">
									<Share2 size={18} aria-hidden="true" />
									分享
								</button>
							</Popover.Dialog>
						</Popover.Content>
					</Popover>
				)}
			</header>
			{summaryNote && (
				<div className="connection-banner" role="alert">
					{summaryNote}
				</div>
			)}
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
							running={!!detail.running || !!streaming}
							summarizing={summarizing}
							refresh={refresh}
							onCancelSummary={cancelSummary}
							onFork={example ? () => undefined : (entryId) => void forkDirectly(entryId)}
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
				{detail && <QueuePanel id={detail.id} queue={detail.queue} refresh={refresh} />}
				<Composer
					key={sessionId}
					sessionId={sessionId}
					queueActive={!example && queued}
					contextUsage={detail?.contextUsage}
					running={!!detail?.running || !!streaming}
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
