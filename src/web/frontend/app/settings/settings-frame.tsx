import { Button } from "@heroui/react";
import { ArrowLeft, CircleCheck, X } from "lucide-react";
import { createContext, type ReactNode, useContext, useEffect, useState } from "react";
import { Link, useNavigate } from "react-router";
import { Notification } from "../shell/notification";

const Feedback = createContext<(message?: string) => void>(() => {});
export const useSettingsFeedback = () => useContext(Feedback);

export function SettingsFeedback({ children }: { children: ReactNode }) {
	const [notice, setNotice] = useState<{ message: string; revision: number } | null>(null);
	return (
		<Feedback
			value={(message = "已自动保存") =>
				setNotice((previous) => ({ message, revision: (previous?.revision ?? 0) + 1 }))
			}
		>
			{children}
			{notice && (
				<Notification
					lifetimeKey={notice.revision}
					className="settings-notification"
					label={notice.message}
					kind="success"
					duration={2600}
					onDismiss={() => setNotice(null)}
				>
					<CircleCheck size={18} aria-hidden="true" />
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
			void navigate("/settings");
		};
		document.addEventListener("keydown", onEscape);
		return () => document.removeEventListener("keydown", onEscape);
	}, [back, navigate]);
	return (
		<>
			{back && (
				<Link to="/settings" className="settings-back">
					<ArrowLeft size={16} aria-hidden="true" />
					模型服务
				</Link>
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
