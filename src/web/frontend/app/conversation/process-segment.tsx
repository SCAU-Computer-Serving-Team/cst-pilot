import { Disclosure } from "@heroui/react";
import { Brain } from "lucide-react";
import { useEffect, useState } from "react";
import { ToolGroup } from "../cards/tool-card";
import type { Message } from "../data/web-state";
import type { TurnPart } from "./conversation-parts";
import { Markdown } from "./markdown";
import { StreamWords } from "./stream-words";

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
		[touched, setTouched] = useState(false);
	useEffect(() => {
		if (!touched) setOpen(live);
	}, [live, touched]);
	return (
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
					<span>{label}</span>
					<Disclosure.Indicator />
				</Disclosure.Trigger>
			</Disclosure.Heading>
			<Disclosure.Content>
				<div className="turn-process-body">
					{parts.map(({ entry, final, part }, index) => {
						const key = `${entry.id ?? entry.message.timestamp}-${index}`;
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
													{live && final ? <StreamWords text={part.part.thinking} /> : part.part.thinking}
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
			</Disclosure.Content>
		</Disclosure>
	);
}
