import { randomUUID } from "node:crypto";
import type { TelemetryEndpoint } from "./config.ts";
import { diagnose } from "./diagnostics.ts";
import type { EnvironmentSnapshot } from "./environment.ts";
import { sendBatch, type UploadAuth } from "./transport.ts";

export interface Heartbeat {
	runId: string;
	sequence: number;
	sentAt: string;
	status: "idle" | "busy" | "closed";
	identity: EnvironmentSnapshot["identity"];
	lastActivityAt: string | null;
	snapshots: { recordId: string; revision: number }[];
}

interface HeartbeatOptions {
	agentDir?: string;
	environment: Promise<EnvironmentSnapshot>;
	now?: () => number;
}
interface PendingHeartbeat {
	heartbeat: Heartbeat;
	auth: UploadAuth;
	endpoint: TelemetryEndpoint;
}
interface HeartbeatDelivery {
	pending?: PendingHeartbeat;
	inflight?: Promise<void>;
}

/** 每个进程 home 一份运行状态；每端点独立单飞，等待中的心跳只保留最新一份。 */
export class HeartbeatRun {
	readonly runId = randomUUID();
	private sequence = 0;
	private generatedAt = -Infinity;
	private lastActivityAt: string | null = null;
	private closed = false;
	private readonly instances = new Map<string, { busy: boolean; revision: () => number | undefined }>();
	private readonly deliveries = new Map<string, HeartbeatDelivery>();
	private readonly options: HeartbeatOptions;

	constructor(options: HeartbeatOptions) {
		this.options = options;
	}

	private now(): number {
		return this.options.now?.() ?? Date.now();
	}

	register(recordId: string, revision: () => number | undefined): void {
		this.instances.set(recordId, { busy: false, revision });
	}

	activity(recordId: string, busy?: boolean): void {
		const instance = this.instances.get(recordId);
		if (!instance || this.closed) return;
		if (busy !== undefined) instance.busy = busy;
		this.lastActivityAt = new Date(this.now()).toISOString();
	}

	remove(recordId: string): void {
		this.instances.delete(recordId);
	}

	async pulse(auth: UploadAuth, endpoints: TelemetryEndpoint[], close = false, force = false): Promise<void> {
		const now = this.now();
		if (this.closed || (!close && !force && now >= this.generatedAt && now - this.generatedAt < 30_000)) return;
		this.closed = close;
		this.generatedAt = now;
		const sequence = ++this.sequence;
		const environment = await this.options.environment;
		const snapshots: Heartbeat["snapshots"] = [];
		for (const [recordId, instance] of this.instances) {
			const revision = instance.revision();
			if (revision !== undefined) snapshots.push({ recordId, revision });
		}
		const heartbeat: Heartbeat = {
			runId: this.runId,
			sequence,
			sentAt: new Date(this.now()).toISOString(),
			status: close ? "closed" : [...this.instances.values()].some((instance) => instance.busy) ? "busy" : "idle",
			identity: { ...environment.identity },
			lastActivityAt: this.lastActivityAt,
			snapshots: snapshots.slice(0, 512),
		};
		await Promise.allSettled(
			endpoints
				.filter((endpoint) => !endpoint.unavailable)
				.map((endpoint) => {
					let delivery = this.deliveries.get(endpoint.url);
					if (!delivery) {
						delivery = {};
						this.deliveries.set(endpoint.url, delivery);
					}
					delivery.pending = { heartbeat, auth, endpoint };
					delivery.inflight ??= this.send(delivery).finally(() => {
						delivery.inflight = undefined;
					});
					return delivery.inflight;
				}),
		);
	}

	private async send(delivery: HeartbeatDelivery): Promise<void> {
		while (delivery.pending) {
			const { heartbeat, auth, endpoint } = delivery.pending;
			delivery.pending = undefined;
			if (this.now() - Date.parse(heartbeat.sentAt) > 90_000) continue;
			let result = await sendBatch(endpoint.url, auth, [], endpoint.ca, heartbeat);
			if (result.kind === "auth" && typeof auth !== "string" && "installation" in auth)
				result = await sendBatch(endpoint.url, auth.installation, [], endpoint.ca, heartbeat);
			if (this.options.agentDir) {
				await diagnose(
					this.options.agentDir,
					result.kind === "accepted" && result.heartbeatAcknowledged
						? { endpoint: endpoint.url, heartbeat: { runId: heartbeat.runId, sequence: heartbeat.sequence } }
						: {
								endpoint: endpoint.url,
								failure: result.kind === "accepted" ? "protocol" : (result.failure ?? "http"),
							},
				);
			}
		}
	}
}
