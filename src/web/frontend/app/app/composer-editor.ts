// 输入框内嵌标记（chip）的序列化与提取。
// chip 在数据里就是它的 token 加一个空格：命令 "/compact"、文件引用 "@磁盘体检.txt"。
// 解析时把开头的命令 token、任意位置的 @文件 token 识别为 chip，其余保持纯文本。

export type EditorSegment =
	| { kind: "text"; text: string }
	| { kind: "chip"; token: string; label: string; icon: "command" | "file" };

function chipLabel(token: string): string {
	if (token.startsWith("/skill:")) return token.slice("/skill:".length);
	return token.slice(1);
}

/** 序列化文本 → 段列表。commands 是合法命令全集（commandLabels 的键）。 */
export function parseEditor(value: string, commands: readonly string[]): EditorSegment[] {
	const segments: EditorSegment[] = [];
	let rest = value;
	const lead = /^(\S+)[ \t]*/.exec(rest);
	if (lead && commands.includes(lead[1] ?? "")) {
		segments.push({ kind: "chip", token: lead[1] ?? "", label: chipLabel(lead[1] ?? ""), icon: "command" });
		rest = rest.slice(lead[0].length);
	}
	const fileToken = /(^|\s)@([^\s@]+)/gm;
	let cursor = 0;
	for (const match of rest.matchAll(fileToken)) {
		const at = match.index ?? 0;
		const prefix = match[1] ?? "";
		const path = match[2] ?? "";
		if (at > cursor) segments.push({ kind: "text", text: rest.slice(cursor, at + prefix.length) });
		segments.push({ kind: "chip", token: `@${path}`, label: path, icon: "file" });
		cursor = at + prefix.length + path.length + 1;
	}
	if (cursor < rest.length) segments.push({ kind: "text", text: rest.slice(cursor) });
	return segments;
}

/** 段列表 → 序列化文本（parseEditor 的逆操作）。chip 后的空格看下文：后文以空白开头或没有后文时不补。 */
export function stringifyEditor(segments: readonly EditorSegment[]): string {
	let out = "";
	for (let index = 0; index < segments.length; index++) {
		const segment = segments[index];
		if (segment.kind === "text") {
			out += segment.text;
			continue;
		}
		out += segment.token;
		const next = segments[index + 1];
		if (next && !(next.kind === "text" && /^\s/u.test(next.text))) out += " ";
	}
	return out;
}

function escapeHtml(value: string): string {
	return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function chipHtml(segment: Extract<EditorSegment, { kind: "chip" }>): string {
	const icon =
		segment.icon === "command"
			? '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m21.64 3.64-6.6 16.72a.5.5 0 0 1-.94.03l-3.1-6.72-6.72-3.1a.5.5 0 0 1 .03-.94L21.67 2.7a.5.5 0 0 1 .71.94Z"/><path d="m14 8 6 6"/></svg>'
			: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/></svg>';
	return `<span class="composer-chip" contenteditable="false" data-token="${escapeHtml(segment.token)}">${icon}<span class="composer-chip-label">${escapeHtml(segment.label)}</span><button type="button" class="chip-remove" aria-label="移除标记" tabindex="-1">×</button></span>`;
}

/** 段列表 → 编辑器 HTML（草稿恢复、插入标记时用）。文本里的换行落成 <br>。 */
export function editorHtml(segments: readonly EditorSegment[]): string {
	return segments
		.map((segment) => (segment.kind === "text" ? escapeHtml(segment.text).replace(/\n/g, "<br>") : chipHtml(segment)))
		.join("");
}

/**
 * 提交前提取：第一个命令 chip 作为指令（/compact、/tree、/skill:*），
 * 文件 chip 还原成 "@path " 并入正文，纯文本原样保留。
 * 手打的命令前缀不在此处理，按普通正文交给后端。
 */
export function extractPayload(value: string, commands: readonly string[]): { command: string; body: string } {
	const segments = parseEditor(value, commands);
	let command = "";
	let body = "";
	for (let index = 0; index < segments.length; index++) {
		const segment = segments[index];
		if (segment.kind === "chip" && segment.token.startsWith("/")) {
			if (!command) command = segment.token;
			continue;
		}
		if (segment.kind === "chip") {
			body += segment.token;
			const next = segments[index + 1];
			if (next && !(next.kind === "text" && /^\s/u.test(next.text))) body += " ";
			continue;
		}
		body += segment.text;
	}
	return { command, body: body.replace(/[ \t]+\n/g, "\n").replace(/^\s+|\s+$/gu, "") };
}
