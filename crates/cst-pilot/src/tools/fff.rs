use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use pi_session::ToolDefinition;
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::path_guard::PathGuard;
use super::{MAX_OUTPUT_BYTES, json_result};

const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 1000;
const MAX_LINE_BYTES: usize = 8 * 1024;
const MAX_VISITED: usize = 500_000;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub context: Option<usize>,
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    #[serde(default)]
    pub literal: bool,
}

pub fn definitions(guard: PathGuard) -> Vec<ToolDefinition> {
    [false, true]
        .into_iter()
        .map(|content_search| {
            let guard = guard.clone();
            let name = if content_search { "ffgrep" } else { "fffind" };
            let mut definition = ToolDefinition::from_typed::<SearchInput, _, _>(
                name,
                name,
                if content_search {
                    "检索文件内容，遵守忽略规则并跳过凭据。"
                } else {
                    "模糊查找文件路径，遵守忽略规则并跳过凭据。"
                },
                move |_, input, cancel, _, ctx| {
                    let guard = guard.clone();
                    async move {
                        let cancel = cancel.unwrap_or_default();
                        let cwd = PathBuf::from(ctx.cwd());
                        let value = tokio::task::spawn_blocking(move || {
                            search(&guard, &cwd, input, content_search, &cancel)
                        })
                        .await
                        .map_err(|_| "文件检索任务异常。".to_owned())??;
                        json_result(value)
                    }
                },
            );
            definition.prompt_snippet = Some(definition.description.clone());
            definition
        })
        .collect()
}

pub fn search(
    guard: &PathGuard,
    cwd: &Path,
    input: SearchInput,
    content_search: bool,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    if input.pattern.len() > 4096 || (content_search && input.pattern.is_empty()) {
        return Err("检索表达式为空或超过 4096 字节。".into());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut remaining_bytes = 256 * 1024 * 1024usize;
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let context = input.context.unwrap_or(0).min(20);
    let (root, include) = search_root(cwd, input.path.as_deref())?;
    let root = guard.check(root)?;
    let excludes = globs(input.exclude.as_deref().unwrap_or(&[]))?;
    let sensitive = input
        .case_sensitive
        .unwrap_or_else(|| input.pattern.chars().any(char::is_uppercase));
    let expression = if input.literal {
        regex::escape(&input.pattern)
    } else {
        input.pattern.clone()
    };
    let regex = if content_search {
        Some(
            RegexBuilder::new(&expression)
                .case_insensitive(!sensitive)
                .size_limit(4 * 1024 * 1024)
                .build()
                .map_err(|_| "内容检索正则表达式无效或过大。")?,
        )
    } else {
        None
    };
    let filter_guard = guard.clone();
    let mut walker = WalkBuilder::new(&root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    walker.filter_entry(move |entry| {
        entry.file_name() != ".git" && filter_guard.check(entry.path()).is_ok()
    });
    let mut matches = Vec::new();
    let mut output_bytes = 512;
    let mut truncated = false;
    let mut skipped = 0usize;
    for (visited, entry) in walker.build().enumerate() {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        if Instant::now() >= deadline {
            return Err("文件检索超过 30 秒，请缩小检索范围。".into());
        }
        if visited >= MAX_VISITED {
            truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let display = path
            .strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let include_path = path.strip_prefix(cwd).unwrap_or(path);
        if include
            .as_ref()
            .is_some_and(|filter| !filter.is_match(include_path))
            || excludes.is_match(include_path)
        {
            continue;
        }
        if let Some(regex) = &regex {
            let file = match guard.open(path) {
                Ok(file) => file,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            let mut before = std::collections::VecDeque::new();
            let mut after = 0usize;
            let mut number = 0usize;
            loop {
                if cancel.is_cancelled() {
                    return Err("文件检索已取消。".into());
                }
                let (bytes, oversized) = read_line(
                    &mut reader,
                    &mut line,
                    cancel,
                    deadline,
                    &mut remaining_bytes,
                )?;
                if bytes == 0 {
                    break;
                }
                number += 1;
                if line.contains(&0) {
                    break;
                }
                if oversized {
                    skipped += 1;
                    before.clear();
                    after = 0;
                    continue;
                }
                let text = String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                let matched = regex.is_match(&text);
                if matched || after > 0 {
                    if matched {
                        while let Some(previous) = before.pop_front() {
                            if !push_bounded(&mut matches, &mut output_bytes, previous, limit)? {
                                truncated = true;
                                break;
                            }
                        }
                    }
                    if truncated
                        || !push_bounded(
                            &mut matches,
                            &mut output_bytes,
                            json!({"path": display, "line": number, "text": text, "matched": matched}),
                            limit,
                        )?
                    {
                        truncated = true;
                        break;
                    }
                    after = if matched { context } else { after - 1 };
                } else if context > 0 {
                    before.push_back(
                        json!({"path": display, "line": number, "text": text, "matched": false}),
                    );
                    if before.len() > context {
                        before.pop_front();
                    }
                }
            }
        } else if fuzzy_match(&input.pattern, &display, sensitive)
            && !push_bounded(
                &mut matches,
                &mut output_bytes,
                json!({"path": display}),
                limit,
            )?
        {
            truncated = true;
        }
        if truncated {
            break;
        }
    }
    Ok(json!({"matches": matches, "truncated": truncated, "skipped": skipped}))
}

fn search_root(cwd: &Path, filter: Option<&str>) -> Result<(PathBuf, Option<GlobSet>), String> {
    let Some(filter) = filter.filter(|s| !s.is_empty()) else {
        return Ok((cwd.into(), None));
    };
    let path = cwd.join(filter);
    if path.exists() {
        return Ok((path, None));
    }
    if filter.contains(['*', '?', '[', '{']) {
        return Ok((cwd.into(), Some(globs(&[filter.into()])?)));
    }
    Err("检索路径不存在。".into())
}

fn globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = pattern.replace('\\', "/");
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(false)
            .build()
            .map_err(|_| "路径过滤表达式无效。".to_owned())?;
        builder.add(glob);
        if pattern.ends_with('/') {
            builder.add(
                GlobBuilder::new(&format!("{pattern}**"))
                    .literal_separator(false)
                    .build()
                    .map_err(|_| "路径排除表达式无效。".to_owned())?,
            );
        }
    }
    builder.build().map_err(|_| "路径过滤表达式无效。".into())
}

fn fuzzy_match(pattern: &str, path: &str, sensitive: bool) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    let (pattern, path) = if sensitive {
        (pattern.to_owned(), path.to_owned())
    } else {
        (pattern.to_lowercase(), path.to_lowercase())
    };
    pattern.split_whitespace().all(|term| {
        let mut chars = term.chars();
        let mut next = chars.next();
        for c in path.chars() {
            if next == Some(c) {
                next = chars.next();
            }
        }
        next.is_none()
    })
}

fn push_bounded(
    matches: &mut Vec<Value>,
    bytes: &mut usize,
    value: Value,
    limit: usize,
) -> Result<bool, String> {
    let size = serde_json::to_vec(&value)
        .map_err(|_| "检索结果无法编码。")?
        .len()
        + 1;
    if matches.len() >= limit || *bytes + size > MAX_OUTPUT_BYTES {
        return Ok(false);
    }
    *bytes += size;
    matches.push(value);
    Ok(true)
}

/// Drain oversized lines without allocating proportional to input size.
fn read_line(
    reader: &mut impl BufRead,
    output: &mut Vec<u8>,
    cancel: &CancellationToken,
    deadline: Instant,
    remaining_bytes: &mut usize,
) -> Result<(usize, bool), String> {
    output.clear();
    let mut total = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        if Instant::now() >= deadline {
            return Err("文件检索超过 30 秒，请缩小检索范围。".into());
        }
        let buf = reader.fill_buf().map_err(|_| "检索文件读取失败。")?;
        if buf.is_empty() {
            break;
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |i| i + 1);
        if n > *remaining_bytes {
            return Err("内容检索超过 256 MiB，请缩小检索范围。".into());
        }
        *remaining_bytes -= n;
        let end = buf[n - 1] == b'\n';
        let keep = n.min(MAX_LINE_BYTES.saturating_sub(output.len()));
        output.extend_from_slice(&buf[..keep]);
        total = total.saturating_add(n);
        reader.consume(n);
        if end {
            break;
        }
    }
    Ok((total, total > MAX_LINE_BYTES))
}
