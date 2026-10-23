import { createHash } from "node:crypto";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { oaHost } from "../../../oauth/oa.ts";
import type { ProviderQuota } from "./quota.ts";

export interface AccountStatus {
	providerId: "cstoa";
	signedIn: boolean;
	requiresLogin: boolean;
	profile: { supported: boolean; studentId: string | null; name: string | null; reason?: string };
}

type AccountRuntime = Pick<ModelRuntime, "listCredentials" | "getAuth">;
type Snapshot = { account: AccountStatus; quota: ProviderQuota };
const TTL_MS = 60_000;

function empty(signedIn: boolean, requiresLogin = false, reason?: string): Snapshot {
	return {
		account: {
			providerId: "cstoa",
			signedIn,
			requiresLogin,
			profile: { supported: true, studentId: null, name: null, ...(reason ? { reason } : {}) },
		},
		quota: {
			provider: "cstoa",
			supported: false,
			requiresLogin: !signedIn || requiresLogin,
			reason: requiresLogin ? "登录已失效，请重新登录。" : (reason ?? "登录后查看额度"),
		},
	};
}

function object(value: unknown): Record<string, unknown> | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: undefined;
}

/** 账号和额度共用一次 OA 查询。缓存限于当前 Web API 内存，资料与凭据不落盘。 */
export class CstoaAccount {
	private generation = 0;
	private cache?: { key: string; at: number; value: Snapshot };
	private inflight?: { key: string; task: Promise<Snapshot> };
	private readonly fetcher: typeof fetch;
	private readonly now: () => number;

	constructor(fetcher: typeof fetch = fetch, now: () => number = Date.now) {
		this.fetcher = fetcher;
		this.now = now;
	}

	invalidate(): void {
		this.generation++;
		this.cache = undefined;
		this.inflight = undefined;
	}

	async getAccount(runtime: AccountRuntime, invalidAuth = false): Promise<AccountStatus> {
		return (await this.read(runtime, invalidAuth)).account;
	}

	async getQuota(runtime: AccountRuntime, invalidAuth = false): Promise<ProviderQuota> {
		return (await this.read(runtime, invalidAuth)).quota;
	}

	private async readAccess(runtime: AccountRuntime, invalidAuth: boolean): Promise<string | Snapshot> {
		const credentials = await runtime.listCredentials();
		const signedIn = credentials.some((entry) => entry.providerId === "cstoa");
		if (!signedIn || invalidAuth) return empty(signedIn, signedIn && invalidAuth);
		try {
			return (await runtime.getAuth("cstoa"))?.auth?.apiKey || empty(true, true);
		} catch {
			return empty(true, true);
		}
	}

	private async read(runtime: AccountRuntime, invalidAuth: boolean, attempt = 0): Promise<Snapshot> {
		const generation = this.generation;
		const access = await this.readAccess(runtime, invalidAuth);
		if (typeof access !== "string") {
			this.invalidate();
			return access;
		}
		let endpoint: URL;
		try {
			const origin = new URL(oaHost());
			if (
				origin.username ||
				origin.password ||
				origin.search ||
				origin.hash ||
				(origin.protocol !== "https:" &&
					!(origin.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname)))
			)
				return empty(true, false, "账号服务地址无效，请检查配置。");
			const credential = (await runtime.listCredentials()).find((entry) => entry.providerId === "cstoa");
			endpoint = new URL(credential?.type === "api_key" ? "/api/agent/key-profile" : "/api/agent/profile", origin);
		} catch {
			return empty(true, false, "账号服务地址无效，请检查配置。");
		}
		if (generation !== this.generation)
			return attempt < 1 ? this.read(runtime, invalidAuth, attempt + 1) : empty(false);
		const key = `${endpoint.href}:${createHash("sha256").update(access).digest("hex")}`;
		if (this.cache?.key === key && this.now() - this.cache.at < TTL_MS) return this.cache.value;
		if (this.inflight && this.inflight.key !== key) this.invalidate();
		const epoch = this.generation;
		let task = this.inflight?.task;
		if (!task) {
			task = this.query(endpoint, access);
			this.inflight = { key, task };
		}
		const value = await task;
		if (epoch !== this.generation) {
			return attempt < 1 ? this.read(runtime, invalidAuth, attempt + 1) : empty(false);
		}
		// 外部 Pi 进程修改凭据时，Web 可能没有收到事件；返回资料前再核对当前身份。
		const currentAccess = await this.readAccess(runtime, invalidAuth);
		if (typeof currentAccess !== "string") {
			this.invalidate();
			return currentAccess;
		}
		if (epoch !== this.generation || currentAccess !== access) {
			if (currentAccess !== access) this.invalidate();
			return attempt < 1 ? this.read(runtime, invalidAuth, attempt + 1) : empty(false);
		}
		if (this.inflight?.task === task) this.inflight = undefined;
		// 网络、鉴权、格式和额度故障均可立即重试，不缓存失败。
		if (value.account.profile.studentId && !value.account.profile.reason && value.quota.supported)
			this.cache = { key, at: this.now(), value };
		return value;
	}

	private async query(endpoint: URL, access: string): Promise<Snapshot> {
		try {
			const response = await this.fetcher(endpoint, {
				headers: { Authorization: `Bearer ${access}`, Accept: "application/json" },
				signal: AbortSignal.timeout(8_000),
				redirect: "error",
			});
			if (!response.ok) {
				await response.body?.cancel();
				if (response.status === 401 || response.status === 403) return empty(true, true);
				return empty(
					true,
					false,
					response.status === 404 || response.status === 405
						? "账号资料暂不可用，请稍后重试。"
						: "账号资料查询失败，请重试。",
				);
			}
			const payload = object(await response.json());
			if (payload?.result === false)
				return payload.code === 19008 ? empty(true, true) : empty(true, false, "账号资料查询失败，请重试。");
			const data = object(payload?.data);
			if (
				payload?.result !== true ||
				typeof data?.memberId !== "string" ||
				!data.memberId.trim() ||
				data.memberId.length > 64 ||
				typeof data.name !== "string" ||
				data.name.length > 256
			)
				return empty(true, false, "账号资料格式无效，请稍后重试。");
			const value = empty(true);
			value.account.profile.studentId = data.memberId.trim();
			value.account.profile.name = data.name.trim() || null;
			if (!value.account.profile.name) value.account.profile.reason = "姓名暂未提供。";
			if (
				data.balanceAvailable === true &&
				typeof data.balance === "number" &&
				Number.isSafeInteger(data.balance) &&
				data.balance >= 0
			) {
				value.quota = {
					provider: "cstoa",
					supported: true,
					balance: data.balance,
					unit: "credit",
					fetchedAt: new Date(this.now()).toISOString(),
				};
			} else value.quota.reason = "额度暂不可用，请重试。";
			return value;
		} catch {
			return empty(true, false, "无法读取账号资料，请检查网络后重试。");
		}
	}
}
