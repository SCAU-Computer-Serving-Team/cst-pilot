import { WifiOff } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson } from "../data/api";
import { useGlobalEvents } from "../data/web-state";

/** 不重发写入；健康检查补充SSE尚未报告故障的情况。 */
export function ConnectionStatus({ enabled }: { enabled: boolean }) {
	const [disconnected, setDisconnected] = useState(false);
	const request = useRef<AbortController | undefined>(undefined),
		active = useRef(true);
	const check = useCallback(async () => {
		request.current?.abort();
		const controller = new AbortController();
		request.current = controller;
		const timeout = window.setTimeout(() => controller.abort(), 2000);
		try {
			await apiJson("/api/health", { signal: controller.signal });
			if (active.current && request.current === controller) setDisconnected(false);
		} catch {
			if (active.current && request.current === controller) setDisconnected(true);
		} finally {
			clearTimeout(timeout);
		}
	}, []);
	useGlobalEvents((name) => {
		if (name === "error") setDisconnected(true);
		if (name === "open") void check();
	}, enabled);
	useEffect(() => {
		if (!enabled) return;
		active.current = true;
		const offline = () => setDisconnected(true),
			online = () => void check();
		window.addEventListener("offline", offline);
		window.addEventListener("online", online);
		const timer = window.setInterval(() => void check(), 3000);
		void check();
		return () => {
			active.current = false;
			request.current?.abort();
			clearInterval(timer);
			window.removeEventListener("offline", offline);
			window.removeEventListener("online", online);
		};
	}, [enabled, check]);
	if (!enabled || !disconnected) return null;
	return (
		<output className="connection-status">
			<WifiOff size={18} aria-hidden="true" />
			<div>
				<strong>与 CST Pilot 的连接已断开</strong>
				<p>正在重连。请确认终端仍在运行；未发送内容会保留。</p>
			</div>
			<button type="button" className="notice-action" onClick={() => void check()}>
				重新连接
			</button>
		</output>
	);
}
