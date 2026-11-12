import { Button, Tooltip } from "@heroui/react";
import { ArrowDown, Copy, GitBranch, GitFork, X } from "lucide-react";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { matchToolResults } from "../cards/match-results";
import type { PendingBitLocker } from "../data/bitlocker-state";
import type { OutgoingMessage } from "../data/message-outbox";
import type { Message } from "../data/web-state";
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
import { UserMessage } from "./user-message";

const NO_OUTGOING: readonly OutgoingMessage[] = [];

function AssistantTurnImpl({
	id,
	turn,
	live,
	resultsByTurn,
	requests,
	onFork,
	onError,
}: {
	id: string;
	turn: Turn;
	live: boolean;
	resultsByTurn: Map<string, Map<string, Message>>;
	requests: PendingBitLocker[];
	onFork: () => void;
	onError: (text: string) => void;
}) {
	const last = turn.assistants.at(-1);
	// 完成时间由服务端随会话保存，刷新和切换页面沿用同一工作区间。
	const [now, setNow] = useState(() => Date.now());
	useEffect(() => {
		if (!live) return;
		setNow(Date.now());
		const timer = window.setInterval(() => setNow(Date.now()), 1000);
		return () => window.clearInterval(timer);
	}, [live]);
	const work = turn.assistants.map(({ message }) => message.work).filter((value) => value !== undefined);
	const startedAt = turn.user?.message.timestamp ?? work[0]?.startedAt ?? turn.assistants[0]?.message.timestamp ?? now;
	const endedAt = work.length ? Math.max(...work.map((value) => value.endedAt)) : undefined;
	const elapsed = formatDuration(Math.max((live ? now : (endedAt ?? startedAt)) - startedAt, live ? 1000 : 0));
	return (
		<AssistantContent
			id={id}
			turn={turn}
			live={live}
			resultsByTurn={resultsByTurn}
			requests={requests}
			firstProcessLabel={!live && endedAt === undefined ? "已完成" : `${live ? "工作中" : "已工作"} ${elapsed}`}
		>
			{last?.id && last.message.stopReason != null && last.message.stopReason !== "toolUse" && (
				<div className="answer-actions">
					<Tooltip delay={80} closeDelay={0}>
						<Button
							variant="ghost"
							isIconOnly
							aria-label="复制回答"
							onPress={() => {
								void navigator.clipboard
									.writeText(textOf(last.message))
									.catch(() => onError("无法复制，请检查浏览器权限。"));
							}}
						>
							<Copy size={16} />
						</Button>
						<Tooltip.Content placement="top">复制</Tooltip.Content>
					</Tooltip>
					<Tooltip delay={80} closeDelay={0}>
						<Button variant="ghost" isIconOnly aria-label="从此处分支新会话" onPress={() => onFork()}>
							<GitFork size={16} />
						</Button>
						<Tooltip.Content placement="top">从此处分支新会话</Tooltip.Content>
					</Tooltip>
					<span>
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
		prev.id === next.id &&
		prev.resultsByTurn === next.resultsByTurn &&
		prev.requests === next.requests &&
		prev.onFork === next.onFork &&
		prev.onError === next.onError &&
		prev.turn.user === next.turn.user &&
		prev.turn.assistants.length === next.turn.assistants.length &&
		prev.turn.assistants.every((entry, index) => entry === next.turn.assistants[index]),
);

export function Conversation({
	id,
	entries,
	streaming,
	requests,
	running,
	summarizing,
	refresh,
	onCancelSummary,
	onFork,
	outgoing = NO_OUTGOING,
}: {
	id: string;
	outgoing?: readonly OutgoingMessage[];
	entries: { id?: string; message: Message }[];
	streaming?: Message;
	requests: PendingBitLocker[];
	running: boolean;
	summarizing: boolean;
	refresh: () => Promise<void>;
	onCancelSummary: () => Promise<void>;
	onFork: () => void;
}) {
	const scroll = useRef<HTMLDivElement>(null);
	const scrollAnimation = useRef<number | null>(null);
	const followingRef = useRef(true);
	const [following, setFollowing] = useState(true);
	const [showBottom, setShowBottom] = useState(false);
	const [error, setError] = useState("");
	const [visibleCount, setVisibleCount] = useState(80);
	const prependAnchor = useRef<{ key: string; top: number } | null>(null);
	const [mountedAt] = useState(() => Date.now());
	const resultsByTurn = useMemo(() => matchToolResults(entries), [entries]);
	const history = useMemo(() => entries.filter(({ message }) => isConversationMessage(message)), [entries]);
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
			followingRef.current = true;
			setFollowing(true);
			return;
		}
		const start = el.scrollTop;
		const started = performance.now();
		const durationToken = getComputedStyle(el).getPropertyValue("--duration-fast").trim();
		const durationValue = Number.parseFloat(durationToken);
		const duration = Number.isFinite(durationValue) ? durationValue * (durationToken.endsWith("ms") ? 1 : 1000) : 250;
		const step = (now: number) => {
			const progress = Math.min((now - started) / duration, 1);
			const eased = 1 - (1 - progress) ** 3;
			el.scrollTop = start + (el.scrollHeight - el.clientHeight - start) * eased;
			if (progress < 1) scrollAnimation.current = requestAnimationFrame(step);
			else {
				scrollAnimation.current = null;
				followingRef.current = true;
				setFollowing(true);
			}
		};
		scrollAnimation.current = requestAnimationFrame(step);
	}
	useEffect(() => {
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const onChange = () => {
			if (!motion.matches || scrollAnimation.current === null) return;
			cancelScroll();
			if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight;
			followingRef.current = true;
			setFollowing(true);
		};
		const el = scroll.current;
		const onWheel = (event: WheelEvent) => {
			cancelScroll();
			if (event.deltaY < 0) {
				followingRef.current = false;
				setFollowing(false);
			}
		};
		const onKeyDown = (event: KeyboardEvent) => {
			if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key)) {
				cancelScroll();
				if (["ArrowUp", "PageUp", "Home"].includes(event.key)) {
					followingRef.current = false;
					setFollowing(false);
				}
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
	}, [cancelScroll]);
	useEffect(() => {
		if (!following) {
			setShowBottom(true);
			return;
		}
		if (!showBottom) return;
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const token = getComputedStyle(document.documentElement).getPropertyValue("--duration-quick").trim();
		const value = Number.parseFloat(token);
		const duration = Number.isFinite(value) ? value * (token.endsWith("ms") ? 1 : 1000) : 150;
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
		if (delta !== 0) container.scrollTop += delta;
	}, [visibleCount]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: entries 与 streaming 只作为滚动触发信号，不在 effect 体内读取
	useLayoutEffect(() => {
		if (following && followingRef.current) scroll.current?.scrollTo({ top: scroll.current.scrollHeight });
	}, [entries, streaming, following, outgoing]);
	return (
		<>
			<div
				className="conversation"
				ref={scroll}
				onScroll={() => {
					const el = scroll.current;
					if (el && scrollAnimation.current === null) {
						const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 100;
						followingRef.current = nearBottom;
						setFollowing(nearBottom);
					}
				}}
			>
				{sliceStart > 0 && (
					<Button
						variant="ghost"
						className="show-earlier"
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
							followingRef.current = false;
							setVisibleCount((count) => count + 80);
							setFollowing(false);
						}}
					>
						显示更早的消息（剩余 {sliceStart} 条）
					</Button>
				)}
				{shownTurns.map((turn, turnIndex) => {
					const turnKey = `turn-${turn.user?.message.clientMessageId ?? turn.user?.message.timestamp ?? turn.summary?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? turnIndex}`;
					if (turn.summary)
						return (
							<div className="turn-summary-wrap" key={turnKey} data-turn-key={turnKey}>
								<SummaryEntry entry={turn.summary} />
							</div>
						);
					const live = turnIndex === active;
					return (
						<div className="turn" key={turnKey} data-turn-key={turnKey}>
							{turn.user && (
								<UserMessage
									message={turn.user.message}
									outgoing={outgoingById.get(turn.user.message.clientMessageId ?? "")}
									fresh={turn.user.message.timestamp >= mountedAt - 2000}
								/>
							)}
							{(turn.assistants.length > 0 || live) && (
								<AssistantTurn
									id={id}
									turn={turn}
									live={live}
									resultsByTurn={resultsByTurn}
									requests={requests}
									onFork={onFork}
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
							className="summary-cancel"
							aria-label="取消分支总结"
							onPress={() => void onCancelSummary()}
						>
							<X size={18} />
						</Button>
					</output>
				)}
				{error && (
					<p role="alert" className="message-error">
						{error}
						<Button
							variant="ghost"
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
					回到底部
				</Button>
			)}
		</>
	);
}
