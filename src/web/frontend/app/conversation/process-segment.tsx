import { Brain } from "lucide-react";
import { useEffect, useState } from "react";
import { ToolGroup } from "../cards/tool-card";
import type { ContentPart, Message } from "../data/web-state";
import { CodeBlock } from "../shell/code-block";
import type { TurnPart } from "./conversation-parts";
import { Markdown } from "./markdown";
import { RunningLight } from "./running-light";
import { StreamWords } from "./stream-words";
import { TranscriptDisclosure, type TranscriptFormat } from "./transcript-disclosure";

export function Thinking({
	part,
	live,
	format,
}: {
	part: Extract<ContentPart, { type: "thinking" }>;
	live: boolean;
	format?: TranscriptFormat;
}) {
	const [expanded, setExpanded] = useState(format === "png");
	const [opened, setOpened] = useState(!!format);
	return (
		<TranscriptDisclosure
			format={format}
			className="thinking-line"
			triggerClassName="thinking-trigger"
			isDisabled={part.redacted}
			isExpanded={expanded}
			onExpandedChange={(value) => {
				setExpanded(value);
				if (value) setOpened(true);
			}}
			heading={
				<>
					<Brain size={16} aria-hidden="true" />
					<RunningLight active={live}>{`已思考${part.redacted ? " · 内容不可用" : ""}`}</RunningLight>
				</>
			}
		>
			{opened && !part.redacted && (
				<section className="tool-text">
					<CodeBlock className="tool-raw">
						{format ? part.thinking : <StreamWords text={part.thinking} />}
					</CodeBlock>
				</section>
			)}
		</TranscriptDisclosure>
	);
}

export function ProcessSegment({
	parts,
	live,
	label,
	waiting,
	results,
	format,
}: {
	parts: TurnPart[];
	live: boolean;
	label: string;
	waiting: boolean;
	results: Map<string, Map<string, Message>>;
	format?: TranscriptFormat;
}) {
	const [open, setOpen] = useState(format === "png" || live),
		[touched, setTouched] = useState(false),
		[opened, setOpened] = useState(!!format || live);
	// 运行时立即挂载；首次展开后保留节点和工具内部状态。
	if (live && !opened) setOpened(true);
	useEffect(() => {
		if (!touched) setOpen(format === "png" || live);
	}, [format, live, touched]);
	return (
		<TranscriptDisclosure
			format={format}
			className="turn-process"
			triggerClassName="turn-process-head"
			heading={<RunningLight active={live}>{label}</RunningLight>}
			isExpanded={open}
			onExpandedChange={(value) => {
				setTouched(true);
				if (value) setOpened(true);
				setOpen(value);
			}}
		>
			{opened && (
				<div className="turn-process-body">
					{parts.map(({ entry, final, part }, index) => {
						const key = `${entry.message.timestamp}-${index}`;
						if (part.kind === "thinking")
							return <Thinking key={key} part={part.part} live={live && final} format={format} />;
						if (part.kind === "tools")
							return (
								<ToolGroup
									key={key}
									calls={part.calls}
									results={results.get(entry.id ?? `stamp:${entry.message.timestamp}`) ?? new Map()}
									live={live && final}
									format={format}
								/>
							);
						return part.kind === "text" ? <Markdown key={key} text={part.text} /> : null;
					})}
					{waiting && (
						<span className="t-shimmer" data-text="......">
							......
						</span>
					)}
				</div>
			)}
		</TranscriptDisclosure>
	);
}
