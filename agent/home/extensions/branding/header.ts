import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { Component, TUI } from "@earendil-works/pi-tui";
import { truncateToWidth, visibleWidth } from "@earendil-works/pi-tui";
import { readKitVersion } from "./version.ts";

// ---------------------------------------------------------------- 品牌常量
const BRAND = "cst-pilot";
const VERSION = readKitVersion();
const TAGLINE = "用心服务，真诚为您";
const ASK_LINE = "CST-pilot on your side";
const TIP_COMMANDS = ["/login", "/scoped-models", "/model", "/resume", "/tree"];

// ---------------------------------------------------------------- ANSI Shadow 字形（每字形 6 行）
const SHADOW_GLYPHS: Record<string, string[]> = {
	C: [" ██████╗", "██╔════╝", "██║     ", "██║     ", "╚██████╗", " ╚═════╝"],
	S: ["███████╗", "██╔════╝", "███████╗", "╚════██║", "███████║", "╚══════╝"],
	T: ["████████╗", "╚══██╔══╝", "   ██║   ", "   ██║   ", "   ██║   ", "   ╚═╝   "],
	"-": ["      ", "      ", "██████", "      ", "      ", "      "],
	P: ["██████╗ ", "██╔══██╗", "██████╔╝", "██╔════╝", "██║     ", "╚═╝     "],
	I: ["██╗", "██║", "██║", "██║", "██║", "╚═╝"],
	L: ["██╗     ", "██║     ", "██║     ", "██║     ", "███████╗", "╚══════╝"],
	O: [" ██████╗ ", "██╔═══██╗", "██║   ██║", "██║   ██║", "╚██████╔╝", " ╚═════╝ "],
};

const SOLID_CHARS = new Set(["█", "▀", "▄", "▌", "▐"]);
const SHADOW_CHARS = new Set(["╗", "╔", "╝", "╚", "║", "═", "╦", "╩", "╣", "╠"]);

function buildShadowArt(text: string): string[] {
	const rows = Array.from({ length: 6 }, () => "");
	for (const ch of text) {
		const glyph = SHADOW_GLYPHS[ch];
		if (!glyph) throw new Error(`no shadow glyph for ${JSON.stringify(ch)}`);
		const w = Math.max(...glyph.map((r) => r.length));
		for (let i = 0; i < 6; i++) rows[i] += `${(glyph[i] ?? "").padEnd(w)} `;
	}
	return rows.map((r) => r.slice(0, -1));
}

// ---------------------------------------------------------------- 渐变色（自上而下）
function hexToRgb(hex: string): [number, number, number] {
	return [
		Number.parseInt(hex.slice(1, 3), 16),
		Number.parseInt(hex.slice(3, 5), 16),
		Number.parseInt(hex.slice(5, 7), 16),
	];
}

function lerp(a: number, b: number, t: number): number {
	return Math.round(a + (b - a) * t);
}

function mix(c1: string, c2: string, t: number): string {
	const a = hexToRgb(c1);
	const b = hexToRgb(c2);
	const part = (v: number) => v.toString(16).padStart(2, "0");
	return `#${part(lerp(a[0], b[0], t))}${part(lerp(a[1], b[1], t))}${part(lerp(a[2], b[2], t))}`;
}

// 实心笔画：三节点折线渐变（节点 0~10：2 湛蓝、6 天蓝、10 浅白）；影线装饰层：暗蓝 -> 淡蓝
const SHADOW_FROM = "#0d47a1"; // 严格蓝带暗端（色相 210°，不偏紫）
const SHADOW_TO = "#bae6fd"; // sky-200

// 终端背景色：终端没有真 alpha，“透明度”都靠向背景色预混来模拟
const BG = "#101413";
function overBg(color: string, alpha: number): string {
	return mix(BG, color, alpha);
}

// 淡入配色：命名色/十六进制色向背景预混 alpha（透明度 1 时原样返回）
const NAMED_HEX: Record<string, string> = { accent: "#7fd7c4", muted: "#969696", dim: "#697370" };
const PLAIN_HEX = "#c8cccc";
function faded(color: string, a: number): string {
	if (a >= 1 || !color) return color;
	return mix(BG, color.startsWith("#") ? color : (NAMED_HEX[color] ?? PLAIN_HEX), a);
}

// LOGO 渐变节点：严格蓝带（色相 199~218°），不用偏青的 sky 系（那会显得灰）
const SOLID_STOPS: Array<[number, string]> = [
	[0.0, "#0d47a1"], // 深湛蓝（蓝-900，严格蓝带）
	[0.2, "#1976d2"], // 节点 2：湛蓝（饱和正蓝，不偏紫）
	[0.6, "#38bdf8"], // 节点 6：天蓝
	[1.0, "#f8fbff"], // 节点 10：浅白
];

function gradSolid(t: number): string {
	for (let i = 1; i < SOLID_STOPS.length; i++) {
		const [t0, c0] = SOLID_STOPS[i - 1] as [number, string];
		const [t1, c1] = SOLID_STOPS[i] as [number, string];
		if (t <= t1) return mix(c0, c1, (t - t0) / (t1 - t0));
	}
	return SOLID_STOPS[SOLID_STOPS.length - 1][1];
}

// ---------------------------------------------------------------- 入场扫浪（拍岸浪：从左漫过，浪身全过后字型显现）
const SWEEP = {
	margin: 3, // 浪体四周比字型多出的边距（列）
	patch: 38, // 浪体长度：波前之后拖着的长长浪身（拍岸漫流）
	speed: 24, // 扫过速度（列/秒）：缓慢漫过
	delay: 0.2, // 起扫延迟（秒）
	slant: 0.55, // 波前斜率（对角浪头）
	cut: 38, // 浪尾过完多少列内字型才显现（浪身全过）
};

// 稳态轻波循环周期（秒）；频率必须与相位循环成整数倍，保证无缝
const LOOP_S = 12;

// 稳态波浪：多层正弦波在艺术字上流动（phase 2π 一循环）
function waveVal(x: number, y: number, phase: number): number {
	const w1 = Math.sin(x * 0.3 + y * 0.8 - phase);
	const w2 = Math.sin(x * 0.15 - y * 0.5 + phase + 2.1);
	const w3 = Math.sin(x * 0.07 + y * 1.1 - phase * 2 + 4.7);
	return w1 * 0.5 + w2 * 0.3 + w3 * 0.2;
}

// 稳态轻波叠加层：同色系呼吸，只在严格蓝带（色相 210~217°）内波动，不引入中性灰白/紫
function waveOverlay(color: string, x: number, y: number, phase: number): string {
	if (!phase) return color;
	const v = waveVal(x, y, phase);
	if (v > 0.75) return mix(color, "#93c5fa", 0.4); // 浪尖：更浅的蓝
	if (v > 0.35) return mix(color, "#60a5fa", 0.22); // 蓝色微光
	if (v > -0.35) return color;
	if (v > -0.7) return mix(color, "#1565c0", 0.25); // 深一档的蓝
	return mix(color, "#0c2f63", 0.3); // 深蓝谷
}

// ---------------------------------------------------------------- 海晶石式杂色（确定性哈希，逐帧重绘不闪烁）
function cellHash(x: number, y: number, salt: number): number {
	let h = (Math.imul(x + 17, 374761393) ^ Math.imul(y + 129, 668265263) ^ Math.imul(salt, 2246822519)) | 0;
	h = Math.imul(h ^ (h >>> 13), 1274126177);
	return ((h ^ (h >>> 16)) >>> 0) / 4294967296; // [0,1)
}

const SPECK_DARK = 0.07; // 深蓝嵌块（少量点缀）
const SPECK_TEAL = 0.12; // 蓝绿交界点缀（少量）
const SPECK_LIGHT = 0.16; // 蓝系高光（不用纯白，避免灰）

function speckledSolid(base: string, x: number, y: number): string {
	// 星光闪点：约 6% 的格子直接跳到亮青白，制造碎钻反光
	if (cellHash(x, y, 7777) < 0.06) return "#bae6fd";
	const r = cellHash(x, y, 1337);
	if (r < SPECK_DARK) return mix(base, "#0c2f63", 0.5);
	if (r < SPECK_TEAL) return mix(base, "#0d9488", 0.35);
	if (r < SPECK_LIGHT) return mix(base, "#93c5fa", 0.35);
	return base;
}

function speckledShadow(base: string, x: number, y: number): string {
	const r = cellHash(x, y, 4242);
	if (r < 0.06) return mix(base, "#0c4a6e", 0.35);
	return base;
}

// ---------------------------------------------------------------- spans 与 ANSI
type Span = { text: string; color?: string; bold?: boolean };

const ANSI_COLORS: Record<string, string> = {
	accent: "\x1b[38;2;127;215;196m",
	muted: "\x1b[38;2;150;150;150m",
	dim: "\x1b[38;2;105;115;112m",
};

function ansiColorPrefix(color: string | undefined): string {
	if (!color) return "";
	const named = ANSI_COLORS[color];
	if (named) return named;
	if (color.startsWith("#")) {
		const [r, g, b] = hexToRgb(color);
		return `\x1b[38;2;${r};${g};${b}m`;
	}
	return "";
}

function toAnsiLine(spans: readonly Span[]): string {
	let out = "";
	for (const s of spans) {
		let pre = "";
		let post = "";
		const c = ansiColorPrefix(s.color);
		if (c) {
			pre += c;
			post = `\x1b[39m${post}`;
		}
		if (s.bold) {
			pre += "\x1b[1m";
			post = `\x1b[22m${post}`;
		}
		out += pre + s.text + post;
	}
	return out;
}

// ---------------------------------------------------------------- 艺术字上色
// 把一行艺术字转成 spans
// rowIndex：含上下浪区 padding 后的行号（波浪几何用）；gradRow/gradCount：渐变映射（仅字型行）
// phase：稳态波浪相位（2π 一循环）；time：入场秒数（Infinity = 跳过入场，字型常显）
function artRowSpans(
	row: string,
	rowIndex: number,
	gradRow: number,
	gradCount: number,
	phase = 0,
	time = Number.POSITIVE_INFINITY,
): Span[] {
	const gradN = Math.max(gradCount, 2);
	const gradT = Math.min(Math.max(gradRow, 0), gradN - 1) / (gradN - 1);
	const solidBase = gradSolid(gradT);
	const shadowBase = mix(SHADOW_FROM, SHADOW_TO, gradT);
	const spans: Span[] = [];
	const animated = Number.isFinite(time);
	const yMid = (gradCount + 2) / 2;
	const paddedW = row.length;
	for (let x = 0; x < row.length; x++) {
		const ch = row[x];
		// ---------------- 入场扫浪 ----------------
		if (animated) {
			const tt = Math.max(time - SWEEP.delay, 0);
			const base = -SWEEP.patch + SWEEP.speed * tt;
			const done = base - SWEEP.cut > paddedW + 2;
			if (!done) {
				const front =
					base +
					(rowIndex - yMid) * SWEEP.slant +
					// 多频锯齿叠加：波前参差不齐，像漫过沙滩的碎浪边缘
					2.2 * Math.sin(rowIndex * 1.7 + tt * 1.1) +
					1.0 * Math.sin(rowIndex * 3.9 + tt * 2.3) +
					0.6 * Math.sin(rowIndex * 0.9 + tt * 0.7 + 1.3);
				const u = front - x; // 波峰掠过本格多远（负 = 还没到）
				// 波峰边缘的不规则浪沫：可越过字缘，越近波前越亮
				if (u > -2.2 && u < 3.5) {
					const fh = cellHash(x, rowIndex, 7101);
					const closeness = 1 - Math.min(1, Math.abs(u) / 3);
					if (fh < 0.07) {
						spans.push({ text: "█", color: overBg("#93c5fa", 0.55 + 0.35 * closeness) });
						continue;
					}
					if (fh < 0.2) {
						spans.push({ text: "*", color: overBg("#93c5fa", 0.5 + 0.4 * closeness) });
						continue;
					}
					if (fh < 0.38) {
						spans.push({ text: "·", color: overBg("#7dd3fc", 0.5 + 0.35 * closeness) });
						continue;
					}
					if (fh < 0.55) {
						spans.push({ text: "▒", color: overBg("#60a5fa", 0.35 + 0.35 * closeness) });
						continue;
					}
				}
				if (u < 0) {
					// 波前尚未到达：字型仍隐藏（初始透明度 0）
					const prev = spans[spans.length - 1];
					if (prev && prev.color === undefined) prev.text += " ";
					else spans.push({ text: " " });
					continue;
				}
				if (u < SWEEP.cut) {
					// 浪体覆盖：拍岸长浪，配色对齐 LOGO：前缘白 → 中段浅蓝 → 后段湛蓝
					const amp =
						(Math.exp(-((u / 2.5) ** 2)) +
							0.85 * Math.exp(-(((u - 6) / 3.5) ** 2)) +
							0.62 * Math.exp(-(((u - 16) / 5.5) ** 2)) +
							0.42 * Math.exp(-(((u - 26) / 6.5) ** 2))) *
						// 逐格哈希抖动：色带边界参差，颜色更不均匀
						(0.8 + 0.45 * cellHash(x, rowIndex, 7303));
					if (amp > 0.9) spans.push({ text: "█", color: "#f8fbff" });
					else if (amp > 0.68) spans.push({ text: "█", color: "#bae6fd" });
					else if (amp > 0.46) spans.push({ text: "█", color: "#7dd3fc" });
					else if (amp > 0.3) spans.push({ text: "█", color: "#0ea5e9" });
					else if (amp > 0.15) spans.push({ text: "█", color: "#1976d2" });
					else if (amp > 0.07) spans.push({ text: "█", color: "#0d47a1" });
					else {
						const prev = spans[spans.length - 1];
						if (prev && prev.color === undefined) prev.text += " ";
						else spans.push({ text: " " });
					}
					continue;
				}
				// u >= cut：浪尾已过，落到下方正常渲染（字型显现）
			}
		}
		if (ch === " ") {
			if (phase) {
				// 心跳浪沫：蓝青系半透明方块（不用灰白），慢呼吸
				const fh = cellHash(x, rowIndex, 8801);
				if (fh < 0.014) {
					const fph = cellHash(x, rowIndex, 8802) * Math.PI * 2;
					const pulse = 0.5 + 0.5 * Math.sin(phase * 2 + fph); // 一循环呼吸 2 拍
					const a = 0.4 + 0.4 * pulse ** 3;
					spans.push({
						text: cellHash(x, rowIndex, 8803) < 0.25 ? "▓" : "▒",
						color: overBg("#60a5fa", a),
					});
					continue;
				}
				// 蓝绿交界的常亮微光点（静态少量）
				if (fh < 0.026) {
					spans.push({ text: "·", color: overBg("#7dd3fc", 0.45) });
					continue;
				}
			}
			// 背景点缀：空隙处偶发暗色星点，偶发亮星
			const bg = cellHash(x, rowIndex, 9001);
			if (bg < 0.008) {
				spans.push({ text: "*", color: "#22d3ee" });
				continue;
			}
			if (bg < 0.05) {
				spans.push({ text: "·", color: mix("#0ea5e9", "#0f172a", 0.75) });
				continue;
			}
			const prev = spans[spans.length - 1];
			if (prev && prev.color === undefined) {
				prev.text += ch;
			} else {
				spans.push({ text: ch });
			}
			continue;
		}
		let color = SOLID_CHARS.has(ch)
			? speckledSolid(solidBase, x, rowIndex)
			: SHADOW_CHARS.has(ch)
				? speckledShadow(shadowBase, x, rowIndex)
				: speckledSolid(solidBase, x, rowIndex);
		// 流动波浪叠加层（稳态轻波；phase 为 0 时无变化）
		if (phase) color = waveOverlay(color, x, gradRow, phase);
		// 顶部不再提亮：渐变规范要求顶部就是湛蓝，任何提亮都会洗蓝
		const prev = spans[spans.length - 1];
		if (prev && prev.color === color) {
			prev.text += ch;
		} else {
			spans.push({ text: ch, color });
		}
	}
	return spans;
}

// 给字型加一圈浪区：上下各一行、左右各 SWEEP.margin 列，波浪可以比字型大一圈
function padArtRows(rows: readonly string[]): string[] {
	const m = SWEEP.margin;
	const w = rows[0].length + 2 * m;
	const blank = " ".repeat(w);
	return [blank, ...rows.map((r) => " ".repeat(m) + r + " ".repeat(m)), blank];
}

// 艺术字居中放入左栏：前导空白 span
function centerArtRow(
	row: string,
	artWidth: number,
	leftWidth: number,
	rowIndex: number,
	gradRow: number,
	gradCount: number,
	phase = 0,
	time = Number.POSITIVE_INFINITY,
): Span[] {
	const pad = Math.floor((leftWidth - artWidth) / 2);
	const spans: Span[] = pad > 0 ? [{ text: " ".repeat(pad) }] : [];
	spans.push(...artRowSpans(row, rowIndex, gradRow, gradCount, phase, time));
	return spans;
}

// 星野行：整行都是背景点缀（艺术字上下的空行用）
function starfieldSpans(width: number, seedRow: number, a = 1): Span[] {
	// 透明度 0：整行纯空格（真隐形）
	if (a <= 0) return [{ text: " ".repeat(width) }];
	const spans: Span[] = [];
	let run = "";
	const flush = () => {
		if (run) {
			spans.push({ text: run });
			run = "";
		}
	};
	for (let x = 0; x < width; x++) {
		const r = cellHash(x, seedRow, 9001);
		if (r < 0.012) {
			flush();
			spans.push({ text: "*", color: faded("#155e75", a) });
		} else if (r < 0.05) {
			flush();
			spans.push({ text: "·", color: faded(mix("#155e75", "#0f172a", 0.5), a) });
		} else {
			run += " ";
		}
	}
	flush();
	return spans;
}

// ---------------------------------------------------------------- 布局（照搬 pi-open-tui）
const MIN_LEFT_WIDTH = 28;
const MIN_TIPS_WIDTH = 16;
const MAX_TIPS_WIDTH = 28;
const COLUMN_GAP = 3;

function headerColumnWidths(innerWidth: number): { leftWidth: number; rightWidth: number; useTips: boolean } {
	if (innerWidth <= 0) return { leftWidth: 0, rightWidth: 0, useTips: false };
	if (innerWidth < MIN_LEFT_WIDTH + COLUMN_GAP + MIN_TIPS_WIDTH) {
		return { leftWidth: innerWidth, rightWidth: 0, useTips: false };
	}
	let rightWidth = Math.min(MAX_TIPS_WIDTH, Math.max(MIN_TIPS_WIDTH, Math.round(innerWidth * 0.28)));
	let leftWidth = innerWidth - COLUMN_GAP - rightWidth;
	if (leftWidth < MIN_LEFT_WIDTH) {
		leftWidth = MIN_LEFT_WIDTH;
		rightWidth = innerWidth - COLUMN_GAP - leftWidth;
	}
	if (leftWidth <= rightWidth) {
		leftWidth = Math.ceil((innerWidth - COLUMN_GAP) * 0.65);
		rightWidth = innerWidth - COLUMN_GAP - leftWidth;
	}
	if (rightWidth < MIN_TIPS_WIDTH || leftWidth < MIN_LEFT_WIDTH) {
		return { leftWidth: innerWidth, rightWidth: 0, useTips: false };
	}
	return { leftWidth, rightWidth, useTips: true };
}

// 按显示宽度截断纯文本（不带任何 ANSI 副作用）；跨 CJK 半字时不切半个字符
function sliceToWidth(text: string, maxWidth: number): string {
	if (visibleWidth(text) <= maxWidth) return text;
	let out = "";
	let used = 0;
	for (const ch of text) {
		const cw = visibleWidth(ch);
		if (used + cw > maxWidth) break;
		out += ch;
		used += cw;
	}
	return out;
}

function padSpans(spans: readonly Span[], width: number): Span[] {
	const out: Span[] = [];
	let used = 0;
	for (const s of spans) {
		const avail = width - used;
		if (avail <= 0) break;
		// 用显示宽度而非字符数：CJK 字符占 2 列，否则含中文的行会溢出
		const w = visibleWidth(s.text);
		if (w <= avail) {
			out.push({ ...s, text: s.text });
			used += w;
			continue;
		}
		const t = sliceToWidth(s.text, avail);
		if (!t) break;
		out.push({ ...s, text: t });
		used += visibleWidth(t);
	}
	if (used < width) out.push({ text: " ".repeat(width - used) });
	return out;
}

// 透明度 0：不发字形，纯空格占位（终端无真透明，背景色预混会渲染成色块，空格才是真隐形）
function ghostText(text: string): string {
	return " ".repeat(visibleWidth(text));
}

function centerSpans(text: string, width: number, color: string | undefined, bold: boolean | undefined, a = 1): Span[] {
	if (width <= 0) return [];
	const w = visibleWidth(text);
	if (a <= 0) {
		return [
			{
				text: ghostText(
					w >= width ? truncateToWidth(text, width, "…") : " ".repeat(Math.floor((width - w) / 2)) + text,
				),
				bold,
			},
		];
	}
	if (w >= width) {
		return [{ text: truncateToWidth(text, width, "…"), color, bold }];
	}
	const pad = Math.floor((width - w) / 2);
	// 与设计稿一致：前导空格并入有色段（空格本身无色，渲染无差，但保证 ANSI 序列一致）
	return [{ text: " ".repeat(pad) + text, color, bold }];
}

function borderLine(left: string, titleSpans: readonly Span[], right: string, width: number): Span[][] {
	if (width <= 1) return [];
	if (width < 8 || titleSpans.length === 0) {
		return [
			[
				{
					text: truncateToWidth(left + "─".repeat(Math.max(0, width - 2)) + right, width),
					color: "accent",
				},
			],
		];
	}
	const before = "─── ";
	const after = " ─────";
	const titlePlain = titleSpans.map((s) => s.text).join("");
	const fill = Math.max(0, width - 2 - (before.length + titlePlain.length + after.length));
	return [
		[
			{ text: left, color: "accent" },
			{ text: before, color: "accent" },
			...titleSpans,
			{ text: after, color: "accent" },
			{ text: "─".repeat(fill), color: "accent" },
			{ text: right, color: "accent" },
		],
	];
}

function twoColumn(leftSpans: readonly Span[], rightSpans: readonly Span[], leftWidth: number): Span[] {
	const out = padSpans(leftSpans, leftWidth);
	out.push({ text: " │ ", color: "accent" });
	out.push(...rightSpans);
	return out;
}

function padBoxLine(contentSpans: readonly Span[], width: number): Span[][] {
	if (width <= 2) return [[{ text: truncateToWidth(contentSpans.map((s) => s.text).join(""), width) }]];
	return [[{ text: "│", color: "accent" }, ...padSpans(contentSpans, width - 2), { text: "│", color: "accent" }]];
}

// ---------------------------------------------------------------- 动态文案
function formatCwd(cwd: string): string {
	const home = process.env.HOME || process.env.USERPROFILE;
	if (!home) return cwd;
	if (cwd.toLowerCase() === home.toLowerCase()) return "~";
	const prefix = home.endsWith("\\") || home.endsWith("/") ? home : home;
	if (!cwd.toLowerCase().startsWith(prefix.toLowerCase())) return cwd;
	const rel = cwd.slice(prefix.length).replace(/^[\\/]/, "");
	return rel ? `~\\${rel}` : "~";
}

function formatModelLabel(model: { provider?: string; id?: string } | null | undefined): string {
	if (!model?.id) return "no-model";
	return model.provider ? `${model.provider}/${model.id}` : model.id;
}

function formatThinkingLabel(level: string): string {
	if (level === "off") return "thinking off";
	return `${level} effort`;
}

// ---------------------------------------------------------------- header 组件
const SHADOW_ART = buildShadowArt("CST-PILOT"); // 76 列 x 6 行
const SHADOW_ART_W = SHADOW_ART[0].length;
const PILOT_ART = buildShadowArt("PILOT"); // 41 列 x 6 行
const PILOT_ART_W = PILOT_ART[0].length;

export class BrandingHeader implements Component {
	// 动画定格上界：~9s 入场 + 1 个完整稳态循环（停在循环接缝，无跳变）。
	// 头部在内容区顶部，一旦滚出视口，任何一行变化都会触发 pi-tui 全量重绘并清原生回滚，
	// 动画必须限时，否则用户永远无法向上滚动阅读历史。
	private static readonly FREEZE_AT = 21;
	private readonly pi: ExtensionAPI;
	private readonly ctx: ExtensionContext;
	private readonly tui: TUI;
	private readonly startAt = Date.now();
	private frozen = false;
	private timer?: ReturnType<typeof setInterval>;

	constructor(pi: ExtensionAPI, ctx: ExtensionContext, tui: TUI) {
		this.pi = pi;
		this.ctx = ctx;
		this.tui = tui;
		// 恢复的历史会话：跳过入场动画，直接定格（避免开场就反复清回滚）。
		// 注意：启动时就会写入 model_change/thinking_level_change 元数据条目，
		// 只有真正的对话内容（message/custom/compaction）才算历史会话。
		try {
			this.frozen = ctx.sessionManager
				.getEntries()
				.some((e) => e.type === "message" || e.type === "custom" || e.type === "compaction");
		} catch {
			this.frozen = false;
		}
		if (this.frozen) return;
		// ~10fps 驱动动画重绘；到 FREEZE_AT 或 dispose 时停止
		this.timer = setInterval(() => {
			if ((Date.now() - this.startAt) / 1000 >= BrandingHeader.FREEZE_AT) {
				this.frozen = true;
				if (this.timer) {
					clearInterval(this.timer);
					this.timer = undefined;
				}
				return;
			}
			this.tui.requestRender();
		}, 100);
	}

	render(width: number): string[] {
		if (width < 24) return [toAnsiLine([{ text: `${BRAND} v${VERSION}`, color: "accent" }])];

		const raw = (Date.now() - this.startAt) / 1000; // 入场秒数
		// 到定格时间：停表，冻结在循环接缝（phase 0，字型全显）；tick 里也会冻结，这里是兑底
		if (!this.frozen && raw >= BrandingHeader.FREEZE_AT) {
			this.frozen = true;
			if (this.timer) {
				clearInterval(this.timer);
				this.timer = undefined;
			}
		}
		const now = this.frozen ? Number.POSITIVE_INFINITY : raw;
		const phase = this.frozen ? 0 : (raw % LOOP_S) * ((Math.PI * 2) / LOOP_S); // 稳态波浪相位（12s 无缝循环）
		const innerWidth = width - 2;
		const { leftWidth, rightWidth, useTips } = headerColumnWidths(innerWidth);
		const model = formatModelLabel(this.ctx.model);
		const effort = formatThinkingLabel(this.pi.getThinkingLevel());
		const cwd = formatCwd(this.ctx.cwd);

		// 艺术字选择：CST-PILOT 阴影体(6行+浪区) -> PILOT 阴影体(6行+浪区) -> 纯文本
		let paddedArtW = 0;
		let artRows: Span[][];
		let artMode: "shadow" | "text";
		if (leftWidth >= SHADOW_ART_W + 2 * SWEEP.margin) {
			const padded = padArtRows(SHADOW_ART);
			paddedArtW = padded[0].length;
			artRows = padded.map((row, i) =>
				centerArtRow(row, padded[0].length, leftWidth, i, i - 1, SHADOW_ART.length, phase, now),
			);
			artMode = "shadow";
		} else if (leftWidth >= PILOT_ART_W + 2 * SWEEP.margin) {
			const padded = padArtRows(PILOT_ART);
			paddedArtW = padded[0].length;
			artRows = padded.map((row, i) =>
				centerArtRow(row, padded[0].length, leftWidth, i, i - 1, PILOT_ART.length, phase, now),
			);
			artMode = "shadow";
		} else {
			artRows = [centerSpans(BRAND.toUpperCase(), leftWidth, "accent", true)];
			artMode = "text";
		}

		// 全部非边框/标题文字的淡入：LOGO 浪完全漫过后（波前走完浪区 + 锯齿余量），按行错峰从无到有
		const SCENE_FADE = { dur: 1.8, guard: 6, stagger: 0.1 };
		const fadeStart =
			artMode === "shadow"
				? SWEEP.delay + (SWEEP.patch + SWEEP.cut + paddedArtW + 2 + SCENE_FADE.guard) / SWEEP.speed
				: 0.6;
		const rowA = (delay: number): number => {
			if (!Number.isFinite(now)) return 1;
			const x = (now - fadeStart - delay) / SCENE_FADE.dur;
			if (x <= 0) return 0;
			if (x >= 1) return 1;
			return x * x * (3 - 2 * x); // smoothstep
		};
		const starA = rowA(0);
		const taglineA = rowA(0);
		const modelA = rowA(0.15);
		const cwdA = rowA(0.3);
		const tipA = (i: number): number => rowA(0.1 + i * SCENE_FADE.stagger);
		const taglineColor = taglineA >= 1 ? "accent" : faded("accent", taglineA);
		// 纯文本降级模式：LOGO 文本同样淡入（无扫浪）
		if (artMode === "text") {
			artRows = [centerSpans(BRAND.toUpperCase(), leftWidth, faded("accent", starA), true, starA)];
		}

		const empty: Span[] = [{ text: "" }];
		const leftLines: Span[][] =
			artMode === "shadow"
				? [
						starfieldSpans(leftWidth, 101, starA),
						...artRows,
						starfieldSpans(leftWidth, 202, starA),
						centerSpans(TAGLINE, leftWidth, taglineColor, true, taglineA),
						centerSpans(`${model} · ${effort}`, leftWidth, faded("muted", modelA), undefined, modelA),
						centerSpans(cwd, leftWidth, faded("dim", cwdA), undefined, cwdA),
					]
				: [
						...artRows,
						starfieldSpans(leftWidth, 303, starA),
						centerSpans(TAGLINE, leftWidth, taglineColor, true, taglineA),
						centerSpans(`${model} · ${effort}`, leftWidth, faded("muted", modelA), undefined, modelA),
						centerSpans(cwd, leftWidth, faded("dim", cwdA), undefined, cwdA),
					];

		const tipDivider = "─".repeat(Math.max(8, Math.min(rightWidth || 16, 22)));
		const [cmd0 = "", cmd1 = "", cmd2 = "", cmd3 = "", cmd4 = ""] = TIP_COMMANDS;
		const tipRows: Array<[string, string | null, boolean]> = [
			["", null, false],
			["Welcome", "accent", true],
			[ASK_LINE, "muted", false],
			[tipDivider, "accent", false],
			["Commands", "accent", true],
			[cmd0, "muted", false],
			[cmd1, "muted", false],
			[cmd2, "muted", false],
			[cmd3, "muted", false],
			[cmd4, "muted", false],
			["", null, false],
		];
		const tipLines: Span[][] = tipRows.map((row, i) => {
			const a = tipA(i);
			// 透明度 0：不发字形，纯空格占位
			return [
				{
					text: a <= 0 ? ghostText(row[0]) : row[0],
					color: row[1] && a > 0 ? faded(row[1], a) : undefined,
					bold: row[2],
				},
			];
		});
		const title: Span[] = [{ text: BRAND, color: "accent", bold: true }, { text: ` v${VERSION}` }];

		const lines: Span[][] = [...borderLine("╭", title, "╮", width)];
		const rowCount = Math.max(leftLines.length, tipLines.length);
		for (let i = 0; i < rowCount; i++) {
			const l = leftLines[i] ?? empty;
			const r = tipLines[i] ?? empty;
			const content = useTips ? twoColumn(l, r, leftWidth) : l;
			lines.push(...padBoxLine(content, width));
		}
		lines.push(...borderLine("╰", [], "╯", width));
		return lines.map((spans) => truncateToWidth(toAnsiLine(spans), width, ""));
	}

	invalidate(): void {}

	dispose(): void {
		if (this.timer) {
			clearInterval(this.timer);
			this.timer = undefined;
		}
	}
}

export function installBrandingHeader(pi: ExtensionAPI, ctx: ExtensionContext): () => void {
	let header: BrandingHeader | undefined;
	ctx.ui.setHeader((tui) => {
		header?.dispose();
		header = new BrandingHeader(pi, ctx, tui);
		// pi-open-tui 启动时直接清屏，Windows Terminal 会把旧页眉留在回滚区。
		// 接管时由 pi 完整重绘，清除旧帧并同步光标状态；后续动画仍按差分重绘。
		tui.requestRender(true);
		return header;
	});
	return () => {
		header?.dispose();
		header = undefined;
		try {
			ctx.ui.setHeader(undefined);
		} catch {
			// 会话替换后旧 ctx 会失效；清理只做 best-effort，不能让进程退出。
		}
	};
}
