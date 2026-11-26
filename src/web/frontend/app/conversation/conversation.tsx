import { Button, Tooltip } from "@heroui/react";
import { ArrowDown, Check, Copy, GitBranch, GitFork, RotateCcw, X } from "lucide-react";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { matchToolResults } from "../cards/match-results";
import type { PendingBitLocker } from "../data/bitlocker-state";
import type { OutgoingMessage } from "../data/message-outbox";
import type { Message } from "../data/web-state";
import { readDurationMs } from "../shell/motion-tokens";
import { setHardwareSamplingActive } from "../shell/shader-gate";
import { useCopyFeedback } from "../shell/use-copy-feedback";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import { AssistantContent } from "./assistant-content";
import {
	activeTurnIndex,
	formatDuration,
	groupTurns,
	isConversationMessage,
	type Turn,
	textOf,
} from "./conversation-parts";
import { SummaryEntry } from "./summary-entry";
import { type UserImagePart, UserMessage } from "./user-message";

const NO_OUTGOING: readonly OutgoingMessage[] = [];

function AssistantTurnImpl({
	id,
	turn,
	live,
	actionsDisabled,
	resultsByTurn,
	requests,
	locateEntryId,
	locateToolCallId,
	onFork,
	onRetryTurn,
	onNotice,
	onError,
}: {
	id: string;
	turn: Turn;
	live: boolean;
	actionsDisabled: boolean;
	resultsByTurn: Map<string, Map<string, Message>>;
	requests: PendingBitLocker[];
	locateEntryId?: string;
	locateToolCallId?: string;
	onFork: (entryId: string) => void;
	onRetryTurn?: (entryId: string, text: string, images: UserImagePart[]) => Promise<boolean>;
	onNotice?: (message: string) => void;
	onError: (text: string) => void;
}) {
	const last = turn.assistants.at(-1);
	// 完成时间由服务端随会话保存，刷新和切换页面沿用同一工作区间。
	const [now, setNow] = useState(() => Date.now());
	const { copied, copy } = useCopyFeedback({
		onCopied: () => onNotice?.("已复制回答"),
		onError: () => onError("无法复制，请检查浏览器权限。"),
	});
	const shell = useWorkspaceShell();
	const tooltipsDisabled = Boolean(shell?.appActive);
	useEffect(() => {
		if (!live) return;
		let timer: number | null = null;
		const start = () => {
			if (document.hidden) return;
			setNow(Date.now());
			if (timer === null) timer = window.setInterval(() => setNow(Date.now()), 1000);
		};
		const stop = () => {
			if (timer !== null) {
				window.clearInterval(timer);
				timer = null;
			}
		};
		const onVisibility = () => {
			if (document.hidden) stop();
			else start();
		};
		start();
		document.addEventListener("visibilitychange", onVisibility);
		return () => {
			stop();
			document.removeEventListener("visibilitychange", onVisibility);
		};
	}, [live]);
	const work = turn.assistants.map(({ message }) => message.work).filter((value) => value !== undefined);
	const startedAt = turn.user?.message.timestamp ?? work[0]?.startedAt ?? turn.assistants[0]?.message.timestamp ?? now;
	const endedAt = work.length ? Math.max(...work.map((value) => value.endedAt)) : undefined;
	const elapsed = formatDuration(Math.max((live ? now : (endedAt ?? startedAt)) - startedAt, live ? 1000 : 0));
	const userEntryId = turn.user?.id;
	const userText = turn.user ? textOf(turn.user.message) : "";
	const userImages: UserImagePart[] =
		turn.user && Array.isArray(turn.user.message.content)
			? turn.user.message.content.filter((part): part is UserImagePart => part.type === "image")
			: [];
	const canRetry =
		!actionsDisabled && !!userEntryId && !!onRetryTurn && (Boolean(userText.trim()) || userImages.length > 0);
	return (
		<AssistantContent
			id={id}
			turn={turn}
			live={live}
			resultsByTurn={resultsByTurn}
			requests={requests}
			locateEntryId={locateEntryId}
			locateToolCallId={locateToolCallId}
			firstProcessLabel={!live && endedAt === undefined ? "已完成" : `${live ? "工作中" : "已工作"} ${elapsed}`}
		>
			{last?.id && last.message.stopReason != null && last.message.stopReason !== "toolUse" && (
				<div className="answer-actions">
					<Tooltip delay={80} closeDelay={0} isDisabled={tooltipsDisabled}>
						<Button
							variant="ghost"
							isIconOnly
							className="btn-icon xs-24"
							data-copied={copied || undefined}
							aria-label={copied ? "已复制回答" : "复制回答"}
							onPress={() => void copy(textOf(last.message))}
						>
							{copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
						</Button>
						<Tooltip.Content className="app-tooltip" placement="top">
							{copied ? "已复制" : "复制"}
						</Tooltip.Content>
					</Tooltip>
					<Tooltip delay={80} closeDelay={0} isDisabled={tooltipsDisabled}>
						<Button
							variant="ghost"
							isIconOnly
							className="btn-icon xs-24"
							isDisabled={actionsDisabled}
							aria-label="从此处分支新会话"
							onPress={() => {
								if (last.id) onFork(last.id);
							}}
						>
							<GitFork size={14} aria-hidden="true" />
						</Button>
						<Tooltip.Content className="app-tooltip" placement="top">
							从此处分支新会话
						</Tooltip.Content>
					</Tooltip>
					<Tooltip delay={80} closeDelay={0} isDisabled={tooltipsDisabled}>
						<Button
							variant="ghost"
							isIconOnly
							className="btn-icon xs-24"
							isDisabled={!canRetry}
							aria-label="重新生成"
							onPress={() => {
								if (userEntryId && onRetryTurn) void onRetryTurn(userEntryId, userText, userImages);
							}}
						>
							<RotateCcw size={14} aria-hidden="true" />
						</Button>
						<Tooltip.Content className="app-tooltip" placement="top">
							重新生成
						</Tooltip.Content>
					</Tooltip>
					<span className="answer-timestamp">
						{new Date(last.message.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}
					</span>
				</div>
			)}
		</AssistantContent>
	);
}
// 轮次按条目引用比较：流式刷新只替换正在生成的条目，历史轮次整体跳过重渲染（含 Markdown 重解析）。
const AssistantTurn = memo(
	AssistantTurnImpl,
	(prev, next) =>
		prev.live === next.live &&
		prev.actionsDisabled === next.actionsDisabled &&
		prev.id === next.id &&
		prev.resultsByTurn === next.resultsByTurn &&
		prev.requests === next.requests &&
		prev.locateEntryId === next.locateEntryId &&
		prev.locateToolCallId === next.locateToolCallId &&
		prev.onFork === next.onFork &&
		prev.onRetryTurn === next.onRetryTurn &&
		prev.onNotice === next.onNotice &&
		prev.onError === next.onError &&
		prev.turn.user === next.turn.user &&
		prev.turn.assistants.length === next.turn.assistants.length &&
		prev.turn.assistants.every((entry, index) => entry === next.turn.assistants[index]),
);

export type LocateRequest = {
	entryId: string;
	query: string;
	nonce: number;
};

export function Conversation({
	id,
	entries,
	streaming,
	requests,
	running,
	actionBusy = false,
	summarizing,
	locate = null,
	refresh,
	onCancelSummary,
	onFork,
	onRetryTurn,
	onNotice,
	outgoing = NO_OUTGOING,
}: {
	id: string;
	outgoing?: readonly OutgoingMessage[];
	entries: { id?: string; message: Message }[];
	streaming?: Message;
	requests: PendingBitLocker[];
	running: boolean;
	actionBusy?: boolean;
	summarizing: boolean;
	locate?: LocateRequest | null;
	refresh: () => Promise<void>;
	onCancelSummary: () => Promise<void>;
	onFork: (entryId: string) => void;
	onRetryTurn?: (entryId: string, text: string, images: UserImagePart[]) => Promise<boolean>;
	onNotice?: (message: string) => void;
}) {
	const actionsDisabled = running || actionBusy;
	const scroll = useRef<HTMLDivElement>(null);
	const scrollAnimation = useRef<number | null>(null);
	const followingRef = useRef(true);
	const lastScrollTopRef = useRef(0);
	const [following, setFollowing] = useState(true);
	const updateFollowing = useCallback((next: boolean) => {
		followingRef.current = next;
		setFollowing(next);
	}, []);
	const [showBottom, setShowBottom] = useState(false);
	const [error, setError] = useState("");
	const [visibleCount, setVisibleCount] = useState(80);
	const prependAnchor = useRef<{ key: string; top: number } | null>(null);
	const [mountedAt] = useState(() => Date.now());
	const resultsByTurn = useMemo(() => matchToolResults(entries), [entries]);
	const history = useMemo(() => entries.filter(({ message }) => isConversationMessage(message)), [entries]);

	// 解析搜索命中定位：若命中工具输出，先映射回对应的 assistant toolCall 所在轮次。
	const locateResolved = useMemo(() => {
		if (!locate?.entryId) return null;
		let toolCallId: string | undefined;
		let targetHistoryIndex = history.findIndex((item) => item.id === locate.entryId);
		if (targetHistoryIndex < 0) {
			const rawTool = entries.find((item) => item.id === locate.entryId && item.message.role === "toolResult");
			if (rawTool?.message.toolCallId) {
				toolCallId = rawTool.message.toolCallId;
				targetHistoryIndex = history.findIndex(
					(item) =>
						item.message.role === "assistant" &&
						Array.isArray(item.message.content) &&
						item.message.content.some((part) => part.type === "toolCall" && part.id === toolCallId),
				);
			}
		}
		if (targetHistoryIndex < 0) return null;
		return {
			entryId: locate.entryId,
			query: locate.query,
			nonce: locate.nonce,
			toolCallId,
			targetHistoryIndex,
			targetEntryId: history[targetHistoryIndex]?.id,
		};
	}, [entries, history, locate]);

	const [activeLocate, setActiveLocate] = useState<{
		entryId: string;
		targetEntryId?: string;
		toolCallId?: string;
		query: string;
		nonce: number;
		fading: boolean;
	} | null>(null);
	const appliedLocateNonce = useRef<number | null>(null);

	// 目标在截断窗外时立即扩窗并暂停跟随底部。
	useEffect(() => {
		if (!locateResolved) return;
		const needed = history.length - locateResolved.targetHistoryIndex + 4;
		if (needed > visibleCount) {
			setVisibleCount(needed);
		}
		if (appliedLocateNonce.current !== locateResolved.nonce) {
			appliedLocateNonce.current = locateResolved.nonce;
			updateFollowing(false);
			setActiveLocate({
				entryId: locateResolved.entryId,
				targetEntryId: locateResolved.targetEntryId,
				toolCallId: locateResolved.toolCallId,
				query: locateResolved.query,
				nonce: locateResolved.nonce,
				fading: false,
			});
		}
	}, [history.length, locateResolved, updateFollowing, visibleCount]);

	const activeLocateNonce = activeLocate?.nonce ?? null;
	// 定位高亮保持 1.3s，随后 500ms 平滑消退并恢复常态（共 1.8s）。
	useEffect(() => {
		if (activeLocateNonce === null) return;
		const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
		const frame = requestAnimationFrame(() => {
			const el = scroll.current?.querySelector<HTMLElement>('[data-located-turn="true"]');
			el?.scrollIntoView({ behavior: reducedMotion ? "auto" : "smooth", block: "center" });
		});
		const fadeTimer = window.setTimeout(() => {
			setActiveLocate((cur) => (cur && cur.nonce === activeLocateNonce ? { ...cur, fading: true } : cur));
		}, 1300);
		const clearTimer = window.setTimeout(() => {
			setActiveLocate((cur) => (cur && cur.nonce === activeLocateNonce ? null : cur));
		}, 1800);
		return () => {
			cancelAnimationFrame(frame);
			window.clearTimeout(fadeTimer);
			window.clearTimeout(clearTimer);
		};
	}, [activeLocateNonce]);
	const sliceStart = useMemo(() => {
		let start = Math.max(0, history.length - visibleCount);
		while (start > 0 && history[start]?.message.role === "assistant") start--;
		return start;
	}, [history, visibleCount]);
	const display = useMemo(() => {
		const shown = history.slice(sliceStart);
		if (!streaming) return shown;
		const active = shown.findIndex(
			({ message }) => message.role === "assistant" && message.timestamp === streaming.timestamp,
		);
		if (active >= 0) {
			const copy = shown.slice();
			copy[active] = { ...copy[active], message: streaming };
			return copy;
		}
		return [...shown, { message: streaming }];
	}, [history, sliceStart, streaming]);
	const turns = useMemo(() => groupTurns(display), [display]);
	const active = activeTurnIndex(turns, running);
	const hardwareSamplingActive = useMemo(() => {
		if (!running || active < 0) return false;
		const liveTurn = turns[active];
		if (!liveTurn) return false;
		for (const entry of liveTurn.assistants) {
			const content = entry.message.content;
			if (!Array.isArray(content)) continue;
			const key = entry.id ?? `stamp:${entry.message.timestamp}`;
			const matched = resultsByTurn.get(key);
			for (const part of content) {
				if (part.type !== "toolCall" || part.name !== "sys") continue;
				const scope = part.arguments?.scope;
				if (scope !== "gpu" && scope !== "sensor") continue;
				if (!part.execution?.result && !matched?.has(part.id)) return true;
			}
		}
		return false;
	}, [running, active, turns, resultsByTurn]);
	useEffect(() => {
		setHardwareSamplingActive(hardwareSamplingActive);
		return () => setHardwareSamplingActive(false);
	}, [hardwareSamplingActive]);
	const pendingTurns = outgoing.filter((item) => !history.some(({ message }) => message.clientMessageId === item.id));
	const outgoingById = new Map(pendingTurns.map((item) => [item.id, item]));
	const shownTurns: Turn[] = [
		...turns,
		...pendingTurns.map((item) => ({
			user: {
				message: { role: "user" as const, content: item.text, timestamp: item.createdAt, clientMessageId: item.id },
			},
			assistants: [],
		})),
	];
	const cancelScroll = useCallback(() => {
		if (scrollAnimation.current === null) return;
		cancelAnimationFrame(scrollAnimation.current);
		scrollAnimation.current = null;
	}, []);
	function goToBottom() {
		const el = scroll.current;
		if (!el) return;
		cancelScroll();
		if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
			el.scrollTop = el.scrollHeight;
			lastScrollTopRef.current = el.scrollTop;
			updateFollowing(true);
			return;
		}
		const start = el.scrollTop;
		const started = performance.now();
		const duration = readDurationMs(el, "--normal", 240);
		const step = (now: number) => {
			const progress = Math.min((now - started) / duration, 1);
			const eased = 1 - (1 - progress) ** 3;
			el.scrollTop = start + (el.scrollHeight - el.clientHeight - start) * eased;
			lastScrollTopRef.current = el.scrollTop;
			if (progress < 1) scrollAnimation.current = requestAnimationFrame(step);
			else {
				scrollAnimation.current = null;
				updateFollowing(true);
			}
		};
		scrollAnimation.current = requestAnimationFrame(step);
	}
	useEffect(() => {
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const onChange = () => {
			if (!motion.matches || scrollAnimation.current === null) return;
			cancelScroll();
			if (scroll.current) {
				scroll.current.scrollTop = scroll.current.scrollHeight;
				lastScrollTopRef.current = scroll.current.scrollTop;
			}
			updateFollowing(true);
		};
		const el = scroll.current;
		const onWheel = (event: WheelEvent) => {
			cancelScroll();
			if (event.deltaY < 0) updateFollowing(false);
		};
		const onKeyDown = (event: KeyboardEvent) => {
			if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key)) {
				cancelScroll();
				if (["ArrowUp", "PageUp", "Home"].includes(event.key)) updateFollowing(false);
			}
		};
		motion.addEventListener("change", onChange);
		el?.addEventListener("wheel", onWheel, { passive: true });
		el?.addEventListener("touchstart", cancelScroll, { passive: true });
		el?.addEventListener("pointerdown", cancelScroll);
		el?.addEventListener("keydown", onKeyDown);
		return () => {
			cancelScroll();
			motion.removeEventListener("change", onChange);
			el?.removeEventListener("wheel", onWheel);
			el?.removeEventListener("touchstart", cancelScroll);
			el?.removeEventListener("pointerdown", cancelScroll);
			el?.removeEventListener("keydown", onKeyDown);
		};
	}, [cancelScroll, updateFollowing]);
	useEffect(() => {
		if (!following) {
			setShowBottom(true);
			return;
		}
		if (!showBottom) return;
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const duration = readDurationMs(document.documentElement, "--fast", 150);
		const timer = window.setTimeout(() => setShowBottom(false), motion.matches ? 0 : duration);
		const finish = () => {
			if (motion.matches) setShowBottom(false);
		};
		motion.addEventListener("change", finish);
		return () => {
			window.clearTimeout(timer);
			motion.removeEventListener("change", finish);
		};
	}, [following, showBottom]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: visibleCount 增载后同步补偿滚动位置
	useLayoutEffect(() => {
		const anchor = prependAnchor.current;
		const container = scroll.current;
		if (!anchor || !container) return;
		prependAnchor.current = null;
		const target = container.querySelector<HTMLElement>(`[data-turn-key="${CSS.escape(anchor.key)}"]`);
		if (!target) return;
		const delta = target.getBoundingClientRect().top - anchor.top;
		if (delta !== 0) {
			container.scrollTop += delta;
			lastScrollTopRef.current = container.scrollTop;
		}
	}, [visibleCount]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: entries 与 streaming 只作为滚动触发信号，不在 effect 体内读取
	useLayoutEffect(() => {
		const el = scroll.current;
		if (el && following && followingRef.current) {
			el.scrollTo({ top: el.scrollHeight });
			lastScrollTopRef.current = el.scrollTop;
		}
	}, [entries, streaming, following, outgoing]);
	return (
		<>
			<div
				className="conversation"
				ref={scroll}
				onScroll={() => {
					const el = scroll.current;
					if (!el) return;
					const prevTop = lastScrollTopRef.current;
					const curTop = el.scrollTop;
					lastScrollTopRef.current = curTop;
					if (scrollAnimation.current !== null) return;
					const dist = el.scrollHeight - curTop - el.clientHeight;
					if (curTop < prevTop - 0.5 && dist >= 0.5) {
						updateFollowing(false);
					} else if (!followingRef.current && curTop > prevTop + 0.5 && dist <= 2) {
						updateFollowing(true);
					} else if (followingRef.current && dist >= 100) {
						updateFollowing(false);
					}
				}}
			>
				{sliceStart > 0 && (
					<Button
						variant="ghost"
						className="btn-ghost show-earlier"
						onPress={() => {
							const container = scroll.current;
							if (container) {
								const containerTop = container.getBoundingClientRect().top;
								const firstVisible = [...container.querySelectorAll<HTMLElement>("[data-turn-key]")].find(
									(node) => node.getBoundingClientRect().bottom > containerTop + 4,
								);
								if (firstVisible?.dataset.turnKey) {
									prependAnchor.current = {
										key: firstVisible.dataset.turnKey,
										top: firstVisible.getBoundingClientRect().top,
									};
								}
							}
							setVisibleCount((count) => count + 80);
							updateFollowing(false);
						}}
					>
						显示更早的消息（剩余 {sliceStart} 条）
					</Button>
				)}
				{shownTurns.map((turn, turnIndex) => {
					const turnKey = `turn-${turn.user?.message.clientMessageId ?? turn.user?.message.timestamp ?? turn.summary?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? turnIndex}`;
					const locateMatchId = activeLocate?.targetEntryId ?? activeLocate?.entryId;
					if (turn.summary) {
						const isLocatedSummary = Boolean(locateMatchId && turn.summary.id === locateMatchId);
						return (
							<div
								className="turn-summary-wrap"
								key={turnKey}
								data-turn-key={turnKey}
								data-located-turn={isLocatedSummary ? "true" : undefined}
								data-locate-fading={isLocatedSummary && activeLocate?.fading ? "true" : undefined}
							>
								<SummaryEntry entry={turn.summary} forceOpen={isLocatedSummary} />
							</div>
						);
					}
					const live = turnIndex === active;
					const isLocatedUser = Boolean(locateMatchId && turn.user?.id === locateMatchId);
					const isLocatedAssistant = Boolean(
						locateMatchId && turn.assistants.some((entry) => entry.id === locateMatchId),
					);
					const isLocatedTurn = isLocatedUser || isLocatedAssistant;
					return (
						<div
							className="turn"
							key={turnKey}
							data-turn-key={turnKey}
							data-located-turn={isLocatedTurn ? "true" : undefined}
							data-locate-fading={isLocatedTurn && activeLocate?.fading ? "true" : undefined}
						>
							{turn.user && (
								<UserMessage
									entryId={turn.user.id}
									message={turn.user.message}
									outgoing={outgoingById.get(turn.user.message.clientMessageId ?? "")}
									fresh={turn.user.message.timestamp >= mountedAt - 2000}
									running={actionsDisabled}
									locateQuery={isLocatedUser ? activeLocate?.query : undefined}
									locateFading={isLocatedUser && Boolean(activeLocate?.fading)}
									onRetryTurn={onRetryTurn}
									onNotice={onNotice}
									onError={setError}
								/>
							)}
							{(turn.assistants.length > 0 || live) && (
								<AssistantTurn
									id={id}
									turn={turn}
									live={live}
									actionsDisabled={actionsDisabled}
									resultsByTurn={resultsByTurn}
									requests={requests}
									locateEntryId={isLocatedAssistant ? activeLocate?.entryId : undefined}
									locateToolCallId={isLocatedAssistant ? activeLocate?.toolCallId : undefined}
									onFork={onFork}
									onRetryTurn={onRetryTurn}
									onNotice={onNotice}
									onError={setError}
								/>
							)}
						</div>
					);
				})}
				{summarizing && (
					<output className="summary-pending">
						<GitBranch size={18} aria-hidden="true" />
						<span>正在总结所选分支…</span>
						<Button
							variant="ghost"
							isIconOnly
							className="btn-icon xs-24 summary-cancel"
							aria-label="取消分支总结"
							onPress={() => void onCancelSummary()}
						>
							<X size={14} aria-hidden="true" />
						</Button>
					</output>
				)}
				{error && (
					<p role="alert" className="message-error">
						{error}
						<Button
							variant="ghost"
							className="btn-ghost"
							onPress={() => {
								setError("");
								void refresh();
							}}
						>
							关闭
						</Button>
					</p>
				)}
			</div>
			{(!following || showBottom) && (
				<Button
					className="back-to-bottom"
					data-hiding={following || undefined}
					aria-hidden={following || undefined}
					isDisabled={following}
					onPress={goToBottom}
				>
					<ArrowDown size={16} aria-hidden="true" />
					<span>回到底部</span>
				</Button>
			)}
		</>
	);
}
