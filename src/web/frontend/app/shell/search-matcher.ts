import PinyinMatch from "pinyin-match";

export const SCORE_EXACT = 1050;
export const SCORE_PREFIX = 1000;
export const SCORE_SUBSTRING = 800;
export const SCORE_PINYIN_FULL = 650;
export const SCORE_PINYIN_INITIAL = 550;
export const SCORE_FUZZY_EDIT1 = 260;
export const SCORE_FUZZY_EDIT2 = 220;
export const SCORE_FUZZY_SUBSEQ = 200;

const MAX_FUZZY_SCAN_CHARS = 1500;

export type MatchSpan = { start: number; end: number };

export type QueryMatch = {
	score: number;
	spans: MatchSpan[];
};

export function normalizeText(input: string): string {
	let out = "";
	for (const char of input) {
		const code = char.codePointAt(0)!;
		const mapped =
			code >= 0xff01 && code <= 0xff5e ? String.fromCharCode(code - 0xfee0) : code === 0x3000 ? " " : char;
		// Keep one scalar for each original character, matching Rust's normalization.
		const lower = mapped.toLowerCase();
		out += String.fromCodePoint(lower.codePointAt(0)!);
	}
	return out;
}

function scalarOffsets(text: string): number[] {
	const offsets = [0];
	let offset = 0;
	for (const char of text) {
		offset += char.length;
		offsets.push(offset);
	}
	return offsets;
}

/** Search API spans count Unicode scalar values; DOM string slices count UTF-16 units. */
export function scalarSpansToUtf16(text: string, spans: readonly MatchSpan[]): MatchSpan[] {
	const offsets = scalarOffsets(text);
	return spans.flatMap((span) => {
		const start = offsets[span.start];
		const end = offsets[span.end];
		return start !== undefined && end !== undefined && start < end ? [{ start, end }] : [];
	});
}

export function splitQueryTerms(query: string): string[] {
	return normalizeText(query)
		.split(/\s+/)
		.filter((term) => term.length > 0);
}

function isCjkCode(code: number): boolean {
	return (
		(code >= 0x4e00 && code <= 0x9fff) || (code >= 0x3400 && code <= 0x4dbf) || (code >= 0xf900 && code <= 0xfaff)
	);
}

function hasCjk(text: string): boolean {
	for (let i = 0; i < text.length; i++) {
		if (isCjkCode(text.charCodeAt(i))) return true;
	}
	return false;
}

function hasAsciiAlpha(text: string): boolean {
	return /[a-z]/.test(text);
}

function scorePinyinSlice(slice: string, term: string): number | null {
	let cjkCount = 0;
	let asciiNonSpace = 0;
	for (let i = 0; i < slice.length; i++) {
		const code = slice.charCodeAt(i);
		if (isCjkCode(code)) cjkCount++;
		else if (!/\s/.test(slice[i])) asciiNonSpace++;
	}
	if (cjkCount === 0) return null;
	const compactTerm = term.replace(/\s+/g, "");
	const pinyinBudget = compactTerm.length - asciiNonSpace;
	return pinyinBudget >= cjkCount * 2 ? SCORE_PINYIN_FULL : SCORE_PINYIN_INITIAL;
}

const hanziPieceCache = new Map<string, boolean>();

function hanziMatchesPiece(ch: string, piece: string, allowPrefix: boolean): boolean {
	const key = `${ch}|${piece}|${allowPrefix ? 1 : 0}`;
	const cached = hanziPieceCache.get(key);
	if (cached !== undefined) return cached;
	let ok = false;
	if (allowPrefix || piece.length === 1) {
		ok = PinyinMatch.match(ch, piece) !== false;
	} else {
		// Append a fixed sentinel Hanzi "啊" (pinyin "a") so pinyin-match enforces a complete syllable boundary.
		const probe = PinyinMatch.match(`${ch}啊`, `${piece}a`);
		ok = Array.isArray(probe) && probe[0] === 0 && probe[1] === 1;
	}
	if (hanziPieceCache.size > 8192) hanziPieceCache.clear();
	hanziPieceCache.set(key, ok);
	return ok;
}

function tryMixedPinyinScan(compact: string, term: string): [number, number] | null {
	const n = compact.length;
	const m = term.length;
	for (let start = 0; start < n; start++) {
		const failed = new Set<number>();
		const dfs = (ci: number, ti: number): number => {
			if (ti === m) return ci - 1;
			if (ci >= n) return -1;
			const stateKey = ci * (m + 1) + ti;
			if (failed.has(stateKey)) return -1;

			const ch = compact[ci];
			if (!isCjkCode(ch.charCodeAt(0))) {
				if (ch === term[ti]) {
					const end = dfs(ci + 1, ti + 1);
					if (end >= 0) return end;
				}
			} else {
				if (ch === term[ti]) {
					const end = dfs(ci + 1, ti + 1);
					if (end >= 0) return end;
				}
				const maxLen = Math.min(6, m - ti);
				for (let len = maxLen; len >= 1; len--) {
					const piece = term.slice(ti, ti + len);
					if (!/^[a-z]+$/.test(piece)) continue;
					if (hanziMatchesPiece(ch, piece, ti + len === m)) {
						const end = dfs(ci + 1, ti + len);
						if (end >= 0) return end;
					}
				}
			}
			failed.add(stateKey);
			return -1;
		};
		const end = dfs(start, 0);
		if (end >= start) return [start, end];
	}
	return null;
}

function tryPinyinMatchCompact(compact: string, term: string): [number, number] | null {
	const direct = PinyinMatch.match(compact, term);
	if (Array.isArray(direct) && typeof direct[0] === "number" && typeof direct[1] === "number") {
		return [direct[0], direct[1]];
	}
	return tryMixedPinyinScan(compact, term);
}

function matchPinyinTerm(normText: string, term: string): { score: number; span: MatchSpan } | null {
	if (!hasAsciiAlpha(term) || !hasCjk(normText)) return null;
	const hit = PinyinMatch.match(normText, term);
	if (Array.isArray(hit)) {
		const [start, endInclusive] = hit;
		if (typeof start === "number" && typeof endInclusive === "number" && start <= endInclusive) {
			const score = scorePinyinSlice(normText.slice(start, endInclusive + 1), term);
			if (score !== null) return { score, span: { start, end: endInclusive + 1 } };
		}
	}

	let compact = "";
	const idxMap: number[] = [];
	for (let i = 0; i < normText.length; i++) {
		if (!/\s/.test(normText[i])) {
			compact += normText[i];
			idxMap.push(i);
		}
	}
	const compactHit = tryPinyinMatchCompact(compact, term);
	if (compactHit) {
		const [cs, ce] = compactHit;
		if (cs <= ce && ce < idxMap.length) {
			const start = idxMap[cs];
			const end = idxMap[ce] + 1;
			const score = scorePinyinSlice(normText.slice(start, end), term);
			if (score !== null) return { score, span: { start, end } };
		}
	}

	return null;
}

function osaDistance(a: readonly string[], b: readonly string[]): number {
	const m = a.length;
	const n = b.length;
	if (m === 0) return n;
	if (n === 0) return m;
	const d: number[][] = Array.from({ length: m + 1 }, () => new Array<number>(n + 1).fill(0));
	for (let i = 0; i <= m; i++) d[i][0] = i;
	for (let j = 0; j <= n; j++) d[0][j] = j;
	for (let i = 1; i <= m; i++) {
		for (let j = 1; j <= n; j++) {
			const cost = a[i - 1] === b[j - 1] ? 0 : 1;
			let val = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + cost);
			if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) {
				val = Math.min(val, d[i - 2][j - 2] + 1);
			}
			d[i][j] = val;
		}
	}
	return d[m][n];
}

function sharedCharCount(term: readonly string[], window: readonly string[]): number {
	let matched = 0;
	const used = new Uint8Array(window.length);
	for (let i = 0; i < term.length; i++) {
		const tc = term[i];
		for (let j = 0; j < window.length; j++) {
			if (!used[j] && window[j] === tc) {
				used[j] = 1;
				matched++;
				break;
			}
		}
	}
	return matched;
}

function matchBoundedEdit(
	normText: readonly string[],
	term: readonly string[],
): { score: number; span: MatchSpan } | null {
	const tlen = term.length;
	if (tlen < 3 || tlen > 48) return null;
	const maxDist = tlen <= 5 ? 1 : 2;
	const scanLen = Math.min(normText.length, MAX_FUZZY_SCAN_CHARS);
	if (scanLen < Math.max(0, tlen - maxDist)) return null;

	const minW = Math.max(2, tlen - maxDist);
	const maxW = Math.min(scanLen, tlen + maxDist);
	let best: { dist: number; span: MatchSpan } | null = null;

	for (let start = 0; start < scanLen; start++) {
		if (/\s/.test(normText[start])) continue;
		for (let wlen = minW; wlen <= maxW; wlen++) {
			const end = start + wlen;
			if (end > scanLen) break;
			if (/\s/.test(normText[end - 1])) continue;
			const win = normText.slice(start, end);
			if (win.some((char) => /\s/.test(char))) continue;
			const shared = sharedCharCount(term, win);
			if (shared < tlen - maxDist) continue;
			if (tlen === 3 && shared < 2) continue;
			const dist = osaDistance(term, win);
			if (dist <= maxDist) {
				if (tlen === 3 && term[0] !== win[0] && term[2] !== win[win.length - 1]) continue;
				if (!best || dist < best.dist) {
					best = { dist, span: { start, end } };
					if (dist === 1) return { score: SCORE_FUZZY_EDIT1, span: best.span };
				}
			}
		}
	}
	if (!best) return null;
	return { score: best.dist <= 1 ? SCORE_FUZZY_EDIT1 : SCORE_FUZZY_EDIT2, span: best.span };
}

function matchBoundedSubsequence(
	normText: readonly string[],
	term: readonly string[],
): { score: number; span: MatchSpan } | null {
	const tlen = term.length;
	if (tlen < 3) return null;
	const scanLen = Math.min(normText.length, MAX_FUZZY_SCAN_CHARS);
	const maxGap = 4;
	let best: { spanLen: number; span: MatchSpan } | null = null;

	for (let start = 0; start < scanLen; start++) {
		if (normText[start] !== term[0]) continue;
		let prev = start;
		let ok = true;
		for (let k = 1; k < tlen; k++) {
			const limit = Math.min(scanLen, prev + 1 + maxGap + 1);
			let found = -1;
			for (let idx = prev + 1; idx < limit; idx++) {
				if (/\s/.test(normText[idx])) break;
				if (normText[idx] === term[k]) {
					found = idx;
					break;
				}
			}
			if (found >= 0) prev = found;
			else {
				ok = false;
				break;
			}
		}
		if (ok) {
			const spanLen = prev - start + 1;
			if (tlen * 10 >= spanLen * 6 && (!best || spanLen < best.spanLen)) {
				best = { spanLen, span: { start, end: prev + 1 } };
			}
		}
	}
	return best ? { score: SCORE_FUZZY_SUBSEQ, span: best.span } : null;
}

function matchSingleTerm(normText: string, term: string): { score: number; span: MatchSpan } | null {
	if (!term || !normText) return null;
	if (normText === term) return { score: SCORE_EXACT, span: { start: 0, end: normText.length } };
	if (normText.startsWith(term)) return { score: SCORE_PREFIX, span: { start: 0, end: term.length } };
	const subIdx = normText.indexOf(term);
	if (subIdx >= 0) return { score: SCORE_SUBSTRING, span: { start: subIdx, end: subIdx + term.length } };

	const pinyinHit = matchPinyinTerm(normText, term);
	if (pinyinHit) return pinyinHit;

	const termChars = [...term];
	if (termChars.length >= 3 && termChars.length <= MAX_FUZZY_SCAN_CHARS) {
		const textChars = [...normText].slice(0, MAX_FUZZY_SCAN_CHARS);
		const hit = matchBoundedEdit(textChars, termChars) ?? matchBoundedSubsequence(textChars, termChars);
		if (hit) {
			const [span] = scalarSpansToUtf16(normText, [hit.span]);
			if (span) return { score: hit.score, span };
		}
	}
	return null;
}

export function mergeSpans(spans: readonly MatchSpan[]): MatchSpan[] {
	const sorted = [...spans].filter((s) => s.start < s.end).sort((a, b) => a.start - b.start || a.end - b.end);
	const merged: MatchSpan[] = [];
	for (const sp of sorted) {
		const last = merged[merged.length - 1];
		if (last && sp.start <= last.end) last.end = Math.max(last.end, sp.end);
		else merged.push({ ...sp });
	}
	return merged;
}

export function matchQuery(text: string, query: string): QueryMatch | null {
	const normQuery = normalizeText(query).trim();
	if (!normQuery) return null;
	const normText = normalizeText(text);
	if (!normText) return null;

	if (normText === normQuery) {
		return { score: SCORE_EXACT, spans: [{ start: 0, end: normText.length }] };
	}
	if (normText.startsWith(normQuery)) {
		return { score: SCORE_PREFIX, spans: [{ start: 0, end: normQuery.length }] };
	}
	const wholeSubIdx = normText.indexOf(normQuery);
	if (wholeSubIdx >= 0) {
		return { score: SCORE_SUBSTRING, spans: [{ start: wholeSubIdx, end: wholeSubIdx + normQuery.length }] };
	}

	const terms = splitQueryTerms(normQuery);
	if (terms.length === 0) return null;

	let total = 0;
	const spans: MatchSpan[] = [];
	for (const term of terms) {
		const hit = matchSingleTerm(normText, term);
		if (!hit) return null;
		total += hit.score;
		spans.push(hit.span);
	}
	return { score: Math.floor(total / terms.length), spans: mergeSpans(spans) };
}

export function collapseWhitespace(text: string): string {
	return text.replace(/\s+/g, " ").trim();
}

export function buildSnippet(
	rawText: string,
	query: string,
	prefix = "",
	maxBodyChars = 36,
): { snippet: string; spans: MatchSpan[] } {
	const clean = collapseWhitespace(rawText);
	if (!clean) return { snippet: prefix, spans: [] };
	const maxBody = Math.max(12, maxBodyChars);
	const matched = matchQuery(clean, query);
	const offsets = scalarOffsets(clean);
	const scalarCount = offsets.length - 1;
	const firstStart = Math.max(0, offsets.indexOf(matched?.spans[0]?.start ?? 0));
	const lead = Math.min(Math.floor(maxBody / 3), firstStart);
	const last = Math.min(scalarCount, firstStart - lead + maxBody);
	const winStart = offsets[Math.max(0, last - maxBody)];
	const winEnd = offsets[last];

	const leading = winStart > 0 ? "…" : "";
	const trailing = winEnd < clean.length ? "…" : "";
	const head = `${prefix}${leading}`;
	const offset = head.length;
	const snippet = `${head}${clean.slice(winStart, winEnd)}${trailing}`;

	const spans: MatchSpan[] = [];
	if (matched) {
		for (const sp of matched.spans) {
			const s = Math.max(sp.start, winStart);
			const e = Math.min(sp.end, winEnd);
			if (s < e) spans.push({ start: offset + (s - winStart), end: offset + (e - winStart) });
		}
	}
	return { snippet, spans };
}
