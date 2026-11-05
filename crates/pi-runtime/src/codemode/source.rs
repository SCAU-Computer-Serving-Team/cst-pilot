use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const RESERVED_WORDS: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

pub fn is_valid_js_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return false;
    }
    !RESERVED_WORDS.contains(&name)
}

/// Normalize a tool name into a valid JavaScript identifier for `tools.<name>`.
pub fn to_codemode_identifier(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len() + 2);
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            sanitized.push(c);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        return "_".to_owned();
    }
    let first = sanitized.chars().next().unwrap_or('_');
    if first.is_ascii_digit() {
        sanitized.insert(0, '_');
    }
    if RESERVED_WORDS.contains(&sanitized.as_str()) {
        sanitized.push('_');
    }
    sanitized
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeSourceOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedCodemodeSource {
    pub code: String,
    pub options: CodemodeSourceOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct CodemodeSourceError {
    pub message: String,
}

impl CodemodeSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn parse_options_comment(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("//")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("@options:")?;
    Some(rest.trim())
}

fn find_misplaced_options_line(text: &str) -> Option<usize> {
    for (idx, line) in text.split('\n').enumerate().skip(1) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if parse_options_comment(line).is_some() {
            return Some(idx + 1);
        }
    }
    None
}

fn parse_non_negative_integer(value: &Value, field: &str) -> Result<u64, CodemodeSourceError> {
    let Some(num) = value.as_f64() else {
        return Err(CodemodeSourceError::new(format!(
            "@options field `{field}` must be a non-negative safe integer"
        )));
    };
    if !num.is_finite() || num.fract() != 0.0 || num < 0.0 || num > MAX_SAFE_INTEGER {
        return Err(CodemodeSourceError::new(format!(
            "@options field `{field}` must be a non-negative safe integer"
        )));
    }
    Ok(num as u64)
}

fn parse_positive_integer(value: &Value, field: &str) -> Result<u64, CodemodeSourceError> {
    let num = parse_non_negative_integer(value, field)?;
    if num == 0 {
        return Err(CodemodeSourceError::new(format!(
            "@options field `{field}` must be a positive integer (> 0)"
        )));
    }
    Ok(num)
}

fn parse_options_line(raw_options: &str) -> Result<CodemodeSourceOptions, CodemodeSourceError> {
    if raw_options.is_empty() {
        return Err(CodemodeSourceError::new(
            "@options must be a JSON object with supported fields (`max_output_tokens`, `timeout_ms`)",
        ));
    }
    let parsed: Value = serde_json::from_str(raw_options).map_err(|_| {
        CodemodeSourceError::new(
            "@options must be valid JSON with supported fields (`max_output_tokens`, `timeout_ms`)",
        )
    })?;
    let Some(obj) = parsed.as_object() else {
        return Err(CodemodeSourceError::new(
            "@options must be a JSON object with supported fields (`max_output_tokens`, `timeout_ms`)",
        ));
    };

    let mut options = CodemodeSourceOptions::default();
    for (key, value) in obj {
        match key.as_str() {
            "max_output_tokens" => {
                options.max_output_tokens = Some(parse_non_negative_integer(value, key)?);
            }
            "timeout_ms" => {
                options.timeout_ms = Some(parse_positive_integer(value, key)?);
            }
            _ => {
                return Err(CodemodeSourceError::new(format!(
                    "@options only supports `max_output_tokens` and `timeout_ms`; got `{key}`"
                )));
            }
        }
    }
    Ok(options)
}

pub fn parse_codemode_source(source: &str) -> Result<ParsedCodemodeSource, CodemodeSourceError> {
    if source.trim().is_empty() {
        return Err(CodemodeSourceError::new(
            "Expected JavaScript source text (non-empty). Pass raw JavaScript code.",
        ));
    }

    if let Some(misplaced_line) = find_misplaced_options_line(source) {
        return Err(CodemodeSourceError::new(format!(
            "The // @options: line must be the first line of the input; found one on line {misplaced_line}"
        )));
    }

    let newline_index = source.find('\n');
    let first_line = match newline_index {
        Some(idx) => {
            let slice = &source[..idx];
            slice.strip_suffix('\r').unwrap_or(slice)
        }
        None => source,
    };

    let Some(options_json) = parse_options_comment(first_line) else {
        return Ok(ParsedCodemodeSource {
            code: source.to_owned(),
            options: CodemodeSourceOptions::default(),
        });
    };

    let options = parse_options_line(options_json)?;
    let Some(idx) = newline_index else {
        return Err(CodemodeSourceError::new(
            "The @options line must be followed by JavaScript source on subsequent lines",
        ));
    };
    let rest = &source[idx..];
    if rest.trim().is_empty() {
        return Err(CodemodeSourceError::new(
            "The @options line must be followed by JavaScript source on subsequent lines",
        ));
    }

    Ok(ParsedCodemodeSource {
        code: rest.to_owned(),
        options,
    })
}
