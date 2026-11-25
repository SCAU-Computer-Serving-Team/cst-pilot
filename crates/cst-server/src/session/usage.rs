use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;

use cst_os::Storage;
use pi_agent::AgentMessage;
use pi_ai::StopReason;
use pi_session::{
    AgentSession, ContextUsage, FileEntry, SessionEntry, SessionManager, context_usage_for_branch,
    find_latest_response, is_virtual_model, parse_session_entry_line,
};
use serde::Serialize;

use super::{SessionPool, SessionPoolError, default_model_reference, explicit_model};
use crate::locks::StorageError;

/// 单次有效模型回复的 Token 分项（与 `pi-ai` 归一化口径一致：`input`、`cache_read`、`cache_write` 互斥）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

/// 当前活跃分支最近一次有效模型调用的用量与缓存命中率。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageSummary {
    pub scope: &'static str,
    pub source_entry_id: Option<String>,
    pub tokens: SessionUsageTokens,
    pub cache_hit_rate: Option<f64>,
}

/// `GET /api/sessions/:id/usage` 响应载荷。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsagePayload {
    pub session_id: String,
    pub leaf_id: Option<String>,
    pub context_usage: Option<ContextUsage>,
    pub usage: SessionUsageSummary,
}

/// 沿当前活跃分支逆序计算缓存命中率与用量：
/// 1. 只统计当前叶节点所在分支，排除其他分叉分支；
/// 2. 遇到 `Compaction`、`ContextEdit`、`BranchSummary` 边界即停止，压缩或分支总结后无新回复时返回 `cache_hit_rate: None`；
/// 3. 取边界之后最后一条非中止、非报错且含有效用量数据的 `Assistant` 回复；
/// 4. 命中率按 `cache_read / (input + cache_read + cache_write)` 计算。
pub fn compute_branch_usage(branch: &[SessionEntry]) -> SessionUsageSummary {
    for entry in branch.iter().rev() {
        match entry {
            SessionEntry::Compaction(_)
            | SessionEntry::ContextEdit(_)
            | SessionEntry::BranchSummary(_) => {
                break;
            }
            SessionEntry::Message(m) => {
                let AgentMessage::Assistant(assistant) = &m.message else {
                    continue;
                };
                if matches!(
                    assistant.stop_reason,
                    StopReason::Aborted | StopReason::Error
                ) {
                    continue;
                }
                let u = &assistant.usage;
                let total_input = u
                    .input
                    .saturating_add(u.cache_read)
                    .saturating_add(u.cache_write);
                let total = if u.total_tokens > 0 {
                    u.total_tokens
                } else {
                    total_input.saturating_add(u.output)
                };
                if total_input == 0 && total == 0 {
                    continue;
                }
                let cache_hit_rate = (total_input > 0).then(|| {
                    u.cache_read as f64
                        / (u.input as f64 + u.cache_read as f64 + u.cache_write as f64)
                });
                return SessionUsageSummary {
                    scope: "last_assistant",
                    source_entry_id: Some(m.id.clone()),
                    tokens: SessionUsageTokens {
                        input: u.input,
                        output: u.output,
                        cache_read: u.cache_read,
                        cache_write: u.cache_write,
                        total,
                    },
                    cache_hit_rate,
                };
            }
            _ => {}
        }
    }

    SessionUsageSummary {
        scope: "last_assistant",
        source_entry_id: None,
        tokens: SessionUsageTokens::default(),
        cache_hit_rate: None,
    }
}

pub fn build_session_usage_payload(
    session_id: &str,
    session: &AgentSession,
) -> SessionUsagePayload {
    session.with_context_branch(|leaf_id, branch, context_usage| SessionUsagePayload {
        session_id: session_id.to_owned(),
        leaf_id: leaf_id.map(str::to_owned),
        context_usage,
        usage: compute_branch_usage(&branch),
    })
}

fn session_not_found() -> SessionPoolError {
    SessionPoolError::NotFound("会话不存在或 ID 不唯一".to_owned())
}

fn usage_io(error: std::io::Error) -> SessionPoolError {
    SessionPoolError::Storage(StorageError::from_io(&error, "读取会话用量"))
}

fn parse_line(line: &[u8]) -> Option<FileEntry> {
    std::str::from_utf8(line)
        .ok()
        .and_then(parse_session_entry_line)
}

fn has_session_id(file: &mut File, id: &str) -> bool {
    const HEADER_LIMIT: u64 = 1024 * 1024;
    let mut header = BufReader::new(file.take(HEADER_LIMIT + 1));
    let mut line = Vec::new();
    let mut scanned = 0;
    loop {
        line.clear();
        let Ok(n) = header.read_until(b'\n', &mut line) else {
            return false;
        };
        scanned += n as u64;
        if n == 0 || scanned > HEADER_LIMIT {
            return false;
        }
        if !line.iter().all(u8::is_ascii_whitespace) {
            return matches!(parse_line(&line), Some(FileEntry::Session(h)) if h.id == id);
        }
    }
}

fn read_saved_manager(
    root: &Path,
    data: &Path,
    id: &str,
) -> Result<Option<SessionManager>, SessionPoolError> {
    if !data.join("sessions").exists() {
        return Ok(None);
    }
    // Honor the configured data-root override while validating and pinning its descendants.
    let (anchor, rel) = data
        .strip_prefix(root)
        .map_or((data, Path::new("")), |rel| (root, rel));
    let storage = Storage::new(anchor).map_err(SessionPoolError::Conflict)?;
    let relative = rel.join("sessions");
    let _root_pin = storage
        .pin_directory(&relative, false)
        .map_err(SessionPoolError::Conflict)?;
    let root = storage.root.join(&relative);
    let mut dirs = vec![relative];
    for entry in fs::read_dir(&root)
        .map_err(usage_io)?
        .filter_map(Result::ok)
    {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            dirs.push(dirs[0].join(entry.file_name()));
        }
    }
    let mut matched = None;
    for dir in dirs {
        let Ok(_pin) = storage.pin_directory(&dir, false) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(storage.root.join(dir)) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(Some(mut file)) = storage.open_read(&path) else {
                continue;
            };
            if has_session_id(&mut file, id) && matched.replace(file).is_some() {
                return Err(session_not_found());
            }
        }
    }
    let Some(mut file) = matched else {
        return Ok(None);
    };
    file.rewind().map_err(usage_io)?;
    let mut reader = BufReader::new(file);
    let mut entries = Vec::new();
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).map_err(usage_io)? != 0 {
        if let Some(entry) = parse_line(&line) {
            entries.push(entry);
        }
        line.clear();
    }
    let Some(FileEntry::Session(header)) = entries.first() else {
        return Err(SessionPoolError::Conflict(
            "会话文件缺少有效头部".to_owned(),
        ));
    };
    if header.id != id {
        return Err(session_not_found());
    }
    SessionManager::in_memory(header.cwd.clone(), None, Some(entries))
        .map(Some)
        .map_err(|err| SessionPoolError::Conflict(err.to_string()))
}

impl SessionPool {
    /// Read a loaded session or project a saved transcript without opening a writer or inbox.
    pub async fn read_session_usage(
        &self,
        id: &str,
    ) -> Result<SessionUsagePayload, SessionPoolError> {
        let _loading = self.load_locks.lock(id).await;
        if self.is_deleted_sync(id)? {
            return Err(session_not_found());
        }
        let loaded = self.slots.lock().await.get(id).cloned();
        if let Some(slot) = loaded {
            return Ok(build_session_usage_payload(id, slot.session()));
        }

        let kit = self.host.kit();
        let root = kit.root().to_owned();
        let data = kit.data_dir().to_owned();
        let saved_id = id.to_owned();
        let saved =
            tokio::task::spawn_blocking(move || read_saved_manager(&root, &data, &saved_id))
                .await
                .map_err(|_| SessionPoolError::Conflict("读取会话用量任务异常退出".to_owned()))??;
        let runtime = self.host.auth().runtime();
        let default_model = || default_model_reference(&self.settings_manager());
        let (leaf_id, usage, context_usage) = if let Some(manager) = saved {
            let branch = manager.get_branch(None);
            let projection = manager.build_session_projection();
            let (provider, model_id) = projection
                .model
                .as_ref()
                .map(|model| (model.provider.clone(), model.model_id.clone()))
                .unwrap_or_else(default_model);
            let model = explicit_model(runtime, &provider, &model_id);
            let window = if is_virtual_model(&model) {
                find_latest_response(&projection.messages)
                    .and_then(|reply| runtime.get_model(&reply.provider, &reply.model))
                    .map_or(model.context_window, |physical| physical.context_window)
            } else {
                model.context_window
            };
            (
                manager.get_leaf_id().map(str::to_owned),
                compute_branch_usage(&branch),
                context_usage_for_branch(&projection, &branch, window),
            )
        } else {
            let unflushed = self.read_unflushed_sync(id);
            if unflushed.is_none()
                && !self
                    .read_created_sync()?
                    .values()
                    .any(|saved| saved.as_deref() == Some(id))
            {
                return Err(session_not_found());
            }
            let (provider, model_id) = unflushed
                .and_then(|saved| saved.model)
                .map(|model| (model.provider, model.model_id))
                .unwrap_or_else(default_model);
            let window = explicit_model(runtime, &provider, &model_id).context_window;
            let context = (window > 0).then_some(ContextUsage {
                tokens: Some(0),
                context_window: window,
                percent: Some(0.0),
            });
            (None, compute_branch_usage(&[]), context)
        };
        Ok(SessionUsagePayload {
            session_id: id.to_owned(),
            leaf_id,
            context_usage,
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::compute_branch_usage;
    use pi_agent::AgentMessage;
    use pi_ai::{AssistantMessage, StopReason, UserContent, UserMessage};
    use pi_session::{
        BranchSummaryEntry, CompactionEntry, ContextEditEntry, SessionEntry, SessionMessageEntry,
    };
    use serde_json::json;

    fn user_entry(id: &str, parent_id: Option<&str>) -> SessionEntry {
        SessionEntry::Message(SessionMessageEntry {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            timestamp: "2026-11-24T00:00:00.000Z".to_owned(),
            message: AgentMessage::User(UserMessage {
                content: UserContent::Text("hello".to_owned()),
                timestamp: 1,
            }),
        })
    }

    fn assistant_entry(
        id: &str,
        parent_id: Option<&str>,
        stop_reason: StopReason,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    ) -> SessionEntry {
        let total_tokens = input + output + cache_read + cache_write;
        let mut msg: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "api": "openai-completions",
            "provider": "deepseek",
            "model": "deepseek-flash",
            "usage": {
                "input": input,
                "output": output,
                "cacheRead": cache_read,
                "cacheWrite": cache_write,
                "totalTokens": total_tokens,
                "cost": {
                    "input": 0.0,
                    "output": 0.0,
                    "cacheRead": 0.0,
                    "cacheWrite": 0.0,
                    "total": 0.0
                }
            },
            "stopReason": "stop",
            "timestamp": 2
        }))
        .expect("assistant message fixture");
        msg.stop_reason = stop_reason;
        SessionEntry::Message(SessionMessageEntry {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            timestamp: "2026-11-24T00:00:01.000Z".to_owned(),
            message: AgentMessage::Assistant(msg),
        })
    }

    fn compaction_entry(id: &str, parent_id: Option<&str>) -> SessionEntry {
        SessionEntry::Compaction(CompactionEntry {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            timestamp: "2026-11-24T00:00:02.000Z".to_owned(),
            summary: "summary".to_owned(),
            first_kept_entry_id: id.to_owned(),
            first_kept_entry_index: None,
            tokens_before: 20_000,
            details: None,
            from_hook: None,
            system_message: None,
            usage: None,
        })
    }

    fn branch_summary_entry(id: &str, parent_id: Option<&str>, from_id: &str) -> SessionEntry {
        SessionEntry::BranchSummary(BranchSummaryEntry {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            timestamp: "2026-11-24T00:00:03.000Z".to_owned(),
            from_id: from_id.to_owned(),
            summary: "branch summary".to_owned(),
            details: None,
            from_hook: None,
            usage: None,
        })
    }

    #[test]
    fn empty_or_user_only_branch_has_no_cache_hit_rate() {
        let empty = compute_branch_usage(&[]);
        assert!(empty.cache_hit_rate.is_none());
        assert!(empty.source_entry_id.is_none());
        assert_eq!(empty.tokens.total, 0);

        let user_only = compute_branch_usage(&[user_entry("u1", None)]);
        assert!(user_only.cache_hit_rate.is_none());
        assert!(user_only.source_entry_id.is_none());
    }

    #[test]
    fn uses_last_valid_assistant_not_cumulative_totals() {
        let branch = vec![
            user_entry("u1", None),
            assistant_entry("a1", Some("u1"), StopReason::Stop, 5000, 200, 0, 0),
            user_entry("u2", Some("a1")),
            assistant_entry("a2", Some("u2"), StopReason::Stop, 1000, 150, 9000, 0),
            // Aborted / errored trailing replies are ignored so the last valid reply stays intact.
            assistant_entry("a3", Some("a2"), StopReason::Error, 100, 0, 0, 0),
            assistant_entry("a4", Some("a3"), StopReason::Aborted, 100, 0, 0, 0),
        ];
        let summary = compute_branch_usage(&branch);
        assert_eq!(summary.source_entry_id.as_deref(), Some("a2"));
        assert_eq!(summary.tokens.input, 1000);
        assert_eq!(summary.tokens.output, 150);
        assert_eq!(summary.tokens.cache_read, 9000);
        assert_eq!(summary.tokens.total, 10_150);
        let rate = summary.cache_hit_rate.expect("hit rate");
        assert!((rate - 0.9).abs() < 1e-9);
    }

    #[test]
    fn resets_after_compaction_and_recomputes_on_post_compaction_reply() {
        let compacted_only = vec![
            user_entry("u1", None),
            assistant_entry("a1", Some("u1"), StopReason::Stop, 1000, 100, 9000, 0),
            compaction_entry("c1", Some("a1")),
        ];
        let after_compact = compute_branch_usage(&compacted_only);
        assert!(after_compact.cache_hit_rate.is_none());
        assert!(after_compact.source_entry_id.is_none());

        let mut with_reply = compacted_only;
        with_reply.push(user_entry("u2", Some("c1")));
        with_reply.push(assistant_entry(
            "a2",
            Some("u2"),
            StopReason::Stop,
            1307,
            136,
            5632,
            0,
        ));
        let post = compute_branch_usage(&with_reply);
        assert_eq!(post.source_entry_id.as_deref(), Some("a2"));
        let expected = 5632.0 / (1307.0 + 5632.0);
        assert!((post.cache_hit_rate.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn stops_at_branch_summary_and_context_edit_boundaries() {
        let after_branch_summary = vec![
            user_entry("u1", None),
            assistant_entry("a1", Some("u1"), StopReason::Stop, 200, 50, 800, 0),
            branch_summary_entry("bs1", Some("a1"), "other_leaf"),
        ];
        assert!(
            compute_branch_usage(&after_branch_summary)
                .cache_hit_rate
                .is_none()
        );

        let context_edit = ContextEditEntry {
            id: "ce1".to_owned(),
            parent_id: Some("a1".to_owned()),
            timestamp: "2026-11-24T00:00:04.000Z".to_owned(),
            target_id: "u1".to_owned(),
            replacement: None,
        };
        let after_edit = vec![
            user_entry("u1", None),
            assistant_entry("a1", Some("u1"), StopReason::Stop, 200, 50, 800, 0),
            SessionEntry::ContextEdit(context_edit),
        ];
        assert!(compute_branch_usage(&after_edit).cache_hit_rate.is_none());
    }

    #[test]
    fn includes_cache_write_in_total_input_denominator() {
        // Anthropic-style reply: 200 fresh input + 6000 cache read + 3800 cache write = 10000 input
        let branch = vec![
            user_entry("u1", None),
            assistant_entry("a1", Some("u1"), StopReason::Stop, 200, 400, 6000, 3800),
        ];
        let summary = compute_branch_usage(&branch);
        assert_eq!(summary.tokens.cache_write, 3800);
        let rate = summary.cache_hit_rate.expect("hit rate");
        assert!((rate - 0.6).abs() < 1e-9);
    }
}
