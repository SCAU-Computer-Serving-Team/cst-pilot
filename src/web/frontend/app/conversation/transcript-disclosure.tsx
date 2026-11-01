import { Disclosure } from "@heroui/react";
import { ChevronDown } from "lucide-react";
import type { ReactNode } from "react";

export type TranscriptFormat = "html" | "png";

/** 聊天页使用组件库动效；导出保留相同内容和样式，并提供离线可用的原生折叠。 */
export function TranscriptDisclosure({
	format,
	className,
	triggerClassName,
	heading,
	suffix,
	indicatorSize = 16,
	children,
	isExpanded,
	onExpandedChange,
	isDisabled,
}: {
	format?: TranscriptFormat;
	className?: string;
	triggerClassName: string;
	heading: ReactNode;
	suffix?: ReactNode;
	indicatorSize?: number;
	children: ReactNode;
	isExpanded: boolean;
	onExpandedChange: (value: boolean) => void;
	isDisabled?: boolean;
}) {
	if (format && isDisabled)
		return (
			<div className={className}>
				<div className={triggerClassName}>{heading}</div>
			</div>
		);
	if (format)
		return (
			<details
				className={`disclosure export-disclosure ${className ?? ""}`}
				open={isExpanded}
				onToggle={(event) => onExpandedChange(event.currentTarget.open)}
			>
				<summary className={`disclosure__trigger ${triggerClassName}`}>
					{heading}
					<span className="disclosure__indicator">
						<ChevronDown size={indicatorSize} aria-hidden="true" />
					</span>
					{suffix}
				</summary>
				<div className="disclosure__content">{children}</div>
			</details>
		);
	return (
		<Disclosure
			className={className}
			isExpanded={isExpanded}
			onExpandedChange={onExpandedChange}
			isDisabled={isDisabled}
		>
			<Disclosure.Heading>
				<Disclosure.Trigger className={triggerClassName}>
					{heading}
					<Disclosure.Indicator>
						<ChevronDown size={indicatorSize} aria-hidden="true" />
					</Disclosure.Indicator>
					{suffix}
				</Disclosure.Trigger>
			</Disclosure.Heading>
			<Disclosure.Content>{children}</Disclosure.Content>
		</Disclosure>
	);
}
