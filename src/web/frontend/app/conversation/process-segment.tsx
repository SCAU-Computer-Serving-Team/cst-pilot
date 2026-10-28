import { Disclosure } from "@heroui/react";
import { Brain } from "lucide-react";
import { useEffect, useState } from "react";
import { ToolGroup } from "../cards/tool-card";
import type { ContentPart, Message } from "../data/web-state";
import type { TurnPart } from "./conversation-parts";
import { Markdown } from "./markdown";
import { RunningLight } from "./running-light";
import { StreamWords } from "./stream-words";

function Thinking({ part, live }: { part: Extract<ContentPart, { type: "thinking" }>; live: boolean }) {
	const [opened, setOpened] = useState(false);
	return (
		<Disclosure
			className="thinking-line"
			isDisabled={part.redacted}
			onExpandedChange={(value) => {
				if (value) setOpened(true);
			}}
		>
			<Disclosure.Heading>
				<Disclosure.Trigger className="thinking-trigger">
					<Brain size={16} aria-hidden="true" />
					<RunningLight active={live}>{`已思考${part.redacted ? " · 内容不可用" : ""}`}</RunningLight>
					<Disclosure.Indicator />
				</Disclosure.Trigger>
			</Disclosure.Heading>
			<Disclosure.Content>
				{opened && !part.redacted && (
					<section className="tool-text">
						<pre className="tool-raw">
							<StreamWords text={part.thinking} />
						</pre>
					</section>
				)}
			</Disclosure.Content>
		</Disclosure>
	);
}

export function ProcessSegment({
	parts,
	live,
	label,
	waiting,
	results,
}: {
	parts: TurnPart[];
	live: boolean;
	label: string;
	waiting: boolean;
	results: Map<string, Map<string, Message>>;
}) {
	const [open, setOpen] = useState(live),
		[touched, setTouched] = useState(false),
		[opened, setOpened] = useState(live);
	// 运行时立即挂载；首次展开后保留节点和工具内部状态。
	if (live && !opened) setOpened(true);
	useEffect(() => {
		if (!touched) setOpen(live);
	}, [live, touched]);
	return (
		<Disclosure
			className="turn-process"
			isExpanded={open}
			onExpandedChange={(value) => {
				setTouched(true);
				if (value) setOpened(true);
				setOpen(value);
			}}
		>
			<Disclosure.Heading>
				<Disclosure.Trigger className="turn-process-head">
					<RunningLight active={live}>{label}</RunningLight>
					<Disclosure.Indicator />
				</Disclosure.Trigger>
			</Disclosure.Heading>
			<Disclosure.Content>
				{opened && (
					<div className="turn-process-body">
						{parts.map(({ entry, final, part }, index) => {
							const key = `${entry.message.timestamp}-${index}`;
							if (part.kind === "thinking") return <Thinking key={key} part={part.part} live={live && final} />;
							if (part.kind === "tools")
								return (
									<ToolGroup
										key={key}
										calls={part.calls}
										results={results.get(entry.id ?? `stamp:${entry.message.timestamp}`) ?? new Map()}
										live={live && final}
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
			</Disclosure.Content>
		</Disclosure>
	);
}
