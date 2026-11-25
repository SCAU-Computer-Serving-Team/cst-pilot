import { useEffect, useRef } from "react";
import homeShader from "../../../../../design/asset/blue-hour.glsl?raw";
import darkPalette from "../../../../../design/asset/blue-hour-dark-palette.png?url";
import lightPalette from "../../../../../design/asset/blue-hour-palette.png?url";
import blueSkyShader from "../../../../../design/asset/blue-sky.glsl?raw";
import nightSkyShader from "../../../../../design/asset/night-sky.glsl?raw";
import { FrameClock } from "./frame-clock";
import {
	type BackgroundKind,
	HOME_SHADER_UNIFORMS,
	isHardwareSamplingActive,
	isSoftwareWebGlRenderer,
	normalizeBackgroundKind,
	shouldAnimateShader,
	skyUniformsForTheme,
	subscribeHardwareSampling,
} from "./shader-gate";
import { currentResolvedTheme, type ResolvedTheme } from "./theme";

const vertexShaderSource = `attribute vec2 a_position;
void main() { gl_Position = vec4(a_position, 0.0, 1.0); }`;

interface CompiledProgram {
	program: WebGLProgram;
	vertex: WebGLShader;
	fragment: WebGLShader;
	position: number;
	locations: Map<string, WebGLUniformLocation | null>;
}

function compileShader(gl: WebGLRenderingContext, type: number, source: string): WebGLShader | null {
	const shader = gl.createShader(type);
	if (!shader) return null;
	gl.shaderSource(shader, source.replace(/^#version 100\s*/, ""));
	gl.compileShader(shader);
	if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
	console.error("背景着色器编译失败：", gl.getShaderInfoLog(shader));
	gl.deleteShader(shader);
	return null;
}

function createProgram(gl: WebGLRenderingContext, fragmentSource: string): CompiledProgram | null {
	const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexShaderSource);
	const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
	if (!vertex || !fragment) {
		if (vertex) gl.deleteShader(vertex);
		if (fragment) gl.deleteShader(fragment);
		return null;
	}
	const program = gl.createProgram();
	if (!program) {
		gl.deleteShader(vertex);
		gl.deleteShader(fragment);
		return null;
	}
	gl.attachShader(program, vertex);
	gl.attachShader(program, fragment);
	gl.linkProgram(program);
	if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
		console.error("背景着色器链接失败：", gl.getProgramInfoLog(program));
		gl.deleteProgram(program);
		gl.deleteShader(vertex);
		gl.deleteShader(fragment);
		return null;
	}
	const position = gl.getAttribLocation(program, "a_position");
	return { program, vertex, fragment, position, locations: new Map() };
}

function destroyProgram(gl: WebGLRenderingContext, compiled: CompiledProgram | null) {
	if (!compiled) return;
	gl.deleteProgram(compiled.program);
	gl.deleteShader(compiled.vertex);
	gl.deleteShader(compiled.fragment);
}

function uniformLocation(
	gl: WebGLRenderingContext,
	compiled: CompiledProgram,
	name: string,
): WebGLUniformLocation | null {
	if (!compiled.locations.has(name)) {
		compiled.locations.set(name, gl.getUniformLocation(compiled.program, name));
	}
	return compiled.locations.get(name) ?? null;
}

export function BlueHour({ kind, paused = false }: { kind: BackgroundKind; paused?: boolean }) {
	const normalizedKind = normalizeBackgroundKind(kind);
	const ref = useRef<HTMLCanvasElement>(null);
	const pausedRef = useRef(paused);
	const syncActivityRef = useRef<((focusedOverride?: boolean) => void) | null>(null);

	useEffect(() => {
		pausedRef.current = paused;
		syncActivityRef.current?.();
	}, [paused]);

	useEffect(() => {
		const canvas = ref.current;
		if (!canvas) return;
		const gl = canvas.getContext("webgl", {
			alpha: true,
			antialias: false,
			depth: false,
			stencil: false,
			failIfMajorPerformanceCaveat: true,
		});
		const isSoftwareGl = (() => {
			if (!gl) return false;
			try {
				const renderer = gl.getParameter(gl.RENDERER);
				const vendor = gl.getParameter(gl.VENDOR);
				if (isSoftwareWebGlRenderer(renderer, vendor)) return true;
				const debugInfo = gl.getExtension("WEBGL_debug_renderer_info");
				if (debugInfo) {
					const unmaskedRenderer = gl.getParameter(debugInfo.UNMASKED_RENDERER_WEBGL);
					const unmaskedVendor = gl.getParameter(debugInfo.UNMASKED_VENDOR_WEBGL);
					if (isSoftwareWebGlRenderer(unmaskedRenderer, unmaskedVendor)) return true;
				}
			} catch {
				/* 驱动受限无法读取渲染器信息时按正常能力路径处理。 */
			}
			return false;
		})();
		if (!gl || isSoftwareGl) {
			if (gl && isSoftwareGl) {
				try {
					gl.getExtension("WEBGL_lose_context")?.loseContext();
				} catch {
					/* 忽略释放软件上下文失败。 */
				}
			}
			canvas.dataset.shaderState = "fallback";
			return () => {
				delete canvas.dataset.shaderState;
			};
		}

		const surface = canvas;
		const context = gl;
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const colorScheme = window.matchMedia("(prefers-color-scheme: dark)");
		const clock = new FrameClock(performance.now());

		let buffer: WebGLBuffer | null = null;
		let palette: WebGLTexture | null = null;
		let homeProgram: CompiledProgram | null = null;
		let daySkyProgram: CompiledProgram | null = null;
		let nightSkyProgram: CompiledProgram | null = null;
		let activeImage: HTMLImageElement | null = null;
		let paletteLoadToken = 0;
		let frame = 0;
		let cssWidth = surface.clientWidth;
		let cssHeight = surface.clientHeight;
		let resolvedTheme: ResolvedTheme = currentResolvedTheme();
		let windowFocused = document.hasFocus();
		let hardwareSampling = isHardwareSamplingActive();
		let ready = false;
		let painted = false;
		let viewportDirty = true;
		let appliedProgram: WebGLProgram | null = null;
		let live = true;
		let contextLost = false;

		function clearPaletteImage() {
			paletteLoadToken++;
			if (activeImage) {
				activeImage.onload = null;
				activeImage.onerror = null;
				activeImage = null;
			}
		}

		function clearGpuHandles() {
			palette = null;
			buffer = null;
			homeProgram = null;
			daySkyProgram = null;
			nightSkyProgram = null;
			appliedProgram = null;
			viewportDirty = true;
		}

		function releaseGpuResources() {
			if (palette) context.deleteTexture(palette);
			if (buffer) context.deleteBuffer(buffer);
			destroyProgram(context, homeProgram);
			destroyProgram(context, daySkyProgram);
			destroyProgram(context, nightSkyProgram);
			clearGpuHandles();
		}

		function setFallbackState() {
			ready = false;
			painted = false;
			surface.style.opacity = "0";
			delete surface.dataset.renderer;
			surface.dataset.shaderState = "fallback";
		}

		function activeVariantName(): string {
			if (normalizedKind === "home") return resolvedTheme === "dark" ? "blue-hour-dark" : "blue-hour-light";
			return resolvedTheme === "dark" ? "night-sky" : "blue-sky";
		}

		function activeProgram(): CompiledProgram | null {
			if (normalizedKind === "home") return homeProgram;
			return resolvedTheme === "dark" ? nightSkyProgram : daySkyProgram;
		}

		function canAnimate(): boolean {
			return shouldAnimateShader({
				paused: pausedRef.current,
				documentHidden: document.hidden,
				windowFocused,
				reducedMotion: motion.matches,
				hardwareSampling,
			});
		}

		function uploadStaticUniforms(compiled: CompiledProgram) {
			// biome-ignore lint/correctness/useHookAtTopLevel: gl.useProgram 是 WebGL 调用，不是 React Hook
			context.useProgram(compiled.program);
			const set1f = (key: string, value: number) =>
				context.uniform1f(uniformLocation(context, compiled, key), value);
			if (normalizedKind === "home") {
				context.uniform1i(uniformLocation(context, compiled, "u_palette"), 0);
				set1f("u_speed", HOME_SHADER_UNIFORMS.speed);
				set1f("u_phase", HOME_SHADER_UNIFORMS.phase);
				set1f("u_count", HOME_SHADER_UNIFORMS.count);
				set1f("u_height", HOME_SHADER_UNIFORMS.height);
				set1f("u_focus", HOME_SHADER_UNIFORMS.focus);
				set1f("u_position", HOME_SHADER_UNIFORMS.position);
				set1f("u_blend", HOME_SHADER_UNIFORMS.blend);
				set1f("u_facets", HOME_SHADER_UNIFORMS.facets);
				set1f("u_grain", HOME_SHADER_UNIFORMS.grain);
			} else {
				const u = skyUniformsForTheme(resolvedTheme);
				set1f("u_speed", u.speed);
				set1f("u_scale", u.scale);
				set1f("u_distortion", u.distortion);
				set1f("u_swirl", u.swirl);
				set1f("u_direction", u.direction);
				set1f("u_grain", u.grain);
			}
			appliedProgram = compiled.program;
		}

		function loadPalette(url: string) {
			clearPaletteImage();
			const token = paletteLoadToken;
			const img = new Image();
			activeImage = img;
			ready = false;
			img.onload = () => {
				if (!live || contextLost || !palette || token !== paletteLoadToken) return;
				context.activeTexture(context.TEXTURE0);
				context.bindTexture(context.TEXTURE_2D, palette);
				context.texImage2D(context.TEXTURE_2D, 0, context.RGB, context.RGB, context.UNSIGNED_BYTE, img);
				ready = true;
				painted = false;
				clock.resume(performance.now());
				paintImmediateOrSchedule();
			};
			img.onerror = () => {
				if (!live || contextLost || token !== paletteLoadToken) return;
				console.error("Blue hour 色表加载失败：", url);
				setFallbackState();
			};
			img.src = url;
		}

		function initGpuResources(): boolean {
			if (!contextLost) releaseGpuResources();
			else clearGpuHandles();

			if (normalizedKind === "home") {
				homeProgram = createProgram(context, homeShader);
				if (!homeProgram) return false;
			} else {
				daySkyProgram = createProgram(context, blueSkyShader);
				nightSkyProgram = createProgram(context, nightSkyShader);
				if (!daySkyProgram || !nightSkyProgram) {
					releaseGpuResources();
					return false;
				}
			}

			buffer = context.createBuffer();
			if (!buffer) {
				releaseGpuResources();
				return false;
			}
			context.bindBuffer(context.ARRAY_BUFFER, buffer);
			context.bufferData(context.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), context.STATIC_DRAW);

			if (normalizedKind === "home") {
				palette = context.createTexture();
				if (!palette) {
					releaseGpuResources();
					return false;
				}
				context.activeTexture(context.TEXTURE0);
				context.bindTexture(context.TEXTURE_2D, palette);
				context.texParameteri(context.TEXTURE_2D, context.TEXTURE_MIN_FILTER, context.LINEAR);
				context.texParameteri(context.TEXTURE_2D, context.TEXTURE_MAG_FILTER, context.LINEAR);
				context.texParameteri(context.TEXTURE_2D, context.TEXTURE_WRAP_S, context.CLAMP_TO_EDGE);
				context.texParameteri(context.TEXTURE_2D, context.TEXTURE_WRAP_T, context.CLAMP_TO_EDGE);
				loadPalette(resolvedTheme === "dark" ? darkPalette : lightPalette);
			} else {
				ready = true;
			}
			return true;
		}

		function schedule() {
			if (!live || contextLost || !ready || (document.hidden && painted) || frame) return;
			frame = requestAnimationFrame((now) => {
				frame = 0;
				draw(now);
			});
		}

		function paintImmediateOrSchedule() {
			if (!live || contextLost || !ready) return;
			if (!document.hidden && cssWidth > 0 && cssHeight > 0) {
				cancelAnimationFrame(frame);
				frame = 0;
				draw(performance.now());
			} else {
				schedule();
			}
		}

		function draw(now: number) {
			if (!live || contextLost || !ready || (document.hidden && painted)) return;
			if (cssWidth <= 0 || cssHeight <= 0) return;
			const compiled = activeProgram();
			if (!compiled || !buffer) return;

			// 限制总像素上限，避免 4K × 高 DPR 在高刷屏逐帧填充过多像素。
			const dpr = Math.max(1, window.devicePixelRatio || 1);
			const ratio = Math.min(dpr, 2, Math.sqrt(2_500_000 / Math.max(1, cssWidth * cssHeight)));
			const width = Math.max(1, Math.round(cssWidth * ratio));
			const height = Math.max(1, Math.round(cssHeight * ratio));
			if (surface.width !== width || surface.height !== height || viewportDirty) {
				surface.width = width;
				surface.height = height;
				context.viewport(0, 0, width, height);
				viewportDirty = false;
			}

			if (appliedProgram !== compiled.program) {
				uploadStaticUniforms(compiled);
			} else {
				// biome-ignore lint/correctness/useHookAtTopLevel: gl.useProgram 是 WebGL 调用，不是 React Hook
				context.useProgram(compiled.program);
			}
			context.bindBuffer(context.ARRAY_BUFFER, buffer);
			context.enableVertexAttribArray(compiled.position);
			context.vertexAttribPointer(compiled.position, 2, context.FLOAT, false, 0, 0);
			context.uniform2f(uniformLocation(context, compiled, "u_resolution"), width, height);

			const animate = canAnimate();
			const elapsed = motion.matches ? 0 : clock.step(now, !animate);
			context.uniform1f(uniformLocation(context, compiled, "u_time"), elapsed);
			context.drawArrays(context.TRIANGLE_STRIP, 0, 4);

			if (!painted) {
				surface.style.opacity = "1";
				surface.dataset.renderer = "webgl";
			}
			surface.dataset.shaderVariant = activeVariantName();
			surface.dataset.shaderState = animate ? "running" : "frozen";
			painted = true;

			if (animate) schedule();
		}

		function syncTheme() {
			const nextTheme = currentResolvedTheme();
			if (nextTheme === resolvedTheme) return;
			resolvedTheme = nextTheme;
			appliedProgram = null;
			if (contextLost) return;
			cancelAnimationFrame(frame);
			frame = 0;
			if (normalizedKind === "home") {
				loadPalette(resolvedTheme === "dark" ? darkPalette : lightPalette);
			} else {
				painted = false;
				clock.resume(performance.now());
				paintImmediateOrSchedule();
			}
		}

		function syncActivity(focusedOverride?: boolean) {
			windowFocused = focusedOverride ?? document.hasFocus();
			if (contextLost || !ready) return;
			cancelAnimationFrame(frame);
			frame = 0;
			if (canAnimate()) {
				clock.resume(performance.now());
				schedule();
			} else {
				if (!painted || motion.matches) {
					paintImmediateOrSchedule();
				} else {
					surface.dataset.shaderState = "frozen";
				}
			}
		}

		syncActivityRef.current = syncActivity;

		function onFocus() {
			syncActivity(true);
		}

		function onBlur() {
			syncActivity(false);
		}

		function onVisibilityChange() {
			syncActivity();
		}

		function onMotionChange() {
			syncActivity();
		}

		function onContextLost(event: Event) {
			event.preventDefault();
			contextLost = true;
			clearPaletteImage();
			clearGpuHandles();
			cancelAnimationFrame(frame);
			frame = 0;
			setFallbackState();
		}

		function onContextRestored() {
			contextLost = false;
			if (!initGpuResources()) {
				setFallbackState();
				return;
			}
			clock.resume(performance.now());
			if (ready) paintImmediateOrSchedule();
		}

		if (!initGpuResources()) {
			setFallbackState();
			return () => {
				live = false;
				syncActivityRef.current = null;
				surface.style.opacity = "";
				delete surface.dataset.renderer;
				delete surface.dataset.shaderVariant;
				delete surface.dataset.shaderState;
			};
		}

		const themeObserver = new MutationObserver(syncTheme);
		themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
		colorScheme.addEventListener("change", syncTheme);

		const resize = new ResizeObserver((entries) => {
			const size = entries[0]?.contentRect;
			if (size && (size.width !== cssWidth || size.height !== cssHeight)) {
				cssWidth = size.width;
				cssHeight = size.height;
				painted = false;
			}
			paintImmediateOrSchedule();
		});
		resize.observe(surface);

		document.addEventListener("visibilitychange", onVisibilityChange);
		motion.addEventListener("change", onMotionChange);
		window.addEventListener("focus", onFocus);
		window.addEventListener("blur", onBlur);
		surface.addEventListener("webglcontextlost", onContextLost);
		surface.addEventListener("webglcontextrestored", onContextRestored);
		const unsubscribeHardwareSampling = subscribeHardwareSampling((active) => {
			hardwareSampling = active;
			syncActivity();
		});

		if (ready) paintImmediateOrSchedule();

		return () => {
			live = false;
			syncActivityRef.current = null;
			clearPaletteImage();
			surface.style.opacity = "";
			delete surface.dataset.renderer;
			delete surface.dataset.shaderVariant;
			delete surface.dataset.shaderState;
			cancelAnimationFrame(frame);
			resize.disconnect();
			themeObserver.disconnect();
			colorScheme.removeEventListener("change", syncTheme);
			document.removeEventListener("visibilitychange", onVisibilityChange);
			motion.removeEventListener("change", onMotionChange);
			window.removeEventListener("focus", onFocus);
			window.removeEventListener("blur", onBlur);
			surface.removeEventListener("webglcontextlost", onContextLost);
			surface.removeEventListener("webglcontextrestored", onContextRestored);
			unsubscribeHardwareSampling();
			if (!contextLost) releaseGpuResources();
			else clearGpuHandles();
		};
	}, [normalizedKind]);

	return <canvas className={`blue-hour blue-hour--${normalizedKind}`} ref={ref} />;
}
