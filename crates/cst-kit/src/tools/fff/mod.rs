mod grep_engine;
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod mft_index;
mod walk_engine;
mod win_volumes;

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use pi_session::ToolDefinition;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::diagnostics::result::{enforce_atomic_matches_bound, structured_search_result};

use super::path_guard::{PathGuard, PathGuardSession, is_unc_or_network_path};

pub(crate) const DEFAULT_LIMIT: usize = 30;
pub(crate) const MAX_LIMIT: usize = 1000;
pub(crate) const MAX_CONTEXT: usize = 20;
pub(crate) const MAX_DEPTH_LIMIT: usize = 1024;
pub(crate) const MAX_PATTERN_BYTES: usize = 4096;
pub(crate) const MAX_LINE_BYTES: usize = 8 * 1024;
pub(crate) const MAX_TOTAL_READ_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_MODIFIED_HOURS: f64 = 87_600.0;
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub(crate) const MAX_TIMEOUT_SECS: u64 = 180;
const VOLUME_ENUM_TIMEOUT: Duration = Duration::from_secs(10);
const WEAK_MATCH_SCORE_THRESHOLD: i32 = 400;
const GLOB_BASE_SCORE: i32 = 5_000;
const MIB_BYTES: f64 = 1_048_576.0;

// ============================================================================
// Public Input / Output DTOs (Independent Schemas + output_schema for Codemode)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FffindKind {
    #[default]
    File,
    Dir,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FffindSortBy {
    Relevance,
    Modified,
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TruncatedReason {
    Timeout,
    ReadBudget,
    VisitedLimit,
    OutputBytes,
    Limit,
    LineLimit,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FffindInput {
    /// 必填。扫描范围：传 "*" 扫描本机全部可见固定盘与可移动盘（跳过光驱与网络盘）；传裸盘符或盘根（如 "C:"、"C:\\"）扫描整盘；传目录或单个文件路径扫描指定范围。拒绝 UNC 与网络映射盘。
    #[schemars(length(min = 1))]
    pub path: String,
    /// 可选。名称或路径匹配表达式，支持完整精确名、通配符（*、?、[]、{}）与空格多词模糊匹配；省略或传 "*"/"**" 表示不按名称过滤；不可传空串或纯空白，非法通配符直接报错（UTF-8 上限 4096 字节）。
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pub pattern: Option<String>,
    /// 可选。条目类型过滤："file" 仅文件（默认），"dir" 仅目录（如查找 WeChat Files、Minidump），"any" 同时返回文件与目录。
    #[serde(default)]
    pub kind: Option<FffindKind>,
    /// 可选。最后一段扩展名白名单，不区分大小写，单个前导点号可选（如 ["dmp", "log", "mp4"]），不支持复合后缀与通配符。不可与 kind="dir" 同传。
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub ext: Option<Vec<String>>,
    /// 可选。排除通配符列表（如 ["*.tmp", "node_modules/"]）。无分隔符匹配任意层级名称，尾 "/" 剪枝匹配目录。
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub exclude: Option<Vec<String>>,
    /// 可选。最大遍历深度，根目录深度为 0，1 仅扫描直接子项。path="*" 时按各卷根计深度，单文件根不可传此字段。
    #[serde(default)]
    #[schemars(range(min = 1, max = 1024))]
    pub max_depth: Option<usize>,
    /// 可选。仅保留最近 N 小时内修改的条目，按调用时刻 UTC 闭区间 [now-N, now] 过滤，未来与未知时间不命中。
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 87600.0))]
    pub modified_within_hours: Option<f64>,
    /// 可选。最小文件逻辑体积，单位二进制 MiB（1 MiB = 1048576 字节），含端点向上取整字节。不可与 kind="dir" 同传。
    #[serde(default, rename = "minSizeMiB")]
    #[schemars(range(min = 0.0))]
    pub min_size_mib: Option<f64>,
    /// 可选。最大文件逻辑体积，单位二进制 MiB，含端点向下取整字节，须大于等于 minSizeMiB；0 仅匹配空文件。不可与 kind="dir" 同传。
    #[serde(default, rename = "maxSizeMiB")]
    #[schemars(range(min = 0.0))]
    pub max_size_mib: Option<f64>,
    /// 可选。排序方式："relevance" 相关性降序，"modified" 修改时间降序，"size" 文件字节数降序。省略时按名称、时间、大小条件自动选择。"size" 不可与 kind="dir" 同传。
    #[serde(default)]
    pub sort_by: Option<FffindSortBy>,
    /// 可选。是否区分大小写，仅影响查询匹配。Windows 默认 false。
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    /// 可选。最多返回的命中条目数，范围 1 至 1000，默认 30。
    #[serde(default)]
    #[schemars(range(min = 1, max = 1000))]
    pub limit: Option<usize>,
    /// 可选。单卷或目标目录检索的超时预算，单位秒，范围 1 至 180，默认 30。全电脑 path="*" 模式下各卷在开始扫描时独立起算该预算。
    #[serde(
        default = "default_timeout_sec",
        deserialize_with = "deserialize_timeout_sec"
    )]
    #[schemars(range(min = 1, max = 180))]
    pub timeout_sec: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FfgrepInput {
    /// 必填。单行正则表达式或字面检索文本，不可为空，UTF-8 上限 4096 字节（空白字符作为正文保留）。
    #[schemars(length(min = 1, max = 4096))]
    pub pattern: String,
    /// 必填。目标目录路径或单个文件路径（如 "C:\\Windows\\Logs\\CBS" 或 "C:\\Windows\\Logs\\CBS\\CBS.log"）。拒绝 UNC、网络映射盘与内嵌通配符。
    #[schemars(length(min = 1))]
    pub path: String,
    /// 可选。文件包含通配符列表（如 ["*.log", "*.ini", "*.xml"]），无分隔符匹配任意层级文件名。仅目录模式允许传入，单文件根传入报错。
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub include: Option<Vec<String>>,
    /// 可选。排除通配符列表（如 ["*.bak", "archive/"]），尾 "/" 剪枝目录且优先于 include。仅目录模式允许传入，单文件根传入报错。
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub exclude: Option<Vec<String>>,
    /// 可选。仅检索最近 N 小时内修改的文件，按调用时刻 UTC 闭区间 [now-N, now] 过滤并在打开句柄上复核。
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 87600.0))]
    pub modified_within_hours: Option<f64>,
    /// 可选。置 true 时按字面子串匹配，不解析正则元字符；默认 false。
    #[serde(default)]
    pub literal: bool,
    /// 可选。是否区分大小写。literal=true 省略时采用智能大小写；正则模式省略时固定 false，显式值或正则标志可覆盖。
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    /// 可选。每条命中行前后各附带的上下文行数，范围 0 至 20，默认 0。触及 limit 时优先保留命中行。
    #[serde(default)]
    #[schemars(range(min = 0, max = 20))]
    pub context: Option<usize>,
    /// 可选。返回总行数上限（含上下文行），范围 1 至 1000，默认 30。
    #[serde(default)]
    #[schemars(range(min = 1, max = 1000))]
    pub limit: Option<usize>,
    /// 可选。内容检索的超时预算，单位秒，范围 1 至 180，默认 30。
    #[serde(
        default = "default_timeout_sec",
        deserialize_with = "deserialize_timeout_sec"
    )]
    #[schemars(range(min = 1, max = 180))]
    pub timeout_sec: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedRoot {
    pub root: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FffindMatch {
    pub path: String,
    pub kind: FffindKind,
    pub size_bytes: Option<u64>,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FffindOutput {
    pub root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned_roots: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_roots: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_roots: Option<Vec<SkippedRoot>>,
    pub matches: Vec<FffindMatch>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_reason: Option<TruncatedReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    pub skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FfgrepMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub matched: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FfgrepOutput {
    pub root: String,
    pub matches: Vec<FfgrepMatch>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_reason: Option<TruncatedReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    pub skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_large_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

// ============================================================================
// Tool Definitions (with output_schema + structured_content)
// ============================================================================

pub fn definitions(guard: PathGuard) -> Vec<ToolDefinition> {
    let find_guard = guard.clone();
    let mut fffind = ToolDefinition::from_typed::<FffindInput, _, _>(
        "fffind",
        "fffind",
        "全电脑或指定目录高速查找文件与目录，返回规范绝对路径、类型、逻辑字节大小与本机修改时间。path 传 \"*\" 扫描本机全部就绪固定盘与可移动盘，也可传盘符、目录或单个文件路径。不读取 .gitignore，自动跳过 .git、重解析目录、网络路径与工具包凭据。",
        move |_, input, cancel, _, ctx| {
            let guard = find_guard.clone();
            async move {
                let cancel = cancel.unwrap_or_default();
                let cwd = PathBuf::from(ctx.cwd());
                let (model_val, details_val) = tokio::task::spawn_blocking(move || {
                    find_paths_with_details(&guard, &cwd, input, &cancel)
                })
                .await
                .map_err(|_| "文件检索任务异常。".to_owned())??;
                structured_search_result(model_val, details_val)
            }
        },
    );
    fffind.prompt_snippet = Some(fffind.description.clone());
    fffind.output_schema = Some(schemars::schema_for!(FffindOutput).to_value());

    let grep_guard = guard;
    let mut ffgrep = ToolDefinition::from_typed::<FfgrepInput, _, _>(
        "ffgrep",
        "ffgrep",
        "在指定目录或单个文件内检索文本内容，支持单行正则与字面匹配、UTF-16 LE/BE（含 BOM）与 GB18030 流式解码、文件包含过滤与时间过滤。目录模式跳过大于 10 MiB 的单文件，传入单个文件路径时启用 256 KiB 持久化流式扫描。不读取 .gitignore，自动跳过二进制文件、网络路径与工具包凭据。",
        move |_, input, cancel, _, ctx| {
            let guard = grep_guard.clone();
            async move {
                let cancel = cancel.unwrap_or_default();
                let cwd = PathBuf::from(ctx.cwd());
                let (model_val, details_val) = tokio::task::spawn_blocking(move || {
                    grep_files_with_details(&guard, &cwd, input, &cancel)
                })
                .await
                .map_err(|_| "内容检索任务异常。".to_owned())??;
                structured_search_result(model_val, details_val)
            }
        },
    );
    ffgrep.prompt_snippet = Some(ffgrep.description.clone());
    ffgrep.output_schema = Some(schemars::schema_for!(FfgrepOutput).to_value());

    vec![fffind, ffgrep]
}

// ============================================================================
// Public Entry Points
// ============================================================================

pub fn find_paths(
    guard: &PathGuard,
    cwd: &Path,
    input: FffindInput,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let (model_val, _) = find_paths_with_details(guard, cwd, input, cancel)?;
    Ok(model_val)
}

pub fn grep_files(
    guard: &PathGuard,
    cwd: &Path,
    input: FfgrepInput,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let (model_val, _) = grep_files_with_details(guard, cwd, input, cancel)?;
    Ok(model_val)
}

fn find_paths_with_details(
    guard: &PathGuard,
    cwd: &Path,
    input: FffindInput,
    cancel: &CancellationToken,
) -> Result<(Value, Value), String> {
    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }

    let timeout_sec = validate_timeout_sec(input.timeout_sec)?;
    let per_vol_budget = Duration::from_secs(timeout_sec);
    let kind = input.kind.unwrap_or(FffindKind::File);
    let limit = validate_limit(input.limit)?;
    if let Some(d) = input.max_depth
        && !(1..=MAX_DEPTH_LIMIT).contains(&d)
    {
        return Err(format!("maxDepth 必须在 1 至 {MAX_DEPTH_LIMIT} 之间。"));
    }

    let exts = validate_exts(input.ext.as_deref())?;
    let size_bounds = validate_size_bounds(input.min_size_mib, input.max_size_mib)?;
    if matches!(kind, FffindKind::Dir) {
        if input.ext.is_some() {
            return Err("kind=\"dir\" 不可与 ext 同时使用。".into());
        }
        if size_bounds.is_some() {
            return Err("kind=\"dir\" 不可与 minSizeMiB 或 maxSizeMiB 同时使用。".into());
        }
        if matches!(input.sort_by, Some(FffindSortBy::Size)) {
            return Err("kind=\"dir\" 不可与 sortBy=\"size\" 同时使用。".into());
        }
    }

    let modified_window = validate_modified_window(input.modified_within_hours)?;
    let excludes = compile_globs(input.exclude.as_deref().unwrap_or(&[]), true)?;

    let raw_pattern = match input.pattern.as_deref() {
        None => None,
        Some(p) => {
            if p.len() > MAX_PATTERN_BYTES {
                return Err(format!("pattern 超过 {MAX_PATTERN_BYTES} 字节上限。"));
            }
            if p.trim().is_empty() {
                return Err("pattern 不可为空串或纯空白；不按名称过滤请省略该字段。".into());
            }
            Some(p)
        }
    };
    let sensitive = input.case_sensitive.unwrap_or_else(|| {
        if cfg!(windows) {
            false
        } else {
            raw_pattern.is_some_and(|p| p.chars().any(char::is_uppercase))
        }
    });
    let query = Arc::new(QueryPlan::parse(raw_pattern, sensitive)?);
    let sort_by = input.sort_by.unwrap_or_else(|| {
        if !query.unfiltered {
            FffindSortBy::Relevance
        } else if modified_window.is_some() {
            FffindSortBy::Modified
        } else if size_bounds.is_some() {
            FffindSortBy::Size
        } else {
            FffindSortBy::Relevance
        }
    });

    if input.path.trim().is_empty() {
        return Err("path 不可为空。".into());
    }
    // Preserve verbatim `\\?\` paths without trimming trailing spaces/dots; trim ordinary inputs.
    let path_arg = if input.path.starts_with(r"\\?\") {
        input.path.as_str()
    } else {
        input.path.trim()
    };

    let session = guard.session();
    let spec = walk_engine::WalkQuerySpec {
        kind,
        exts,
        excludes,
        max_depth: input.max_depth,
        modified_window,
        size_bounds,
        sort_by,
        query: Arc::clone(&query),
        limit,
    };

    if path_arg == "*" {
        return run_whole_pc_find(&session, spec, timeout_sec, cancel);
    }

    let resolved_path = resolve_explicit_search_path(cwd, path_arg)?;
    let verified_root = session.verify_search_root(&resolved_path)?;
    let root_display = normalize_path(&verified_root.canonical_path);

    if !verified_root.is_dir {
        if input.max_depth.is_some() {
            return Err("单文件路径不支持 maxDepth 参数。".into());
        }
        return run_single_file_find(&verified_root, &root_display, &spec, timeout_sec);
    }

    // Try elevated NTFS MFT+USN memory index first (for volume root or subtree on NTFS).
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut method = if cfg!(windows) { "native-walk" } else { "walk" };
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut vol_index_status: Option<Map<String, Value>> = None;
    let mut candidates = Vec::new();
    let mut truncated_by_limit = false;
    let mut timed_out = false;
    let mut queue_overflowed = false;
    let mut had_non_policy_failure = false;
    let mut extra_notices: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut walk_budget = per_vol_budget;

    #[cfg(windows)]
    let mut handled_by_mft = false;
    #[cfg(not(windows))]
    let handled_by_mft = false;

    #[cfg(windows)]
    let mut single_root_mft_eligible = false;
    #[cfg(not(windows))]
    let single_root_mft_eligible = false;

    #[cfg(windows)]
    if let Some((drive, subtree_rel)) =
        mft_index::split_windows_drive_and_subtree(&verified_root.canonical_path)
    {
        if crate::diagnostics::volume::volume_identity(&drive)
            .is_ok_and(|id| id.filesystem.eq_ignore_ascii_case("NTFS"))
        {
            single_root_mft_eligible = true;
        }
        let mgr = mft_index::SearchIndexManager::global();
        match mgr.get_or_build_windows(&drive, false, per_vol_budget, cancel)? {
            mft_index::MftVolumeResult::Ready {
                index: idx,
                status,
                deadline: vol_deadline,
            } => {
                let (mft_cands, mft_limit, mft_skipped, mft_timed_out) = idx.query(
                    &session,
                    &verified_root,
                    subtree_rel.as_deref(),
                    &spec,
                    vol_deadline,
                    cancel,
                )?;
                candidates = mft_cands;
                truncated_by_limit = mft_limit;
                skipped = mft_skipped;
                timed_out = mft_timed_out;
                method = "mft-index";
                let mut st_map = Map::new();
                st_map.insert(
                    format!("{drive}/"),
                    json!(match status {
                        mft_index::IndexBuildStatus::ColdBuilt => "cold-built",
                        mft_index::IndexBuildStatus::UsnSynced => "usn-synced",
                    }),
                );
                vol_index_status = Some(st_map);
                handled_by_mft = true;
            }
            mft_index::MftVolumeResult::TimedOut => {
                timed_out = true;
                method = "mft-index";
                handled_by_mft = true;
            }
            mft_index::MftVolumeResult::Fallback { remaining_budget } => {
                walk_budget = remaining_budget;
            }
        }
    }

    if !handled_by_mft {
        let is_vol_root = is_volume_root_path(&verified_root.canonical_path);
        let outcome = walk_engine::run_walk_find(
            &session,
            vec![walk_engine::WalkRootTarget {
                display_root: root_display.clone(),
                verified_root,
                is_volume_root: is_vol_root,
                budget: walk_budget,
            }],
            spec.clone(),
            cancel,
        )?;
        candidates = outcome.candidates;
        truncated_by_limit = outcome.truncated_by_limit;
        timed_out = outcome.timed_out;
        queue_overflowed = outcome.queue_overflowed;
        had_non_policy_failure |= outcome.had_non_policy_failure;
        skipped = outcome.skipped;
        if let Some(sk) = outcome.skipped_roots.first() {
            let msg = match sk.reason.as_str() {
                "worker_pool_busy" => "工作线程池繁忙，未启动目录扫描，请稍后重试。",
                "worker_spawn_failed" => "目录扫描线程启动失败，请稍后重试。",
                _ => "目录扫描异常中止，结果不完整，请重试。",
            };
            extra_notices.push(msg.to_owned());
        }
    }

    let suggest_admin_retry = should_suggest_admin_retry(single_root_mft_eligible);

    finalize_fffind_response(
        root_display,
        None,
        None,
        None,
        candidates,
        &spec,
        truncated_by_limit,
        timed_out,
        queue_overflowed,
        had_non_policy_failure,
        skipped,
        method,
        vol_index_status,
        timeout_sec,
        suggest_admin_retry,
        extra_notices,
    )
}

fn run_single_file_find(
    verified_root: &super::path_guard::VerifiedSearchEntry,
    root_display: &str,
    spec: &walk_engine::WalkQuerySpec,
    timeout_sec: u64,
) -> Result<(Value, Value), String> {
    let mut candidates = Vec::new();
    if !matches!(spec.kind, FffindKind::Dir) {
        let name = verified_root
            .canonical_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let ext_ok = spec.exts.is_empty() || ext_matches(name, &spec.exts);
        let size_ok = size_within_bounds(verified_root.size_bytes, spec.size_bounds);
        let time_ok =
            path_within_modified_window(verified_root.modified_secs, spec.modified_window);
        let excl_ok = spec.excludes.is_empty()
            || (!spec.excludes.is_match(name) && !spec.excludes.is_match(root_display));
        let mut scratch = String::new();
        if ext_ok
            && size_ok
            && time_ok
            && excl_ok
            && let Some(score) = spec.query.score_entry(name, name, &mut scratch)
        {
            candidates.push(FindCandidate {
                score,
                path: root_display.to_owned(),
                is_dir: false,
                size_bytes: verified_root.size_bytes,
                modified_secs: verified_root.modified_secs,
                modified_iso: verified_root.modified_secs.and_then(format_local_rfc3339),
            });
        }
    }
    finalize_fffind_response(
        root_display.to_owned(),
        None,
        None,
        None,
        candidates,
        spec,
        false,
        false,
        false,
        false,
        0,
        if cfg!(windows) { "native-walk" } else { "walk" },
        None,
        timeout_sec,
        false,
        Vec::new(),
    )
}

fn run_whole_pc_find(
    session: &PathGuardSession,
    spec: walk_engine::WalkQuerySpec,
    timeout_sec: u64,
    cancel: &CancellationToken,
) -> Result<(Value, Value), String> {
    let per_vol_budget = Duration::from_secs(timeout_sec);
    // Volume enumeration uses a bounded probe window that does not consume each volume's scan budget.
    let enum_deadline = Instant::now() + per_vol_budget.min(VOLUME_ENUM_TIMEOUT);
    let enumeration = win_volumes::enumerate_whole_pc_volumes(enum_deadline, cancel)?;
    if enumeration.ready.is_empty() {
        return Err("未找到任何可访问的本机固定盘或可移动盘。".into());
    }

    let mut skipped_roots = enumeration.skipped;
    let mut had_non_policy_failure = enumeration.had_non_policy_failure;
    let mut mft_roots = Vec::new();
    let mut mft_partial_roots = Vec::new();
    let mut walk_targets = Vec::new();
    let mut mft_eligible_roots: HashSet<String> = HashSet::new();
    let mut vol_index_status = Map::new();
    let mut all_candidates = Vec::new();
    let mut truncated_by_limit = false;
    let mut timed_out = false;
    let mut skipped_count = 0usize;

    let mgr = mft_index::SearchIndexManager::global();
    let mut mft_candidates_vols = Vec::new();

    for vol in enumeration.ready {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        let verified_root = match session.verify_search_root(&vol.native_root) {
            Ok(v) => v,
            Err(err) => {
                if err.contains("权限拒绝") {
                    // Policy-denied root never increments degraded failure or leaks existence.
                    continue;
                }
                had_non_policy_failure = true;
                skipped_roots.push(SkippedRoot {
                    root: vol.display_root,
                    reason: "access_denied".into(),
                });
                continue;
            }
        };

        let is_ntfs_local = !vol.is_subst && vol.filesystem.eq_ignore_ascii_case("NTFS");
        if is_ntfs_local {
            mft_eligible_roots.insert(vol.display_root.clone());
        }
        if is_ntfs_local
            && crate::diagnostics::mft::mft_enabled()
            && cst_os::is_elevated().unwrap_or(false)
        {
            mft_candidates_vols.push((vol, verified_root));
        } else {
            walk_targets.push(walk_engine::WalkRootTarget {
                display_root: vol.display_root,
                verified_root,
                is_volume_root: !vol.is_subst,
                budget: per_vol_budget,
            });
        }
    }

    // Dispatch NTFS MFT volumes concurrently; cold builds queue on `BuildSlotPool` (bounded) and
    // each volume accumulates active USN + cold-build work against `per_vol_budget`.
    enum MftVolDispatch {
        Queried {
            display_root: String,
            candidates: Vec<FindCandidate>,
            truncated_by_limit: bool,
            skipped: usize,
            timed_out: bool,
            status: mft_index::IndexBuildStatus,
        },
        TimedOut {
            display_root: String,
        },
        Fallback(walk_engine::WalkRootTarget),
    }
    let mft_outcomes: Mutex<Vec<Result<MftVolDispatch, String>>> = Mutex::new(Vec::new());
    if !mft_candidates_vols.is_empty() {
        std::thread::scope(|scope| {
            for (vol, verified_root) in mft_candidates_vols {
                let mgr_ref = &mgr;
                let spec_ref = &spec;
                let outcomes_ref = &mft_outcomes;
                scope.spawn(move || {
                    let res = match mgr_ref.get_or_build_windows(
                        &vol.drive,
                        vol.is_subst,
                        per_vol_budget,
                        cancel,
                    ) {
                        Ok(mft_index::MftVolumeResult::Ready {
                            index: idx,
                            status,
                            deadline: vol_deadline,
                        }) => match idx.query(
                            session,
                            &verified_root,
                            None,
                            spec_ref,
                            vol_deadline,
                            cancel,
                        ) {
                            Ok((cands, lim, sk, to)) => Ok(MftVolDispatch::Queried {
                                display_root: vol.display_root,
                                candidates: cands,
                                truncated_by_limit: lim,
                                skipped: sk,
                                timed_out: to,
                                status,
                            }),
                            Err(e) => Err(e),
                        },
                        Ok(mft_index::MftVolumeResult::TimedOut) => Ok(MftVolDispatch::TimedOut {
                            display_root: vol.display_root,
                        }),
                        Ok(mft_index::MftVolumeResult::Fallback { remaining_budget }) => {
                            Ok(MftVolDispatch::Fallback(walk_engine::WalkRootTarget {
                                display_root: vol.display_root,
                                verified_root,
                                is_volume_root: !vol.is_subst,
                                budget: remaining_budget,
                            }))
                        }
                        Err(e) => Err(e),
                    };
                    if let Ok(mut guard) = outcomes_ref.lock() {
                        guard.push(res);
                    }
                });
            }
        });
    }
    let mut used_mft = false;
    for item in mft_outcomes.into_inner().unwrap_or_default() {
        match item? {
            MftVolDispatch::Queried {
                display_root,
                candidates: cands,
                truncated_by_limit: lim,
                skipped: sk,
                timed_out: to,
                status,
            } => {
                used_mft = true;
                all_candidates.extend(cands);
                truncated_by_limit |= lim;
                skipped_count += sk;
                timed_out |= to;
                vol_index_status.insert(
                    display_root.clone(),
                    json!(match status {
                        mft_index::IndexBuildStatus::ColdBuilt => "cold-built",
                        mft_index::IndexBuildStatus::UsnSynced => "usn-synced",
                    }),
                );
                if to {
                    mft_partial_roots.push(display_root);
                } else {
                    mft_roots.push(display_root);
                }
            }
            MftVolDispatch::TimedOut { display_root } => {
                used_mft = true;
                timed_out = true;
                mft_partial_roots.push(display_root);
            }
            MftVolDispatch::Fallback(fallback_target) => {
                walk_targets.push(fallback_target);
            }
        }
    }

    if mft_roots.is_empty() && mft_partial_roots.is_empty() && walk_targets.is_empty() {
        return Err("所有本机磁盘均无访问权限，无法执行全电脑检索。".into());
    }

    let mut partial_roots = mft_partial_roots;
    let mut completed_walk_roots = Vec::new();
    let mut queue_overflowed = false;
    let mut used_walk = false;

    if !walk_targets.is_empty() {
        used_walk = true;
        let outcome = walk_engine::run_walk_find(session, walk_targets, spec.clone(), cancel)?;
        all_candidates.extend(outcome.candidates);
        truncated_by_limit |= outcome.truncated_by_limit;
        timed_out |= outcome.timed_out;
        queue_overflowed |= outcome.queue_overflowed;
        had_non_policy_failure |= outcome.had_non_policy_failure;
        skipped_count += outcome.skipped;
        completed_walk_roots.extend(outcome.completed_roots);
        partial_roots.extend(outcome.partial_roots);
        skipped_roots.extend(outcome.skipped_roots);
    }

    let mut scanned_roots = mft_roots;
    scanned_roots.extend(completed_walk_roots);
    scanned_roots.sort();
    partial_roots.sort();
    skipped_roots.sort_by(|a, b| a.root.cmp(&b.root));

    let method = match (used_mft, used_walk) {
        (true, false) => "mft-index",
        (false, true) => "native-walk",
        (true, true) => "mixed",
        (false, false) => "native-walk",
    };

    let any_partial_mft_eligible = partial_roots.iter().any(|r| mft_eligible_roots.contains(r));
    let suggest_admin_retry = should_suggest_admin_retry(any_partial_mft_eligible);

    finalize_fffind_response(
        "*".to_owned(),
        Some(scanned_roots),
        (!partial_roots.is_empty()).then_some(partial_roots),
        (!skipped_roots.is_empty()).then_some(skipped_roots),
        all_candidates,
        &spec,
        truncated_by_limit,
        timed_out,
        queue_overflowed,
        had_non_policy_failure,
        skipped_count,
        method,
        (!vol_index_status.is_empty()).then_some(vol_index_status),
        timeout_sec,
        suggest_admin_retry,
        Vec::new(),
    )
}

fn should_suggest_admin_retry(has_mft_eligible_target: bool) -> bool {
    #[cfg(windows)]
    {
        has_mft_eligible_target
            && crate::diagnostics::mft::mft_enabled()
            && matches!(cst_os::is_elevated(), Ok(false))
    }
    #[cfg(not(windows))]
    {
        let _ = has_mft_eligible_target;
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_fffind_response(
    root: String,
    scanned_roots: Option<Vec<String>>,
    partial_roots: Option<Vec<String>>,
    skipped_roots: Option<Vec<SkippedRoot>>,
    mut candidates: Vec<FindCandidate>,
    spec: &walk_engine::WalkQuerySpec,
    mut truncated_by_limit: bool,
    timed_out: bool,
    queue_overflowed: bool,
    had_non_policy_failure: bool,
    skipped: usize,
    method: &str,
    vol_index_status: Option<Map<String, Value>>,
    timeout_sec: u64,
    suggest_admin_retry: bool,
    extra_notices: Vec<String>,
) -> Result<(Value, Value), String> {
    // Deduplicate identical canonical paths BEFORE checking `limit`, keeping the best-ranked copy.
    candidates.sort_by(|a, b| cmp_find_candidate(a, b, spec.sort_by));
    let mut seen_paths: HashSet<String> = HashSet::with_capacity(candidates.len());
    candidates.retain(|c| seen_paths.insert(c.path.clone()));
    if candidates.len() > spec.limit {
        candidates.truncate(spec.limit);
        truncated_by_limit = true;
    }

    let top_score = candidates.first().map(|c| c.score);
    let matches: Vec<FffindMatch> = candidates
        .into_iter()
        .map(|c| FffindMatch {
            path: c.path,
            kind: if c.is_dir {
                FffindKind::Dir
            } else {
                FffindKind::File
            },
            size_bytes: c.size_bytes,
            modified: c.modified_iso,
        })
        .collect();

    let reason = if timed_out {
        Some(TruncatedReason::Timeout)
    } else if queue_overflowed {
        Some(TruncatedReason::VisitedLimit)
    } else if truncated_by_limit {
        Some(TruncatedReason::Limit)
    } else {
        None
    };
    let degraded = timed_out || queue_overflowed || had_non_policy_failure;
    let mut notices = extra_notices;
    if timed_out {
        if let Some(ref partials) = partial_roots {
            if suggest_admin_retry {
                notices.push(format!(
                    "全电脑检索中部分卷达到单卷 {timeout_sec} 秒预算，已保留已扫范围内的高分命中，{} 未扫完；可缩小 path、调高 timeoutSec（最高 {MAX_TIMEOUT_SECS} 秒）或以管理员权限重试。",
                    partials.join("、")
                ));
            } else {
                notices.push(format!(
                    "全电脑检索中部分卷达到单卷 {timeout_sec} 秒预算，已保留已扫范围内的高分命中，{} 未扫完；可缩小 path 或调高 timeoutSec（最高 {MAX_TIMEOUT_SECS} 秒）重试。",
                    partials.join("、")
                ));
            }
        } else if matches.is_empty() {
            if suggest_admin_retry {
                notices.push(format!(
                    "文件检索达到 {timeout_sec} 秒预算且未找到匹配项；可缩小 path 范围、调高 timeoutSec（最高 {MAX_TIMEOUT_SECS} 秒）或以管理员权限重试。"
                ));
            } else {
                notices.push(format!(
                    "文件检索达到 {timeout_sec} 秒预算且未找到匹配项；可缩小 path 范围或调高 timeoutSec（最高 {MAX_TIMEOUT_SECS} 秒）后重试。"
                ));
            }
        } else if suggest_admin_retry {
            notices.push(format!(
                "文件检索达到 {timeout_sec} 秒预算，已保留已扫范围内的高分命中；可缩小 path 范围、调高 timeoutSec（最高 {MAX_TIMEOUT_SECS} 秒）或以管理员权限重试获取完整结果。"
            ));
        } else {
            notices.push(format!(
                "文件检索达到 {timeout_sec} 秒预算，已保留已扫范围内的高分命中；可缩小 path 范围或调高 timeoutSec（最高 {MAX_TIMEOUT_SECS} 秒）获取完整结果。"
            ));
        }
    } else if queue_overflowed {
        notices.push(
            "目录待扫队列达到内存上限，部分深层目录未扫描；请缩小 path 范围或传 maxDepth。"
                .to_owned(),
        );
    }
    if !spec.query.unfiltered
        && !spec.query.has_glob
        && top_score.is_some_and(|s| s < WEAK_MATCH_SCORE_THRESHOLD)
    {
        notices.push("仅找到弱相关路径匹配，建议补充文件名关键词或缩小 path 范围。".to_owned());
    }

    let output = FffindOutput {
        root,
        scanned_roots,
        partial_roots,
        skipped_roots,
        matches,
        truncated: reason.is_some(),
        truncated_reason: reason,
        degraded: degraded.then_some(true),
        skipped,
        notice: (!notices.is_empty()).then(|| notices.join(" ")),
        elapsed_ms: None,
    };

    let mut model_map = match serde_json::to_value(&output).map_err(|_| "检索结果无法编码。")?
    {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    // Enforce atomic whole-row 50 KiB bound (leaving 32-byte headroom for outer `elapsedMs`).
    let _ = enforce_atomic_matches_bound(&mut model_map)?;
    let mut details_map = model_map.clone();
    details_map.insert("method".into(), json!(method));
    if let Some(st) = vol_index_status {
        details_map.insert("indexStatus".into(), Value::Object(st));
    }

    Ok((Value::Object(model_map), Value::Object(details_map)))
}

fn grep_files_with_details(
    guard: &PathGuard,
    cwd: &Path,
    input: FfgrepInput,
    cancel: &CancellationToken,
) -> Result<(Value, Value), String> {
    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    let timeout_sec = validate_timeout_sec(input.timeout_sec)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_sec);
    if input.pattern.is_empty() || input.pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!(
            "pattern 不可为空且不得超过 {MAX_PATTERN_BYTES} 字节。"
        ));
    }
    if input.pattern.contains(['\r', '\n']) {
        return Err("ffgrep 仅支持单行检索，pattern 不得包含换行符。".into());
    }
    if input.path.trim().is_empty() || input.path.trim() == "*" {
        return Err("ffgrep 的 path 必须指定具体目录或单个文件路径，不支持 \"*\"。".into());
    }
    let path_arg = if input.path.starts_with(r"\\?\") {
        input.path.as_str()
    } else {
        input.path.trim()
    };

    let limit = validate_limit(input.limit)?;
    let context = validate_context(input.context)?;
    let modified_window = validate_modified_window(input.modified_within_hours)?;

    let resolved_path = resolve_explicit_search_path(cwd, path_arg)?;
    let session = guard.session();
    let verified_root = session.verify_search_root(&resolved_path)?;
    let root_display = normalize_path(&verified_root.canonical_path);

    if !verified_root.is_dir && (input.include.is_some() || input.exclude.is_some()) {
        return Err("ffgrep 单文件模式不支持 include 或 exclude 参数。".into());
    }

    let include = match input.include.as_deref() {
        Some(list) if !list.is_empty() => Some(compile_globs(list, false)?),
        Some(_) => None,
        None => None,
    };
    let excludes = compile_globs(input.exclude.as_deref().unwrap_or(&[]), true)?;

    // Smart-case only for literal mode; regex defaults to case-insensitive when omitted
    // so uppercase metacharacters like `\S` or `\p{Lu}` never accidentally toggle sensitivity.
    let sensitive = input.case_sensitive.unwrap_or_else(|| {
        if input.literal {
            input.pattern.chars().any(char::is_uppercase)
        } else {
            false
        }
    });
    let matcher = Arc::new(grep_engine::ContentMatcher::build(
        &input.pattern,
        input.literal,
        sensitive,
    )?);

    let outcome = grep_engine::run_grep(
        &session,
        &verified_root,
        include,
        excludes,
        matcher,
        modified_window,
        limit,
        context,
        timeout_sec,
        deadline,
        cancel,
    )?;

    let output = FfgrepOutput {
        root: root_display,
        matches: outcome.matches,
        truncated: outcome.truncated,
        truncated_reason: outcome.truncated_reason,
        degraded: outcome.degraded.then_some(true),
        skipped: outcome.skipped,
        skipped_large_files: (outcome.skipped_large_files > 0)
            .then_some(outcome.skipped_large_files),
        notice: (!outcome.notices.is_empty()).then(|| outcome.notices.join(" ")),
        elapsed_ms: None,
    };

    let mut model_map = match serde_json::to_value(&output).map_err(|_| "检索结果无法编码。")?
    {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    let _ = enforce_atomic_matches_bound(&mut model_map)?;
    let details_map = model_map.clone();
    Ok((Value::Object(model_map), Value::Object(details_map)))
}

// ============================================================================
// Validation, Path Normalization, Glob & QueryPlan Helpers
// ============================================================================

fn default_timeout_sec() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

fn deserialize_timeout_sec<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(DEFAULT_TIMEOUT_SECS))
}

fn validate_timeout_sec(timeout_sec: u64) -> Result<u64, String> {
    if (1..=MAX_TIMEOUT_SECS).contains(&timeout_sec) {
        Ok(timeout_sec)
    } else {
        Err(format!(
            "timeoutSec 必须在 1 至 {MAX_TIMEOUT_SECS} 秒之间。"
        ))
    }
}

fn validate_limit(limit: Option<usize>) -> Result<usize, String> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(v) if (1..=MAX_LIMIT).contains(&v) => Ok(v),
        Some(_) => Err(format!("limit 必须在 1 至 {MAX_LIMIT} 之间。")),
    }
}

fn validate_context(context: Option<usize>) -> Result<usize, String> {
    match context {
        None => Ok(0),
        Some(v) if v <= MAX_CONTEXT => Ok(v),
        Some(_) => Err(format!("context 必须在 0 至 {MAX_CONTEXT} 之间。")),
    }
}

fn validate_exts(exts: Option<&[String]>) -> Result<Vec<String>, String> {
    let Some(list) = exts else {
        return Ok(Vec::new());
    };
    if list.len() > 32 {
        return Err("ext 最多允许传入 32 项。".into());
    }
    let mut out = Vec::with_capacity(list.len());
    let mut seen = HashSet::new();
    for raw in list {
        let trimmed = raw.trim();
        let stripped = trimmed.strip_prefix('.').unwrap_or(trimmed);
        if stripped.is_empty()
            || stripped.len() > 32
            || stripped.contains('.')
            || stripped.contains(['*', '?', '[', ']', '{', '}', '/', '\\', ' '])
        {
            return Err(format!(
                "ext 项 \"{raw}\" 无效：仅支持单段扩展名（如 \"dmp\"），复合后缀请使用 pattern。"
            ));
        }
        let lower = stripped.to_ascii_lowercase();
        if seen.insert(lower.clone()) {
            out.push(lower);
        }
    }
    Ok(out)
}

fn validate_size_bounds(
    min_mib: Option<f64>,
    max_mib: Option<f64>,
) -> Result<Option<(u64, u64)>, String> {
    // `2.0_f64.powi(64)` is exact in IEEE-754; `u64::MAX as f64` rounds up to `2^64`.
    const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;
    let min_bytes = match min_mib {
        None => None,
        Some(v) if !v.is_finite() || v < 0.0 => {
            return Err("minSizeMiB 必须为非负有限数字。".into());
        }
        Some(v) => {
            let scaled = (v * MIB_BYTES).ceil();
            if !scaled.is_finite() || scaled >= TWO_POW_64 {
                return Err("minSizeMiB 超出支持范围。".into());
            }
            Some(scaled as u64)
        }
    };
    let max_bytes = match max_mib {
        None => None,
        Some(v) if !v.is_finite() || v < 0.0 => {
            return Err("maxSizeMiB 必须为非负有限数字。".into());
        }
        Some(v) => {
            let scaled = (v * MIB_BYTES).floor();
            if !scaled.is_finite() || scaled >= TWO_POW_64 {
                return Err("maxSizeMiB 超出支持范围。".into());
            }
            Some(scaled as u64)
        }
    };
    match (min_bytes, max_bytes) {
        (None, None) => Ok(None),
        (Some(lo), Some(hi)) => {
            if lo > hi {
                return Err("minSizeMiB 不得大于 maxSizeMiB。".into());
            }
            Ok(Some((lo, hi)))
        }
        (Some(lo), None) => Ok(Some((lo, u64::MAX))),
        (None, Some(hi)) => Ok(Some((0, hi))),
    }
}

fn validate_modified_window(hours: Option<f64>) -> Result<Option<(i64, i64)>, String> {
    let Some(h) = hours else {
        return Ok(None);
    };
    if !h.is_finite() || h <= 0.0 || h > MAX_MODIFIED_HOURS {
        return Err(format!(
            "modifiedWithinHours 必须大于 0 且不超过 {MAX_MODIFIED_HOURS}。"
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时钟异常。")?
        .as_secs() as i64;
    let delta = (h * 3600.0).ceil() as i64;
    Ok(Some((now.saturating_sub(delta), now)))
}

fn compile_globs(patterns: &[String], is_exclude: bool) -> Result<GlobSet, String> {
    if patterns.len() > 64 {
        return Err("过滤规则最多允许 64 项。".into());
    }
    let mut builder = GlobSetBuilder::new();
    for raw in patterns {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.len() > 256 {
            return Err("路径过滤规则不可为空或超过 256 字符。".into());
        }
        let normalized = trimmed.replace('\\', "/");
        let glob = GlobBuilder::new(&normalized)
            .case_insensitive(cfg!(windows))
            .literal_separator(false)
            .build()
            .map_err(|_| format!("路径过滤表达式 \"{raw}\" 无效。"))?;
        builder.add(glob);
        if is_exclude && normalized.ends_with('/') {
            let sub = GlobBuilder::new(&format!("{normalized}**"))
                .case_insensitive(cfg!(windows))
                .literal_separator(false)
                .build()
                .map_err(|_| format!("路径排除表达式 \"{raw}\" 无效。"))?;
            builder.add(sub);
        }
    }
    builder.build().map_err(|_| "路径过滤表达式无效。".into())
}

/// Validates and resolves explicit `path` according to ADR-025 rules:
/// - Rejects embedded `*`/`?` wildcards (allows literal `[` / `{` directory names on Windows).
/// - Normalizes exact bare Windows drive `"C:"` -> `"C:\\"`, rejects `"C:foo"` and `"\foo"`.
/// - Rejects UNC / network paths before touching I/O.
/// - Resolves relative paths strictly against session `cwd`.
pub(crate) fn resolve_explicit_search_path(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let raw = if let Some(rest) = raw.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/')
        {
            rest
        } else {
            raw
        }
    } else {
        raw
    };
    if raw.contains(['*', '?']) {
        return Err(
            "path 不支持内嵌通配符；按名称查找请用 pattern，按文件筛选请用 include 或 ext。".into(),
        );
    }
    #[cfg(windows)]
    {
        let bytes = raw.as_bytes();
        // Exact bare drive `"C:"` -> `"C:\\"`
        if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let p = PathBuf::from(format!("{}\\", raw));
            if is_unc_or_network_path(&p) {
                return Err("不支持检索 UNC 网络共享或网络映射盘。".into());
            }
            return Ok(p);
        }
        // Reject drive-relative `"C:foo"` (drive letter + colon without separator).
        if bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] != b'\\'
            && bytes[2] != b'/'
        {
            return Err("不支持盘内相对路径（如 C:foo），请传入完整路径如 C:\\foo。".into());
        }
        // Reject root-relative `"\foo"` or `"/foo"` (not UNC `\\` or `//`).
        if (raw.starts_with('\\') && !raw.starts_with(r"\\"))
            || (raw.starts_with('/') && !raw.starts_with("//"))
        {
            return Err("不支持缺盘符的根相对路径，请传入含盘符的完整路径。".into());
        }
        let candidate = Path::new(raw);
        if is_unc_or_network_path(candidate) {
            return Err("不支持检索 UNC 网络共享或网络映射盘。".into());
        }
        if candidate.is_absolute() {
            return Ok(candidate.to_path_buf());
        }
        let joined = cwd.join(candidate);
        if is_unc_or_network_path(&joined) {
            return Err("不支持检索 UNC 网络共享或网络映射盘。".into());
        }
        Ok(joined)
    }
    #[cfg(not(windows))]
    {
        let _ = is_unc_or_network_path;
        let candidate = Path::new(raw);
        if candidate.is_absolute() {
            Ok(candidate.to_path_buf())
        } else {
            Ok(cwd.join(candidate))
        }
    }
}

pub(crate) fn is_volume_root_path(path: &Path) -> bool {
    let norm = normalize_path(path);
    let bytes = norm.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

pub(crate) fn normalize_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    if let Some(rest) = raw.strip_prefix("//?/UNC/") {
        return format!("//{rest}");
    }
    #[cfg(windows)]
    if let Some(rest) = raw.strip_prefix("//?/") {
        return rest.to_owned();
    }
    raw
}

pub(crate) fn worker_threads() -> usize {
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    cores.saturating_sub(2).clamp(1, 8)
}

pub(crate) fn is_git_dir_name(name: &std::ffi::OsStr) -> bool {
    #[cfg(windows)]
    {
        name.to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(".git"))
    }
    #[cfg(not(windows))]
    {
        name == ".git"
    }
}

pub(crate) fn dir_is_excluded(path: &Path, root: &Path, excludes: &GlobSet) -> bool {
    let rel = path
        .strip_prefix(root)
        .ok()
        .map(normalize_path)
        .unwrap_or_else(|| normalize_path(path));
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    excludes.is_match(&rel)
        || excludes.is_match(format!("{rel}/"))
        || (!name.is_empty() && (excludes.is_match(name) || excludes.is_match(format!("{name}/"))))
}

pub(crate) fn ext_matches(filename: &str, exts: &[String]) -> bool {
    let Some((_, ext)) = filename.rsplit_once('.') else {
        return false;
    };
    exts.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

pub(crate) fn size_within_bounds(size: Option<u64>, bounds: Option<(u64, u64)>) -> bool {
    let Some((lo, hi)) = bounds else {
        return true;
    };
    size.is_some_and(|s| s >= lo && s <= hi)
}

pub(crate) fn path_within_modified_window(mtime: Option<i64>, window: Option<(i64, i64)>) -> bool {
    let Some((lo, hi)) = window else {
        return true;
    };
    mtime.is_some_and(|t| t >= lo && t <= hi)
}

pub(crate) fn format_local_rfc3339(unix_secs: i64) -> Option<String> {
    Local
        .timestamp_opt(unix_secs, 0)
        .single()
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

// ============================================================================
// QueryPlan & Deterministic Total Order for fffind
// ============================================================================

#[derive(Clone, Debug)]
pub(crate) struct FindCandidate {
    pub score: i32,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: Option<u64>,
    pub modified_secs: Option<i64>,
    pub modified_iso: Option<String>,
}

/// Deterministic total order shared by per-worker heaps, cross-volume merges, and final output.
pub(crate) fn cmp_find_candidate(
    a: &FindCandidate,
    b: &FindCandidate,
    sort_by: FffindSortBy,
) -> Ordering {
    match sort_by {
        FffindSortBy::Relevance => b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)),
        FffindSortBy::Modified => match (a.modified_secs, b.modified_secs) {
            (Some(ma), Some(mb)) => mb.cmp(&ma).then_with(|| a.path.cmp(&b.path)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.path.cmp(&b.path),
        },
        FffindSortBy::Size => match (a.size_bytes, b.size_bytes) {
            (Some(sa), Some(sb)) => sb.cmp(&sa).then_with(|| a.path.cmp(&b.path)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.path.cmp(&b.path),
        },
    }
}

enum QueryTerm {
    Glob {
        matcher: GlobSet,
        match_full_path: bool,
        literals: Vec<String>,
    },
    Fuzzy(String),
}

pub(crate) struct QueryPlan {
    pub unfiltered: bool,
    pub sensitive: bool,
    pub has_glob: bool,
    has_filename_glob: bool,
    /// Full trimmed pattern folded for exact-filename comparison BEFORE whitespace split
    /// (supports filenames containing spaces like `"My Report.pdf"`).
    exact_full_name: Option<String>,
    terms: Vec<QueryTerm>,
}

fn has_glob_meta(term: &str) -> bool {
    term.contains(['*', '?', '[', ']', '{', '}'])
}

fn extract_glob_literals(term: &str, sensitive: bool) -> Vec<String> {
    let mut cleaned = String::with_capacity(term.len());
    let mut bracket = 0usize;
    let mut brace = 0usize;
    for ch in term.chars() {
        match ch {
            '\\' => cleaned.push('/'),
            '[' => {
                bracket += 1;
                cleaned.push(' ');
            }
            ']' if bracket > 0 => {
                bracket -= 1;
                cleaned.push(' ');
            }
            '{' => {
                brace += 1;
                cleaned.push(' ');
            }
            '}' if brace > 0 => {
                brace -= 1;
                cleaned.push(' ');
            }
            '*' | '?' | '!' | ',' if bracket == 0 && brace == 0 => cleaned.push(' '),
            _ if bracket > 0 || brace > 0 => {}
            _ => cleaned.push(ch),
        }
    }
    cleaned
        .split_whitespace()
        .map(|part| part.trim_matches('/'))
        .filter(|part| !part.is_empty())
        .map(|part| {
            if sensitive {
                part.to_owned()
            } else {
                part.to_lowercase()
            }
        })
        .collect()
}

fn compile_query_glob(term: &str, sensitive: bool) -> Result<(GlobSet, bool), String> {
    let normalized = term.replace('\\', "/");
    let match_full_path = normalized.contains('/');
    let mut builder = GlobSetBuilder::new();
    let main = GlobBuilder::new(&normalized)
        .case_insensitive(!sensitive)
        .literal_separator(!match_full_path)
        .build()
        .map_err(|_| format!("pattern 中的通配符 \"{term}\" 语法无效。"))?;
    builder.add(main);
    if match_full_path && !normalized.starts_with('/') && !normalized.starts_with("**/") {
        let sub = GlobBuilder::new(&format!("**/{normalized}"))
            .case_insensitive(!sensitive)
            .literal_separator(false)
            .build()
            .map_err(|_| format!("pattern 中的通配符 \"{term}\" 语法无效。"))?;
        builder.add(sub);
    }
    let set = builder
        .build()
        .map_err(|_| format!("pattern 中的通配符 \"{term}\" 语法无效。"))?;
    Ok((set, match_full_path))
}

impl QueryPlan {
    pub(crate) fn parse(pattern: Option<&str>, sensitive: bool) -> Result<Self, String> {
        let Some(raw) = pattern else {
            return Ok(Self {
                unfiltered: true,
                sensitive,
                has_glob: false,
                has_filename_glob: false,
                exact_full_name: None,
                terms: Vec::new(),
            });
        };
        let trimmed = raw.trim();
        if trimmed == "*" || trimmed == "**" {
            return Ok(Self {
                unfiltered: true,
                sensitive,
                has_glob: false,
                has_filename_glob: false,
                exact_full_name: None,
                terms: Vec::new(),
            });
        }

        let exact_full_name = if !has_glob_meta(trimmed) && !trimmed.contains(['/', '\\']) {
            Some(if sensitive {
                trimmed.to_owned()
            } else {
                trimmed.to_lowercase()
            })
        } else {
            None
        };

        let mut parsed: Vec<(bool, usize, QueryTerm)> = Vec::new();
        let mut has_glob = false;
        let mut has_filename_glob = false;
        for token in trimmed.split_whitespace() {
            if token == "*" || token == "**" {
                continue;
            }
            let len = token.chars().count();
            if has_glob_meta(token) {
                let (matcher, match_full_path) = compile_query_glob(token, sensitive)?;
                let literals = extract_glob_literals(token, sensitive);
                has_glob = true;
                if !match_full_path {
                    has_filename_glob = true;
                }
                parsed.push((
                    true,
                    len,
                    QueryTerm::Glob {
                        matcher,
                        match_full_path,
                        literals,
                    },
                ));
            } else {
                let normalized = token.replace('\\', "/");
                let folded = if sensitive {
                    normalized
                } else {
                    normalized.to_lowercase()
                };
                parsed.push((false, len, QueryTerm::Fuzzy(folded)));
            }
        }
        if parsed.is_empty() {
            return Ok(Self {
                unfiltered: true,
                sensitive,
                has_glob: false,
                has_filename_glob: false,
                exact_full_name: None,
                terms: Vec::new(),
            });
        }
        parsed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        let terms = parsed.into_iter().map(|(_, _, t)| t).collect();
        Ok(Self {
            unfiltered: false,
            sensitive,
            has_glob,
            has_filename_glob,
            exact_full_name,
            terms,
        })
    }

    pub(crate) fn fast_reject_filename(&self, filename: &str) -> bool {
        if !self.has_filename_glob {
            return false;
        }
        for term in &self.terms {
            if let QueryTerm::Glob {
                matcher,
                match_full_path: false,
                ..
            } = term
                && !matcher.is_match(filename)
            {
                return true;
            }
        }
        false
    }

    pub(crate) fn score_entry(
        &self,
        rel_path: &str,
        orig_filename: &str,
        scratch: &mut String,
    ) -> Option<i32> {
        if self.unfiltered {
            return Some(0);
        }
        // Check exact full filename BEFORE whitespace tokenization (covers filenames with spaces).
        if let Some(ref exact) = self.exact_full_name {
            let eq = if self.sensitive {
                orig_filename == exact.as_str()
            } else {
                orig_filename.eq_ignore_ascii_case(exact) || orig_filename.to_lowercase() == *exact
            };
            if eq {
                let depth = rel_path.bytes().filter(|&b| b == b'/').count() as i32;
                let len_penalty = (rel_path.len().min(400) as i32) / 4;
                return Some(10_500 - depth * 8 - len_penalty);
            }
        }

        if self.has_glob {
            for term in &self.terms {
                if let QueryTerm::Glob {
                    matcher,
                    match_full_path,
                    ..
                } = term
                {
                    let target = if *match_full_path {
                        rel_path
                    } else {
                        orig_filename
                    };
                    if !matcher.is_match(target) {
                        return None;
                    }
                }
            }
        }

        let haystack: &str = if self.sensitive {
            rel_path
        } else {
            lowercase_into(rel_path, scratch);
            scratch.as_str()
        };
        let filename_start = haystack.rfind('/').map_or(0, |i| i + 1);
        let mut total_score = 0i32;
        for term in &self.terms {
            let score = match term {
                QueryTerm::Fuzzy(token) => score_term(token, haystack, rel_path, filename_start)?,
                QueryTerm::Glob { literals, .. } => {
                    if literals.is_empty() {
                        GLOB_BASE_SCORE
                    } else {
                        let mut lit_sum = 0i32;
                        let mut lit_hits = 0i32;
                        for lit in literals {
                            if let Some(s) = score_term(lit, haystack, rel_path, filename_start) {
                                lit_sum = lit_sum.saturating_add(s);
                                lit_hits += 1;
                            }
                        }
                        let bonus = if lit_hits > 0 {
                            (lit_sum / lit_hits) / 2
                        } else {
                            0
                        };
                        GLOB_BASE_SCORE.saturating_add(bonus)
                    }
                }
            };
            total_score = total_score.saturating_add(score);
        }
        let avg = total_score / (self.terms.len() as i32);
        let depth = rel_path.bytes().filter(|&b| b == b'/').count() as i32;
        let len_penalty = (rel_path.len().min(400) as i32) / 4;
        Some(avg - depth * 8 - len_penalty)
    }
}

fn lowercase_into(input: &str, out: &mut String) {
    out.clear();
    if input.is_ascii() {
        out.reserve(input.len());
        for b in input.bytes() {
            out.push(b.to_ascii_lowercase() as char);
        }
    } else {
        out.push_str(&input.to_lowercase());
    }
}

fn score_term(term: &str, haystack: &str, original: &str, filename_start: usize) -> Option<i32> {
    let filename = &haystack[filename_start..];
    let orig_filename = original.rsplit('/').next().unwrap_or(original);

    if filename == term {
        return Some(10_000);
    }
    if let Some(stem) = filename.rsplit_once('.').map(|(s, _)| s)
        && stem == term
    {
        return Some(9_500);
    }
    if filename.starts_with(term) {
        return Some(6_000 + (term.len().min(64) as i32) * 8);
    }
    if let Some(pos) = filename.find(term) {
        let bonus = if is_boundary_at(orig_filename, pos) {
            800
        } else {
            0
        };
        return Some(4_000 + bonus - (pos.min(128) as i32) * 4);
    }
    if let Some(pos) = haystack.find(term) {
        let bonus = if is_boundary_at(original, pos) {
            500
        } else {
            0
        };
        return Some(2_500 + bonus - (pos.min(256) as i32) * 2);
    }
    if let Some(sub) = subsequence_score(term, filename, orig_filename) {
        return Some(1_200 + sub.clamp(-500, 700));
    }
    subsequence_score(term, haystack, original).map(|s| s.min(600))
}

fn is_boundary_at(text: &str, byte_idx: usize) -> bool {
    if byte_idx == 0 {
        return true;
    }
    let Some(prev) = text.get(..byte_idx).and_then(|s| s.chars().next_back()) else {
        return false;
    };
    if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ') {
        return true;
    }
    let Some(curr) = text.get(byte_idx..).and_then(|s| s.chars().next()) else {
        return false;
    };
    prev.is_lowercase() && curr.is_uppercase()
}

fn subsequence_score(term: &str, haystack: &str, original: &str) -> Option<i32> {
    let mut term_chars = term.chars();
    let mut target = term_chars.next()?;
    let mut score = 0i32;
    let mut prev_idx: Option<usize> = None;
    let mut run_len = 0i32;
    let mut orig_iter = original.chars();
    let mut prev_orig: Option<char> = None;

    for (idx, ch) in haystack.chars().enumerate() {
        let orig_ch = orig_iter.next().unwrap_or(ch);
        if ch == target {
            score += 16;
            if prev_idx.is_some_and(|p| p + 1 == idx) {
                run_len += 1;
                score += 24 + (run_len.min(8) * 4);
            } else {
                if let Some(p) = prev_idx {
                    let gap = (idx - p - 1).min(32) as i32;
                    score -= gap * 2;
                }
                run_len = 0;
            }
            if idx == 0
                || prev_orig.is_some_and(|p| {
                    matches!(p, '/' | '\\' | '_' | '-' | '.' | ' ')
                        || (p.is_lowercase() && orig_ch.is_uppercase())
                })
            {
                score += 20;
            }
            prev_idx = Some(idx);
            match term_chars.next() {
                Some(next) => target = next,
                None => return Some(score),
            }
        }
        prev_orig = Some(orig_ch);
    }
    None
}

#[cfg(test)]
mod tests;
