const CSP =
	"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'";

/** 只内嵌随包样式与字体。保留原生 details，移除运行脚本及动作控件。 */
export async function offlineHtml(title: string, theme: "light" | "dark"): Promise<string> {
	const assets = new Map<string, Promise<string>>();
	function inlineAsset(raw: string, base: string): Promise<string> {
		const url = new URL(raw, base);
		if (url.protocol === "data:") return Promise.resolve(raw);
		if (url.origin !== location.origin || !/^\/(fonts|assets)\//.test(url.pathname))
			throw new Error("样式引用了非本机资源");
		let task = assets.get(url.href);
		if (!task) {
			task = (async () => {
				const response = await fetch(url.href);
				if (!response.ok) throw new Error("导出字体无法读取");
				const blob = await response.blob();
				return new Promise<string>((resolve, reject) => {
					const reader = new FileReader();
					reader.onload = () => resolve(String(reader.result));
					reader.onerror = () => reject(new Error("导出字体无法读取"));
					reader.readAsDataURL(blob);
				});
			})();
			assets.set(url.href, task);
		}
		return task;
	}
	const family = (value: string) => value.replace(/["']/g, "").trim();
	const loadedFonts = [...document.fonts].filter((font) => font.status === "loaded");
	function serialize(rules: CSSRuleList): string {
		return [...rules]
			.map((rule) => {
				if (
					rule instanceof CSSFontFaceRule &&
					!loadedFonts.some(
						(font) =>
							family(font.family) === family(rule.style.getPropertyValue("font-family")) &&
							font.weight === (rule.style.getPropertyValue("font-weight") || "normal") &&
							font.style === (rule.style.getPropertyValue("font-style") || "normal"),
					)
				)
					return "";
				if (rule instanceof CSSGroupingRule)
					return `${rule.cssText.slice(0, rule.cssText.indexOf("{") + 1)}${serialize(rule.cssRules)}}`;
				return rule.cssText;
			})
			.join("\n");
	}
	// 所有独立资源同时读取；相同 URL 共用一次请求，不内嵌未使用的字体。
	const sheets = await Promise.all(
		[...document.styleSheets].map(async (sheet) => {
			const css = serialize(sheet.cssRules);
			const matches = [...css.matchAll(/url\(\s*(?:"([^"]*)"|'([^']*)'|([^)]*))\s*\)/g)];
			const replacements = await Promise.all(
				matches.map((match) => inlineAsset((match[1] ?? match[2] ?? match[3]).trim(), sheet.href ?? location.href)),
			);
			let text = "",
				cursor = 0;
			for (const [index, match] of matches.entries()) {
				text += `${css.slice(cursor, match.index)}url("${replacements[index]}")`;
				cursor = match.index + match[0].length;
			}
			return text + css.slice(cursor);
		}),
	);
	const source = document.querySelector(".export-document");
	if (!source) throw new Error("导出正文为空");
	const clone = source.cloneNode(true) as HTMLElement;
	for (const element of clone.querySelectorAll(
		"script, button, input, select, textarea, .code-copy-feedback, .special-tool-details, .special-tool-delivery",
	))
		element.remove();
	for (const element of clone.querySelectorAll("*")) {
		for (const attribute of [...element.attributes])
			if (attribute.name.startsWith("on")) element.removeAttribute(attribute.name);
	}
	const documentTitle = document.createElement("title");
	documentTitle.textContent = title;
	return `<!DOCTYPE html><html lang="zh-CN" data-theme="${theme}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="${CSP}">${documentTitle.outerHTML}<style>${sheets.join("\n").replace(/<\/style/gi, "<\\/style")}</style></head><body class="export-page">${clone.outerHTML}</body></html>`;
}
