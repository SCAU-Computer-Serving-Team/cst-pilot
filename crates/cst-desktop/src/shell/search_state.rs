use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use cst_client::{
    ContentSearchBestHitDto, ContentSearchMatchDto, SearchContentKind, SessionSearchResponseDto,
    SessionSummary, SessionViewSnapshot, TitleSearchMatchDto,
};
use cst_search::{build_snippet, collapse_whitespace, match_query};
use serde_json::Value;

pub const SEARCH_PAGE_SIZE: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandAction {
    /// Prefetched batch was appended immediately; optionally prefetch the next page starting at `next_offset`.
    Appended { next_prefetch_offset: Option<usize> },
    /// A prefetch request for the next batch is already in flight; result will append upon completion.
    WaitingInFlight,
    /// No prefetched batch exists yet; caller should fetch starting at `offset`.
    FetchNow { offset: usize },
    /// No more results available.
    None,
}

/// Manages sidebar search results, instant local preview, debounced backend reconciliation,
/// and chained background prefetch of the next 10 items per group.
#[derive(Debug, Clone, Default)]
pub struct SidebarSearchState {
    pub query: String,
    pub request_seq: u64,
    pub snapshot_token: Option<String>,
    pub backend_synced: bool,
    pub loading_first_page: bool,

    pub title_matches: Vec<TitleSearchMatchDto>,
    pub title_total: usize,
    pub title_has_more: bool,
    pub prefetched_titles: Option<(Vec<TitleSearchMatchDto>, bool)>,
    pub prefetch_title_in_flight: bool,
    pub auto_expand_title_pending: bool,

    pub content_matches: Vec<ContentSearchMatchDto>,
    pub content_total: usize,
    pub content_has_more: bool,
    pub prefetched_contents: Option<(Vec<ContentSearchMatchDto>, bool)>,
    pub prefetch_content_in_flight: bool,
    pub auto_expand_content_pending: bool,
}

impl SidebarSearchState {
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn is_empty_result(&self) -> bool {
        self.title_matches.is_empty() && self.content_matches.is_empty()
    }

    pub fn clear(&mut self) {
        self.request_seq = self.request_seq.wrapping_add(1);
        self.query.clear();
        self.snapshot_token = None;
        self.backend_synced = false;
        self.loading_first_page = false;
        self.title_matches.clear();
        self.title_total = 0;
        self.title_has_more = false;
        self.prefetched_titles = None;
        self.prefetch_title_in_flight = false;
        self.auto_expand_title_pending = false;
        self.content_matches.clear();
        self.content_total = 0;
        self.content_has_more = false;
        self.prefetched_contents = None;
        self.prefetch_content_in_flight = false;
        self.auto_expand_content_pending = false;
    }

    /// Starts a new query immediately using 0ms local matches over all titles and preloaded sessions.
    pub fn begin_query(&mut self, query: String, local_preview: SessionSearchResponseDto) -> u64 {
        self.request_seq = self.request_seq.wrapping_add(1);
        self.query = query;
        self.snapshot_token = None;
        self.backend_synced = false;
        self.loading_first_page = true;

        self.title_matches = local_preview.title_matches;
        self.title_total = local_preview.title_total;
        self.title_has_more = local_preview.title_has_more;
        self.prefetched_titles = None;
        self.prefetch_title_in_flight = false;
        self.auto_expand_title_pending = false;

        self.content_matches = local_preview.content_matches;
        self.content_total = local_preview.content_total;
        self.content_has_more = local_preview.content_has_more;
        self.prefetched_contents = None;
        self.prefetch_content_in_flight = false;
        self.auto_expand_content_pending = false;

        self.request_seq
    }

    /// Merges the backend's authoritative page 1 (0..10) response.
    /// Returns `(prefetch_title_offset, prefetch_content_offset)` for immediate background preloading.
    pub fn apply_first_page(
        &mut self,
        seq: u64,
        resp: SessionSearchResponseDto,
    ) -> Option<(Option<usize>, Option<usize>)> {
        if seq != self.request_seq || !self.is_active() {
            return None;
        }
        self.backend_synced = true;
        self.loading_first_page = false;
        self.snapshot_token = (!resp.snapshot_token.is_empty()).then_some(resp.snapshot_token);

        self.title_matches = resp.title_matches;
        self.title_total = resp.title_total;
        self.title_has_more = resp.title_has_more;
        self.prefetched_titles = None;
        self.prefetch_title_in_flight = false;

        self.content_matches = resp.content_matches;
        self.content_total = resp.content_total;
        self.content_has_more = resp.content_has_more;
        self.prefetched_contents = None;
        self.prefetch_content_in_flight = false;

        let next_title = self.title_has_more.then_some(self.title_matches.len());
        let next_content = self.content_has_more.then_some(self.content_matches.len());
        if next_title.is_some() {
            self.prefetch_title_in_flight = true;
        }
        if next_content.is_some() {
            self.prefetch_content_in_flight = true;
        }
        Some((next_title, next_content))
    }

    pub fn mark_first_page_failed(&mut self, seq: u64) {
        if seq == self.request_seq {
            self.loading_first_page = false;
        }
    }

    /// Stores a background-prefetched title page (next 10). If the user clicked Expand while
    /// it was in flight, appends immediately and returns the offset for the next chained prefetch.
    pub fn finish_title_prefetch(
        &mut self,
        seq: u64,
        items: Vec<TitleSearchMatchDto>,
        has_more: bool,
    ) -> Option<usize> {
        if seq != self.request_seq {
            return None;
        }
        self.prefetch_title_in_flight = false;
        if self.auto_expand_title_pending {
            self.auto_expand_title_pending = false;
            append_unique_titles(&mut self.title_matches, items);
            self.title_has_more = has_more && self.title_matches.len() < self.title_total;
            if self.title_has_more {
                self.prefetch_title_in_flight = true;
                return Some(self.title_matches.len());
            }
            return None;
        }
        self.prefetched_titles = Some((items, has_more));
        None
    }

    pub fn finish_content_prefetch(
        &mut self,
        seq: u64,
        items: Vec<ContentSearchMatchDto>,
        has_more: bool,
    ) -> Option<usize> {
        if seq != self.request_seq {
            return None;
        }
        self.prefetch_content_in_flight = false;
        if self.auto_expand_content_pending {
            self.auto_expand_content_pending = false;
            append_unique_contents(&mut self.content_matches, items);
            self.content_has_more = has_more && self.content_matches.len() < self.content_total;
            if self.content_has_more {
                self.prefetch_content_in_flight = true;
                return Some(self.content_matches.len());
            }
            return None;
        }
        self.prefetched_contents = Some((items, has_more));
        None
    }

    pub fn fail_title_prefetch(&mut self, seq: u64) {
        if seq == self.request_seq {
            self.prefetch_title_in_flight = false;
            self.auto_expand_title_pending = false;
        }
    }

    pub fn fail_content_prefetch(&mut self, seq: u64) {
        if seq == self.request_seq {
            self.prefetch_content_in_flight = false;
            self.auto_expand_content_pending = false;
        }
    }

    pub fn click_expand_titles(&mut self) -> ExpandAction {
        if !self.title_has_more {
            return ExpandAction::None;
        }
        if let Some((items, has_more)) = self.prefetched_titles.take() {
            append_unique_titles(&mut self.title_matches, items);
            self.title_has_more = has_more && self.title_matches.len() < self.title_total;
            let next_offset = if self.title_has_more {
                self.prefetch_title_in_flight = true;
                Some(self.title_matches.len())
            } else {
                None
            };
            return ExpandAction::Appended {
                next_prefetch_offset: next_offset,
            };
        }
        if self.prefetch_title_in_flight {
            self.auto_expand_title_pending = true;
            return ExpandAction::WaitingInFlight;
        }
        self.prefetch_title_in_flight = true;
        self.auto_expand_title_pending = true;
        ExpandAction::FetchNow {
            offset: self.title_matches.len(),
        }
    }

    pub fn click_expand_contents(&mut self) -> ExpandAction {
        if !self.content_has_more {
            return ExpandAction::None;
        }
        if let Some((items, has_more)) = self.prefetched_contents.take() {
            append_unique_contents(&mut self.content_matches, items);
            self.content_has_more = has_more && self.content_matches.len() < self.content_total;
            let next_offset = if self.content_has_more {
                self.prefetch_content_in_flight = true;
                Some(self.content_matches.len())
            } else {
                None
            };
            return ExpandAction::Appended {
                next_prefetch_offset: next_offset,
            };
        }
        if self.prefetch_content_in_flight {
            self.auto_expand_content_pending = true;
            return ExpandAction::WaitingInFlight;
        }
        self.prefetch_content_in_flight = true;
        self.auto_expand_content_pending = true;
        ExpandAction::FetchNow {
            offset: self.content_matches.len(),
        }
    }
}

fn append_unique_titles(target: &mut Vec<TitleSearchMatchDto>, incoming: Vec<TitleSearchMatchDto>) {
    let existing: HashSet<String> = target.iter().map(|t| t.session_id.clone()).collect();
    for item in incoming {
        if !existing.contains(&item.session_id) {
            target.push(item);
        }
    }
}

fn append_unique_contents(
    target: &mut Vec<ContentSearchMatchDto>,
    incoming: Vec<ContentSearchMatchDto>,
) {
    let existing: HashSet<String> = target.iter().map(|c| c.session_id.clone()).collect();
    for item in incoming {
        if !existing.contains(&item.session_id) {
            target.push(item);
        }
    }
}

fn extract_entry_searchable(entry: &Value) -> Option<(String, SearchContentKind, u64, String)> {
    let entry_id = entry.get("id").and_then(Value::as_str)?.to_owned();
    let msg = entry.get("message").unwrap_or(entry);
    let role = msg.get("role").and_then(Value::as_str)?;
    let ts = msg.get("timestamp").and_then(Value::as_u64).unwrap_or(0);

    match role {
        "user" => {
            let text = extract_json_content_text(msg.get("content")?);
            let clean = collapse_whitespace(&text);
            (!clean.is_empty()).then_some((entry_id, SearchContentKind::User, ts, clean))
        }
        "assistant" => {
            let text = extract_json_content_text(msg.get("content")?);
            let clean = collapse_whitespace(&text);
            (!clean.is_empty()).then_some((entry_id, SearchContentKind::Assistant, ts, clean))
        }
        "toolResult" => {
            let tool_name = msg.get("toolName").and_then(Value::as_str).unwrap_or("");
            if tool_name.eq_ignore_ascii_case("bitlocker") {
                return None;
            }
            let text = extract_json_content_text(msg.get("content")?);
            let clean = collapse_whitespace(&text);
            (!clean.is_empty()).then_some((entry_id, SearchContentKind::Tool, ts, clean))
        }
        "branchSummary" | "compactionSummary" => {
            let text = msg.get("summary").and_then(Value::as_str)?;
            let clean = collapse_whitespace(text);
            (!clean.is_empty()).then_some((entry_id, SearchContentKind::Summary, ts, clean))
        }
        _ => None,
    }
}

fn extract_json_content_text(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|part| {
                // Only `type == "text"` blocks are searchable; skip `thinking`, `toolCall`, `image`.
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Runs a 0ms local search over all in-memory session titles + all preloaded session snapshots
/// (recent 10 + active session), capping each group at `SEARCH_PAGE_SIZE` (10) and pinning
/// `active_session_id` to index 0 when matched.
pub fn search_local_preloaded_sessions(
    query: &str,
    active_session_id: Option<&str>,
    sessions: &[SessionSummary],
    loaded_sessions: &BTreeMap<String, Arc<SessionViewSnapshot>>,
) -> SessionSearchResponseDto {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return SessionSearchResponseDto::default();
    }

    let mut title_scored: Vec<(bool, u32, String, TitleSearchMatchDto)> = Vec::new();
    let mut content_scored: Vec<(bool, u32, u8, usize, String, ContentSearchMatchDto)> = Vec::new();

    for sess in sessions {
        let title = if sess.title.trim().is_empty() {
            "新对话"
        } else {
            sess.title.as_str()
        };
        let is_placeholder = sess.title.trim().is_empty() || title == "新对话";
        let is_active = active_session_id == Some(sess.id.as_str());
        let sort_ts = sess.updated_at.clone().unwrap_or_default();

        if (!is_placeholder || trimmed == "新对话")
            && let Some(m) = match_query(title, trimmed)
        {
            title_scored.push((
                is_active,
                m.score,
                sort_ts.clone(),
                TitleSearchMatchDto {
                    session_id: sess.id.clone(),
                    title: title.to_owned(),
                    updated_at: sess.updated_at.clone(),
                    score: m.score,
                },
            ));
        }

        if let Some(view) = loaded_sessions.get(&sess.id) {
            let mut hit_count = 0usize;
            let mut best: Option<(u32, u8, u64, String, SearchContentKind, String)> = None;
            for entry in view.entries.iter() {
                let Some((eid, kind, ts, text)) = extract_entry_searchable(entry) else {
                    continue;
                };
                if let Some(m) = match_query(&text, trimmed) {
                    hit_count += 1;
                    let prio = kind.rank_priority();
                    let replace = match &best {
                        None => true,
                        Some((bs, bp, bt, _, _, _)) => (m.score, prio, ts) > (*bs, *bp, *bt),
                    };
                    if replace {
                        best = Some((m.score, prio, ts, eid, kind, text));
                    }
                }
            }
            if let Some((score, prio, _, entry_id, kind, text)) = best {
                let prefix = kind.snippet_prefix(true);
                let (snippet, spans) = build_snippet(&text, trimmed, prefix, 36);
                content_scored.push((
                    is_active,
                    score,
                    prio,
                    hit_count,
                    sort_ts,
                    ContentSearchMatchDto {
                        session_id: sess.id.clone(),
                        title: title.to_owned(),
                        updated_at: sess.updated_at.clone(),
                        hit_count,
                        score,
                        best_hit: ContentSearchBestHitDto {
                            entry_id,
                            kind,
                            snippet,
                            on_active_branch: true,
                            branch_leaf_id: None,
                            spans,
                        },
                    },
                ));
            }
        }
    }

    title_scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.session_id.cmp(&b.3.session_id))
    });
    content_scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| b.4.cmp(&a.4))
            .then_with(|| a.5.session_id.cmp(&b.5.session_id))
    });

    let title_total = title_scored.len();
    let title_has_more = title_total > SEARCH_PAGE_SIZE;
    let title_matches = title_scored
        .into_iter()
        .take(SEARCH_PAGE_SIZE)
        .map(|x| x.3)
        .collect();

    let content_total = content_scored.len();
    let content_has_more = content_total > SEARCH_PAGE_SIZE;
    let content_matches = content_scored
        .into_iter()
        .take(SEARCH_PAGE_SIZE)
        .map(|x| x.5)
        .collect();

    SessionSearchResponseDto {
        query: trimmed.to_owned(),
        snapshot_token: String::new(),
        title_matches,
        title_total,
        title_has_more,
        content_matches,
        content_total,
        content_has_more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_title_dto(id: &str) -> TitleSearchMatchDto {
        TitleSearchMatchDto {
            session_id: id.to_owned(),
            title: format!("会话 {id}"),
            updated_at: Some("2026-11-25T10:00:00Z".into()),
            score: 800,
        }
    }

    fn make_content_dto(id: &str) -> ContentSearchMatchDto {
        ContentSearchMatchDto {
            session_id: id.to_owned(),
            title: format!("会话 {id}"),
            updated_at: Some("2026-11-25T10:00:00Z".into()),
            hit_count: 1,
            score: 800,
            best_hit: ContentSearchBestHitDto {
                entry_id: format!("e-{id}"),
                kind: SearchContentKind::Assistant,
                snippet: "本地缓存占用".into(),
                on_active_branch: true,
                branch_leaf_id: None,
                spans: Vec::new(),
            },
        }
    }

    #[test]
    fn chained_prefetch_appends_instantly_and_triggers_next_batch() {
        let mut state = SidebarSearchState::default();
        let seq = state.begin_query("缓存".into(), SessionSearchResponseDto::default());

        let page1 = SessionSearchResponseDto {
            query: "缓存".into(),
            snapshot_token: "tok-1".into(),
            title_matches: (0..10).map(|i| make_title_dto(&format!("t{i}"))).collect(),
            title_total: 24,
            title_has_more: true,
            content_matches: (0..10)
                .map(|i| make_content_dto(&format!("c{i}")))
                .collect(),
            content_total: 25,
            content_has_more: true,
        };
        let prefetch = state.apply_first_page(seq, page1).expect("page1 applied");
        assert_eq!(prefetch, (Some(10), Some(10)));
        assert!(state.prefetch_title_in_flight);
        assert!(state.prefetch_content_in_flight);

        // Background prefetch finishes silently for items 10..20
        let page2_contents: Vec<_> = (10..20)
            .map(|i| make_content_dto(&format!("c{i}")))
            .collect();
        assert_eq!(
            state.finish_content_prefetch(seq, page2_contents, true),
            None
        );
        assert_eq!(state.content_matches.len(), 10);

        // Clicking Expand appends 10..20 instantly and schedules prefetch 20..30
        let act = state.click_expand_contents();
        assert_eq!(
            act,
            ExpandAction::Appended {
                next_prefetch_offset: Some(20)
            }
        );
        assert_eq!(state.content_matches.len(), 20);
        assert!(state.content_has_more);
        assert!(state.prefetch_content_in_flight);

        // Clicking Expand before 20..30 finishes reuses the in-flight prefetch
        assert_eq!(state.click_expand_contents(), ExpandAction::WaitingInFlight);
        let page3_contents: Vec<_> = (20..25)
            .map(|i| make_content_dto(&format!("c{i}")))
            .collect();
        assert_eq!(
            state.finish_content_prefetch(seq, page3_contents, false),
            None
        );
        assert_eq!(state.content_matches.len(), 25);
        assert!(!state.content_has_more);
    }

    #[test]
    fn local_preloaded_search_pins_active_and_skips_thinking() {
        let sessions = vec![
            SessionSummary {
                id: "s-newer".into(),
                title: "浏览器卡顿".into(),
                updated_at: Some("2026-11-25T12:00:00Z".into()),
                running: false,
            },
            SessionSummary {
                id: "s-active".into(),
                title: "磁盘诊断".into(),
                updated_at: Some("2026-11-24T12:00:00Z".into()),
                running: false,
            },
        ];
        let snap_newer = Arc::new(
            SessionViewSnapshot::from_http_payload(
                json!({
                    "id": "s-newer",
                    "entries": [
                        {
                            "id": "e1",
                            "message": {
                                "role": "assistant",
                                "timestamp": 1000,
                                "content": [
                                    {"type": "thinking", "thinking": "绝密思考词ONLY_THINKING"},
                                    {"type": "text", "text": "清理本地缓存目录"}
                                ]
                            }
                        }
                    ]
                }),
                1,
            )
            .unwrap(),
        );
        let snap_active = Arc::new(
            SessionViewSnapshot::from_http_payload(
                json!({
                    "id": "s-active",
                    "entries": [
                        {
                            "id": "e2",
                            "message": {
                                "role": "toolResult",
                                "toolName": "disk",
                                "timestamp": 900,
                                "content": [
                                    {"type": "text", "text": "驱动安装包缓存占用 4.8 GB"}
                                ]
                            }
                        }
                    ]
                }),
                1,
            )
            .unwrap(),
        );
        let mut loaded = BTreeMap::new();
        loaded.insert("s-newer".into(), snap_newer);
        loaded.insert("s-active".into(), snap_active);

        let res = search_local_preloaded_sessions("hc", Some("s-active"), &sessions, &loaded);
        assert_eq!(res.content_matches.len(), 2);
        // Active session must be pinned first even though it is older and tool-only
        assert_eq!(res.content_matches[0].session_id, "s-active");
        assert!(
            res.content_matches[0]
                .best_hit
                .snippet
                .starts_with("工具 · ")
        );

        // Thinking content must never match
        let thinking_res =
            search_local_preloaded_sessions("ONLY_THINKING", Some("s-active"), &sessions, &loaded);
        assert!(thinking_res.content_matches.is_empty());
    }
}
