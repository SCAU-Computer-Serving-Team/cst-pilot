import type {
	ContentSearchMatch,
	SearchContentKind,
	SessionDetail,
	SessionRow,
	SessionSearchResponse,
	TitleSearchMatch,
} from "../data/web-state.ts";
import { buildSnippet, collapseWhitespace, matchQuery } from "./search-matcher.ts";

export const SEARCH_PAGE_SIZE = 10;

export type ExpandAction =
	| { kind: "appended"; nextPrefetchOffset?: number }
	| { kind: "waiting_in_flight" }
	| { kind: "fetch_now"; offset: number }
	| { kind: "none" };

export type SidebarSearchState = {
	query: string;
	requestSeq: number;
	snapshotToken: string;
	backendSynced: boolean;
	loadingFirstPage: boolean;

	titleMatches: TitleSearchMatch[];
	titleTotal: number;
	titleHasMore: boolean;
	prefetchedTitles: { items: TitleSearchMatch[]; hasMore: boolean } | null;
	prefetchTitleInFlight: boolean;
	autoExpandTitlePending: boolean;

	contentMatches: ContentSearchMatch[];
	contentTotal: number;
	contentHasMore: boolean;
	prefetchedContents: { items: ContentSearchMatch[]; hasMore: boolean } | null;
	prefetchContentInFlight: boolean;
	autoExpandContentPending: boolean;
};

export function createInitialSidebarSearchState(): SidebarSearchState {
	return {
		query: "",
		requestSeq: 0,
		snapshotToken: "",
		backendSynced: false,
		loadingFirstPage: false,
		titleMatches: [],
		titleTotal: 0,
		titleHasMore: false,
		prefetchedTitles: null,
		prefetchTitleInFlight: false,
		autoExpandTitlePending: false,
		contentMatches: [],
		contentTotal: 0,
		contentHasMore: false,
		prefetchedContents: null,
		prefetchContentInFlight: false,
		autoExpandContentPending: false,
	};
}

export function clearSidebarSearchState(prev: SidebarSearchState): SidebarSearchState {
	return {
		...createInitialSidebarSearchState(),
		requestSeq: prev.requestSeq + 1,
	};
}

export function beginSidebarSearchQuery(
	prev: SidebarSearchState,
	query: string,
	localPreview: SessionSearchResponse,
): SidebarSearchState {
	return {
		query,
		requestSeq: prev.requestSeq + 1,
		snapshotToken: "",
		backendSynced: false,
		loadingFirstPage: true,
		titleMatches: localPreview.titleMatches,
		titleTotal: localPreview.titleTotal,
		titleHasMore: localPreview.titleHasMore,
		prefetchedTitles: null,
		prefetchTitleInFlight: false,
		autoExpandTitlePending: false,
		contentMatches: localPreview.contentMatches,
		contentTotal: localPreview.contentTotal,
		contentHasMore: localPreview.contentHasMore,
		prefetchedContents: null,
		prefetchContentInFlight: false,
		autoExpandContentPending: false,
	};
}

export function applyFirstSearchPage(
	prev: SidebarSearchState,
	seq: number,
	resp: SessionSearchResponse,
): {
	state: SidebarSearchState;
	prefetchTitleOffset?: number;
	prefetchContentOffset?: number;
} {
	if (seq !== prev.requestSeq || !prev.query.trim()) {
		return { state: prev };
	}
	const prefetchTitleOffset = resp.titleHasMore ? resp.titleMatches.length : undefined;
	const prefetchContentOffset = resp.contentHasMore ? resp.contentMatches.length : undefined;
	return {
		state: {
			...prev,
			backendSynced: true,
			loadingFirstPage: false,
			snapshotToken: resp.snapshotToken,
			titleMatches: resp.titleMatches,
			titleTotal: resp.titleTotal,
			titleHasMore: resp.titleHasMore,
			prefetchedTitles: null,
			prefetchTitleInFlight: prefetchTitleOffset !== undefined,
			contentMatches: resp.contentMatches,
			contentTotal: resp.contentTotal,
			contentHasMore: resp.contentHasMore,
			prefetchedContents: null,
			prefetchContentInFlight: prefetchContentOffset !== undefined,
		},
		prefetchTitleOffset,
		prefetchContentOffset,
	};
}

export function replaceExpiredSearchSnapshot(
	prev: SidebarSearchState,
	seq: number,
	resp: SessionSearchResponse,
): SidebarSearchState {
	if (
		seq !== prev.requestSeq ||
		resp.query !== prev.query ||
		!resp.snapshotToken ||
		resp.snapshotToken === prev.snapshotToken
	)
		return prev;
	const { state } = applyFirstSearchPage(prev, seq, resp);
	return {
		...state,
		requestSeq: seq + 1,
		prefetchTitleInFlight: false,
		prefetchContentInFlight: false,
		autoExpandTitlePending: false,
		autoExpandContentPending: false,
	};
}

function appendUnique<T extends { sessionId: string }>(target: readonly T[], incoming: readonly T[]): T[] {
	const seen = new Set(target.map((item) => item.sessionId));
	const next = [...target];
	for (const item of incoming) {
		if (!seen.has(item.sessionId)) {
			seen.add(item.sessionId);
			next.push(item);
		}
	}
	return next;
}

export function finishTitlePrefetch(
	prev: SidebarSearchState,
	seq: number,
	items: readonly TitleSearchMatch[],
	hasMore: boolean,
): { state: SidebarSearchState; nextPrefetchOffset?: number } {
	if (seq !== prev.requestSeq) return { state: prev };
	if (prev.autoExpandTitlePending) {
		const merged = appendUnique(prev.titleMatches, items);
		const stillMore = hasMore && merged.length < prev.titleTotal;
		const nextPrefetchOffset = stillMore ? merged.length : undefined;
		return {
			state: {
				...prev,
				titleMatches: merged,
				titleHasMore: stillMore,
				prefetchedTitles: null,
				autoExpandTitlePending: false,
				prefetchTitleInFlight: nextPrefetchOffset !== undefined,
			},
			nextPrefetchOffset,
		};
	}
	return {
		state: {
			...prev,
			prefetchedTitles: { items: [...items], hasMore },
			prefetchTitleInFlight: false,
		},
	};
}

export function finishContentPrefetch(
	prev: SidebarSearchState,
	seq: number,
	items: readonly ContentSearchMatch[],
	hasMore: boolean,
): { state: SidebarSearchState; nextPrefetchOffset?: number } {
	if (seq !== prev.requestSeq) return { state: prev };
	if (prev.autoExpandContentPending) {
		const merged = appendUnique(prev.contentMatches, items);
		const stillMore = hasMore && merged.length < prev.contentTotal;
		const nextPrefetchOffset = stillMore ? merged.length : undefined;
		return {
			state: {
				...prev,
				contentMatches: merged,
				contentHasMore: stillMore,
				prefetchedContents: null,
				autoExpandContentPending: false,
				prefetchContentInFlight: nextPrefetchOffset !== undefined,
			},
			nextPrefetchOffset,
		};
	}
	return {
		state: {
			...prev,
			prefetchedContents: { items: [...items], hasMore },
			prefetchContentInFlight: false,
		},
	};
}

export function clickExpandTitles(prev: SidebarSearchState): { state: SidebarSearchState; action: ExpandAction } {
	if (!prev.titleHasMore) return { state: prev, action: { kind: "none" } };
	if (prev.prefetchedTitles) {
		const merged = appendUnique(prev.titleMatches, prev.prefetchedTitles.items);
		const stillMore = prev.prefetchedTitles.hasMore && merged.length < prev.titleTotal;
		const nextPrefetchOffset = stillMore ? merged.length : undefined;
		return {
			state: {
				...prev,
				titleMatches: merged,
				titleHasMore: stillMore,
				prefetchedTitles: null,
				prefetchTitleInFlight: nextPrefetchOffset !== undefined,
			},
			action: { kind: "appended", nextPrefetchOffset },
		};
	}
	if (prev.prefetchTitleInFlight) {
		return {
			state: { ...prev, autoExpandTitlePending: true },
			action: { kind: "waiting_in_flight" },
		};
	}
	return {
		state: { ...prev, prefetchTitleInFlight: true, autoExpandTitlePending: true },
		action: { kind: "fetch_now", offset: prev.titleMatches.length },
	};
}

export function clickExpandContents(prev: SidebarSearchState): { state: SidebarSearchState; action: ExpandAction } {
	if (!prev.contentHasMore) return { state: prev, action: { kind: "none" } };
	if (prev.prefetchedContents) {
		const merged = appendUnique(prev.contentMatches, prev.prefetchedContents.items);
		const stillMore = prev.prefetchedContents.hasMore && merged.length < prev.contentTotal;
		const nextPrefetchOffset = stillMore ? merged.length : undefined;
		return {
			state: {
				...prev,
				contentMatches: merged,
				contentHasMore: stillMore,
				prefetchedContents: null,
				prefetchContentInFlight: nextPrefetchOffset !== undefined,
			},
			action: { kind: "appended", nextPrefetchOffset },
		};
	}
	if (prev.prefetchContentInFlight) {
		return {
			state: { ...prev, autoExpandContentPending: true },
			action: { kind: "waiting_in_flight" },
		};
	}
	return {
		state: { ...prev, prefetchContentInFlight: true, autoExpandContentPending: true },
		action: { kind: "fetch_now", offset: prev.contentMatches.length },
	};
}

function kindPriority(kind: SearchContentKind): number {
	switch (kind) {
		case "user":
		case "assistant":
			return 2;
		case "summary":
			return 1;
		case "tool":
			return 0;
	}
}

function kindSnippetPrefix(kind: SearchContentKind, onActiveBranch: boolean): string {
	if (!onActiveBranch) return "历史分支 · ";
	if (kind === "tool") return "工具 · ";
	if (kind === "summary") return "总结 · ";
	return "";
}

function extractEntrySearchable(
	entry: SessionDetail["entries"][number],
): { entryId: string; kind: SearchContentKind; timestamp: number; text: string } | null {
	const msg = entry.message;
	const ts = typeof msg.timestamp === "number" ? msg.timestamp : 0;
	if (msg.role === "user" || msg.role === "assistant") {
		const raw =
			typeof msg.content === "string"
				? msg.content
				: Array.isArray(msg.content)
					? msg.content
							.filter((part): part is { type: "text"; text: string } => part.type === "text")
							.map((part) => part.text)
							.join("\n")
					: "";
		const clean = collapseWhitespace(raw);
		return clean ? { entryId: entry.id, kind: msg.role, timestamp: ts, text: clean } : null;
	}
	if (msg.role === "toolResult") {
		if ((msg.toolName ?? "").toLowerCase() === "bitlocker") return null;
		const raw =
			typeof msg.content === "string"
				? msg.content
				: Array.isArray(msg.content)
					? msg.content
							.filter((part): part is { type: "text"; text: string } => part.type === "text")
							.map((part) => part.text)
							.join("\n")
					: "";
		const clean = collapseWhitespace(raw);
		return clean ? { entryId: entry.id, kind: "tool", timestamp: ts, text: clean } : null;
	}
	if (msg.role === "branchSummary" || msg.role === "compactionSummary") {
		const clean = collapseWhitespace(msg.summary ?? "");
		return clean ? { entryId: entry.id, kind: "summary", timestamp: ts, text: clean } : null;
	}
	return null;
}

/** 0ms 本地预搜索：遍历全量会话标题与已预载的近 10 个会话活动分支，置顶当前打开会话，每组上限 10 条。 */
export function searchLocalPreloadedSessions(
	query: string,
	activeSessionId: string | undefined,
	sessions: readonly SessionRow[],
	cachedDetails: ReadonlyMap<string, SessionDetail>,
): SessionSearchResponse {
	const trimmed = query.trim();
	if (!trimmed) {
		return {
			query: "",
			snapshotToken: "",
			titleMatches: [],
			titleTotal: 0,
			titleHasMore: false,
			contentMatches: [],
			contentTotal: 0,
			contentHasMore: false,
		};
	}

	const titleScored: { active: boolean; score: number; updatedAt: string; item: TitleSearchMatch }[] = [];
	const contentScored: {
		active: boolean;
		score: number;
		prio: number;
		hitCount: number;
		updatedAt: string;
		item: ContentSearchMatch;
	}[] = [];

	for (const session of sessions) {
		const title = session.title.trim() || "新对话";
		const isPlaceholder = !session.title.trim() || title === "新对话";
		const isActive = activeSessionId === session.id;
		const sortTs = session.updatedAt ?? "";

		if (!isPlaceholder || trimmed === "新对话") {
			const hit = matchQuery(title, trimmed);
			if (hit) {
				titleScored.push({
					active: isActive,
					score: hit.score,
					updatedAt: sortTs,
					item: {
						sessionId: session.id,
						title,
						updatedAt: session.updatedAt,
						score: hit.score,
					},
				});
			}
		}

		const detail = cachedDetails.get(session.id);
		if (!detail) continue;

		let hitCount = 0;
		let best: {
			score: number;
			prio: number;
			ts: number;
			entryId: string;
			kind: SearchContentKind;
			text: string;
		} | null = null;

		for (const entry of detail.entries) {
			const parsed = extractEntrySearchable(entry);
			if (!parsed) continue;
			const hit = matchQuery(parsed.text, trimmed);
			if (!hit) continue;
			hitCount += 1;
			const prio = kindPriority(parsed.kind);
			if (
				!best ||
				hit.score > best.score ||
				(hit.score === best.score && prio > best.prio) ||
				(hit.score === best.score && prio === best.prio && parsed.timestamp > best.ts)
			) {
				best = {
					score: hit.score,
					prio,
					ts: parsed.timestamp,
					entryId: parsed.entryId,
					kind: parsed.kind,
					text: parsed.text,
				};
			}
		}

		if (best) {
			const prefix = kindSnippetPrefix(best.kind, true);
			const { snippet, spans } = buildSnippet(best.text, trimmed, prefix, 36);
			contentScored.push({
				active: isActive,
				score: best.score,
				prio: best.prio,
				hitCount,
				updatedAt: sortTs,
				item: {
					sessionId: session.id,
					title,
					updatedAt: session.updatedAt,
					hitCount,
					score: best.score,
					bestHit: {
						entryId: best.entryId,
						kind: best.kind,
						snippet,
						onActiveBranch: true,
						spans,
					},
				},
			});
		}
	}

	titleScored.sort(
		(a, b) =>
			Number(b.active) - Number(a.active) ||
			b.score - a.score ||
			b.updatedAt.localeCompare(a.updatedAt) ||
			a.item.sessionId.localeCompare(b.item.sessionId),
	);
	contentScored.sort(
		(a, b) =>
			Number(b.active) - Number(a.active) ||
			b.score - a.score ||
			b.prio - a.prio ||
			b.hitCount - a.hitCount ||
			b.updatedAt.localeCompare(a.updatedAt) ||
			a.item.sessionId.localeCompare(b.item.sessionId),
	);

	return {
		query: trimmed,
		snapshotToken: "",
		titleMatches: titleScored.slice(0, SEARCH_PAGE_SIZE).map((x) => x.item),
		titleTotal: titleScored.length,
		titleHasMore: titleScored.length > SEARCH_PAGE_SIZE,
		contentMatches: contentScored.slice(0, SEARCH_PAGE_SIZE).map((x) => x.item),
		contentTotal: contentScored.length,
		contentHasMore: contentScored.length > SEARCH_PAGE_SIZE,
	};
}
