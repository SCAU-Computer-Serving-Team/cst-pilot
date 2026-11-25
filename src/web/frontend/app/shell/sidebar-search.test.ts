import assert from "node:assert/strict";
import { test } from "node:test";
import type { ContentSearchMatch, SessionDetail, SessionRow, TitleSearchMatch } from "../data/web-state.ts";
import {
	applyFirstSearchPage,
	beginSidebarSearchQuery,
	clickExpandContents,
	createInitialSidebarSearchState,
	finishContentPrefetch,
	searchLocalPreloadedSessions,
} from "./sidebar-search.ts";

function makeTitle(id: string): TitleSearchMatch {
	return { sessionId: id, title: `会话 ${id}`, updatedAt: "2026-11-25T10:00:00Z", score: 800 };
}

function makeContent(id: string): ContentSearchMatch {
	return {
		sessionId: id,
		title: `会话 ${id}`,
		updatedAt: "2026-11-25T10:00:00Z",
		hitCount: 1,
		score: 800,
		bestHit: {
			entryId: `e-${id}`,
			kind: "assistant",
			snippet: "本地缓存占用",
			onActiveBranch: true,
			spans: [],
		},
	};
}

test("chained background prefetch appends next 10 items instantly on expand and prefetches further", () => {
	let state = createInitialSidebarSearchState();
	state = beginSidebarSearchQuery(state, "缓存", {
		query: "缓存",
		snapshotToken: "",
		titleMatches: [],
		titleTotal: 0,
		titleHasMore: false,
		contentMatches: [],
		contentTotal: 0,
		contentHasMore: false,
	});
	const seq = state.requestSeq;

	const first = applyFirstSearchPage(state, seq, {
		query: "缓存",
		snapshotToken: "tok-1",
		titleMatches: Array.from({ length: 10 }, (_, i) => makeTitle(`t${i}`)),
		titleTotal: 22,
		titleHasMore: true,
		contentMatches: Array.from({ length: 10 }, (_, i) => makeContent(`c${i}`)),
		contentTotal: 25,
		contentHasMore: true,
	});
	state = first.state;
	assert.equal(first.prefetchTitleOffset, 10);
	assert.equal(first.prefetchContentOffset, 10);
	assert.equal(state.prefetchContentInFlight, true);

	// 后台预取完第 11..20 条，不打扰当前列表
	const pf2 = finishContentPrefetch(
		state,
		seq,
		Array.from({ length: 10 }, (_, i) => makeContent(`c${i + 10}`)),
		true,
	);
	state = pf2.state;
	assert.equal(state.contentMatches.length, 10);
	assert.equal(pf2.nextPrefetchOffset, undefined);

	// 点击展开：立即追加 10..20，并立即触发预取 20..30
	const exp1 = clickExpandContents(state);
	state = exp1.state;
	assert.deepEqual(exp1.action, { kind: "appended", nextPrefetchOffset: 20 });
	assert.equal(state.contentMatches.length, 20);
	assert.equal(state.contentHasMore, true);
	assert.equal(state.prefetchContentInFlight, true);

	// 预取未完就再次点击展开：挂载待追加，完成时自动落盘
	const exp2 = clickExpandContents(state);
	state = exp2.state;
	assert.deepEqual(exp2.action, { kind: "waiting_in_flight" });

	const pf3 = finishContentPrefetch(
		state,
		seq,
		Array.from({ length: 5 }, (_, i) => makeContent(`c${i + 20}`)),
		false,
	);
	state = pf3.state;
	assert.equal(state.contentMatches.length, 25);
	assert.equal(state.contentHasMore, false);
});

test("searchLocalPreloadedSessions pins active session, includes tool results, and excludes thinking/bitlocker", () => {
	const sessions: SessionRow[] = [
		{ id: "s-newer", title: "浏览器卡顿", updatedAt: "2026-11-25T12:00:00Z", running: false },
		{ id: "s-active", title: "磁盘诊断", updatedAt: "2026-11-24T12:00:00Z", running: false },
	];
	const cached = new Map<string, SessionDetail>([
		[
			"s-newer",
			{
				id: "s-newer",
				running: false,
				messages: [],
				queue: { version: 0, paused: false, items: [] },
				ui: [],
				entries: [
					{
						id: "e1",
						message: {
							role: "assistant",
							timestamp: 1000,
							content: [
								{ type: "thinking", thinking: "秘密思考ONLY_THINK" },
								{ type: "text", text: "清理本地缓存目录" },
							],
						},
					},
					{
						id: "e-bl",
						message: {
							role: "toolResult",
							toolName: "bitlocker",
							timestamp: 1100,
							content: [{ type: "text", text: "RECOVERY_KEY_SECRET" }],
						},
					},
				],
			},
		],
		[
			"s-active",
			{
				id: "s-active",
				running: false,
				messages: [],
				queue: { version: 0, paused: false, items: [] },
				ui: [],
				entries: [
					{
						id: "e2",
						message: {
							role: "toolResult",
							toolName: "disk",
							timestamp: 900,
							content: [{ type: "text", text: "驱动安装包缓存占用 4.8 GB" }],
						},
					},
				],
			},
		],
	]);

	const hits = searchLocalPreloadedSessions("hc", "s-active", sessions, cached);
	assert.equal(hits.contentMatches.length, 2);
	assert.equal(hits.contentMatches[0]?.sessionId, "s-active");
	assert.equal(hits.contentMatches[0]?.bestHit.snippet.startsWith("工具 · "), true);

	assert.equal(searchLocalPreloadedSessions("ONLY_THINK", "s-active", sessions, cached).contentMatches.length, 0);
	assert.equal(
		searchLocalPreloadedSessions("RECOVERY_KEY_SECRET", "s-active", sessions, cached).contentMatches.length,
		0,
	);
});
