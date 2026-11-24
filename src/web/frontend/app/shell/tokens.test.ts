import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(appDir, "../../../..");

interface PenVariable {
	type: string;
	value: string | number | Array<{ value: string; theme?: { mode?: string } }>;
}

function normalizeLf(text: string): string {
	return text.replace(/\r\n/g, "\n");
}

function parseCssBlock(css: string, anchor: string): Map<string, string> {
	const start = css.indexOf(anchor);
	assert.notEqual(start, -1, `tokens.css 缺少规则块：${anchor}`);
	const openBrace = css.indexOf("{", start);
	assert.notEqual(openBrace, -1, `规则块未闭合：${anchor}`);
	let depth = 0;
	let end = -1;
	for (let i = openBrace; i < css.length; i++) {
		if (css[i] === "{") depth++;
		else if (css[i] === "}") {
			depth--;
			if (depth === 0) {
				end = i;
				break;
			}
		}
	}
	assert.notEqual(end, -1, `规则块括号不匹配：${anchor}`);
	const body = css.slice(openBrace + 1, end);
	const map = new Map<string, string>();
	for (const match of body.matchAll(/--([a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
		map.set(match[1], match[2].trim());
	}
	return map;
}

function listFilesByExt(dir: string, exts: string[]): string[] {
	const files: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const full = join(dir, entry.name);
		if (entry.isDirectory()) files.push(...listFilesByExt(full, exts));
		else if (exts.some((ext) => entry.name.endsWith(ext))) files.push(full);
	}
	return files;
}

test("tokens.css 完整且精确同步 design/cst-pilot-web.pen 全部 50 项静态基础令牌", () => {
	const pen = JSON.parse(readFileSync(resolve(repoRoot, "design/cst-pilot-web.pen"), "utf8")) as {
		variables: Record<string, PenVariable>;
	};
	const css = normalizeLf(readFileSync(resolve(appDir, "tokens.css"), "utf8"));
	const rootVars = parseCssBlock(css, ":root {");
	const lightVars = parseCssBlock(css, ':root,\n:root[data-theme="light"]');
	const darkVars = parseCssBlock(css, ':root[data-theme="dark"]');
	const systemDarkBlockStart = css.indexOf("@media (prefers-color-scheme: dark)");
	assert.notEqual(systemDarkBlockStart, -1, "tokens.css 缺少 system 深色媒体查询");
	const systemDarkVars = parseCssBlock(css.slice(systemDarkBlockStart), ':root[data-theme="system"]');

	const entries = Object.entries(pen.variables);
	assert.equal(entries.length, 50, "cst-pilot-web.pen 静态变量数应为 50 项");

	for (const [name, spec] of entries) {
		if (spec.type === "number") {
			const expected = `${spec.value}px`;
			assert.equal(rootVars.get(name), expected, `数值令牌 --${name} 未对齐`);
		} else if (typeof spec.value === "string") {
			assert.equal(lightVars.get(name)?.toUpperCase(), spec.value.toUpperCase(), `固定色令牌 --${name} 浅色未对齐`);
			assert.equal(darkVars.get(name)?.toUpperCase(), spec.value.toUpperCase(), `固定色令牌 --${name} 深色未对齐`);
			assert.equal(
				systemDarkVars.get(name)?.toUpperCase(),
				spec.value.toUpperCase(),
				`固定色令牌 --${name} system 深色未对齐`,
			);
		} else if (Array.isArray(spec.value)) {
			const light = spec.value.find((item) => item.theme?.mode === "light")?.value;
			const dark = spec.value.find((item) => item.theme?.mode === "dark")?.value;
			assert.ok(light && dark, `双态色令牌 --${name} 缺少浅深定义`);
			assert.equal(lightVars.get(name)?.toUpperCase(), light.toUpperCase(), `色令牌 --${name} 浅色未对齐`);
			assert.equal(darkVars.get(name)?.toUpperCase(), dark.toUpperCase(), `色令牌 --${name} 深色未对齐`);
			assert.equal(
				systemDarkVars.get(name)?.toUpperCase(),
				dark.toUpperCase(),
				`色令牌 --${name} system 深色未对齐`,
			);
		}
	}

	assert.deepEqual([...darkVars.entries()], [...systemDarkVars.entries()], "dark 与 system 深色变量块必须逐项一致");
});

test("tokens.css 注册全部 calc() 时长 @property 并包含减少动态与减少透明度降级", () => {
	const css = normalizeLf(readFileSync(resolve(appDir, "tokens.css"), "utf8"));
	const rootVars = parseCssBlock(css, ":root {");
	for (const [name, value] of rootVars.entries()) {
		if (value.includes("var(--motion-scale)") || /^var\(--(fast|normal|slow)\)$/.test(value)) {
			assert.ok(
				new RegExp(`@property\\s+--${name}\\s*\\{\\s*syntax:\\s*"<time>"`).test(css),
				`动效时长令牌 --${name} 缺少 @property <time> 注册`,
			);
		}
	}
	assert.ok(css.includes("@media (prefers-reduced-transparency: reduce)"), "缺少减少透明度降级媒体查询");
	assert.ok(css.includes(":root[data-theme] {\n    --motion-scale: 0;"), "缺少高特异性减少动态 --motion-scale 归零");
});

test("前端 CSS 与二维码配色已彻底清除旧版青灰与旧品牌蓝硬编码色值", () => {
	const legacyHexes = [
		"#131a1d",
		"#0b1215",
		"#1d2428",
		"#242c2f",
		"#f5f6f8",
		"#d5dee3",
		"#e3ecf1",
		"#465a9f",
		"#7785de",
		"#1b85f2",
		"#0665be",
		"#007b3a",
		"#bf091b",
		"#0f1517",
		"#77838a",
	];
	for (const file of listFilesByExt(appDir, [".css"])) {
		const content = readFileSync(file, "utf8").toLowerCase();
		for (const hex of legacyHexes) {
			assert.equal(content.includes(hex), false, `${file} 仍残留旧版硬编码色值 ${hex}`);
		}
	}
	const oauthPanel = readFileSync(resolve(appDir, "auth/oauth-panel.tsx"), "utf8").toLowerCase();
	assert.equal(oauthPanel.includes("#1a2024"), false, "oauth-panel.tsx 仍残留旧版青灰前景色 #1a2024");
});

test("阶段 2 全局外壳、侧栏与浮层体系遵守 248px / 44px / 384px 规范且无旧悬停覆盖", () => {
	const tokensCss = normalizeLf(readFileSync(resolve(appDir, "tokens.css"), "utf8"));
	const stylesCss = normalizeLf(readFileSync(resolve(appDir, "styles.css"), "utf8"));
	const settingsCss = normalizeLf(readFileSync(resolve(appDir, "settings/settings.css"), "utf8"));
	const surfacesCss = normalizeLf(readFileSync(resolve(appDir, "shell/surfaces.css"), "utf8"));
	const layoutTsx = normalizeLf(readFileSync(resolve(appDir, "routes/layout.tsx"), "utf8"));

	assert.ok(stylesCss.includes('@import "./shell/surfaces.css";'), "styles.css 必须引入 surfaces.css");
	assert.ok(stylesCss.includes("width: 248px;"), "侧栏宽度必须对齐 248px");
	assert.ok(stylesCss.includes("margin-left: 248px;"), "工作区左外边距必须对齐 248px");
	assert.equal(stylesCss.includes("264px"), false, "styles.css 不得残留旧 264px 侧栏宽度");
	assert.equal(settingsCss.includes("264px"), false, "settings.css 不得残留旧 264px 侧栏宽度");
	assert.equal(layoutTsx.includes("264"), false, "layout.tsx 不得残留旧 264px 通知偏移");
	assert.ok(stylesCss.includes("height: var(--h-header);"), "页头高度必须对齐 --h-header (44px)");
	assert.ok(stylesCss.includes("width: min(384px"), "确认与模态弹窗宽度必须对齐 384px");
	assert.ok(
		surfacesCss.includes("background-color: var(--bg-item-active);"),
		"菜单项悬停/聚焦必须使用 --bg-item-active",
	);
	assert.ok(
		surfacesCss.includes("background-color: var(--status-danger-tint);"),
		"危险菜单项悬停必须使用 --status-danger-tint",
	);
	assert.equal(
		stylesCss.includes("padding-left: 64px"),
		false,
		"非主页折叠页头改用内联按钮，不得残留 64px 左内边距补丁",
	);
	// 验证减少透明度/无 backdrop-filter 通过 CSS 变量统一驱动，避免被后续浮层覆盖
	assert.ok(tokensCss.includes("--glass-bg-2:"), "tokens.css 必须定义 --glass-bg-2 衍生变量");
	assert.ok(tokensCss.includes("--scrim-bg:"), "tokens.css 必须定义 --scrim-bg 衍生变量");
	assert.equal(
		stylesCss.includes("backdrop-filter: blur("),
		false,
		"styles.css 浮层与遮罩必须复用 var(--glass-blur-2)/var(--scrim-blur)，禁止硬编码 blur()",
	);
	// 验证 960px 响应式覆盖位于基础 .chat-header / .settings-content 规则之后
	const baseChatHeader = stylesCss.indexOf(".chat-header {");
	const baseSettingsContent = stylesCss.indexOf(".settings-content {");
	const tailMedia960 = stylesCss.lastIndexOf("@media (max-width: 960px)");
	assert.ok(
		tailMedia960 > baseChatHeader && tailMedia960 > baseSettingsContent,
		"960px 响应式覆盖必须位于 .chat-header 与 .settings-content 基础规则之后",
	);
});
