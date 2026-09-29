import { randomUUID } from "node:crypto";
import type { ExtensionUIContext } from "@earendil-works/pi-coding-agent";

export type Question = {
	requestId: string;
	kind: "select" | "confirm" | "input" | "editor";
	title: string;
	options?: string[];
	message?: string;
	prefill?: string;
};
type Pending = { question: Question; settle(value: string | boolean | undefined): void };

/** Bridges supported extension prompts to Web; terminal components remain unavailable. */
export class WebUiBridge {
	private pending = new Map<string, Pending>();
	private listener?: (questions: Question[]) => void;
	private closed = false;

	onChange(listener: (questions: Question[]) => void): void {
		this.listener = listener;
	}
	snapshot(): Question[] {
		return [...this.pending.values()].map(({ question }) => question);
	}

	private ask(
		question: Omit<Question, "requestId">,
		opts?: { signal?: AbortSignal; timeout?: number },
	): Promise<string | boolean | undefined> {
		if (this.closed || opts?.signal?.aborted) return Promise.resolve(question.kind === "confirm" ? false : undefined);
		const requestId = randomUUID();
		return new Promise((resolve) => {
			let timer: ReturnType<typeof setTimeout> | undefined;
			const finish = (value: string | boolean | undefined) => {
				if (!this.pending.delete(requestId)) return;
				if (timer) clearTimeout(timer);
				opts?.signal?.removeEventListener("abort", abort);
				this.listener?.(this.snapshot());
				resolve(value);
			};
			const abort = () => finish(question.kind === "confirm" ? false : undefined);
			this.pending.set(requestId, { question: { ...question, requestId }, settle: finish });
			this.listener?.(this.snapshot());
			opts?.signal?.addEventListener("abort", abort, { once: true });
			if (opts?.timeout && opts.timeout > 0) timer = setTimeout(abort, opts.timeout);
		});
	}

	respond(requestId: string, value: unknown): void {
		const entry = this.pending.get(requestId);
		if (!entry) throw new Error("提问已失效");
		const { question } = entry;
		if (question.kind === "confirm") {
			if (typeof value !== "boolean") throw new Error("回答格式无效");
		} else if (value !== null && typeof value !== "string") throw new Error("回答格式无效");
		if (question.kind === "select" && value !== null && !question.options?.includes(value as string))
			throw new Error("选项不存在");
		entry.settle(value === null ? undefined : (value as string | boolean));
	}

	close(): void {
		this.closed = true;
		for (const entry of [...this.pending.values()])
			entry.settle(entry.question.kind === "confirm" ? false : undefined);
	}

	/** Only four portable dialogs have browser equivalents in this checkpoint. */
	context(): ExtensionUIContext {
		const context: Partial<ExtensionUIContext> = {
			select: async (title, options, opts) =>
				(await this.ask({ kind: "select", title, options }, opts)) as string | undefined,
			confirm: async (title, message, opts) =>
				(await this.ask({ kind: "confirm", title, message }, opts)) as boolean,
			input: async (title, prefill, opts) =>
				(await this.ask({ kind: "input", title, prefill }, opts)) as string | undefined,
			editor: async (title, prefill) => (await this.ask({ kind: "editor", title, prefill })) as string | undefined,
			notify: () => {},
			onTerminalInput: () => () => {},
			setStatus: () => {},
			setWorkingMessage: () => {},
			setWorkingVisible: () => {},
			setWorkingIndicator: () => {},
			setHiddenThinkingLabel: () => {},
			setWidget: () => {},
			setFooter: () => {},
			setHeader: () => {},
			setTitle: () => {},
			custom: async () => {
				throw new Error("Web 不支持终端自定义组件");
			},
			pasteToEditor: () => {},
			setEditorText: () => {},
			getEditorText: () => "",
			addAutocompleteProvider: () => {},
			setEditorComponent: () => {},
			getEditorComponent: () => undefined,
			theme: undefined,
			getAllThemes: () => [],
			getTheme: () => undefined,
			setTheme: () => ({ success: false, error: "Web 主题由设置控制" }),
			getToolsExpanded: () => false,
			setToolsExpanded: () => {},
		};
		return context as ExtensionUIContext;
	}
}
