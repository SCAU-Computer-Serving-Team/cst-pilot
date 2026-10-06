import { useEffect, useLayoutEffect, useRef } from "react";
import homeShader from "../../../design/asset/blue-hour.glsl?raw";
import airShader from "../../../design/asset/blue-hour-air.glsl?raw";
import airDarkPalette from "../../../design/asset/blue-hour-air-dark-palette.png?url";
import darkPalette from "../../../design/asset/blue-hour-dark-palette.png?url";
import lightPalette from "../../../design/asset/blue-hour-palette.png?url";
import { FrameClock } from "./frame-clock";
import { regionFadeMask } from "./region-fade";

function exitProgress() {
	const value = getComputedStyle(document.documentElement, "::view-transition-new(home-regions)").getPropertyValue(
		"--home-exit-progress",
	);
	return Math.max(0, Math.min(1, Number.parseFloat(value) || 0));
}

const vertexShader = `attribute vec2 a_position;
void main() { gl_Position = vec4(a_position, 0.0, 1.0); }`;

type BackgroundKind = "home" | "air";

function compile(gl: WebGLRenderingContext, type: number, source: string): WebGLShader | null {
	const shader = gl.createShader(type);
	if (!shader) return null;
	gl.shaderSource(shader, source.replace(/^#version 100\s*/, ""));
	gl.compileShader(shader);
	if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
	console.error("Blue hour shader 编译失败：", gl.getShaderInfoLog(shader));
	gl.deleteShader(shader);
	return null;
}

function prefersDark(): boolean {
	const theme = document.documentElement.dataset.theme;
	return theme === "dark" || (theme !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
}

export function BlueHour({
	kind,
	paused = false,
	fading = false,
}: {
	kind: BackgroundKind;
	paused?: boolean;
	fading?: boolean;
}) {
	const ref = useRef<HTMLCanvasElement>(null);
	const fade = useRef(fading);
	const restart = useRef<(() => void) | null>(null);
	useLayoutEffect(() => {
		fade.current = fading;
		restart.current?.();
	}, [fading]);

	useEffect(() => {
		const canvas = ref.current;
		if (!canvas) return;
		const gl = canvas.getContext("webgl", { alpha: true, antialias: false, premultipliedAlpha: false });
		if (!gl) {
			const clock = new FrameClock(performance.now());
			const fallbackMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
			let frame = 0;
			let origin: number | undefined;
			const draw = (now: number) => {
				frame = 0;
				if (!fade.current || document.hidden) return;
				const travel = (20.75 + clock.step(now, paused || fallbackMotion.matches) * 0.36) * 0.48;
				origin ??= Math.ceil(travel);
				canvas.parentElement!.style.maskImage = regionFadeMask(travel, origin, exitProgress(), canvas.clientWidth);
				frame = requestAnimationFrame(draw);
			};
			restart.current = () => {
				if (!fade.current) {
					origin = undefined;
					canvas.parentElement!.style.maskImage = "";
				} else if (!frame) frame = requestAnimationFrame(draw);
			};
			const resume = () => {
				clock.resume(performance.now());
				restart.current?.();
			};
			document.addEventListener("visibilitychange", resume);
			fallbackMotion.addEventListener("change", resume);
			restart.current();
			return () => {
				cancelAnimationFrame(frame);
				document.removeEventListener("visibilitychange", resume);
				fallbackMotion.removeEventListener("change", resume);
				restart.current = null;
			};
		}
		const vertex = compile(gl, gl.VERTEX_SHADER, vertexShader);
		const fragment = compile(gl, gl.FRAGMENT_SHADER, kind === "home" ? homeShader : airShader);
		if (!vertex || !fragment) {
			if (vertex) gl.deleteShader(vertex);
			if (fragment) gl.deleteShader(fragment);
			return;
		}
		const program = gl.createProgram();
		if (!program) {
			gl.deleteShader(vertex);
			gl.deleteShader(fragment);
			return;
		}
		gl.attachShader(program, vertex);
		gl.attachShader(program, fragment);
		gl.linkProgram(program);
		if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
			console.error("Blue hour shader 链接失败：", gl.getProgramInfoLog(program));
			gl.deleteProgram(program);
			gl.deleteShader(vertex);
			gl.deleteShader(fragment);
			return;
		}
		// biome-ignore lint/correctness/useHookAtTopLevel: gl.useProgram 是 WebGL 调用，不是 React Hook
		gl.useProgram(program);
		const buffer = gl.createBuffer();
		if (!buffer) {
			gl.deleteProgram(program);
			gl.deleteShader(vertex);
			gl.deleteShader(fragment);
			return;
		}
		gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
		gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
		const position = gl.getAttribLocation(program, "a_position");
		gl.enableVertexAttribArray(position);
		gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
		const palette = gl.createTexture();
		if (!palette) {
			gl.deleteBuffer(buffer);
			gl.deleteProgram(program);
			gl.deleteShader(vertex);
			gl.deleteShader(fragment);
			return;
		}
		gl.activeTexture(gl.TEXTURE0);
		gl.bindTexture(gl.TEXTURE_2D, palette);
		gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
		gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
		gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
		gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
		const locations = new Map<string, WebGLUniformLocation | null>();
		const location = (key: string) => {
			if (!locations.has(key)) locations.set(key, gl.getUniformLocation(program, key));
			return locations.get(key) ?? null;
		};
		gl.uniform1i(location("u_palette"), 0);
		const set = (key: string, value: number) => gl.uniform1f(location(key), value);
		let dark = prefersDark();
		set("u_speed", 30);
		set("u_phase", kind === "home" ? 20.75 : 0);
		set("u_grain", kind === "home" ? 0.015 : dark ? 0.015 : 0.003);
		if (kind === "home") {
			set("u_count", 11);
			set("u_height", dark ? 70 : 60);
			set("u_focus", dark ? 63 : 50);
			set("u_position", dark ? 64 : 50);
			set("u_blend", dark ? 81 : 65);
			set("u_facets", 35);
		} else {
			set("u_drift", 1);
			set("u_softness", 24);
		}
		const surface = canvas;
		const context = gl;
		const image = new Image();
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		let frame = 0;
		const clock = new FrameClock(performance.now());
		let cssWidth = surface.clientWidth;
		let cssHeight = surface.clientHeight;
		let ready = false;
		let live = true;
		let painted = false;
		let exitOrigin: number | undefined;
		restart.current = () => {
			if (!fade.current) {
				exitOrigin = undefined;
				surface.parentElement!.style.maskImage = "";
			}
			schedule();
		};

		function schedule() {
			if (!live || (!ready && !fade.current) || (document.hidden && painted) || frame) return;
			frame = requestAnimationFrame((now) => {
				frame = 0;
				draw(now);
			});
		}

		function draw(now: number) {
			if (!live || (document.hidden && painted)) return;
			if (!ready) {
				if (fade.current) {
					const travel = (20.75 + clock.step(now, motion.matches || paused) * 0.36) * 0.48;
					exitOrigin ??= Math.ceil(travel);
					surface.parentElement!.style.maskImage = regionFadeMask(travel, exitOrigin, exitProgress(), cssWidth);
					schedule();
				}
				return;
			}
			if (surface.parentElement!.style.maskImage) surface.parentElement!.style.maskImage = "";
			// Shader 为渐变背景：限制总像素，避免 4K × 高 DPR 在高刷屏逐帧填充过多像素。
			const ratio = Math.min(devicePixelRatio, 2, Math.sqrt(2_500_000 / Math.max(1, cssWidth * cssHeight)));
			const width = Math.max(1, Math.round(cssWidth * ratio));
			const height = Math.max(1, Math.round(cssHeight * ratio));
			if (surface.width !== width || surface.height !== height) {
				surface.width = width;
				surface.height = height;
				context.viewport(0, 0, width, height);
			}
			context.uniform2f(location("u_resolution"), width, height);
			const elapsed = clock.step(now, motion.matches || paused);
			set("u_time", elapsed);
			if (kind === "home" && fade.current) {
				exitOrigin ??= Math.ceil((20.75 + elapsed * 0.36) * 0.48);
				set("u_exit_origin", exitOrigin);
				set("u_exit_progress", exitProgress());
			} else {
				exitOrigin = undefined;
				set("u_exit_progress", 0);
			}
			set("u_css_width", cssWidth);
			context.drawArrays(context.TRIANGLE_STRIP, 0, 4);
			if (!painted) {
				surface.style.opacity = "1";
				surface.dataset.renderer = "webgl";
			}
			painted = true;
			if (!motion.matches && !paused && !document.hidden) schedule();
		}

		image.onload = () => {
			if (!live) return;
			gl.bindTexture(gl.TEXTURE_2D, palette);
			gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, gl.RGB, gl.UNSIGNED_BYTE, image);
			ready = true;
			clock.resume(performance.now());
			schedule();
		};
		image.onerror = () => console.error("Blue hour 色表加载失败：", image.src);
		const paletteUrl = () =>
			kind === "air" ? (dark ? airDarkPalette : lightPalette) : dark ? darkPalette : lightPalette;
		image.src = paletteUrl();
		const colorScheme = window.matchMedia("(prefers-color-scheme: dark)");
		function syncTheme() {
			const nextDark = prefersDark();
			if (nextDark === dark) return;
			dark = nextDark;
			set("u_grain", kind === "home" ? 0.015 : dark ? 0.015 : 0.003);
			if (kind === "home") {
				set("u_height", dark ? 70 : 60);
				set("u_focus", dark ? 63 : 50);
				set("u_position", dark ? 64 : 50);
				set("u_blend", dark ? 81 : 65);
			}
			ready = false;
			painted = false;
			cancelAnimationFrame(frame);
			frame = 0;
			image.src = paletteUrl();
		}
		const themeObserver = new MutationObserver(syncTheme);
		themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
		colorScheme.addEventListener("change", syncTheme);
		const resize = new ResizeObserver((entries) => {
			const size = entries[0]?.contentRect;
			if (size) {
				cssWidth = size.width;
				cssHeight = size.height;
			}
			schedule();
		});
		resize.observe(canvas);
		const resume = () => {
			clock.resume(performance.now());
			schedule();
		};
		document.addEventListener("visibilitychange", resume);
		motion.addEventListener("change", resume);
		return () => {
			live = false;
			restart.current = null;
			delete surface.dataset.renderer;
			cancelAnimationFrame(frame);
			resize.disconnect();
			themeObserver.disconnect();
			colorScheme.removeEventListener("change", syncTheme);
			document.removeEventListener("visibilitychange", resume);
			motion.removeEventListener("change", resume);
			image.onload = null;
			image.onerror = null;
			gl.deleteTexture(palette);
			gl.deleteBuffer(buffer);
			gl.deleteProgram(program);
			gl.deleteShader(vertex);
			gl.deleteShader(fragment);
		};
	}, [kind, paused]);

	return <canvas className={`blue-hour blue-hour--${kind}`} ref={ref} />;
}
