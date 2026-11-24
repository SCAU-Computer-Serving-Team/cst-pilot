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
const HARDWARE_SAMPLING_EVENT = "cst-pilot:hardware-sampling";
let hardwareSamplingActive = false;

export function isHardwareSamplingActive(): boolean {
	return hardwareSamplingActive;
}

const SOFTWARE_RENDERER_PATTERN =
	/swiftshader|llvmpipe|softpipe|microsoft basic render|software renderer|subzero|mesa offscreen|angle.*warp/i;

/** 识别已知软件渲染器（如 Chrome SwiftShader / Mesa llvmpipe / Windows WARP），强制回退静态底图。 */
export function isSoftwareWebGlRenderer(renderer: unknown, vendor?: unknown): boolean {
	const text = `${typeof renderer === "string" ? renderer : ""} ${typeof vendor === "string" ? vendor : ""}`.trim();
	return text.length > 0 && SOFTWARE_RENDERER_PATTERN.test(text);
}

/** 更新硬件采样停帧标志并跨标签页同步，供 sys(gpu)/sys(sensor) 采样期间强制 0 FPS 停帧。 */
export function setHardwareSamplingActive(active: boolean): void {
	if (hardwareSamplingActive === active) return;
	hardwareSamplingActive = active;
	if (typeof window !== "undefined") {
		window.dispatchEvent(new CustomEvent<boolean>(HARDWARE_SAMPLING_EVENT, { detail: active }));
	}
	if (typeof BroadcastChannel !== "undefined") {
		try {
			const channel = new BroadcastChannel(HARDWARE_SAMPLING_CHANNEL);
			channel.postMessage({ active });
			channel.close();
		} catch {
			/* 隐私模式或受限环境下忽略跨标签页广播失败。 */
		}
	}
}

export function subscribeHardwareSampling(onChange: (active: boolean) => void): () => void {
	if (typeof window === "undefined") return () => {};
	const onLocal = (event: Event) => {
		if (event instanceof CustomEvent && typeof event.detail === "boolean") {
			hardwareSamplingActive = event.detail;
			onChange(hardwareSamplingActive);
		}
	};
	window.addEventListener(HARDWARE_SAMPLING_EVENT, onLocal);
	let channel: BroadcastChannel | null = null;
	if (typeof BroadcastChannel !== "undefined") {
		try {
			channel = new BroadcastChannel(HARDWARE_SAMPLING_CHANNEL);
			channel.onmessage = (event: MessageEvent<{ active?: unknown }>) => {
				if (typeof event.data?.active === "boolean") {
					hardwareSamplingActive = event.data.active;
					onChange(hardwareSamplingActive);
				}
			};
		} catch {
			channel = null;
		}
	}
	return () => {
		window.removeEventListener(HARDWARE_SAMPLING_EVENT, onLocal);
		channel?.close();
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
