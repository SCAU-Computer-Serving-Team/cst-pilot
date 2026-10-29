import {
	type CSSProperties,
	createContext,
	type ReactNode,
	useContext,
	useEffect,
	useId,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { NotificationLifetime } from "./notification-lifetime";

type NotificationProps = {
	children: ReactNode;
	className: string;
	label: string;
	kind?: "info" | "success" | "warning" | "danger";
	/** Zero keeps recovery and deletion notices persistent. */
	duration?: number;
	/** Refresh one notification in place without stacking another surface. */
	lifetimeKey?: number;
	onDismiss?: () => void;
};
type ExitStyle = CSSProperties & {
	"--notification-exit-opacity": string;
	"--notification-exit-transform": string;
};
type Notice = { id: string; props: NotificationProps; exiting: boolean; exitStyle?: ExitStyle };
type Registry = {
	update: (id: string, props: NotificationProps) => void;
	remove: (id: string) => void;
	complete: (id: string) => void;
};
const NotificationRegistry = createContext<Registry | null>(null);

/** Keep the surface mounted through its exit, including route-owned and recovery notifications. */
export function NotificationRegion({ children, contentInset = 0 }: { children: ReactNode; contentInset?: number }) {
	const [notices, setNotices] = useState<Notice[]>([]);
	const registry = useMemo<Registry>(
		() => ({
			update(id, props) {
				setNotices((current) =>
					current.some((notice) => notice.id === id)
						? current.map((notice) => (notice.id === id ? { id, props, exiting: false } : notice))
						: [...current, { id, props, exiting: false }],
				);
			},
			remove(id) {
				const node = document.getElementById(id);
				const style = node && getComputedStyle(node);
				const exitStyle: ExitStyle | undefined = style
					? {
							"--notification-exit-opacity": style.opacity,
							"--notification-exit-transform": style.transform,
						}
					: undefined;
				setNotices((current) =>
					current.map((notice) =>
						notice.id === id && !notice.exiting ? { ...notice, exiting: true, exitStyle } : notice,
					),
				);
			},
			complete(id) {
				setNotices((current) => current.filter((notice) => notice.id !== id || !notice.exiting));
			},
		}),
		[],
	);
	return (
		<NotificationRegistry value={registry}>
			{children}
			<div className="notification-region" style={{ "--notification-inset": `${contentInset}px` } as CSSProperties}>
				{notices.map((notice) => (
					<NotificationSurface key={notice.id} notice={notice} registry={registry} />
				))}
			</div>
		</NotificationRegistry>
	);
}

export function Notification({
	children,
	className,
	label,
	kind = "info",
	duration = 0,
	lifetimeKey = 0,
	onDismiss,
}: NotificationProps) {
	const registry = useContext(NotificationRegistry);
	const id = useId();
	useLayoutEffect(() => {
		registry?.update(id, { children, className, label, kind, duration, lifetimeKey, onDismiss });
	}, [registry, id, children, className, label, kind, duration, lifetimeKey, onDismiss]);
	useLayoutEffect(() => () => registry?.remove(id), [registry, id]);
	return null;
}

function NotificationSurface({ notice, registry }: { notice: Notice; registry: Registry }) {
	const { id, props, exiting } = notice;
	const { children, className, label, kind, duration = 0, lifetimeKey = 0, onDismiss } = props;
	const element = useRef<HTMLOutputElement>(null);
	const [entered, setEntered] = useState(false);
	const lifetime = useRef<NotificationLifetime | null>(null);
	const dismiss = useRef(onDismiss);
	dismiss.current = onDismiss;
	useLayoutEffect(() => {
		const node = element.current;
		if (!node) return;
		let active = true;
		const media = matchMedia("(prefers-reduced-motion: reduce)");
		const finish = () => {
			if (!active) return;
			if (exiting) registry.complete(id);
			else setEntered(true);
		};
		const animations = node.getAnimations();
		if (media.matches || document.hidden || !animations.length) finish();
		else void Promise.allSettled(animations.map((animation) => animation.finished)).then(finish);
		const changed = () => {
			if (media.matches) finish();
		};
		media.addEventListener("change", changed);
		return () => {
			active = false;
			media.removeEventListener("change", changed);
		};
	}, [exiting, registry, id]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 每次保存成功重置当前通知的展示时间。
	useEffect(() => {
		if (!entered || exiting || duration <= 0) return;
		const timer = new NotificationLifetime(duration, () => dismiss.current?.());
		lifetime.current = timer;
		const node = element.current;
		if (node?.matches(":hover")) timer.pause("pointer");
		if (node?.contains(document.activeElement)) timer.pause("focus");
		const visibility = () => (document.hidden ? timer.pause("hidden") : timer.resume("hidden"));
		document.addEventListener("visibilitychange", visibility);
		visibility();
		return () => {
			timer.dispose();
			lifetime.current = null;
			document.removeEventListener("visibilitychange", visibility);
		};
	}, [duration, entered, exiting, lifetimeKey]);
	return (
		<output
			id={id}
			ref={element}
			style={notice.exitStyle}
			className={`notification ${className}`}
			aria-label={label}
			aria-live={exiting ? "off" : "polite"}
			aria-atomic="true"
			aria-hidden={exiting || undefined}
			inert={exiting}
			data-kind={kind}
			data-state={exiting ? "closing" : entered ? "open" : "entering"}
			onPointerEnter={() => lifetime.current?.pause("pointer")}
			onPointerLeave={() => lifetime.current?.resume("pointer")}
			onFocusCapture={() => lifetime.current?.pause("focus")}
			onBlurCapture={(event) => {
				if (!event.currentTarget.contains(event.relatedTarget)) lifetime.current?.resume("focus");
			}}
		>
			{children}
		</output>
	);
}

/** The icon owns disclosure; the floating text never changes the notification's row height. */
export function NotificationDetail({ icon, children }: { icon: ReactNode; children: ReactNode }) {
	const id = useId();
	const [open, setOpen] = useState(false);
	return (
		<fieldset
			aria-label="通知详情"
			className="notification-disclosure"
			onPointerEnter={() => setOpen(true)}
			onPointerLeave={(event) => {
				if (!event.currentTarget.contains(document.activeElement)) setOpen(false);
			}}
			onBlur={(event) => {
				if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
			}}
		>
			<button
				type="button"
				className="notification-detail-trigger"
				aria-label="查看通知详情"
				aria-describedby={open ? id : undefined}
				aria-expanded={open}
				onFocus={() => setOpen(true)}
				onClick={() => setOpen(true)}
				onKeyDown={(event) => {
					if (event.key === "Escape") {
						event.stopPropagation();
						setOpen(false);
					}
				}}
			>
				{icon}
			</button>
			<span id={id} className="notification-detail" role="tooltip" data-open={open}>
				{children}
			</span>
		</fieldset>
	);
}
