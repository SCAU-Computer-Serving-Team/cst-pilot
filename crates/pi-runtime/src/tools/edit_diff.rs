use icu_normalizer::ComposingNormalizer;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

pub fn split_bom(content: &str) -> (&str, &str) {
    if let Some(rest) = content.strip_prefix('\u{feff}') {
        ("\u{feff}", rest)
    } else {
        ("", content)
    }
}

pub fn detect_line_ending(content: &str) -> LineEnding {
    let Some(lf_idx) = content.find('\n') else {
        return LineEnding::Lf;
    };
    let Some(crlf_idx) = content.find("\r\n") else {
        return LineEnding::Lf;
    };
    if crlf_idx < lf_idx {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => text.to_owned(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

/// 归一化文本用于模糊匹配（对照 `normalizeForFuzzyMatch`）：
/// 1. 剥离每行末尾空白字符。
/// 2. 应用 Unicode NFKC 兼容归一化。
/// 3. 将弯单引号/双引号、各种破折号/连字符以及特殊不换行空格统一转换为 ASCII 等价字符。
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let normalized = ComposingNormalizer::new_nfkc().normalize(text);
    let lines: Vec<String> = normalized
        .split('\n')
        .map(|line| {
            let mut norm = String::with_capacity(line.len());
            for ch in line.chars() {
                match ch {
                    // Fullwidth ASCII variants (NFKC)
                    '\u{FF01}'..='\u{FF5E}' => {
                        if let Some(ascii) = char::from_u32(ch as u32 - 0xFEE0) {
                            norm.push(ascii);
                        } else {
                            norm.push(ch);
                        }
                    }
                    // Smart single quotes -> '
                    '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => norm.push('\''),
                    // Smart double quotes -> "
                    '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => norm.push('"'),
                    // Unicode dashes/hyphens/minus -> -
                    '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                    | '\u{2212}' => norm.push('-'),
                    // Special Unicode spaces -> ' '
                    '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => {
                        norm.push(' ')
                    }
                    other => norm.push(other),
                }
            }
            norm.trim_end().to_owned()
        })
        .collect();
    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplacement {
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

fn split_lines_with_endings(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    content.split_inclusive('\n').collect()
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(
    lines: &[LineSpan],
    rep: &TextReplacement,
) -> Result<(usize, usize), String> {
    let rep_start = rep.match_index;
    let rep_end = rep.match_index + rep.match_length;

    let mut start_line = None;
    for (i, line) in lines.iter().enumerate() {
        if rep_start >= line.start && rep_start < line.end {
            start_line = Some(i);
            break;
        }
    }
    let Some(start_idx) = start_line else {
        return Err("Replacement range is outside the base content.".to_owned());
    };

    let mut end_idx = start_idx;
    while end_idx < lines.len() && lines[end_idx].end < rep_end {
        end_idx += 1;
    }
    if end_idx >= lines.len() {
        return Err("Replacement range is outside the base content.".to_owned());
    }
    Ok((start_idx, end_idx + 1))
}

fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_owned();
    for rep in replacements.iter().rev() {
        let idx = rep.match_index - offset;
        let end = idx + rep.match_length;
        result.replace_range(idx..end, &rep.new_text);
    }
    result
}

/// 在保留未修改行原始字节/空白的前提下，将基于 `base_content` 匹配的替换应用回 `original_content`（对照 `applyReplacementsPreservingUnchangedLines`）。
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_owned(),
        );
    }

    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);

    let mut groups: Vec<(usize, usize, Vec<TextReplacement>)> = Vec::new();
    for rep in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, &rep)?;
        if let Some(last) = groups.last_mut()
            && start_line < last.1
        {
            last.1 = last.1.max(end_line);
            last.2.push(rep);
            continue;
        }
        groups.push((start_line, end_line, vec![rep]));
    }

    let mut orig_line_idx = 0usize;
    let mut result = String::with_capacity(original_content.len());
    for (start_line, end_line, group_reps) in groups {
        for &line in &original_lines[orig_line_idx..start_line] {
            result.push_str(line);
        }
        let group_start_offset = base_lines[start_line].start;
        let group_end_offset = base_lines[end_line - 1].end;
        let replaced_slice = apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group_reps,
            group_start_offset,
        );
        result.push_str(&replaced_slice);
        orig_line_idx = end_line;
    }
    for &line in &original_lines[orig_line_idx..] {
        result.push_str(line);
    }
    Ok(result)
}

pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(exact_idx) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: exact_idx,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_owned(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if let Some(fuzzy_idx) = (!fuzzy_old.is_empty())
        .then(|| fuzzy_content.find(&fuzzy_old))
        .flatten()
    {
        FuzzyMatchResult {
            found: true,
            index: fuzzy_idx,
            match_length: fuzzy_old.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        }
    } else {
        FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_owned(),
        }
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if fuzzy_old.is_empty() {
        return 0;
    }
    // Count overlapping occurrences too (for example `aaa` in `aaaa`).
    fuzzy_content
        .char_indices()
        .filter(|(i, _)| fuzzy_content[*i..].starts_with(&fuzzy_old))
        .count()
}

pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let total = edits.len();
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|e| Edit {
            old_text: normalize_to_lf(&e.old_text),
            new_text: normalize_to_lf(&e.new_text),
        })
        .collect();

    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            if total == 1 {
                return Err(format!("oldText must not be empty in {path}."));
            }
            return Err(format!("edits[{i}].oldText must not be empty in {path}."));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|e| fuzzy_find_text(normalized_content, &e.old_text))
        .collect();
    let used_fuzzy = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let replacement_base = if used_fuzzy {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_owned()
    };

    let mut matched_edits = Vec::with_capacity(total);
    for (i, edit) in normalized_edits.iter().enumerate() {
        let m = fuzzy_find_text(&replacement_base, &edit.old_text);
        if !m.found {
            if total == 1 {
                return Err(format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                ));
            }
            return Err(format!(
                "Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines."
            ));
        }
        let occ = count_occurrences(&replacement_base, &edit.old_text);
        if occ > 1 {
            if total == 1 {
                return Err(format!(
                    "Found {occ} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
                ));
            }
            return Err(format!(
                "Found {occ} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
            ));
        }
        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: m.index,
            match_length: m.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|m| m.match_index);
    let line_spans = get_line_spans(&replacement_base);
    for i in 1..matched_edits.len() {
        let prev = &matched_edits[i - 1];
        let curr = &matched_edits[i];
        let as_replacement = |m: &MatchedEdit| TextReplacement {
            match_index: m.match_index,
            match_length: m.match_length,
            new_text: String::new(),
        };
        let previous_lines = get_replacement_line_range(&line_spans, &as_replacement(prev))?;
        let current_lines = get_replacement_line_range(&line_spans, &as_replacement(curr))?;
        if prev.match_index + prev.match_length > curr.match_index
            || previous_lines.1 > current_lines.0
        {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                prev.edit_index, curr.edit_index
            ));
        }
    }

    let reps: Vec<TextReplacement> = matched_edits
        .into_iter()
        .map(|m| TextReplacement {
            match_index: m.match_index,
            match_length: m.match_length,
            new_text: m.new_text,
        })
        .collect();

    let new_content = if used_fuzzy {
        apply_replacements_preserving_unchanged_lines(normalized_content, &replacement_base, &reps)?
    } else {
        apply_replacements(&replacement_base, &reps, 0)
    };

    if normalized_content == new_content {
        if total == 1 {
            return Err(format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            ));
        }
        return Err(format!(
            "No changes made to {path}. The replacements produced identical content."
        ));
    }

    Ok(AppliedEditsResult {
        base_content: normalized_content.to_owned(),
        new_content,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

pub fn generate_diff_string(old_content: &str, new_content: &str) -> DiffResult {
    let diff = TextDiff::from_lines(old_content, new_content);
    let width = old_content
        .lines()
        .count()
        .max(new_content.lines().count())
        .max(1)
        .to_string()
        .len();
    let mut first_changed_line = None;
    let mut output = Vec::new();
    let mut new_line = 1;
    for (group_index, group) in diff.grouped_ops(4).iter().enumerate() {
        if group_index > 0 {
            output.push(" ...".to_owned());
        }
        for op in group {
            new_line = new_line.max(op.new_range().start + 1);
            for change in diff.iter_changes(op) {
                let (sign, line) = match change.tag() {
                    ChangeTag::Equal => (' ', change.new_index().unwrap() + 1),
                    ChangeTag::Delete => {
                        first_changed_line.get_or_insert(new_line);
                        ('-', change.old_index().unwrap() + 1)
                    }
                    ChangeTag::Insert => {
                        first_changed_line.get_or_insert(change.new_index().unwrap() + 1);
                        ('+', change.new_index().unwrap() + 1)
                    }
                };
                if change.tag() != ChangeTag::Delete {
                    new_line = line + 1;
                }
                output.push(format!(
                    "{sign}{line:>width$} {}",
                    change.value().trim_end_matches('\n')
                ));
            }
        }
    }
    DiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str) -> String {
    TextDiff::from_lines(old_content, new_content)
        .unified_diff()
        .context_radius(4)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}
