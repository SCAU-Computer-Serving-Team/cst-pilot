import { createContext, type ReactNode, useContext, useState } from "react";
import { createPortal } from "react-dom";

const NotificationTarget = createContext<HTMLDivElement | null>(null);

/** One viewport-level region keeps notifications clear of transformed page content. */
export function NotificationRegion({ children }: { children: ReactNode }) {
	const [target, setTarget] = useState<HTMLDivElement | null>(null);
	return (
		<NotificationTarget value={target}>
			{children}
			<div ref={setTarget} className="notification-region" />
		</NotificationTarget>
	);
}

export function Notification({
	children,
	className,
	label,
	kind = "info",
}: {
	children: ReactNode;
	className: string;
	label: string;
	kind?: "info" | "success" | "warning" | "danger";
}) {
	const target = useContext(NotificationTarget);
	if (!target) return null;
	return createPortal(
		<output
			className={`notification ${className}`}
			aria-label={label}
			aria-live="polite"
			aria-atomic="true"
			data-kind={kind}
		>
			{children}
		</output>,
		target,
	);
}
