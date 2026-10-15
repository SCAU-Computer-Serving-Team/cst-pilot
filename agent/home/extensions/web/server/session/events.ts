import { randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { browserJson } from "./browser-json.ts";

/** Process-local, bounded replay of a single session's SDK events. */
export class SessionEvents {
	private readonly epoch = randomUUID();
	private sequence = 0;
	private readonly history: { sequence: number; frame: string }[] = [];
	private readonly clients = new Set<ServerResponse>();
	private readonly unsubscribe: () => void;

	private readonly heartbeatMs: number;
	constructor(session?: AgentSession, heartbeatMs = 20_000) {
		this.heartbeatMs = heartbeatMs;
		this.unsubscribe = session?.subscribe((event) => this.publish("session", event)) ?? (() => {});
	}

	publish(name: "session" | "queue" | "state", value: unknown): void {
		let data: string;
		try {
			data = browserJson(value);
		} catch {
			return;
		}
		const sequence = ++this.sequence;
		const frame = `id: ${this.epoch}:${sequence}\nevent: ${name}\ndata: ${data}\n\n`;
		this.history.push({ sequence, frame });
		if (this.history.length > 256) this.history.shift();
		for (const client of this.clients) {
			if (client.writableEnded || client.destroyed) {
				this.clients.delete(client);
				continue;
			}
			try {
				// write(false) 已接收帧，只表示缓冲超过高水位。正常大帧不能因此断流。
				// 对长期不读取的客户端限制积压；重连仍使用现有历史回放或 reset。
				if (client.writableLength > 8 * 1024 * 1024) {
					this.clients.delete(client);
					client.end();
				} else client.write(frame);
			} catch {
				this.clients.delete(client);
			}
		}
	}

	serve(request: IncomingMessage, response: ServerResponse): void {
		response.writeHead(200, {
			"Content-Type": "text/event-stream; charset=utf-8",
			"Cache-Control": "no-store",
			Connection: "keep-alive",
			"X-Accel-Buffering": "no",
		});
		const last = request.headers["last-event-id"];
		if (typeof last === "string") {
			const [epoch, value] = last.split(":");
			const sequence = Number(value);
			const oldest = this.history[0]?.sequence ?? this.sequence + 1;
			if (
				epoch !== this.epoch ||
				!Number.isSafeInteger(sequence) ||
				sequence < oldest - 1 ||
				sequence > this.sequence
			) {
				response.write("event: reset\ndata: {}\n\n");
			} else {
				for (const entry of this.history) if (entry.sequence > sequence) response.write(entry.frame);
			}
		} else {
			response.write(": connected\n\n");
		}
		this.clients.add(response);
		const heartbeat = setInterval(() => {
			if (response.writableEnded || response.destroyed) return;
			try {
				response.write("event: heartbeat\ndata: {}\n\n");
			} catch {
				this.clients.delete(response);
				clearInterval(heartbeat);
			}
		}, this.heartbeatMs);
		heartbeat.unref();
		response.on("close", () => {
			clearInterval(heartbeat);
			this.clients.delete(response);
		});
	}

	close(): void {
		this.unsubscribe();
		for (const response of this.clients) response.end();
		this.clients.clear();
	}
}
