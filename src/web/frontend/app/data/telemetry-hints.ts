/** 通知本机进程保存与补发；页面生命周期不改变会话生命周期。 */
export function attachTelemetryHints(target: EventTarget, request: typeof fetch = fetch): () => void {
	const notify = (event: Event) => {
		void request("/api/lifecycle/telemetry", {
			method: "POST",
			credentials: "same-origin",
			keepalive: true,
			headers: { "Content-Type": "application/json", "X-CST-Web-Request": "1" },
			body: JSON.stringify({ reason: event.type }),
		}).catch(() => undefined);
	};
	const events = ["pagehide", "pageshow", "online"];
	for (const name of events) target.addEventListener(name, notify);
	return () => {
		for (const name of events) target.removeEventListener(name, notify);
	};
}
