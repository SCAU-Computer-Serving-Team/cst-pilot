import type { ExportDocumentSource } from "../../shared/exports.ts";

/** 由 Puppeteer 在页面中执行，只读取已完成排版的文档。 */
export function measureExportDocument(): { height: number; cuts: number[] } {
	const source = document.querySelector(".export-document");
	if (!source) throw new Error("导出正文为空");
	const height = Math.ceil(source.getBoundingClientRect().bottom + scrollY);
	const cuts = [...source.querySelectorAll(".export-message, .markdown > *, tr, .export-tool")].map((node) =>
		Math.ceil(node.getBoundingClientRect().bottom + scrollY + 12),
	);
	for (const node of source.querySelectorAll("pre")) {
		const rect = node.getBoundingClientRect(),
			style = getComputedStyle(node);
		const line = Number.parseFloat(style.lineHeight);
		if (line > 0)
			for (
				let y = rect.top + scrollY + Number.parseFloat(style.paddingTop) + line;
				y < rect.bottom + scrollY;
				y += line
			)
				cuts.push(Math.round(y));
	}
	return { height, cuts };
}

/** 只返回正文和样式引用；资源内嵌与文件组装由后端执行。函数不依赖外层运行时。 */
export function captureExportDocument(): ExportDocumentSource {
	const source = document.querySelector(".export-document");
	if (!source) throw new Error("导出正文为空");
	const normalizeWeight = (weight: string) => (weight === "normal" ? "400" : weight === "bold" ? "700" : weight);
	const key = (family: string, weight: string, style: string) =>
		`${family.replace(/["']/g, "").trim().toLowerCase()}|${normalizeWeight(weight)}|${style}`;
	const loaded = new Set(
		[...document.fonts]
			.filter((font) => font.status === "loaded")
			.map((font) => key(font.family, font.weight, font.style)),
	);
	function serialize(rules: CSSRuleList): string {
		return [...rules]
			.map((rule) => {
				if (
					rule instanceof CSSFontFaceRule &&
					!loaded.has(
						key(
							rule.style.getPropertyValue("font-family"),
							rule.style.getPropertyValue("font-weight") || "normal",
							rule.style.getPropertyValue("font-style") || "normal",
						),
					)
				)
					return "";
				const nested = Reflect.get(rule, "cssRules") as CSSRuleList | undefined;
				if (nested && rule.type !== CSSRule.STYLE_RULE)
					return `${rule.cssText.slice(0, rule.cssText.indexOf("{") + 1)}${serialize(nested)}}`;
				return rule.cssText;
			})
			.join("\n");
	}
	const styles = [...document.styleSheets].map((sheet) => ({
		css: serialize(sheet.cssRules),
		base: sheet.href ?? location.href,
	}));
	const clone = source.cloneNode(true) as HTMLElement;
	for (const element of clone.querySelectorAll(
		"script, button, input, select, textarea, .code-copy-feedback, .special-tool-details, .special-tool-delivery",
	))
		element.remove();
	for (const element of [clone, ...clone.querySelectorAll("*")]) {
		for (const attribute of [...element.attributes])
			if (attribute.name.toLowerCase().startsWith("on")) element.removeAttribute(attribute.name);
	}
	return { body: clone.outerHTML, styles };
}
