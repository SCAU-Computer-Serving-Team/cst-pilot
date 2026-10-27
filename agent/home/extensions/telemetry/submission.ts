import { billingLookup } from "./billing.ts";
import { withinBudget } from "./close-budget.ts";
import type { SessionState } from "./collect.ts";
import type { TelemetryEndpoint } from "./config.ts";
import { queueRecord } from "./delivery.ts";
import { diagnose } from "./diagnostics.ts";
import { removeDraft, writeDraft } from "./draft.ts";
import { draftOwner } from "./draft-owner.ts";
import type { EnvironmentSnapshot } from "./environment.ts";
import { buildRecord, type SessionRecord } from "./record.ts";

interface SubmissionOptions {
	agentDir: string;
	state: SessionState;
	version: string;
	environment: Promise<EnvironmentSnapshot>;
	endpoints: TelemetryEndpoint[];
	flush: () => Promise<void>;
}

/** 每个实例串行持久化；事件到达时冻结统计，异步操作不再读取该版本的累计状态。 */
export class SessionSubmission {
	private revision = 0;
	private persistedRevision?: number;

	get snapshotRevision(): number | undefined {
		return this.persistedRevision;
	}
	private chain: Promise<void> = Promise.resolve();
	private submitted = "";
	private closing = false;
	private finalRecord?: SessionRecord;
	private finalizing?: Promise<void>;
	private billing?: Promise<void>;
	private sealedFinal = false;

	private readonly options: SubmissionOptions;

	constructor(options: SubmissionOptions) {
		this.options = options;
	}

	private capture(reason = "", contextEntries?: number): SessionRecord {
		const { state, version } = this.options;
		return {
			...buildRecord(state, { endReason: reason, endedAt: Date.now(), contextEntries }, version, null),
			v: "0.2",
			revision: ++this.revision,
			final: reason !== "",
		};
	}

	private enqueue(operation: () => Promise<void>): Promise<void> {
		const task = this.chain.then(operation);
		this.chain = task.catch(() => undefined);
		void task.catch(() => diagnose(this.options.agentDir, { failure: "storage" }));
		return task;
	}

	private async persist(record: SessionRecord, requests = this.options.state.cstoaBilling?.snapshot()): Promise<void> {
		const { agentDir, state, environment } = this.options;
		Object.assign(record, await environment);
		await writeDraft(agentDir, state.recordId, {
			record,
			lastTurnEndedAt: Date.parse(record.endedAt),
			cstoaRequests: requests,
			owner: await draftOwner(),
		});
		this.persistedRevision = record.revision;
		await diagnose(agentDir, {
			saved: { recordId: record.recordId, revision: record.revision, final: record.final },
		});
	}

	checkpoint(): Promise<void> {
		if (this.closing) return this.chain;
		const record = this.capture();
		const requests = this.options.state.cstoaBilling?.snapshot();
		return this.enqueue(() => this.persist(record, requests));
	}

	private hasUsage(): boolean {
		const { state } = this.options;
		return state.prompts > 0 || state.turns > 0 || (state.cstoaBilling?.summary()?.requests ?? 0) > 0;
	}

	submit(): Promise<void> {
		const { state } = this.options;
		if (this.closing || !this.hasUsage()) return this.chain;
		const record = this.capture();
		const requests = state.cstoaBilling?.snapshot();
		const { revision: _revision, endedAt: _endedAt, durationMs: _duration, ...stats } = record;
		const fingerprint = JSON.stringify(stats);
		return this.enqueue(async () => {
			if (fingerprint === this.submitted) return;
			await this.persist(record, requests);
			await queueRecord(this.options.agentDir, record, this.options.endpoints);
			this.submitted = fingerprint;
			void this.options.flush().catch(() => undefined);
		});
	}

	/** 先入队未知账单版本；补查完成后通过新版本提交，不改写已发送版本。 */
	settleBilling(): void {
		if (this.closing || this.billing) return;
		const billing = this.options.state.cstoaBilling;
		if (!billing) return;
		const before = JSON.stringify(billing.summary());
		this.billing = billing
			.settle(billingLookup(this.options.agentDir, fetch, AbortSignal.timeout(4_500)))
			.then(async () => {
				if (!this.closing && before !== JSON.stringify(billing.summary())) await this.submit();
			})
			.catch(() => undefined)
			.finally(() => {
				this.billing = undefined;
			});
	}

	finalize(reason: string, contextEntries?: number, signal = AbortSignal.timeout(4_500)): Promise<void> {
		if (this.finalizing) return this.finalizing;
		this.closing = true;
		if (!this.hasUsage()) {
			this.finalizing = this.enqueue(() => removeDraft(this.options.agentDir, this.options.state.recordId));
			return this.finalizing;
		}
		// 先保存关闭边界，账单查询中断时仍可恢复。
		this.finalRecord ??= this.capture(reason, contextEntries);
		const record = this.finalRecord;
		const requests = this.options.state.cstoaBilling?.snapshot();
		this.finalizing = this.enqueue(async () => {
			await this.persist(record, requests);
			const billing = this.options.state.cstoaBilling;
			if (billing && !this.sealedFinal) {
				if (this.billing) await withinBudget(this.billing, signal);
				const lookup = billingLookup(this.options.agentDir, fetch, signal);
				if (!signal.aborted) await withinBudget(billing.settle(lookup), signal);
				if (!signal.aborted && billing.summary()?.requests !== billing.summary()?.settledRequests) {
					await withinBudget(new Promise((resolve) => setTimeout(resolve, 300)), signal);
					if (!signal.aborted) await withinBudget(billing.settle(lookup), signal);
				}
				const summary = billing.summary();
				if (JSON.stringify(record.cstoaBilling) !== JSON.stringify(summary)) {
					this.finalRecord = { ...record, revision: ++this.revision, cstoaBilling: summary };
					await this.persist(this.finalRecord);
				}
			}
			this.sealedFinal = true;
			await queueRecord(this.options.agentDir, this.finalRecord!, this.options.endpoints);
			await removeDraft(this.options.agentDir, this.options.state.recordId);
			void this.options.flush().catch(() => undefined);
		}).catch((error: unknown) => {
			this.finalizing = undefined;
			throw error;
		});
		return this.finalizing;
	}
}
