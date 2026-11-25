import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { buildSnippet, matchQuery, scalarSpansToUtf16 } from "./search-matcher.ts";

type GoldenCase = {
	name: string;
	text: string;
	query: string;
	expectMatch: boolean;
	minScore: number;
	maxScore: number;
};

type OrderingPair = {
	name: string;
	query: string;
	higher: string;
	lower: string;
};

type GoldenFixture = {
	cases: GoldenCase[];
	orderingPairs: OrderingPair[];
};

const fixturePath = path.resolve(import.meta.dirname, "../../../../../tests/fixtures/search-golden.json");
const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf8")) as GoldenFixture;

test("shared golden fixture matches contract identically to Rust cst-search", () => {
	for (const item of fixture.cases) {
		const res = matchQuery(item.text, item.query);
		assert.equal(
			res !== null,
			item.expectMatch,
			`case "${item.name}" (text=${JSON.stringify(item.text)}, query=${JSON.stringify(item.query)}) got ${JSON.stringify(res)}`,
		);
		if (res) {
			assert.ok(
				res.score >= item.minScore && res.score <= item.maxScore,
				`case "${item.name}" score ${res.score} out of [${item.minScore}..${item.maxScore}]`,
			);
			assert.ok(res.spans.length > 0, `case "${item.name}" should return highlight spans`);
			for (const sp of res.spans) {
				assert.ok(
					sp.start >= 0 && sp.start < sp.end && sp.end <= item.text.length,
					`case "${item.name}" invalid span ${JSON.stringify(sp)}`,
				);
			}
		}
	}

	for (const pair of fixture.orderingPairs) {
		const hi = matchQuery(pair.higher, pair.query);
		const lo = matchQuery(pair.lower, pair.query);
		assert.ok(hi, `pair "${pair.name}" higher did not match`);
		assert.ok(lo, `pair "${pair.name}" lower did not match`);
		assert.ok(hi.score > lo.score, `pair "${pair.name}" expected ${hi.score} > ${lo.score}`);
	}
});

test("API spans use scalar offsets for Chinese and non-BMP characters", () => {
	const text = "工具 · 🙂显卡";
	const spans = scalarSpansToUtf16(text, [
		{ start: 5, end: 8 },
		{ start: 6, end: 8 },
		{ start: 7, end: 20 },
	]);
	assert.deepEqual(
		spans.map(({ start, end }) => text.slice(start, end)),
		["🙂显卡", "显卡"],
	);
});

test("literal and fuzzy matches stay on character boundaries", () => {
	const literal = matchQuery("İ🙂显卡", "显卡");
	assert.deepEqual(literal?.spans, [{ start: 3, end: 5 }]);
	assert.equal(matchQuery("🙂😂", "🙂🙃"), null);
	const { snippet, spans } = buildSnippet(`${"🙂".repeat(30)}显卡${"🙂".repeat(30)}`, "显卡", "工具 · ", 12);
	assert.ok([...snippet].every((char) => !/^[\uD800-\uDFFF]$/u.test(char)));
	assert.deepEqual(
		spans.map(({ start, end }) => snippet.slice(start, end)),
		["显卡"],
	);
});

test("buildSnippet centers around matched span and keeps prefix", () => {
	const long =
		"这是一段很长的前导诊断说明，用来检查系统盘占用情况以及各种历史记录，空间主要由休眠文件与本地缓存占用，建议按步骤释放。";
	const { snippet, spans } = buildSnippet(long, "huancun", "工具 · ", 24);
	assert.ok(snippet.startsWith("工具 · "));
	assert.ok(snippet.includes("缓存"));
	assert.ok(spans.length > 0);
});
