import { randomUUID } from "node:crypto";
import type { BitLockerInteraction } from "./bitlocker-core.ts";
import type { BitLockerVolume } from "./bitlocker-data.ts";
import { validateRecoveryPassword } from "./bitlocker-password.ts";

export class BitLockerInputError extends Error {}
export class BitLockerExpiredError extends Error {}
export type BitLockerPendingRequest = {
	requestId: string;
	toolCallId: string;
	entryId?: string;
	kind: "password" | "parameter" | "decrypt";
	expiresAt: number;
	targets: BitLockerVolume[];
};
type Answer = string | boolean | undefined;
export class BitLockerWebInteraction {
	readonly epoch = randomUUID();
	private pending = new Map<string, { request: BitLockerPendingRequest; finish: (value: Answer) => void }>();
	private listeners = new Set<() => void>();
	private closed = false;
	private version = 0;
	private readonly timeoutMs: number;
	constructor(timeoutMs = 180_000) {
		this.timeoutMs = timeoutMs;
	}
	get revision(): number {
		return this.version;
	}
	snapshot(): BitLockerPendingRequest[] {
		return structuredClone([...this.pending.values()].map((x) => x.request));
	}
	onChange(listener: () => void): () => void {
		this.listeners.add(listener);
		return () => this.listeners.delete(listener);
	}
	private changed() {
		this.version++;
		for (const listener of this.listeners) listener();
	}
	close(): void {
		this.closed = true;
		for (const item of [...this.pending.values()]) item.finish(undefined);
		this.listeners.clear();
	}
	respond(requestId: string, input: Record<string, unknown>): void {
		const item = this.pending.get(requestId);
		if (!item) throw new BitLockerExpiredError("请求已处理或失效");
		if (Date.now() >= item.request.expiresAt) {
			item.finish(undefined);
			throw new BitLockerExpiredError("请求已过期");
		}
		if (Object.keys(input).length !== 1) throw new BitLockerInputError("请只提交密码、确认或取消");
		if (input.cancel === true) {
			item.finish(undefined);
			return;
		}
		if (item.request.kind === "password") {
			if (typeof input.password !== "string") throw new BitLockerInputError("请提供恢复密码");
			let password: string;
			try {
				password = validateRecoveryPassword(input.password);
			} catch {
				throw new BitLockerInputError("恢复密码必须为八组六位数字，用连字符分隔");
			}
			item.finish(password);
		} else {
			if (typeof input.confirm !== "boolean") throw new BitLockerInputError("请明确确认或取消");
			item.finish(input.confirm);
		}
	}
	forCall(toolCallId: string, entryId?: string): BitLockerInteraction {
		return {
			available: !this.closed,
			password: async (volume, signal) => {
				const value = await this.wait(toolCallId, entryId, "password", [volume], signal);
				return typeof value === "string" ? value : undefined;
			},
			confirmParameter: async (volume, signal) =>
				(await this.wait(toolCallId, entryId, "parameter", [volume], signal)) === true,
			confirmDecryption: async (volumes, signal) =>
				(await this.wait(toolCallId, entryId, "decrypt", volumes, signal)) === true,
		};
	}
	private wait(
		toolCallId: string,
		entryId: string | undefined,
		kind: BitLockerPendingRequest["kind"],
		targets: BitLockerVolume[],
		signal?: AbortSignal,
	): Promise<Answer> {
		if (this.closed || signal?.aborted) return Promise.resolve(undefined);
		return new Promise((resolve) => {
			const requestId = randomUUID();
			const request: BitLockerPendingRequest = {
				requestId,
				toolCallId,
				entryId,
				kind,
				targets: structuredClone(targets),
				expiresAt: Date.now() + this.timeoutMs,
			};
			const abort = () => finish(undefined);
			const timer = setTimeout(abort, this.timeoutMs);
			const finish = (value: Answer) => {
				if (!this.pending.delete(requestId)) return;
				clearTimeout(timer);
				signal?.removeEventListener("abort", abort);
				resolve(value);
				this.changed();
			};
			this.pending.set(requestId, { request, finish });
			signal?.addEventListener("abort", abort, { once: true });
			this.changed();
		});
	}
}
const key = Symbol.for("cst-pilot.bitlocker.web-interactions");
const holder = globalThis as typeof globalThis & { [key]?: Map<string, BitLockerWebInteraction> };
holder[key] ??= new Map();
export const bitLockerWebInteractions = holder[key];
