import { Button, Disclosure, Tooltip } from "@heroui/react";
import { ArrowDown, ChevronDown, Copy, GitBranch, GitFork, X } from "lucide-react";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { matchToolResults } from "../cards/match-results";
import { SpecialTool } from "../cards/special-tool";
import type { PendingBitLocker } from "../data/bitlocker-state";
import type { Message } from "../data/web-state";
import { PreviewableImage } from "../images/image-preview";
import { CodeBlock } from "../shell/code-block";
import { ErrorNotice } from "../shell/error-notice";
import {
	activeTurnIndex,
	formatDuration,
	groupTurns,
	splitTurn,
	type Turn,
	type TurnEntry,
	textOf,
} from "./conversation-parts";
import { Markdown } from "./markdown";
import { ProcessSegment } from "./process-segment";
import { processSegmentLabel, turnSegments } from "./turn-segments";

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
	const { process, finalText } = splitTurn(turn.assistants);
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
		<article className="assistant-block assistant-turn" data-live={live || undefined}>
			{turnSegments(process).map((segment, index) => {
				if (segment.kind === "visible")
					return (
						<div className="turn-visible" key={segment.key}>
							{segment.parts.map(({ entry, final, part }) =>
								part.kind === "special" ? (
									<SpecialTool
										key={`${entry.message.timestamp}-${part.call.id}`}
										call={part.call}
										sessionId={id}
										entryId={entry.id}
										requests={requests}
										result={resultsByTurn
											.get(entry.id ?? `stamp:${entry.message.timestamp}`)
											?.get(part.call.id)}
										live={live && final}
									/>
								) : null,
							)}
						</div>
					);
				const label =
					index === 0
						? !live && endedAt === undefined
							? "已完成"
							: `${live ? "工作中" : "已工作"} ${elapsed}`
						: processSegmentLabel(segment.parts, resultsByTurn, live);
				return label ? (
					<ProcessSegment
						key={segment.key}
						parts={segment.parts}
						live={live}
						label={label}
						waiting={live && !process.length && !finalText}
						results={resultsByTurn}
					/>
				) : null;
			})}
			{finalText && <Markdown text={finalText} live={live} />}
			{last?.message.stopReason === "error" && (
				<ErrorNotice model message={last.message.errorMessage || "模型调用失败，请检查登录状态和模型设置。"} />
			)}
			{last?.message.stopReason === "aborted" && <p className="message-warning">已停止生成</p>}
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
		</article>
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

/** 分支总结消息：标题行是折叠触发器（分支图标 + 标题 + 时间 + 展开箭头，箭头在最右端），展开后正文收进灰底原文块，与工具调用原文块同一规范；总结输出按原文呈现，不在聊天流里直接渲染。 */
const SummaryEntry = memo(function SummaryEntry({ entry }: { entry: TurnEntry }) {
	const [open, setOpen] = useState(false);
	return (
		<article className="assistant-block summary-block">
			<Disclosure isExpanded={open} onExpandedChange={setOpen}>
				<Disclosure.Heading>
					<Disclosure.Trigger className="summary-head">
						<GitBranch size={18} aria-hidden="true" />
						<span className="summary-title">分支已总结</span>
						<Disclosure.Indicator>
							<ChevronDown size={16} aria-hidden="true" />
						</Disclosure.Indicator>
						<span className="summary-time">
							{new Date(entry.message.timestamp).toLocaleTimeString("zh-CN", {
								hour: "2-digit",
								minute: "2-digit",
							})}
						</span>
					</Disclosure.Trigger>
				</Disclosure.Heading>
				<Disclosure.Content>
					<section className="tool-text">
						<CodeBlock className="tool-raw">{entry.message.summary ?? ""}</CodeBlock>
					</section>
				</Disclosure.Content>
			</Disclosure>
		</article>
	);
});

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
}: {
	id: string;
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
	const [following, setFollowing] = useState(true);
	const [showBottom, setShowBottom] = useState(false);
	const [error, setError] = useState("");
	const [visibleCount, setVisibleCount] = useState(80);
	const [mountedAt] = useState(() => Date.now());
	const resultsByTurn = useMemo(() => matchToolResults(entries), [entries]);
	const history = useMemo(() => entries.filter(({ message }) => message.role !== "toolResult"), [entries]);
	const display = useMemo(() => {
		const shown = history.slice(-visibleCount);
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
	}, [history, visibleCount, streaming]);
	const turns = useMemo(() => groupTurns(display), [display]);
	const active = activeTurnIndex(turns, running);
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
			setFollowing(true);
		};
		const el = scroll.current;
		const onKeyDown = (event: KeyboardEvent) => {
			if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key)) cancelScroll();
		};
		motion.addEventListener("change", onChange);
		el?.addEventListener("wheel", cancelScroll, { passive: true });
		el?.addEventListener("touchstart", cancelScroll, { passive: true });
		el?.addEventListener("pointerdown", cancelScroll);
		el?.addEventListener("keydown", onKeyDown);
		return () => {
			cancelScroll();
			motion.removeEventListener("change", onChange);
			el?.removeEventListener("wheel", cancelScroll);
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
	// biome-ignore lint/correctness/useExhaustiveDependencies: entries 与 streaming 只作为滚动触发信号，不在 effect 体内读取
	useLayoutEffect(() => {
		if (following) scroll.current?.scrollTo({ top: scroll.current.scrollHeight });
	}, [entries, streaming, following]);
	return (
		<>
			<div
				className="conversation"
				ref={scroll}
				onScroll={() => {
					const el = scroll.current;
					if (el && scrollAnimation.current === null)
						setFollowing(el.scrollHeight - el.scrollTop - el.clientHeight < 100);
				}}
			>
				{history.length > visibleCount && (
					<Button
						variant="ghost"
						className="show-earlier"
						onPress={() => {
							setVisibleCount((count) => count + 80);
							setFollowing(false);
						}}
					>
						显示更早的消息（剩余 {history.length - visibleCount} 条）
					</Button>
				)}
				{turns.map((turn, turnIndex) => {
					const turnKey = `turn-${turn.user?.message.timestamp ?? turn.summary?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? turnIndex}`;
					if (turn.summary) return <SummaryEntry key={turnKey} entry={turn.summary} />;
					const live = turnIndex === active;
					return (
						<div className="turn" key={turnKey}>
							{turn.user && (
								<div
									className="user-line"
									data-fresh={turn.user.message.timestamp >= mountedAt - 2000 || undefined}
								>
									<div className="user-bubble">
										{textOf(turn.user.message)}
										{Array.isArray(turn.user.message.content) &&
											turn.user.message.content
												.filter((part) => part.type === "image")
												.map((part, imageIndex) => (
													<PreviewableImage
														key={`${imageIndex}-${part.mimeType}`}
														className="message-image"
														alt={`附图 ${imageIndex + 1}`}
														src={`data:${part.mimeType};base64,${part.data}`}
													/>
												))}
									</div>
								</div>
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
