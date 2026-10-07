import { CircleAlert, KeyRound, WifiOff } from "lucide-react";
import { Link } from "react-router";
import { presentError } from "../data/error-presenter";

export function ErrorNotice({
	message,
	onRetry,
	model = false,
	input = false,
}: {
	message: string;
	onRetry?: () => void;
	model?: boolean;
	input?: boolean;
}) {
	const error = presentError(message);
	const Icon = error.kind === "auth" ? KeyRound : error.kind === "connection" ? WifiOff : CircleAlert;
	return (
		<section className="error-notice" data-kind={error.kind} role="alert">
			<Icon size={18} aria-hidden="true" />
			<div className="error-notice-body">
				<strong>{input && error.kind === "connection" ? "提交结果尚未确认" : error.title}</strong>
				<p>
					{input && error.kind === "connection"
						? "输入已保留。连接恢复后再次发送，系统会核查此次提交。"
						: error.description}
				</p>
				<details>
					<summary>技术详情</summary>
					<pre>{error.detail}</pre>
				</details>
			</div>
			{error.kind === "auth" ? (
				<Link className="notice-action" to="/settings#accounts">
					重新登录
				</Link>
			) : onRetry ? (
				<button type="button" className="notice-action" onClick={onRetry}>
					重试
				</button>
			) : model ? (
				<Link className="notice-action" to="/settings">
					模型设置
				</Link>
			) : null}
		</section>
	);
}
