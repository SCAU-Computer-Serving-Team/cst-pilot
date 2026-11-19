import { Button, Card } from "@heroui/react";
import {
	Braces,
	CheckCheck,
	CircleCheck,
	CirclePause,
	CircleX,
	Clock3,
	Copy,
	Info,
	LoaderCircle,
	TriangleAlert,
} from "lucide-react";
import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { RunningLight } from "../conversation/running-light";
import { TranscriptDisclosure, type TranscriptFormat } from "../conversation/transcript-disclosure";
import type { ContentPart, Message } from "../data/web-state";
import { PreviewableImage } from "../images/image-preview";
import { CodeBlock } from "../shell/code-block";
import { type Block, type CardView, mapTool } from "./map-tool";
import { isStandaloneTool, splitToolCalls, type ToolState, toolState } from "./tool-state";

type Call = Extract<ContentPart, { type: "toolCall" }>;
const stateLabels: Record<ToolState, string> = {
	pending: "等待执行",
	running: "执行中",
	interrupted: "未完成或已中断",
	error: "执行失败",
	degraded: "部分结果",
	success: "已完成",
};

function StatusIcon({ state, size = 16 }: { state: ToolState; size?: number }) {
	const Icon =
		state === "pending"
			? Clock3
			: state === "running"
				? LoaderCircle
				: state === "interrupted"
					? CirclePause
					: state === "success"
						? CircleCheck
						: state === "degraded"
							? TriangleAlert
							: CircleX;
	return (
		<Icon
			aria-hidden="true"
			className={`tool-status-icon tool-status-icon--${state === "degraded" ? "warning" : state}`}
			size={size}
		/>
	);
}

function Listing({ block, all = false }: { block: Extract<Block, { kind: "listing" }>; all?: boolean }) {
	const [visible, setVisible] = useState(all ? block.items.length : 40);
	return (
		<section aria-label={block.id} className="tool-listing">
			<ol>
				{block.items.slice(0, visible).map((item, index) => (
					<li key={`${index}-${item.name}`}>
						<strong>{item.name}</strong>
						<span title={item.metrics.map((metric) => `${metric.label}：${metric.value}`).join(" · ")}>
							{item.metrics.map((metric) => metric.value).join(" · ")}
						</span>
					</li>
				))}
			</ol>
			{visible < block.items.length && (
				<Button variant="ghost" onPress={() => setVisible((value) => value + 40)}>
					继续显示（剩余 {block.items.length - visible} 项）
				</Button>
			)}
		</section>
	);
}
function Notice({ block }: { block: Extract<Block, { kind: "notice" }> }) {
	const Icon = block.status === "danger" ? CircleX : block.status === "warning" ? TriangleAlert : Info;
	return (
		<p className={`tool-notice tool-notice--${block.status}`}>
			<Icon className="tool-notice-icon" size={14} aria-hidden="true" />
			<span>{block.text}</span>
		</p>
	);
}
function renderBlock(block: Block, all = false) {
	if (block.kind === "fields")
		return (
			<section aria-label={block.title ?? block.id} className="tool-fields">
				{block.title && <h4>{block.title}</h4>}
				<dl>
					{block.rows.map((row, index) => (
						<div key={`${index}-${row.label}`} className="tool-field">
							<dt>{row.label}</dt>
							<dd>{row.value}</dd>
						</div>
					))}
				</dl>
			</section>
		);
	if (block.kind === "listing") return <Listing block={block} all={all} />;
	if (block.kind === "notice") return <Notice block={block} />;
	if (block.kind === "text")
		return (
			<section className="tool-text">
				<CodeBlock className="tool-raw">{block.text}</CodeBlock>
			</section>
		);
	if (block.kind === "image")
		return ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(block.mimeType) ? (
			all ? (
				<img className="tool-image" src={`data:${block.mimeType};base64,${block.data}`} alt="工具返回的图片" />
			) : (
				<PreviewableImage
					className="tool-image"
					src={`data:${block.mimeType};base64,${block.data}`}
					alt="工具返回的图片"
				/>
			)
		) : (
			<p>图片格式不支持显示</p>
		);
	return (
		<section className="tool-commands">
			<ol>
				{block.items.map((item, index) => (
					<li key={`${index}-${item.command}`}>
						<span className="tool-command-meta">
							{item.shell} · {item.admin ? "管理员" : "普通权限"} ｜ {item.summary}
						</span>
						<div className="tool-command">
							<pre>{item.command}</pre>
							<Button
								variant="ghost"
								isIconOnly
								aria-label="复制命令"
								onPress={() => {
									void navigator.clipboard.writeText(item.command);
								}}
							>
								<Copy size={14} />
							</Button>
						</div>
					</li>
				))}
			</ol>
		</section>
	);
}

/** 聊天卡片与只读导出共用块渲染；导出完整展示列表。 */
export function ToolBlocks({ blocks, all = false }: { blocks: Block[]; all?: boolean }) {
	return blocks.map((block) => <div key={block.id}>{renderBlock(block, all)}</div>);
}

export const ToolCard = memo(function ToolCard({
	call,
	result,
	precomputedView,
	live,
	format,
}: {
	call: Call;
	result?: Message;
	precomputedView?: CardView;
	live?: boolean;
	format?: TranscriptFormat;
}) {
	const startedAt = call.execution?.startedAt;
	const endedAt = call.execution?.endedAt;
	const special = isStandaloneTool(call, result);
	const [open, setOpen] = useState(special);
	const [touched, setTouched] = useState(false);
	useEffect(() => {
		if (!touched && special) setOpen(true);
	}, [special, touched]);
	const [raw, setRaw] = useState(false);
	const [now, setNow] = useState(Date.now());
	useEffect(() => {
		if (result || endedAt !== undefined || !live || startedAt === undefined) return;
		setNow(Date.now());
		const timer = window.setInterval(() => setNow(Date.now()), 1000);
		return () => window.clearInterval(timer);
	}, [result, live, startedAt, endedAt]);
	const view = precomputedView ?? (result ? mapTool(call, result) : undefined);
	const fieldContainer = useRef<HTMLDivElement>(null);
	useLayoutEffect(() => {
		if (!open || !result) return;
		const container = fieldContainer.current;
		if (!container) return;
		let active = true;
		const measure = () => {
			const labels = [...container.querySelectorAll(".tool-fields dt")];
			if (!labels.length) return;
			const context = document.createElement("canvas").getContext("2d");
			if (!context) return;
			const style = getComputedStyle(labels[0]);
			context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
			const width = Math.max(...labels.map((label) => context.measureText(label.textContent ?? "").width));
			container.style.setProperty("--tool-label-width", `${Math.ceil(width) + 22}px`);
		};
		measure();
		void document.fonts.ready.then(() => {
			if (active) measure();
		});
		return () => {
			active = false;
		};
	}, [open, result]);
	const details = result && "details" in result ? (result as Message & { details?: unknown }).details : undefined;
	const state = toolState(result, view?.status, live && endedAt === undefined, startedAt !== undefined);
	const status = stateLabels[state];
	const scope = typeof call.arguments.scope === "string" ? call.arguments.scope : undefined;
	const invocation = `${call.name}${scope ? `(${scope})` : ""}`;
	const argumentSummary = Object.fromEntries(
		Object.entries(call.arguments).filter(([key]) => key !== "items" && key !== "data"),
	);
	// 画布把参数摘要写成 key: value 风格。
	const callText = `${call.name}(${Object.entries(argumentSummary)
		.map(([key, value]) => `${key}: ${JSON.stringify(value)}`)
		.join(
			", ",
		)}${Array.isArray(call.arguments.items) ? `${Object.keys(argumentSummary).length ? ", " : ""}items: ${call.arguments.items.length}` : ""})`;
	const elapsed =
		startedAt !== undefined && (endedAt !== undefined || (live && !result))
			? `${(Math.max(0, (endedAt ?? now) - startedAt) / 1000).toFixed(1)}s`
			: undefined;
	const heading = (
		<>
			<StatusIcon state={state} />
			<RunningLight active={state === "running" || state === "pending"}>
				{state === "running" ? `正在调用 ${call.name} 工具` : `${invocation} · ${status}`}
			</RunningLight>
			{elapsed && <time className="tool-elapsed">{elapsed}</time>}
		</>
	);
	const content = (
		<div className="tool-card-detail" ref={fieldContainer}>
			<Card.Content>
				<div className="tool-card-body">
					<code className="tool-invocation">{callText}</code>
					{!view ? (
						state === "running" || state === "pending" ? null : (
							<p>无返回结果</p>
						)
					) : (
						<ToolBlocks blocks={view.blocks} all={!!format} />
					)}
				</div>
			</Card.Content>
			{result && (
				<TranscriptDisclosure
					format={format}
					isExpanded={raw}
					onExpandedChange={setRaw}
					triggerClassName="tool-raw-toggle"
					indicatorSize={12}
					heading={
						<>
							<Braces size={12} aria-hidden="true" />
							<span>原始数据</span>
							<span className="tool-raw-spacer" />
						</>
					}
				>
					<CodeBlock className="tool-raw">
						{(view?.raw || JSON.stringify(details) || "无原始结果").slice(0, 100000)}
						{(view?.raw.length ?? 0) > 100000 && "\n……显示已截断"}
					</CodeBlock>
				</TranscriptDisclosure>
			)}
		</div>
	);
	return (
		<Card
			variant="transparent"
			className={`tool-card ${special ? "tool-card--special" : ""} ${state === "error" ? "tool-card--error" : ""}`}
		>
			{special ? (
				<>
					<Card.Header>
						<div className="tool-card-heading tool-static-heading">{heading}</div>
					</Card.Header>
					{content}
				</>
			) : (
				<TranscriptDisclosure
					format={format}
					isExpanded={open}
					onExpandedChange={(value) => {
						setTouched(true);
						setOpen(value);
					}}
					triggerClassName="tool-card-heading"
					heading={heading}
				>
					{content}
				</TranscriptDisclosure>
			)}
		</Card>
	);
});

function CollapsedToolGroup({
	calls,
	results,
	live,
	format,
}: {
	calls: Call[];
	results: Map<string, Message>;
	live?: boolean;
	format?: TranscriptFormat;
}) {
	const [open, setOpen] = useState(format === "png");
	const entries = calls.map((call) => {
		const result = results.get(call.id);
		return { call, result, view: result ? mapTool(call, result) : undefined };
	});
	const states = entries.map((entry) => entry.view?.status);
	const failed = states.includes("error");
	const degraded = states.includes("degraded");
	const finished = states.every((state) => state != null);
	return (
		<TranscriptDisclosure
			format={format}
			className="tool-group"
			isExpanded={open}
			onExpandedChange={setOpen}
			triggerClassName="conversation-status tool-group-heading"
			heading={
				<>
					{!finished ? (
						<StatusIcon state={live ? "running" : "interrupted"} size={18} />
					) : failed ? (
						<StatusIcon state="error" size={18} />
					) : degraded ? (
						<StatusIcon state="degraded" size={18} />
					) : (
						<CheckCheck size={18} className="success-icon" aria-hidden="true" />
					)}
					<RunningLight active={!finished && !!live}>
						{!finished
							? live
								? "正在调用多个工具…"
								: "检查未完成"
							: failed
								? "部分检查失败"
								: degraded
									? "检查返回部分结果"
									: "已完成检查"}
					</RunningLight>
					<small>{calls.length} 次工具调用</small>
				</>
			}
		>
			{entries.map(({ call, result, view }) => (
				<ToolCard key={call.id} call={call} result={result} precomputedView={view} live={live} format={format} />
			))}
		</TranscriptDisclosure>
	);
}

// 调用数组含单次执行时间，逐项引用和结果表不变时跳过重绘。
export const ToolGroup = memo(
	function ToolGroup({
		calls,
		results,
		live,
		format,
	}: {
		calls: Call[];
		results: Map<string, Message>;
		live?: boolean;
		format?: TranscriptFormat;
	}) {
		return (
			<>
				{splitToolCalls(calls, results).map((segment, index) =>
					segment.kind === "standalone" ? (
						<ToolCard
							key={`${index}-${segment.calls[0].id}`}
							call={segment.calls[0]}
							result={results.get(segment.calls[0].id)}
							live={live}
							format={format}
						/>
					) : (
						<CollapsedToolGroup
							key={`${index}-${segment.calls[0].id}`}
							calls={segment.calls}
							results={results}
							live={live}
							format={format}
						/>
					),
				)}
			</>
		);
	},
	(prev, next) =>
		prev.results === next.results &&
		prev.live === next.live &&
		prev.format === next.format &&
		prev.calls.length === next.calls.length &&
		prev.calls.every((call, index) => call === next.calls[index]),
);
