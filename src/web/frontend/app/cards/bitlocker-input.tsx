import { ExternalLink, Eye, EyeOff } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, apiJson, sessionPath } from "../data/api";
import type { BitLockerSnapshot, PendingBitLocker } from "../data/bitlocker-state";
import { conversionLabel, lockLabel } from "./special-tool-data";

export function BitLockerInput({ id, request, onDone }: { id: string; request: PendingBitLocker; onDone: () => void }) {
	const [password, setPassword] = useState("");
	const [show, setShow] = useState(false);
	const [busy, setBusy] = useState(false);
	const [uncertain, setUncertain] = useState(false);
	const [error, setError] = useState("");
	const [now, setNow] = useState(Date.now());
	useEffect(() => {
		const timer = window.setInterval(() => setNow(Date.now()), 1000);
		return () => window.clearInterval(timer);
	}, []);
	const seconds = Math.max(0, Math.ceil((request.expiresAt - now) / 1000));
	useEffect(() => {
		if (!seconds) {
			setPassword("");
			setShow(false);
		}
	}, [seconds]);
	async function reconcile() {
		setBusy(true);
		try {
			const snapshot = await apiJson<BitLockerSnapshot>(sessionPath(id, "/bitlocker"));
			if (!snapshot.requests.some((r) => r.requestId === request.requestId)) {
				onDone();
				return;
			}
			setUncertain(false);
			setBusy(false);
			setError("请求仍待处理，请重新输入或取消。");
		} catch {
			setUncertain(true);
			setBusy(false);
			setError("连接尚未恢复。重新同步后可继续操作。");
		}
	}
	async function submit(cancel = false) {
		if (busy || uncertain || !seconds) return;
		if (!cancel && request.kind === "password" && !/^\d{6}(?:-\d{6}){7}$/.test(password.trim())) {
			setError("请输入八组六位数字，用连字符分隔。");
			return;
		}
		setBusy(true);
		setError("");
		const answer = cancel
			? { cancel: true }
			: request.kind === "password"
				? { password: password.trim() }
				: { confirm: true };
		setPassword("");
		setShow(false);
		try {
			await apiJson(sessionPath(id, `/bitlocker/${request.requestId}`), { method: "POST", body: answer });
			onDone();
		} catch (cause) {
			if (cause instanceof ApiFailure && cause.status === 400) {
				setBusy(false);
				setError(cause.message);
			} else {
				setUncertain(true);
				await reconcile();
			}
		}
	}
	return (
		<form
			className="special-tool-input"
			onSubmit={(e) => {
				e.preventDefault();
				void submit();
			}}
		>
			{request.kind === "decrypt" ? (
				<>
					<p className="special-tool-impact">将解除加密保护，完成后移除保护器。</p>
					<div className="special-tool-targets">
						{request.targets.map((target) => (
							<div className="special-tool-target" key={target.volumeId}>
								<strong>{target.drive ?? "无盘符"}</strong>
								<div>
									<span>{conversionLabel(target.conversionStatus)}</span>
									<small>{lockLabel(target.lockStatus)}</small>
									{(target.conversionStatus === 2 || target.conversionStatus === 4) && (
										<small>将转为解密</small>
									)}
								</div>
							</div>
						))}
					</div>
					<small>仅处理清单内目标。</small>
					<small>部分失败不自动回滚。</small>
				</>
			) : request.kind === "parameter" ? (
				<p>
					将使用工具参数中的恢复密码。凭据可能已进入模型上下文并发送给模型服务商，原用户消息也可能留在本地草稿或收件箱。确认后只解锁，不关闭加密。
				</p>
			) : (
				<>
					<label htmlFor={`key-${request.requestId}`}>恢复密码（48 位）</label>
					<div className="special-tool-password">
						<input
							id={`key-${request.requestId}`}
							type={show ? "text" : "password"}
							autoComplete="off"
							spellCheck={false}
							value={password}
							disabled={busy || uncertain || !seconds}
							aria-invalid={!!error}
							aria-describedby={error ? `error-${request.requestId}` : undefined}
							onChange={(e) => setPassword(e.target.value)}
						/>
						<button
							type="button"
							className="special-tool-icon-button"
							disabled={busy || uncertain || !seconds}
							aria-label={show ? "隐藏恢复密码" : "显示恢复密码"}
							aria-pressed={show}
							onClick={() => setShow(!show)}
						>
							{show ? <Eye size={16} /> : <EyeOff size={16} />}
						</button>
					</div>
				</>
			)}
			{error && (
				<p id={`error-${request.requestId}`} className="special-tool-action-error" role="alert">
					{error}
				</p>
			)}
			{uncertain && (
				<button type="button" className="special-tool-link" disabled={busy} onClick={() => void reconcile()}>
					重新同步
				</button>
			)}
			<div className="special-tool-actions">
				<button type="submit" className="special-tool-primary" disabled={busy || uncertain || !seconds}>
					{request.kind === "decrypt" ? "确认关闭加密" : request.kind === "parameter" ? "确认解锁" : "解锁"}
				</button>
				<button
					type="button"
					className="special-tool-secondary"
					disabled={busy || uncertain || !seconds}
					onClick={() => void submit(true)}
				>
					取消
				</button>
				<small>
					{seconds
						? `剩余 ${Math.floor(seconds / 60)
								.toString()
								.padStart(2, "0")}:${(seconds % 60).toString().padStart(2, "0")}`
						: "请求已过期"}
				</small>
			</div>
			{request.kind === "password" && (
				<a
					className="special-tool-link special-tool-recovery"
					href="https://aka.ms/myrecoverykey"
					target="_blank"
					rel="noreferrer"
				>
					查找恢复密钥 <ExternalLink size={12} aria-hidden="true" />
				</a>
			)}
		</form>
	);
}
