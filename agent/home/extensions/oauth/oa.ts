/**
 * OA 登录扩展 · 设备授权与令牌交换。
 *
 * 规格：doc/auth/README.md「范围与流程」「OA 接口」「登录有效期」。
 * 协议是 OAuth 2.0 设备授权流（RFC 8628），只使用 Node 内置模块与全局 fetch：
 *
 * 1. POST /api/oauth/device_authorization 申请设备码（按 IP 限流，无需鉴权）。
 * 2. 把完整授权链接交给 callbacks.onDeviceCode（见下方「verificationUri 约定」），
 *    按 interval 轮询 POST /api/oauth/token。
 * 3. 轮询错误按 RFC 8628 处理：authorization_pending / slow_down / expired_token / access_denied。
 * 4. 成功返回 { access, refresh, expires }；默认模式 refresh 为空串。
 * 5. refresh(credentials, signal) 用 refresh_token 换轮换后的凭据，响应调用方给出的超时信号。
 *
 * verificationUri 约定：与 pi 内核内置的 Kimi provider 一致，把
 * verification_uri_complete（带 user_code 的完整链接）填进 verificationUri，
 * TUI 原生对话框点击打开即预填数字码；Web 端直接编码该链接渲染二维码，
 * 这样两端都不需要另建字段传输通道。
 */

import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { getDeviceId } from "./device.ts";

const DEFAULT_HOST = "https://www.cstoa.top";
const DEFAULT_POLL_INTERVAL_SECONDS = 5;
const DEFAULT_EXPIRES_IN_SECONDS = 300;
const MIN_POLL_INTERVAL_MS = 1000;
/** RFC 8628 §3.5：slow_down 时轮询间隔至少 +5 秒。 */
const SLOW_DOWN_INCREMENT_MS = 5000;
const REQUEST_TIMEOUT_MS = 30_000;
const DEVICE_CODE_GRANT = "urn:ietf:params:oauth:grant-type:device_code";
/** M1 只申请模型作用域；遥测等作用域接入时单独扩展（README「作用域」）。 */
const SCOPES = ["llm:chat"];
/** pi 内核用这个固定文案区分「用户取消」与真实失败。 */
const CANCELLED = "Login cancelled";

export interface OaCredentials {
	access: string;
	refresh: string;
	expires: number;
	[key: string]: unknown;
}

export interface DeviceAuthorization {
	deviceCode: string;
	userCode: string;
	verificationUriComplete: string;
	intervalSeconds: number;
	expiresInSeconds: number;
}

/** pi 设备码回调的最小形状，与内核 OAuthDeviceCodeInfo 结构兼容。 */
export interface DeviceCodeCallbacks {
	onDeviceCode(info: {
		userCode: string;
		verificationUri: string;
		intervalSeconds?: number;
		expiresInSeconds?: number;
	}): void;
	signal?: AbortSignal;
}

/** 测试可通过 CSTOA_OA_HOST 指向本地 mock；发行版 pi.cmd 会清除该变量防止被宿主机劫持。 */
export function oaHost(): string {
	const override = process.env.CSTOA_OA_HOST?.trim();
	return (override || DEFAULT_HOST).replace(/\/+$/, "");
}

function requestSignal(signal: AbortSignal): AbortSignal {
	return AbortSignal.any([signal, AbortSignal.timeout(REQUEST_TIMEOUT_MS)]);
}

function abortError(): Error {
	return new Error(CANCELLED);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
	return new Promise((resolve, reject) => {
		if (signal.aborted) {
			reject(abortError());
			return;
		}
		const onAbort = () => {
			clearTimeout(timer);
			reject(abortError());
		};
		const timer = setTimeout(() => {
			signal.removeEventListener("abort", onAbort);
			resolve();
		}, ms);
		signal.addEventListener("abort", onAbort, { once: true });
	});
}

async function readJson(response: Response): Promise<Record<string, unknown> | null> {
	try {
		const value = await response.json();
		return value && typeof value === "object" ? (value as Record<string, unknown>) : null;
	} catch {
		return null;
	}
}

async function requestJson(
	url: string,
	body: Record<string, unknown>,
	signal: AbortSignal,
): Promise<{ response: Response; body: Record<string, unknown> | null }> {
	let response: Response;
	try {
		response = await fetch(url, {
			method: "POST",
			headers: { "Content-Type": "application/json", Accept: "application/json" },
			body: JSON.stringify(body),
			signal: requestSignal(signal),
		});
	} catch {
		if (signal.aborted) throw abortError();
		throw new Error("无法连接 OA 服务器，请检查网络后重试。");
	}
	return { response, body: await readJson(response) };
}

/** 验证授权链接只允许 http(s)，避免把任意 URI 交给浏览器或二维码。 */
function trustedHttpUrl(value: unknown): string | undefined {
	if (typeof value !== "string" || !value) return undefined;
	try {
		const url = new URL(value);
		if (url.protocol !== "https:" && url.protocol !== "http:") return undefined;
		return url.href;
	} catch {
		return undefined;
	}
}

function platform(): string {
	if (process.platform === "win32") return "Windows";
	if (process.platform === "darwin") return "macOS";
	return process.platform;
}

/** agent/package.json 的版本号（agentDir 是 agent/home）；读不到就留空。 */
async function agentVersion(agentDir: string): Promise<string> {
	try {
		const parsed = JSON.parse(await readFile(join(agentDir, "..", "package.json"), "utf8")) as {
			version?: unknown;
		};
		return typeof parsed.version === "string" ? parsed.version : "";
	} catch {
		return "";
	}
}

export async function startDeviceAuthorization(agentDir: string, signal: AbortSignal): Promise<DeviceAuthorization> {
	const host = oaHost();
	const { response, body } = await requestJson(
		`${host}/api/oauth/device_authorization`,
		{
			device_id: await getDeviceId(agentDir),
			name: "cst-pilot",
			platform: platform(),
			agent_version: await agentVersion(agentDir),
			scope: SCOPES,
		},
		signal,
	);
	if (!response.ok || !body) {
		throw new Error(`设备授权申请失败（HTTP ${response.status}），请稍后重试。`);
	}

	const deviceCode = body.device_code;
	const userCode = body.user_code;
	const verificationUriComplete = trustedHttpUrl(body.verification_uri_complete);
	if (typeof deviceCode !== "string" || !deviceCode || typeof userCode !== "string" || !verificationUriComplete) {
		throw new Error("设备授权响应缺少必要字段，请联系管理员。");
	}
	const interval = body.interval;
	const expiresIn = body.expires_in;
	return {
		deviceCode,
		userCode,
		verificationUriComplete,
		intervalSeconds:
			typeof interval === "number" && Number.isFinite(interval) && interval > 0
				? interval
				: DEFAULT_POLL_INTERVAL_SECONDS,
		expiresInSeconds:
			typeof expiresIn === "number" && Number.isFinite(expiresIn) && expiresIn > 0
				? expiresIn
				: DEFAULT_EXPIRES_IN_SECONDS,
	};
}

function credentialsFrom(body: Record<string, unknown>): OaCredentials {
	const access = body.access_token;
	const expiresIn = body.expires_in;
	if (typeof access !== "string" || !access) {
		throw new Error("令牌响应缺少 access_token。");
	}
	const seconds = typeof expiresIn === "number" && Number.isFinite(expiresIn) && expiresIn > 0 ? expiresIn : 7200;
	return {
		access,
		refresh: typeof body.refresh_token === "string" ? body.refresh_token : "",
		expires: Date.now() + seconds * 1000,
	};
}

export async function pollForToken(
	host: string,
	authorization: DeviceAuthorization,
	signal: AbortSignal,
): Promise<OaCredentials> {
	const deadline = Date.now() + authorization.expiresInSeconds * 1000;
	let intervalMs = Math.max(MIN_POLL_INTERVAL_MS, Math.floor(authorization.intervalSeconds * 1000));
	// RFC 8628：轮询前先等一个 interval，避免因早轮询不断触发 slow_down。
	await sleep(Math.min(intervalMs, Math.max(0, deadline - Date.now())), signal);

	while (Date.now() < deadline) {
		const { response, body } = await requestJson(
			`${host}/api/oauth/token`,
			{ grant_type: DEVICE_CODE_GRANT, device_code: authorization.deviceCode },
			signal,
		);
		if (response.ok && body && typeof body.access_token === "string") {
			return credentialsFrom(body);
		}

		const error = typeof body?.error === "string" ? body.error : "";
		if (error === "authorization_pending") {
			// 继续等待下一次轮询。
		} else if (error === "slow_down") {
			intervalMs += SLOW_DOWN_INCREMENT_MS;
		} else if (error === "access_denied") {
			throw new Error("授权已被拒绝，请重新发起登录。");
		} else if (error === "expired_token") {
			throw new Error("设备码已过期，请重新发起登录。");
		} else if (response.status >= 500) {
			throw new Error(`OA 服务暂时不可用（HTTP ${response.status}），请稍后重试。`);
		} else if (error === "invalid_grant" || error === "invalid_request") {
			throw new Error("设备码无效，请重新发起登录。");
		} else {
			throw new Error(`令牌请求失败（HTTP ${response.status}）${error ? `：${error}` : "。"}`);
		}

		const remaining = deadline - Date.now();
		if (remaining <= 0) break;
		await sleep(Math.min(intervalMs, remaining), signal);
	}
	throw new Error("设备码已过期，请重新发起登录。");
}

export async function login(agentDir: string, callbacks: DeviceCodeCallbacks): Promise<OaCredentials> {
	const signal = callbacks.signal ?? new AbortController().signal;
	const host = oaHost();
	const authorization = await startDeviceAuthorization(agentDir, signal);
	callbacks.onDeviceCode({
		userCode: authorization.userCode,
		// 与内核 Kimi provider 相同：传完整链接，TUI 点击即预填、Web 直接生成二维码。
		verificationUri: authorization.verificationUriComplete,
		intervalSeconds: authorization.intervalSeconds,
		expiresInSeconds: authorization.expiresInSeconds,
	});
	return pollForToken(host, authorization, signal);
}

export async function refresh(credentials: OaCredentials, signal: AbortSignal): Promise<OaCredentials> {
	const refreshToken = typeof credentials.refresh === "string" ? credentials.refresh : "";
	if (!refreshToken) {
		throw new Error("没有可用的刷新令牌，请重新登录。");
	}
	const { response, body } = await requestJson(
		`${oaHost()}/api/oauth/token`,
		{ grant_type: "refresh_token", refresh_token: refreshToken },
		signal,
	);
	if (response.ok && body && typeof body.access_token === "string") {
		return credentialsFrom(body);
	}
	const error = typeof body?.error === "string" ? body.error : "";
	if (response.status === 401 || response.status === 403 || error === "invalid_grant" || error === "invalid_request") {
		throw new Error("登录状态已失效，请重新登录。");
	}
	throw new Error(`令牌刷新失败（HTTP ${response.status}），请检查网络后重试。`);
}
