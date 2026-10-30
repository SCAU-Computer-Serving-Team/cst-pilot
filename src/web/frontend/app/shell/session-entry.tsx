import { Check } from "lucide-react";
import type { CSSProperties, MouseEvent } from "react";
import { Link } from "react-router";
import type { SessionItem } from "./session-actions";

export function SessionEntry({
	session,
	active,
	managing,
	selected,
	disabled,
	preview,
	index,
	viewTransition,
	onToggle,
	onNavigate,
	onContextMenu,
}: {
	session: SessionItem;
	active: boolean;
	managing: boolean;
	selected: boolean;
	disabled: boolean;
	preview: boolean;
	index: number;
	viewTransition?: boolean;
	onToggle: () => void;
	onNavigate?: (event: MouseEvent<HTMLAnchorElement>) => void;
	onContextMenu: (event: MouseEvent<HTMLElement>) => void;
}) {
	const content = (
		<>
			<span className="session-check-control" aria-hidden="true">
				<Check size={12} />
			</span>
			<span className="sidebar-selection-text">{session.title}</span>
		</>
	);
	return (
		<div className="sidebar-session-row" style={{ "--startup-row": Math.min(index + 1, 11) } as CSSProperties}>
			{managing ? (
				<label
					className="sidebar-session sidebar-session-select"
					data-session-id={session.id}
					data-selected={selected || undefined}
					data-disabled={disabled || undefined}
					onContextMenu={onContextMenu}
					title={session.title}
				>
					<input
						className="session-native-check"
						type="checkbox"
						checked={selected}
						disabled={disabled}
						onChange={onToggle}
					/>
					{content}
				</label>
			) : (
				<Link
					className={`sidebar-session ${active ? "selected" : ""}`}
					data-session-id={session.id}
					aria-current={active ? "page" : undefined}
					to={`/s/${session.id}${preview ? "?preview=1" : ""}`}
					viewTransition={viewTransition}
					onClick={onNavigate}
					onContextMenu={onContextMenu}
					title={session.title}
				>
					{content}
				</Link>
			)}
		</div>
	);
}
