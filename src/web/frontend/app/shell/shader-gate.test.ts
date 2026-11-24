import assert from "node:assert/strict";
import { test } from "node:test";
import {
	HOME_SHADER_UNIFORMS,
	isSoftwareWebGlRenderer,
	normalizeBackgroundKind,
	shouldAnimateShader,
	skyUniformsForTheme,
} from "./shader-gate.ts";
import { resolveTheme } from "./theme.ts";

test("主题解析覆盖 light、dark 与 system 跟随系统", () => {
	assert.equal(resolveTheme("light", true), "light");
	assert.equal(resolveTheme("dark", false), "dark");
	assert.equal(resolveTheme("system", false), "light");
	assert.equal(resolveTheme("system", true), "dark");
});

test("背景类型兼容旧 air 别名并归一化为 sky", () => {
	assert.equal(normalizeBackgroundKind("home"), "home");
	assert.equal(normalizeBackgroundKind("sky"), "sky");
	assert.equal(normalizeBackgroundKind("air"), "sky");
});

test("着色器帧门控仅在可见、聚焦、未暂停且非减少动态时运行", () => {
	assert.equal(
		shouldAnimateShader({ paused: false, documentHidden: false, windowFocused: true, reducedMotion: false }),
		true,
	);
	assert.equal(
		shouldAnimateShader({ paused: true, documentHidden: false, windowFocused: true, reducedMotion: false }),
		false,
	);
	assert.equal(
		shouldAnimateShader({ paused: false, documentHidden: true, windowFocused: true, reducedMotion: false }),
		false,
	);
	assert.equal(
		shouldAnimateShader({ paused: false, documentHidden: false, windowFocused: false, reducedMotion: false }),
		false,
	);
	assert.equal(
		shouldAnimateShader({ paused: false, documentHidden: false, windowFocused: true, reducedMotion: true }),
		false,
	);
	assert.equal(
		shouldAnimateShader({
			paused: false,
			documentHidden: false,
			windowFocused: true,
			reducedMotion: false,
			hardwareSampling: true,
		}),
		false,
	);
});

test("主页与登录页着色器参数严格对齐 cst-pilot-web.pen 与 storybook.html", () => {
	assert.deepEqual(HOME_SHADER_UNIFORMS, {
		speed: 30,
		phase: 20.75,
		count: 11,
		height: 60,
		focus: 50,
		position: 50,
		blend: 65,
		facets: 35,
		grain: 0.015,
	});
	assert.deepEqual(skyUniformsForTheme("light"), {
		speed: 22,
		scale: 72,
		distortion: 48,
		swirl: 38,
		direction: 0,
		grain: 4,
	});
	assert.deepEqual(skyUniformsForTheme("dark"), {
		speed: 14,
		scale: 42,
		distortion: 46,
		swirl: 28,
		direction: 0,
		grain: 8,
	});
});

test("软件 WebGL 渲染器检测识别 SwiftShader / llvmpipe / WARP 且放行硬件 GPU", () => {
	assert.equal(
		isSoftwareWebGlRenderer("ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero)))", "Google Inc."),
		true,
	);
	assert.equal(isSoftwareWebGlRenderer("SwiftShader Device (Subzero)"), true);
	assert.equal(isSoftwareWebGlRenderer("llvmpipe (LLVM 17.0.6, 256 bits)", "Mesa"), true);
	assert.equal(isSoftwareWebGlRenderer("Microsoft Basic Render Driver", "Microsoft"), true);
	assert.equal(
		isSoftwareWebGlRenderer("ANGLE (NVIDIA, NVIDIA GeForce RTX 4070 Direct3D11 vs_5_0 ps_5_0)", "Google Inc."),
		false,
	);
	assert.equal(isSoftwareWebGlRenderer("Apple M3 Pro", "Apple"), false);
	assert.equal(isSoftwareWebGlRenderer(null, undefined), false);
});
