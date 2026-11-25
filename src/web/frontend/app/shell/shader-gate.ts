import type { ResolvedTheme } from "./theme.ts";

export type BackgroundKind = "home" | "sky" | "air";
export type NormalizedBackgroundKind = "home" | "sky";

export interface ShaderGateInputs {
	paused: boolean;
	documentHidden: boolean;
	windowFocused: boolean;
	reducedMotion: boolean;
	hardwareSampling?: boolean;
}

const HARDWARE_SAMPLING_CHANNEL = "cst-pilot-hardware-sampling";
const SAMPLING_HEARTBEAT_MS = 15_000;
// Hidden tabs can be throttled to one timer wakeup per minute.
const SAMPLING_LEASE_MS = 90_000;

export class HardwareSamplingSources {
	local = false;
	private readonly remote = new Map<string, number>();

	update(source: string, expiresAt: number | null) {
		if (expiresAt === null) this.remote.delete(source);
		else this.remote.set(source, expiresAt);
	}

	nextExpiry(now: number): number {
		let next = Infinity;
		for (const [source, expiresAt] of this.remote) {
			if (expiresAt <= now) this.remote.delete(source);
			else next = Math.min(next, expiresAt);
		}
		return next;
	}

	active(now: number): boolean {
		return this.nextExpiry(now) < Infinity || this.local;
	}
}

type SamplingMessage = { source: string; expiresAt: number | null } | { request: true };

const sampling = new HardwareSamplingSources();
const listeners = new Set<(active: boolean) => void>();
let notifiedActive = false;
let channel: BroadcastChannel | null | undefined;
let source: string;
let heartbeat: number | undefined;
let expiry: number | undefined;

export function isHardwareSamplingActive(): boolean {
	return sampling.active(Date.now());
}

function notifySampling() {
	const now = Date.now();
	const next = sampling.nextExpiry(now);
	window.clearTimeout(expiry);
	expiry = next < Infinity ? window.setTimeout(notifySampling, next - now) : undefined;
	const active = sampling.local || next < Infinity;
	if (active !== notifiedActive) {
		notifiedActive = active;
		for (const listener of listeners) listener(active);
	}
}

function announceSampling(active: boolean) {
	channel?.postMessage({ source, expiresAt: active ? Date.now() + SAMPLING_LEASE_MS : null });
}

function connectSamplingChannel() {
	if (channel !== undefined || typeof BroadcastChannel === "undefined") return;
	try {
		channel = new BroadcastChannel(HARDWARE_SAMPLING_CHANNEL);
		source = crypto.randomUUID();
		channel.onmessage = ({ data }: MessageEvent<SamplingMessage>) => {
			if (!data || typeof data !== "object") return;
			if ("request" in data) {
				if (sampling.local) announceSampling(true);
			} else if (typeof data.source === "string" && (data.expiresAt === null || Number.isFinite(data.expiresAt))) {
				sampling.update(
					data.source,
					data.expiresAt === null ? null : Math.min(data.expiresAt, Date.now() + SAMPLING_LEASE_MS),
				);
				notifySampling();
			}
		};
		// A tab opened mid-sample requests current leases without waiting for the next heartbeat.
		channel.postMessage({ request: true });
		window.addEventListener("pagehide", () => {
			if (sampling.local) announceSampling(false);
		});
		window.addEventListener("pageshow", () => {
			if (sampling.local) announceSampling(true);
		});
	} catch {
		channel?.close();
		channel = null;
	}
}

const SOFTWARE_RENDERER_PATTERN =
	/swiftshader|llvmpipe|softpipe|microsoft basic render|software renderer|subzero|mesa offscreen|angle.*warp/i;

/** 识别已知软件渲染器（如 Chrome SwiftShader / Mesa llvmpipe / Windows WARP），强制回退静态底图。 */
export function isSoftwareWebGlRenderer(renderer: unknown, vendor?: unknown): boolean {
	const text = `${typeof renderer === "string" ? renderer : ""} ${typeof vendor === "string" ? vendor : ""}`.trim();
	return text.length > 0 && SOFTWARE_RENDERER_PATTERN.test(text);
}

/** Publish this tab's sampling state without changing any other tab's lease. */
export function setHardwareSamplingActive(active: boolean): void {
	if (typeof window === "undefined" || sampling.local === active) return;
	sampling.local = active;
	connectSamplingChannel();
	window.clearInterval(heartbeat);
	heartbeat = active && channel ? window.setInterval(() => announceSampling(true), SAMPLING_HEARTBEAT_MS) : undefined;
	announceSampling(active);
	notifySampling();
}

export function subscribeHardwareSampling(onChange: (active: boolean) => void): () => void {
	if (typeof window === "undefined") return () => {};
	connectSamplingChannel();
	listeners.add(onChange);
	return () => {
		listeners.delete(onChange);
	};
}

export interface HomeShaderUniforms {
	speed: number;
	phase: number;
	count: number;
	height: number;
	focus: number;
	position: number;
	blend: number;
	facets: number;
	grain: number;
}

export interface SkyShaderUniforms {
	speed: number;
	scale: number;
	distortion: number;
	swirl: number;
	direction: number;
	grain: number;
}

/** 对齐 design/cst-pilot-web.pen 主页背景（B9yQGT / V7NF4）与桌面端 presets.rs。 */
export const HOME_SHADER_UNIFORMS: HomeShaderUniforms = {
	speed: 30,
	phase: 20.75,
	count: 11,
	height: 60,
	focus: 50,
	position: 50,
	blend: 65,
	facets: 35,
	grain: 0.015,
};

/** 对齐 design/cst-pilot-web.pen 00F（rtOFG / Q3yEX8）与 storybook.html。 */
export function skyUniformsForTheme(theme: ResolvedTheme): SkyShaderUniforms {
	if (theme === "dark") {
		return { speed: 14, scale: 42, distortion: 46, swirl: 28, direction: 0, grain: 8 };
	}
	return { speed: 22, scale: 72, distortion: 48, swirl: 38, direction: 0, grain: 4 };
}

export function normalizeBackgroundKind(kind: BackgroundKind): NormalizedBackgroundKind {
	return kind === "home" ? "home" : "sky";
}

/** 仅在可见、聚焦、未暂停、未处于硬件采样且未开启减少动态时推进时间轴；其余状态 0 FPS 停帧。 */
export function shouldAnimateShader(inputs: ShaderGateInputs): boolean {
	return (
		!inputs.paused &&
		!inputs.documentHidden &&
		inputs.windowFocused &&
		!inputs.reducedMotion &&
		!inputs.hardwareSampling
	);
}
