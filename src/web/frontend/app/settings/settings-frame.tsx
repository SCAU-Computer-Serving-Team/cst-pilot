import { Button } from "@heroui/react";
import { ArrowLeft, CircleCheck, TriangleAlert, X } from "lucide-react";
import { type ComponentProps, createContext, type ReactNode, useContext, useEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";
import { useGlobalEvents } from "../data/web-state";
import { Notification } from "../shell/notification";
import { loadSetting, settingReady } from "./settings-resource-cache";

const Feedback = createContext<(message?: string, kind?: "success" | "warning") => void>(() => {});
export const useSettingsFeedback = () => useContext(Feedback);

function resourceForSettingsRoute(path: string): string | undefined {
	if (path === "/settings/general") return "/api/settings";
	if (path === "/settings/models") return "/api/models";
	if (path === "/settings" || path === "/settings/add" || path === "/settings/provider" || path === "/settings/custom")
		return "/api/auth";
	return undefined;
}
export function SettingsLink({ to, onClick, ...props }: ComponentProps<typeof Link>) {
	const { pathname } = useLocation();
	const navigate = useNavigate();
	const waiting = useRef(false);
	return (
		<Link
			{...props}
			to={to}
			viewTransition
			prefetch={props.prefetch ?? "intent"}
			onClick={(event) => {
				onClick?.(event);
				if (
					event.defaultPrevented ||
					event.button !== 0 ||
					event.metaKey ||
					event.ctrlKey ||
					event.shiftKey ||
					event.altKey ||
					typeof to !== "string"
				)
					return;
				const target = to.split("?")[0];
				const resource = resourceForSettingsRoute(target);
				if (resource && !settingReady(resource)) {
					event.preventDefault();
					if (waiting.current) return;
					waiting.current = true;
					void loadSetting(resource)
						.then(() => {
							if (window.location.pathname !== pathname) return;
							void navigate(to, { viewTransition: true });
						})
						.catch(() => {
							if (window.location.pathname === pathname) void navigate(to);
						})
						.finally(() => {
							waiting.current = false;
						});
					return;
				}
			}}
		/>
	);
}

export function SettingsFeedback({ children }: { children: ReactNode }) {
	const [notice, setNotice] = useState<{ message: string; revision: number; kind: "success" | "warning" } | null>(
		null,
	);
	useGlobalEvents((name, raw) => {
		if (name !== "state") return;
		try {
			const event = JSON.parse(raw?.data ?? "null");
			if (event?.type === "models_changed" && typeof event.warning === "string")
				setNotice((previous) => ({
					message: event.warning,
					kind: "warning",
					revision: (previous?.revision ?? 0) + 1,
				}));
		} catch {
			/* 重连后由设置页统一重读。 */
		}
	});
	return (
		<Feedback
			value={(message = "已自动保存", kind = "success") =>
				setNotice((previous) => ({ message, kind, revision: (previous?.revision ?? 0) + 1 }))
			}
		>
			{children}
			{notice && (
				<Notification
					lifetimeKey={notice.revision}
					className="settings-notification"
					label={notice.message}
					kind={notice.kind}
					duration={2600}
					onDismiss={() => setNotice(null)}
				>
					{notice.kind === "warning" ? (
						<TriangleAlert size={18} aria-hidden="true" />
					) : (
						<CircleCheck size={18} aria-hidden="true" />
					)}
					<span className="notification-message">{notice.message}</span>
					<button
						type="button"
						className="notification-close"
						aria-label="关闭通知"
						onClick={() => setNotice(null)}
					>
						<X size={18} aria-hidden="true" />
					</button>
				</Notification>
			)}
		</Feedback>
	);
}

export function SettingsHeading({
	title,
	description,
	action,
	back = false,
}: {
	title: string;
	description?: string;
	action?: ReactNode;
	back?: boolean;
}) {
	const navigate = useNavigate();
	useEffect(() => {
		if (!back) return;
		const onEscape = (event: KeyboardEvent) => {
			if (
				event.key !== "Escape" ||
				event.defaultPrevented ||
				event.isComposing ||
				document.querySelector('dialog[open], [role="listbox"], [role="tooltip"]')
			)
				return;
			event.preventDefault();
			void navigate("/settings", { viewTransition: true });
		};
		document.addEventListener("keydown", onEscape);
		return () => document.removeEventListener("keydown", onEscape);
	}, [back, navigate]);
	return (
		<>
			{back && (
				<SettingsLink to="/settings" className="settings-back">
					<ArrowLeft size={16} aria-hidden="true" />
					模型服务
				</SettingsLink>
			)}
			<div className="settings-page-heading">
				<div>
					<h2>{title}</h2>
					{description && <p>{description}</p>}
				</div>
				{action}
			</div>
		</>
	);
}

export function SettingsSection({
	title,
	summary,
	children,
}: {
	title: string;
	summary?: ReactNode;
	children: ReactNode;
}) {
	return (
		<section className="settings-section">
			<header>
				<h3>{title}</h3>
				{summary && <span>{summary}</span>}
			</header>
			{children}
		</section>
	);
}

export function SettingsLoadState({
	loading,
	error,
	onRetry,
}: {
	loading: boolean;
	error: string;
	onRetry: () => void;
}) {
	return (
		<>
			{loading && <output className="settings-empty">正在加载…</output>}
			{error && (
				<div className="settings-load-error" role="alert">
					<span>{error}</span>
					<Button size="sm" variant="ghost" onPress={onRetry}>
						重试
					</Button>
				</div>
			)}
		</>
	);
}
