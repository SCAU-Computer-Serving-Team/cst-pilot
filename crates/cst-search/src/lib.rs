//! Shared fuzzy + Chinese Pinyin matching, snippet extraction, and session search DTOs
//! for `cst-server`, `cst-client`, and `cst-desktop`.

use std::sync::LazyLock;

use ib_matcher::matcher::{IbMatcher, PinyinMatchConfig};
use ib_matcher::pinyin::{PinyinData, PinyinNotation};
use serde::{Deserialize, Serialize};

pub const SCORE_EXACT: u32 = 1050;
pub const SCORE_PREFIX: u32 = 1000;
pub const SCORE_SUBSTRING: u32 = 800;
pub const SCORE_PINYIN_FULL: u32 = 650;
pub const SCORE_PINYIN_INITIAL: u32 = 550;
pub const SCORE_FUZZY_EDIT1: u32 = 260;
pub const SCORE_FUZZY_EDIT2: u32 = 220;
pub const SCORE_FUZZY_SUBSEQ: u32 = 200;

/// Maximum characters scanned by bounded fuzzy fallback to keep per-chunk latency predictable.
const MAX_FUZZY_SCAN_CHARS: usize = 1500;

static PINYIN_DATA: LazyLock<PinyinData> =
    LazyLock::new(|| PinyinData::new(PinyinNotation::Ascii | PinyinNotation::AsciiFirstLetter));

/// Character-index highlight range `[start, end)` in Unicode scalar values (matching JS string slices
/// for BMP text and Rust `.chars()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryMatch {
    pub score: u32,
    pub spans: Vec<MatchSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchContentKind {
    User,
    Assistant,
    Summary,
    Tool,
}

impl SearchContentKind {
    pub fn rank_priority(self) -> u8 {
        match self {
            Self::User | Self::Assistant => 3,
            Self::Summary => 2,
            Self::Tool => 1,
        }
    }

    pub fn snippet_prefix(self, on_active_branch: bool) -> &'static str {
        if !on_active_branch {
            "历史分支 · "
        } else {
            match self {
                Self::User | Self::Assistant => "",
                Self::Summary => "总结 · ",
                Self::Tool => "工具 · ",
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleSearchMatchDto {
    pub session_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub score: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchBestHitDto {
    pub entry_id: String,
    pub kind: SearchContentKind,
    pub snippet: String,
    pub on_active_branch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_leaf_id: Option<String>,
    #[serde(default)]
    pub spans: Vec<MatchSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchMatchDto {
    pub session_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub hit_count: usize,
    pub score: u32,
    pub best_hit: ContentSearchBestHitDto,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchResponseDto {
    pub query: String,
    pub snapshot_token: String,
    pub title_matches: Vec<TitleSearchMatchDto>,
    pub title_total: usize,
    pub title_has_more: bool,
    pub content_matches: Vec<ContentSearchMatchDto>,
    pub content_total: usize,
    pub content_has_more: bool,
}

/// Normalizes fullwidth ASCII/space and lowercases characters while keeping 1:1 char positions
/// for BMP text (CJK + ASCII + symbols).
pub fn normalize_char(c: char) -> char {
    let code = c as u32;
    let mapped = if (0xFF01..=0xFF5E).contains(&code) {
        char::from_u32(code - 0xFEE0).unwrap_or(c)
    } else if code == 0x3000 {
        ' '
    } else {
        c
    };
    mapped.to_lowercase().next().unwrap_or(mapped)
}

pub fn normalize_text(input: &str) -> String {
    input.chars().map(normalize_char).collect()
}

pub fn split_query_terms(query: &str) -> Vec<String> {
    normalize_text(query)
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_cjk(c: char) -> bool {
    let code = c as u32;
    (0x4E00..=0x9FFF).contains(&code)
        || (0x3400..=0x4DBF).contains(&code)
        || (0x20000..=0x2A6DF).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
}

fn has_ascii_alpha(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_alphabetic())
}

fn has_cjk(chars: &[char]) -> bool {
    chars.iter().copied().any(is_cjk)
}

fn byte_range_to_char_span(s: &str, start_byte: usize, end_byte: usize) -> MatchSpan {
    let start = s[..start_byte].chars().count();
    let len = s[start_byte..end_byte].chars().count();
    MatchSpan {
        start,
        end: start + len,
    }
}

fn match_pinyin_term(norm_text: &str, norm_chars: &[char], term: &str) -> Option<(u32, MatchSpan)> {
    if !has_ascii_alpha(term) || !has_cjk(norm_chars) {
        return None;
    }

    // 1. Check strict full-pinyin first (`Ascii` notation only) so full pinyin outranks initials.
    let full_matcher = IbMatcher::builder(term)
        .case_insensitive(true)
        .pinyin(
            PinyinMatchConfig::builder(PinyinNotation::Ascii)
                .data(&PINYIN_DATA)
                .build(),
        )
        .build();
    if let Some(m) = full_matcher.find(norm_text) {
        let span = byte_range_to_char_span(norm_text, m.start(), m.end());
        let matched_slice = &norm_chars[span.start..span.end];
        let has_matched_cjk = matched_slice.iter().copied().any(is_cjk);
        if has_matched_cjk {
            return Some((SCORE_PINYIN_FULL, span));
        }
    }

    // 2. Check combined full + initial pinyin (`Ascii | AsciiFirstLetter`).
    let mixed_matcher = IbMatcher::builder(term)
        .case_insensitive(true)
        .pinyin(
            PinyinMatchConfig::builder(PinyinNotation::Ascii | PinyinNotation::AsciiFirstLetter)
                .data(&PINYIN_DATA)
                .build(),
        )
        .build();
    if let Some(m) = mixed_matcher.find(norm_text) {
        let span = byte_range_to_char_span(norm_text, m.start(), m.end());
        return Some((SCORE_PINYIN_INITIAL, span));
    }

    // 3. Space-stripped fallback for mixed queries like `cpankongjian` vs `c 盘空间`.
    if norm_chars.contains(&' ') {
        let mut compact = String::with_capacity(norm_text.len());
        let mut idx_map: Vec<usize> = Vec::with_capacity(norm_chars.len());
        for (i, &ch) in norm_chars.iter().enumerate() {
            if !ch.is_whitespace() {
                compact.push(ch);
                idx_map.push(i);
            }
        }
        if let Some(m) = full_matcher.find(compact.as_str()) {
            let cs = byte_range_to_char_span(&compact, m.start(), m.end());
            if cs.start < idx_map.len() && cs.end > cs.start && cs.end <= idx_map.len() {
                return Some((
                    SCORE_PINYIN_FULL,
                    MatchSpan {
                        start: idx_map[cs.start],
                        end: idx_map[cs.end - 1] + 1,
                    },
                ));
            }
        }
        if let Some(m) = mixed_matcher.find(compact.as_str()) {
            let cs = byte_range_to_char_span(&compact, m.start(), m.end());
            if cs.start < idx_map.len() && cs.end > cs.start && cs.end <= idx_map.len() {
                return Some((
                    SCORE_PINYIN_INITIAL,
                    MatchSpan {
                        start: idx_map[cs.start],
                        end: idx_map[cs.end - 1] + 1,
                    },
                ));
            }
        }
    }

    None
}

fn shared_char_count(term_chars: &[char], window: &[char]) -> usize {
    let mut matched = 0usize;
    let mut used = [false; 64];
    let limit = window.len().min(64);
    for &tc in term_chars {
        for (idx, &wc) in window[..limit].iter().enumerate() {
            if !used[idx] && wc == tc {
                used[idx] = true;
                matched += 1;
                break;
            }
        }
    }
    matched
}

fn match_bounded_edit(norm_chars: &[char], term_chars: &[char]) -> Option<(u32, MatchSpan)> {
    let tlen = term_chars.len();
    if !(3..=48).contains(&tlen) {
        return None;
    }
    let max_dist = if tlen <= 5 { 1 } else { 2 };
    let scan_len = norm_chars.len().min(MAX_FUZZY_SCAN_CHARS);
    if scan_len < tlen.saturating_sub(max_dist) {
        return None;
    }

    let term_str: String = term_chars.iter().collect();
    let min_w = tlen.saturating_sub(max_dist).max(2);
    let max_w = (tlen + max_dist).min(scan_len);

    let mut best: Option<(usize, MatchSpan)> = None;
    let mut buf = String::with_capacity(max_w * 4);

    for start in 0..scan_len {
        // Avoid starting in the middle of spaces
        if norm_chars[start].is_whitespace() {
            continue;
        }
        for wlen in min_w..=max_w {
            let end = start + wlen;
            if end > scan_len {
                break;
            }
            if norm_chars[end - 1].is_whitespace() {
                continue;
            }
            let window = &norm_chars[start..end];
            // If window crosses whitespace and query has no whitespace, reject to avoid stitching unrelated words
            if window.iter().any(|c| c.is_whitespace()) {
                continue;
            }
            if shared_char_count(term_chars, window) < tlen.saturating_sub(max_dist) {
                continue;
            }
            if tlen == 3 && shared_char_count(term_chars, window) < 2 {
                continue;
            }
            buf.clear();
            for &ch in window {
                buf.push(ch);
            }
            let dist = strsim::osa_distance(&term_str, &buf);
            if dist <= max_dist {
                // For very short 3-char queries, require first or last char to match to suppress random noise
                if tlen == 3
                    && term_chars[0] != window[0]
                    && term_chars[2] != window[window.len() - 1]
                {
                    continue;
                }
                let replace = match best {
                    None => true,
                    Some((best_d, _)) => dist < best_d,
                };
                if replace {
                    best = Some((dist, MatchSpan { start, end }));
                    if dist == 1 {
                        // Optimal edit distance found at earliest start
                        return Some((SCORE_FUZZY_EDIT1, MatchSpan { start, end }));
                    }
                }
            }
        }
    }

    best.map(|(dist, span)| {
        let score = if dist <= 1 {
            SCORE_FUZZY_EDIT1
        } else {
            SCORE_FUZZY_EDIT2
        };
        (score, span)
    })
}

fn match_bounded_subsequence(norm_chars: &[char], term_chars: &[char]) -> Option<(u32, MatchSpan)> {
    let tlen = term_chars.len();
    if tlen < 3 {
        return None;
    }
    let scan_len = norm_chars.len().min(MAX_FUZZY_SCAN_CHARS);
    const MAX_GAP: usize = 4;

    let mut best: Option<(usize, MatchSpan)> = None;
    for start in 0..scan_len {
        if norm_chars[start] != term_chars[0] {
            continue;
        }
        let mut prev = start;
        let mut matched_all = true;
        for &tc in &term_chars[1..] {
            let search_end = (prev + 1 + MAX_GAP + 1).min(scan_len);
            let mut found = None;
            for (idx, &hc) in norm_chars
                .iter()
                .enumerate()
                .take(search_end)
                .skip(prev + 1)
            {
                if hc == tc {
                    found = Some(idx);
                    break;
                }
            }
            if let Some(pos) = found {
                prev = pos;
            } else {
                matched_all = false;
                break;
            }
        }
        if matched_all {
            let span_len = prev - start + 1;
            // Require >= 60% coverage (`tlen * 10 >= span_len * 6`)
            if tlen * 10 >= span_len * 6 {
                let candidate = MatchSpan {
                    start,
                    end: prev + 1,
                };
                let replace = match best {
                    None => true,
                    Some((best_len, _)) => span_len < best_len,
                };
                if replace {
                    best = Some((span_len, candidate));
                }
            }
        }
    }

    best.map(|(_, span)| (SCORE_FUZZY_SUBSEQ, span))
}

fn match_single_term(norm_text: &str, norm_chars: &[char], term: &str) -> Option<(u32, MatchSpan)> {
    let term_chars: Vec<char> = term.chars().collect();
    if term_chars.is_empty() || norm_chars.is_empty() {
        return None;
    }

    // Tier 1: exact / prefix
    if norm_text == term {
        return Some((
            SCORE_EXACT,
            MatchSpan {
                start: 0,
                end: norm_chars.len(),
            },
        ));
    }
    if norm_text.starts_with(term) {
        return Some((
            SCORE_PREFIX,
            MatchSpan {
                start: 0,
                end: term_chars.len(),
            },
        ));
    }

    // Tier 2: contiguous substring
    if let Some(byte_pos) = norm_text.find(term) {
        let start = norm_text[..byte_pos].chars().count();
        return Some((
            SCORE_SUBSTRING,
            MatchSpan {
                start,
                end: start + term_chars.len(),
            },
        ));
    }

    // Tier 3 & 4: Chinese Pinyin (full / initials / mixed)
    if let Some(hit) = match_pinyin_term(norm_text, norm_chars, term) {
        return Some(hit);
    }

    // Tier 5: bounded fuzzy (disabled for short 1-2 char queries)
    if term_chars.len() >= 3 {
        if let Some(hit) = match_bounded_edit(norm_chars, &term_chars) {
            return Some(hit);
        }
        if let Some(hit) = match_bounded_subsequence(norm_chars, &term_chars) {
            return Some(hit);
        }
    }

    None
}

pub fn merge_spans(mut spans: Vec<MatchSpan>) -> Vec<MatchSpan> {
    if spans.is_empty() {
        return spans;
    }
    spans.sort_by_key(|s| (s.start, s.end));
    let mut merged: Vec<MatchSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.start >= span.end {
            continue;
        }
        if let Some(last) = merged.last_mut()
            && span.start <= last.end
        {
            last.end = last.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged
}

/// Matches a normalized or raw query against raw `text`.
/// All whitespace-separated query terms must match (AND semantics).
pub fn match_query(text: &str, query: &str) -> Option<QueryMatch> {
    let norm_query = normalize_text(query).trim().to_owned();
    if norm_query.is_empty() {
        return None;
    }
    let norm_text = normalize_text(text);
    let norm_chars: Vec<char> = norm_text.chars().collect();
    if norm_chars.is_empty() {
        return None;
    }

    // Whole-phrase literal tiers first so queries containing spaces like "c 盘空间"
    // earn exact/prefix/substring scores before falling back to multi-term AND.
    let q_char_len = norm_query.chars().count();
    if norm_text == norm_query {
        return Some(QueryMatch {
            score: SCORE_EXACT,
            spans: vec![MatchSpan {
                start: 0,
                end: norm_chars.len(),
            }],
        });
    }
    if norm_text.starts_with(&norm_query) {
        return Some(QueryMatch {
            score: SCORE_PREFIX,
            spans: vec![MatchSpan {
                start: 0,
                end: q_char_len,
            }],
        });
    }
    if let Some(byte_pos) = norm_text.find(&norm_query) {
        let start = norm_text[..byte_pos].chars().count();
        return Some(QueryMatch {
            score: SCORE_SUBSTRING,
            spans: vec![MatchSpan {
                start,
                end: start + q_char_len,
            }],
        });
    }

    let terms = split_query_terms(&norm_query);
    if terms.is_empty() {
        return None;
    }

    let mut total_score = 0u64;
    let mut spans = Vec::with_capacity(terms.len());
    for term in &terms {
        let (score, span) = match_single_term(&norm_text, &norm_chars, term)?;
        total_score += u64::from(score);
        spans.push(span);
    }

    let avg_score = (total_score / terms.len() as u64) as u32;
    Some(QueryMatch {
        score: avg_score,
        spans: merge_spans(spans),
    })
}

/// Collapses whitespace/newlines in `text` into single spaces for compact snippet rendering.
pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(512));
    let mut prev_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Extracts a compact single-line snippet centered around the first matched span,
/// prepending the optional prefix (`""`, `"总结 · "`, `"工具 · "`, `"历史分支 · "`).
pub fn build_snippet(
    raw_text: &str,
    query: &str,
    prefix: &str,
    max_body_chars: usize,
) -> (String, Vec<MatchSpan>) {
    let clean = collapse_whitespace(raw_text);
    let chars: Vec<char> = clean.chars().collect();
    if chars.is_empty() {
        return (prefix.to_owned(), Vec::new());
    }
    let max_body = max_body_chars.max(12);
    let matched = match_query(&clean, query);
    let first_start = matched
        .as_ref()
        .and_then(|m| m.spans.first())
        .map(|s| s.start)
        .unwrap_or(0);

    let (win_start, win_end) = if chars.len() <= max_body {
        (0, chars.len())
    } else {
        let lead = (max_body / 3).min(first_start);
        let mut s = first_start.saturating_sub(lead);
        let mut e = (s + max_body).min(chars.len());
        if e - s < max_body {
            s = e.saturating_sub(max_body);
        }
        if e < chars.len() {
            e = e.min(chars.len());
        }
        (s, e)
    };

    let mut snippet = String::with_capacity(prefix.len() + max_body * 4 + 8);
    snippet.push_str(prefix);
    let has_leading = win_start > 0;
    if has_leading {
        snippet.push('…');
    }
    let offset = snippet.chars().count();
    for &ch in &chars[win_start..win_end] {
        snippet.push(ch);
    }
    if win_end < chars.len() {
        snippet.push('…');
    }

    let adjusted_spans = matched
        .map(|m| {
            m.spans
                .into_iter()
                .filter_map(|sp| {
                    let s = sp.start.max(win_start);
                    let e = sp.end.min(win_end);
                    (s < e).then(|| MatchSpan {
                        start: offset + (s - win_start),
                        end: offset + (e - win_start),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    (snippet, adjusted_spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GoldenFixture {
        cases: Vec<GoldenCase>,
        ordering_pairs: Vec<OrderingPair>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GoldenCase {
        name: String,
        text: String,
        query: String,
        expect_match: bool,
        min_score: u32,
        max_score: u32,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OrderingPair {
        name: String,
        query: String,
        higher: String,
        lower: String,
    }

    #[test]
    fn shared_golden_fixture_matches_contract() {
        let raw = include_str!("../../../tests/fixtures/search-golden.json");
        let fixture: GoldenFixture = serde_json::from_str(raw).expect("valid golden fixture");

        for case in &fixture.cases {
            let res = match_query(&case.text, &case.query);
            assert_eq!(
                res.is_some(),
                case.expect_match,
                "case `{}` (text={:?}, query={:?}) match expectation mismatch: got {:?}",
                case.name,
                case.text,
                case.query,
                res
            );
            if let Some(m) = res {
                assert!(
                    m.score >= case.min_score && m.score <= case.max_score,
                    "case `{}` score {} out of expected [{}..={}]",
                    case.name,
                    m.score,
                    case.min_score,
                    case.max_score
                );
                assert!(
                    !m.spans.is_empty(),
                    "case `{}` returned empty highlight spans",
                    case.name
                );
                let char_len = case.text.chars().count();
                for sp in &m.spans {
                    assert!(
                        sp.start < sp.end && sp.end <= char_len,
                        "case `{}` invalid span {:?} for len {}",
                        case.name,
                        sp,
                        char_len
                    );
                }
            }
        }

        for pair in &fixture.ordering_pairs {
            let hi = match_query(&pair.higher, &pair.query)
                .unwrap_or_else(|| panic!("pair `{}` higher did not match", pair.name));
            let lo = match_query(&pair.lower, &pair.query)
                .unwrap_or_else(|| panic!("pair `{}` lower did not match", pair.name));
            assert!(
                hi.score > lo.score,
                "pair `{}` expected higher ({}) > lower ({})",
                pair.name,
                hi.score,
                lo.score
            );
        }
    }

    #[test]
    fn snippet_centers_around_match_with_prefix() {
        let long = "这是一段很长的前导诊断说明，用来检查系统盘占用情况以及各种历史记录，空间主要由休眠文件与本地缓存占用，建议按步骤释放。";
        let (snippet, spans) = build_snippet(long, "huancun", "工具 · ", 24);
        assert!(snippet.starts_with("工具 · "));
        assert!(snippet.contains("缓存"));
        assert!(!spans.is_empty());
    }
}
