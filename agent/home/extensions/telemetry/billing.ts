import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { oaHost } from "../oauth/oa.ts";

export interface BillingRequest {
	requestId?: string;
	quotaPoints: number | null;
}
export interface BillingSummary {
	requests: number;
	settledRequests: number;
	/** 已确认账单之和；无已确认账单时为 null。结合覆盖数量读取。 */
	quotaPoints: number | null;
}
export type BillingLookup = (ids: string[]) => Promise<Map<string, number | null>>;

/** 请求 ID 仅存本地草稿，定稿对外只提供累计值和覆盖数量。 */
export class CstoaBilling {
	private readonly requests: BillingRequest[];
	constructor(requests: BillingRequest[] = []) {
		this.requests = requests.map((item) => ({ ...item }));
	}
	start(): (id?: string) => void {
		const item: BillingRequest = { quotaPoints: null };
		this.requests.push(item);
		return (id) => {
			item.requestId = id;
		};
	}
	snapshot(): BillingRequest[] {
		const seen = new Set<string>();
		return this.requests
			.filter((item) => {
				if (!item.requestId) return true;
				if (seen.has(item.requestId)) return false;
				seen.add(item.requestId);
				return true;
			})
			.map((item) => ({ ...item }));
	}
	summary(): BillingSummary | undefined {
		const items = this.snapshot();
		if (!items.length) return undefined;
		const settled = items.filter((item) => item.quotaPoints !== null);
		return {
			requests: items.length,
			settledRequests: settled.length,
			quotaPoints: settled.length ? settled.reduce((sum, item) => sum + item.quotaPoints!, 0) : null,
		};
	}
	async settle(lookup: BillingLookup): Promise<void> {
		const ids = this.snapshot()
			.filter((item) => item.requestId && item.quotaPoints === null)
			.map((item) => item.requestId!);
		for (let offset = 0; offset < ids.length; offset += 50) {
			try {
				const values = await lookup(ids.slice(offset, offset + 50));
				for (const item of this.requests) {
					const value = item.requestId ? values.get(item.requestId) : undefined;
					if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) item.quotaPoints = value;
				}
			} catch {
				/* 查询失败保留未知值；下一轮或定稿再查。 */
			}
		}
	}
}

/** 只读当前 OA 凭据，不刷新、不写回。安装签名不具备账单查询权限。 */
export function billingLookup(agentDir: string, fetcher: typeof fetch = fetch, signal?: AbortSignal): BillingLookup {
	return async (ids) => {
		const raw = JSON.parse(await readFile(join(agentDir, "auth.json"), "utf8"));
		const entry = raw.cstoa;
		const token = entry?.type === "api_key" ? entry.key : entry?.expires > Date.now() ? entry.access : undefined;
		if (typeof token !== "string" || !token) return new Map();
		const response = await fetcher(`${oaHost()}/api/agent/llm/billing`, {
			method: "POST",
			headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
			body: JSON.stringify({ requestIds: ids }),
			signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(2_000)]) : AbortSignal.timeout(2_000),
			redirect: "error",
		});
		if (!response.ok) {
			await response.body?.cancel();
			return new Map();
		}
		const value = await response.json();
		if (value?.result !== true || value.data?.unit !== "quota_point" || !Array.isArray(value.data.requests))
			return new Map();
		const allowed = new Set(ids);
		const values = new Map<string, number | null>();
		for (const item of value.data.requests) {
			if (
				allowed.has(item?.requestId) &&
				(item.quotaPoints === null || (Number.isSafeInteger(item.quotaPoints) && item.quotaPoints >= 0))
			)
				values.set(item.requestId, item.quotaPoints);
		}
		return values;
	};
}
