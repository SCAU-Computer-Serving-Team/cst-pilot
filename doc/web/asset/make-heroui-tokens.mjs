// 把 HeroUI v3 默认主题的 variables.css 转成 DTCG 色卡。
//
//   node make-heroui-tokens.mjs <variables.css> <out.tokens.json>
//
// 输入文件取自 @heroui/styles 的 dist/themes/default/variables.css：
//   npm pack @heroui/styles@3.2.5
//   tar -xzf heroui-styles-3.2.5.tgz
//
// 只处理颜色变量：单值颜色（oklch/rgba/transparent）、var() 别名和 color-mix() 派生值。
// color-mix 按 CSS Color 5 的 premultiplied alpha 规则预计算，因为兼容版要求不发 color-mix()。

import { readFileSync, writeFileSync } from "node:fs";

// ---------------------------------------------------------------- 颜色空间

function oklchToOklab(L, C, H) {
	const rad = (H * Math.PI) / 180;
	return [L, C * Math.cos(rad), C * Math.sin(rad)];
}

function oklabToOklch(L, a, b) {
	const C = Math.sqrt(a * a + b * b);
	let H = (Math.atan2(b, a) * 180) / Math.PI;
	if (H < 0) H += 360;
	return [L, C, H];
}

function linearToSrgb(v) {
	const c = v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055;
	return Math.min(255, Math.max(0, Math.round(c * 255)));
}

function oklabToHex(L, a, b) {
	const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
	const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
	const s_ = L - 0.0894841775 * a - 1.291485548 * b;
	const l = l_ ** 3;
	const m = m_ ** 3;
	const s = s_ ** 3;
	const r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
	const g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
	const bb = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
	const hex = (v) => linearToSrgb(v).toString(16).padStart(2, "0");
	return `#${hex(r)}${hex(g)}${hex(bb)}`;
}

// ---------------------------------------------------------------- CSS 值解析

// 解析颜色字面量，返回 [L, a, b, alpha] 或 null
function parseColor(text) {
	const t = text.trim();

	const oklch = /^oklch\(\s*([\d.]+)(%?)\s+([\d.]+)\s+([\d.]+)(?:deg)?\s*(?:\/\s*([\d.]+)(%?)\s*)?\)$/i.exec(t);
	if (oklch) {
		let L = Number.parseFloat(oklch[1]);
		if (oklch[2] === "%") L /= 100;
		const C = Number.parseFloat(oklch[3]);
		const H = Number.parseFloat(oklch[4]);
		let alpha = 1;
		if (oklch[5] !== undefined) {
			alpha = Number.parseFloat(oklch[5]);
			if (oklch[6] === "%") alpha /= 100;
		}
		return [...oklchToOklab(L, C, H), alpha];
	}

	const rgba = /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/i.exec(t);
	if (rgba) {
		// 走 sRGB -> OKLab，保证和其他值同一空间
		const [r, g, b] = [1, 2, 3].map((i) => {
			const v = Number.parseFloat(rgba[i]) / 255;
			return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
		});
		const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
		const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
		const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
		return [
			0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
			1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
			0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
			rgba[4] === undefined ? 1 : Number.parseFloat(rgba[4]),
		];
	}

	if (t === "transparent") return [0, 0, 0, 0];
	return null;
}

function round(v, digits) {
	const f = 10 ** digits;
	const r = Math.round(v * f) / f;
	return Object.is(r, -0) ? 0 : r;
}

// [L,a,b,alpha] -> DTCG 颜色对象
function toDtcg(lab) {
	const [L, a, b, alpha] = lab;
	const [ocL, ocC, ocH] = oklabToOklch(L, a, b);
	const out = {
		colorSpace: "oklch",
		components: [round(ocL, 4), round(ocC, 4), round(ocH, 1)],
		hex: oklabToHex(L, a, b),
	};
	if (alpha < 1) out.alpha = round(alpha, 4);
	return out;
}

// ---------------------------------------------------------------- 变量提取

const cssPath = process.argv[2];
const outPath = process.argv[3];
const css = readFileSync(cssPath, "utf8");

// 按「选择器块」切分。HeroUI 的 variables.css 里没有嵌套规则，逐字符扫花括号即可。
function collectBlocks(text) {
	const blocks = [];
	const re = /([^{}]+)\{([^{}]*)\}/g;
	let m = re.exec(text);
	while (m) {
		blocks.push({ selector: m[1].trim(), body: m[2] });
		m = re.exec(text);
	}
	return blocks;
}

function parseDecls(body) {
	const decls = [];
	const re = /(--[\w-]+)\s*:\s*([^;]+);/g;
	let m = re.exec(body);
	while (m) {
		decls.push({ name: m[1], value: m[2].replace(/\s+/g, " ").trim() });
		m = re.exec(body);
	}
	return decls;
}

const blocks = collectBlocks(css).map((b) => ({ ...b, decls: parseDecls(b.body) }));

// 只保留颜色声明。别名和 color-mix 先原样留着，稍后解析。
function isColorText(value) {
	if (/^var\(--[\w-]+\)$/.test(value)) return true;
	if (/^color-mix\(/i.test(value)) return true;
	return parseColor(value) !== null;
}

const groups = {};
for (const block of blocks) {
	const s = block.selector;
	let group;
	if (/data-vibrant-palette/.test(s)) group = "vibrant";
	else if (/:host|\:root|\.dark|\.light|\.default|data-theme/.test(s)) {
		if (/\.dark|data-theme="dark"/.test(s)) group = "dark";
		else if (/\.light|\.default|data-theme="light"|data-theme="default"/.test(s)) group = "light";
		else group = "common";
	} else continue;

	groups[group] ??= {};
	for (const d of block.decls) {
		if (isColorText(d.value)) groups[group][stripVarName(d.name)] = d.value;
	}
}

// ---------------------------------------------------------------- 解析与求解

function stripVarName(name) {
	return name.replace(/^--/, "");
}

// 在 common / 指定主题里找变量定义
function lookup(name, theme) {
	const key = stripVarName(name);
	if (groups[theme] && key in groups[theme]) return { value: groups[theme][key], scope: theme };
	if (key in groups.common) return { value: groups.common[key], scope: "common" };
	return null;
}

function resolveText(text, theme, seen) {
	const t = text.trim();

	const literal = parseColor(t);
	if (literal) return literal;

	// var(--name) 或 var(--name, 回退值)
	const varCall = /^var\(--([\w-]+)(?:,\s*([\s\S]+))?\)$/.exec(t);
	if (varCall) {
		const inner = resolve(varCall[1], theme, seen);
		if (inner) return inner;
		if (varCall[2]) return resolveText(varCall[2], theme, seen);
		return null;
	}

	const mix = /^color-mix\(\s*in\s+[\w-]+\s*,\s*([\s\S]+)\)\s*$/i.exec(t);
	if (mix) return mixColors(splitMixArgs(mix[1]), theme, seen);

	return null;
}

function resolve(name, theme, seen = new Set()) {
	if (seen.has(name)) throw new Error(`循环引用: ${name}`);
	seen.add(name);

	const found = lookup(name, theme);
	if (!found) return null;
	return resolveText(found.value, theme, seen);
}

// 拆 color-mix 的参数。只在括号深度 0 的逗号处切开，
// 因为 var(--a, 回退值) 内部也有逗号。
function splitMixArgs(text) {
	const raw = [];
	let depth = 0;
	let cur = "";
	for (const ch of text) {
		if (ch === "(") depth++;
		else if (ch === ")") depth--;
		if (ch === "," && depth === 0) {
			raw.push(cur);
			cur = "";
		} else {
			cur += ch;
		}
	}
	raw.push(cur);

	return raw.map((part) => {
		const p = part.trim();
		const m = /^([\s\S]+?)\s+([\d.]+)%$/.exec(p);
		if (m) return { color: m[1].trim(), weight: Number.parseFloat(m[2]) };
		return { color: p, weight: null };
	});
}

function mixColors(parts, theme, seen) {
	if (parts.length !== 2) throw new Error("只支持两色 color-mix");
	const [c1, c2] = parts;
	// 省略的百分比按 CSS 规则补齐
	let w1 = c1.weight;
	let w2 = c2.weight;
	if (w1 === null && w2 === null) {
		w1 = 50;
		w2 = 50;
	} else if (w1 === null) w1 = 100 - w2;
	else if (w2 === null) w2 = 100 - w1;

	const colorOf = (part) => resolveText(part.color, theme, seen);

	const a1 = colorOf(c1);
	const a2 = colorOf(c2);
	const t1 = w1 / (w1 + w2);
	const t2 = w2 / (w1 + w2);

	// CSS Color 5：premultiplied alpha 插值
	const alpha = a1[3] * t1 + a2[3] * t2;
	if (alpha === 0) return [0, 0, 0, 0];
	const out = [0, 0, 0, alpha];
	for (let i = 0; i < 3; i++) {
		out[i] = (a1[i] * a1[3] * t1 + a2[i] * a2[3] * t2) / alpha;
	}
	return out;
}

// ---------------------------------------------------------------- 输出

const GROUP_KEYS = {
	light: "light",
	dark: "dark",
	vibrant: "vibrant",
};

// --white/--black/--snow/--eclipse 是原始值，其余写在公共块里的（--accent 等）
// 是跨主题共享的语义值，单独一组，避免把两者混为一谈。
const PRIMITIVE_NAMES = new Set(["white", "black", "snow", "eclipse"]);

const result = {
	$schema: "https://www.designtokens.org/schemas/2025.10/format.json",
	$description:
		"HeroUI v3 默认主题（@heroui/styles 3.2.5，themes/default/variables.css）的原始配色系统。" +
		"解析自上游 CSS，用于与 CST Pilot 自有配色对照。" +
		"light / dark 两组只收录各自主题块里声明的变量；两组都未声明的取 shared，shared 也未声明的取 primitive。" +
		"每个令牌的 $extensions 记录上游原始 CSS 值；color-mix() 派生值已按 CSS Color 5 的 premultiplied alpha 规则预计算。",
	color: {
		$type: "color",
		primitive: {},
		shared: {},
	},
};

let count = 0;
for (const name of Object.keys(groups.common ?? {})) {
	const bucketName = PRIMITIVE_NAMES.has(name) ? "primitive" : "shared";
	const lab = resolve(name, "light");
	if (!lab) {
		console.error(`跳过 ${name}: 无法求解 (${groups.common[name]})`);
		continue;
	}
	result.color[bucketName][name] = {
		$value: toDtcg(lab),
		$extensions: { "dev.heroui.source": groups.common[name] },
	};
	count++;
}

for (const [group, key] of Object.entries(GROUP_KEYS)) {
	if (!groups[group]) continue;
	const bucket = {};
	for (const name of Object.keys(groups[group])) {
		const theme = group === "vibrant" ? "light" : group;
		let lab;
		try {
			lab = resolve(name, theme);
		} catch (err) {
			console.error(`跳过 ${name}: ${err.message}`);
			continue;
		}
		if (!lab) {
			console.error(`跳过 ${name}: 无法求解 (${groups[group][name]})`);
			continue;
		}
		bucket[name] = {
			$value: toDtcg(lab),
			$extensions: { "dev.heroui.source": groups[group][name] },
		};
		count++;
	}
	result.color[key] = bucket;
}

writeFileSync(outPath, `${JSON.stringify(result, null, 2)}\n`, "utf8");
console.log(`写入 ${outPath}`);
console.log(`令牌数 ${count}`);
for (const key of Object.keys(result.color)) {
	if (key.startsWith("$")) continue;
	console.log(`  ${key.padEnd(10)} ${Object.keys(result.color[key]).length}`);
}
