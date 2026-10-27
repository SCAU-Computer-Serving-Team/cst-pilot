import { Button, Popover } from "@heroui/react";
import { Download, EllipsisVertical, HardDrive, ListTree, Share2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useParams, useSearchParams, useViewTransitionState } from "react-router";
import { Composer } from "../composer/composer";
import { Conversation } from "../conversation/conversation";
import { visibleQueueItems } from "../conversation/conversation-parts";
import { QuestionPanel } from "../conversation/question-panel";
import { QueuePanel } from "../conversation/queue-panel";
import { apiJson, sessionPath } from "../data/api";
import type { PendingBitLocker } from "../data/bitlocker-state";
import { presentError } from "../data/error-presenter";
import { useSessionDetail, useSessionTitle } from "../data/web-state";
import { ErrorNotice } from "../shell/error-notice";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { sampleSessions } from "../shell/sample-sessions";
import { branchSummaryChoice, type NavigateResult, summaryBody, summaryTagText } from "../tree/branch-summary";
import { ForkPicker, forkTargets } from "../tree/fork-picker";

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
	const { detail, streaming, error, refresh } = useSessionDetail(example ? undefined : sessionId);
	const [forkOpen, setForkOpen] = useState(false);
	const [messageMenuOpen, setMessageMenuOpen] = useState(false);
	usePanelDismiss(messageMenuOpen, () => setMessageMenuOpen(false));
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
	const [forkSource, setForkSource] = useState("");
	// 派生跳转带入的来源提示：换路由即读取，读后立刻清掉历史状态避免刷新后复现
	useEffect(() => {
		const state = location.state as { forkSource?: string } | null;
		if (state?.forkSource) {
			setForkSource(state.forkSource);
			window.history.replaceState({}, "");
		}
	}, [location.state]);
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
	async function send(
		text: string,
		config: { messageId: string; images: { mimeType: string; data: string }[]; skill?: string },
	) {
		if (!sessionId) return;
		setForkSource("");
		// 提交成功即返回（立即清空输入框、复位发送按钮）；队列与气泡由 SSE 事件驱动的刷新补齐，
		// 避免落盘窗口内 GET 偶发变慢时按钮长时间置灰。
		await apiJson(sessionPath(sessionId, "/messages"), {
			method: "POST",
			idempotencyKey: config.messageId,
			body: {
				id: config.messageId,
				text,
				delivery: "queue",
				images: config.images,
				...(config.skill ? { skill: config.skill } : {}),
			},
		});
		void refresh();
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
	return (
		<main className="chat-main">
			<header className="chat-header">
				<HardDrive size={22} />
				<h1>{title}</h1>
				{example && <span className="preview-tag">设计预览 · 示例数据</span>}
				{!example && (
					<Popover isOpen={messageMenuOpen} onOpenChange={setMessageMenuOpen}>
						<Button variant="ghost" isIconOnly className="chat-menu-trigger" aria-label="消息菜单">
							<EllipsisVertical size={22} />
						</Button>
						<Popover.Content placement="bottom end" className="sidebar-account-popover">
							<Popover.Dialog aria-label="消息菜单" className="sidebar-account-menu">
								<Link className="sidebar-account-item" to={`/s/${sessionId}/tree`} viewTransition>
									<ListTree size={18} aria-hidden="true" />
									分支树
								</Link>
								<button type="button" className="sidebar-account-item" disabled title="计划中">
									<Download size={18} aria-hidden="true" />
									导出会话
								</button>
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
			{forkSource && (
				<output className="fork-source-banner">
					<span>{`派生自：${forkSource}`}</span>
					<Button variant="ghost" isIconOnly aria-label="关闭提示" onPress={() => setForkSource("")}>
						<X size={14} />
					</Button>
				</output>
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
							requests={detail.bitlocker?.requests ?? EMPTY_REQUESTS}
							streaming={streaming}
							running={!!detail.running || !!streaming}
							summarizing={summarizing}
							refresh={refresh}
							onCancelSummary={cancelSummary}
							onFork={example ? () => undefined : () => setForkOpen(true)}
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
					onFork={example ? undefined : () => setForkOpen(true)}
				/>
			</div>
			{forkOpen && (
				<ForkPicker
					sessionId={sessionId ?? ""}
					targets={forkTargets(detail?.entries ?? [])}
					running={!!detail?.running || !!streaming}
					onClose={() => setForkOpen(false)}
				/>
			)}
		</main>
	);
}
