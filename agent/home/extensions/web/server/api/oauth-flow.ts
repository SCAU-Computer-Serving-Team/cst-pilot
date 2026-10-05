import { randomUUID } from "node:crypto";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { InboxConflict } from "../session/inbox.ts";

type Interaction = Parameters<ModelRuntime["login"]>[2];
type Prompt = Parameters<Interaction["prompt"]>[0];
type Event = Parameters<Interaction["notify"]>[0];
type PromptView<T> = T extends unknown ? Omit<T, "signal"> & { id: string } : never;
export type OAuthPromptView = PromptView<Prompt>;
export interface OAuthFlowView {
	authType?: "oauth" | "api_key";
	flowId: string;
	revision: number;
	state: "pending" | "succeeded" | "failed" | "cancelled";
	deviceCode?: { userCode: string; verificationUri: string; intervalSeconds: number; expiresInSeconds: number };
	authUrl?: { url: string; instructions?: string };
	prompt?: OAuthPromptView;
	message?: string;
	links?: { url: string; label?: string }[];
	error?: string;
}
interface Flow {
	view: OAuthFlowView;
	controller: AbortController;
	expiresAt?: number;
	task: Promise<void>;
	pending?: { resolve(value: string): void; reject(reason: Error): void; cleanup(): void };
}
function httpUrl(value: string): string {
	const url = new URL(value);
	if (!["https:", "http:"].includes(url.protocol)) throw new Error("请求的授权链接格式无效");
	return url.href;
}

/** Pi 负责登录和凭据保存；此模块只把交互步骤转换为可恢复的 Web 状态。 */
export class OAuthFlows {
	private readonly flows = new Map<string, Flow>();
	private readonly changed: (providerId: string, view: OAuthFlowView) => void;
	constructor(changed: (providerId: string, view: OAuthFlowView) => void) {
		this.changed = changed;
	}

	status(providerId: string): OAuthFlowView | { state: "idle" } {
		const flow = this.flows.get(providerId);
		if (!flow) return { state: "idle" };
		const view = structuredClone(flow.view);
		if (view.deviceCode && flow.expiresAt)
			view.deviceCode.expiresInSeconds = Math.max(0, Math.ceil((flow.expiresAt - Date.now()) / 1000));
		return view;
	}
	private publish(providerId: string) {
		const flow = this.flows.get(providerId);
		if (flow) flow.view.revision++;
		const view = this.status(providerId);
		if (view.state !== "idle") this.changed(providerId, view);
	}
	start(
		runtime: ModelRuntime,
		providerId: string,
		options: {
			type?: "oauth" | "api_key";
			key?: string;
			wrap?: (login: () => Promise<unknown>) => Promise<unknown>;
		} = {},
	): Flow {
		const existing = this.flows.get(providerId);
		if (existing?.view.state === "pending") return existing;
		const authType = options.type ?? "oauth";
		const flow: Flow = {
			view: { flowId: randomUUID(), revision: 0, state: "pending", authType },
			controller: new AbortController(),
			task: Promise.resolve(),
		};
		this.flows.set(providerId, flow);
		const timer = setTimeout(() => this.cancel(providerId, flow.view.flowId), 15 * 60_000);
		timer.unref();
		let firstKey = options.key;
		const login = () =>
			runtime.login(providerId, authType, {
				signal: flow.controller.signal,
				prompt: (question) => {
					if (question.type === "secret" && firstKey !== undefined) {
						const key = firstKey;
						firstKey = undefined;
						return Promise.resolve(key);
					}
					return this.prompt(providerId, flow, question);
				},
				notify: (event) => this.notify(providerId, flow, event),
			});
		flow.task = (options.wrap ? options.wrap(login) : login())
			.then(() => {
				if (flow.controller.signal.aborted) return;
				flow.view.state = "succeeded";
			})
			.catch((error: unknown) => {
				flow.view.state = flow.controller.signal.aborted ? "cancelled" : "failed";
				if (!flow.controller.signal.aborted)
					flow.view.error = error instanceof Error ? error.message : "登录未完成，请重试。";
			})
			.finally(() => {
				firstKey = undefined;
				clearTimeout(timer);
				this.clearPrompt(flow);
				delete flow.view.authUrl;
				delete flow.view.deviceCode;
				delete flow.view.links;
				if (this.flows.get(providerId) === flow) this.publish(providerId);
			});
		return flow;
	}
	private clearPrompt(flow: Flow) {
		flow.pending?.cleanup();
		flow.pending = undefined;
		delete flow.view.prompt;
	}
	private prompt(providerId: string, flow: Flow, question: Prompt): Promise<string> {
		if (flow.controller.signal.aborted || question.signal?.aborted)
			return Promise.reject(new Error("Login cancelled"));
		if (flow.pending) return Promise.reject(new Error("请求的授权步骤仍在等待回答"));
		return new Promise((resolve, reject) => {
			const { signal, ...view } = question;
			flow.view.prompt = { ...view, id: randomUUID() };
			const cancel = () => {
				this.clearPrompt(flow);
				reject(new Error("Login cancelled"));
				this.publish(providerId);
			};
			flow.pending = {
				resolve,
				reject,
				cleanup: () => {
					flow.controller.signal.removeEventListener("abort", cancel);
					signal?.removeEventListener("abort", cancel);
				},
			};
			flow.controller.signal.addEventListener("abort", cancel, { once: true });
			signal?.addEventListener("abort", cancel, { once: true });
			this.publish(providerId);
		});
	}
	private notify(providerId: string, flow: Flow, event: Event) {
		if (flow.controller.signal.aborted || this.flows.get(providerId) !== flow) return;
		if (event.type === "device_code") {
			const seconds = event.expiresInSeconds ?? 300;
			flow.expiresAt = Date.now() + seconds * 1000;
			flow.view.deviceCode = {
				userCode: event.userCode,
				verificationUri: httpUrl(event.verificationUri),
				intervalSeconds: event.intervalSeconds ?? 5,
				expiresInSeconds: seconds,
			};
		} else if (event.type === "auth_url") {
			flow.view.authUrl = { url: httpUrl(event.url), instructions: event.instructions };
		} else {
			flow.view.message = event.message;
			if (event.type === "info") flow.view.links = event.links?.map((link) => ({ ...link, url: httpUrl(link.url) }));
		}
		this.publish(providerId);
	}
	async ready(providerId: string, flow: Flow): Promise<OAuthFlowView | { state: "idle" }> {
		const deadline = Date.now() + 10_000;
		while (
			flow.view.state === "pending" &&
			!flow.view.deviceCode &&
			!flow.view.authUrl &&
			!flow.view.prompt &&
			!flow.view.message
		) {
			if (Date.now() >= deadline) {
				this.cancel(providerId, flow.view.flowId);
				throw new Error("请求授权信息超时，请重试。");
			}
			await new Promise((resolve) => setTimeout(resolve, 25));
		}
		return this.status(providerId);
	}
	respond(providerId: string, flowId: string, promptId: string, value: string) {
		const flow = this.flows.get(providerId);
		const prompt = flow?.view.prompt;
		if (
			!flow ||
			flow.view.flowId !== flowId ||
			flow.view.state !== "pending" ||
			prompt?.id !== promptId ||
			!flow.pending
		)
			throw new InboxConflict("授权步骤已更新，请按当前页面继续。");
		if (
			value.length > 8192 ||
			(prompt.type !== "text" && !value.trim()) ||
			(prompt.type === "select" && !prompt.options.some((option) => option.id === value))
		)
			throw new Error("请求的授权回答无效");
		const resolve = flow.pending.resolve;
		this.clearPrompt(flow);
		resolve(value);
		this.publish(providerId);
	}
	cancel(providerId: string, flowId?: string): boolean {
		const flow = this.flows.get(providerId);
		if (!flow || flow.view.state !== "pending" || (flowId && flow.view.flowId !== flowId)) return false;
		flow.view.state = "cancelled";
		flow.controller.abort();
		this.clearPrompt(flow);
		delete flow.view.authUrl;
		delete flow.view.deviceCode;
		this.publish(providerId);
		return true;
	}
	async cancelAndWait(providerId: string) {
		const flow = this.flows.get(providerId);
		this.cancel(providerId);
		await flow?.task;
	}
	async close() {
		for (const provider of this.flows.keys()) this.cancel(provider);
		await Promise.all([...this.flows.values()].map((flow) => flow.task));
	}
}
