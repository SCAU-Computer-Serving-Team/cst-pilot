import { Button, Disclosure } from "@heroui/react";
import { ArrowDown, Brain, ChevronDown, Copy, Download, GitBranch, GitFork, X } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { matchToolResults } from "../cards/match-results";
import { ToolGroup } from "../cards/tool-card";
import { sessionPath } from "../data/api";
import type { Message } from "../data/web-state";
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
import { LiveMarkdown, Markdown } from "./markdown";
import { StreamWords } from "./stream-words";

function AssistantTurnImpl({
	id,
	turn,
	live,
	resultsByTurn,
	onFork,
	onError,
}: {
	id: string;
	turn: Turn;
	live: boolean;
	resultsByTurn: Map<string, Map<string, Message>>;
	onFork: () => void;
	onError: (text: string) => void;
}) {
	const { process, finalText } = splitTurn(turn.assistants);
	const last = turn.assistants.at(-1);
	// 折叠状态：运行中展开、结束后自动折叠；用户手动操作过后不再跟随。
	const [open, setOpen] = useState(live);
	const [touched, setTouched] = useState(false);
	useEffect(() => {
		if (!touched) setOpen(live);
	}, [live, touched]);
	// 计时：运行中每秒推进，结束时冻结；历史轮次取轮内最后一个时间戳（助手消息的时间戳是消息开始时刻，为近似值）。
	const wasLive = useRef(false);
	const [frozenAt, setFrozenAt] = useState<number>();
	const [now, setNow] = useState(() => Date.now());
	useEffect(() => {
		if (!live) return;
		setNow(Date.now());
		const timer = window.setInterval(() => setNow(Date.now()), 1000);
		return () => window.clearInterval(timer);
	}, [live]);
	useEffect(() => {
		if (live) wasLive.current = true;
		else if (wasLive.current && frozenAt === undefined) setFrozenAt(Date.now());
	}, [live, frozenAt]);
	const startedAt = turn.user?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? now;
	const historyEnd = Math.max(
		startedAt,
		...turn.assistants.flatMap((entry) => [
			entry.message.timestamp,
			...[...(resultsByTurn.get(entry.id ?? `stamp:${entry.message.timestamp}`)?.values() ?? [])].map(
				(result) => result.timestamp,
			),
		]),
	);
	const elapsed = formatDuration(Math.max((live ? now : (frozenAt ?? historyEnd)) - startedAt, live ? 1000 : 0));
	return (
		<article className="assistant-block assistant-turn" data-live={live || undefined}>
			{/* 工作时长属于整轮回答；完成时保留过程头，只更新状态与折叠。 */}
			<Disclosure
				className="turn-process"
				isExpanded={open}
				onExpandedChange={(value) => {
					setTouched(true);
					setOpen(value);
				}}
			>
				<Disclosure.Heading>
					<Disclosure.Trigger className="turn-process-head">
						<span>{live ? `工作中 ${elapsed}` : `已工作 ${elapsed}`}</span>
						<Disclosure.Indicator />
					</Disclosure.Trigger>
				</Disclosure.Heading>
				<Disclosure.Content>
					<div className="turn-process-body">
						{process.map(({ entry, final, part }, partIndex) => {
							const key = `part-${partIndex}`;
							if (part.kind === "thinking")
								return (
									<Disclosure className="thinking-line" key={key} isDisabled={part.part.redacted}>
										<Disclosure.Heading>
											<Disclosure.Trigger className="thinking-trigger">
												<Brain size={16} aria-hidden="true" />
												<span>已思考{part.part.redacted ? " · 内容不可用" : ""}</span>
												<Disclosure.Indicator />
											</Disclosure.Trigger>
										</Disclosure.Heading>
										<Disclosure.Content>
											{!part.part.redacted && (
												<section className="tool-text">
													<pre className="tool-raw">
														{live && final ? (
															<StreamWords text={part.part.thinking} />
														) : (
															part.part.thinking
														)}
													</pre>
												</section>
											)}
										</Disclosure.Content>
									</Disclosure>
								);
							if (part.kind === "tools")
								return (
									<ToolGroup
										key={key}
										calls={part.calls}
										results={resultsByTurn.get(entry.id ?? `stamp:${entry.message.timestamp}`) ?? new Map()}
										live={live && final}
										startedAt={entry.message.timestamp}
									/>
								);
							return <Markdown key={key} text={part.text} />;
						})}
						{live && process.length === 0 && !finalText && (
							<span className="t-shimmer" data-text="......">
								......
							</span>
						)}
					</div>
				</Disclosure.Content>
			</Disclosure>
			{finalText && (live ? <LiveMarkdown text={finalText} /> : <Markdown text={finalText} />)}
			{last?.message.stopReason === "error" && (
				<ErrorNotice model message={last.message.errorMessage || "模型调用失败，请检查登录状态和模型设置。"} />
			)}
			{last?.message.stopReason === "aborted" && <p className="message-warning">已停止生成</p>}
			{last?.id && last.message.stopReason != null && last.message.stopReason !== "toolUse" && (
				<div className="answer-actions">
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
					<Button variant="ghost" isIconOnly aria-label="派生会话" onPress={() => onFork()}>
						<GitFork size={16} />
					</Button>
					<a href={sessionPath(id, "/export")} download={`session-${id}.md`} aria-label="导出会话">
						<Download size={16} />
					</a>
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
						<pre className="tool-raw">{entry.message.summary ?? ""}</pre>
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
	running,
	summarizing,
	refresh,
	onCancelSummary,
	onFork,
}: {
	id: string;
	entries: { id?: string; message: Message }[];
	streaming?: Message;
	running: boolean;
	summarizing: boolean;
	refresh: () => Promise<void>;
	onCancelSummary: () => Promise<void>;
	onFork: () => void;
}) {
	const scroll = useRef<HTMLDivElement>(null);
	const [following, setFollowing] = useState(true);
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
	// biome-ignore lint/correctness/useExhaustiveDependencies: entries 与 streaming 只作为滚动触发信号，不在 effect 体内读取
	useEffect(() => {
		if (following) scroll.current?.scrollTo({ top: scroll.current.scrollHeight });
	}, [entries, streaming, following]);
	return (
		<>
			<div
				className="conversation"
				ref={scroll}
				onScroll={() => {
					const el = scroll.current;
					if (el) setFollowing(el.scrollHeight - el.scrollTop - el.clientHeight < 100);
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
					const turnKey =
						turn.user?.id ??
						turn.summary?.id ??
						turn.assistants[0]?.id ??
						`turn-${turn.user?.message.timestamp ?? turn.summary?.message.timestamp ?? turn.assistants[0]?.message.timestamp ?? turnIndex}`;
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
													<img
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
			{!following && (
				<Button
					className="back-to-bottom"
					onPress={() => {
						scroll.current?.scrollTo({
							top: scroll.current.scrollHeight,
							behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth",
						});
						setFollowing(true);
					}}
				>
					<ArrowDown size={14} aria-hidden="true" />
					回到底部
				</Button>
			)}
		</>
	);
}
