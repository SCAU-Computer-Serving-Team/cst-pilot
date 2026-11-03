use serde::de::DeserializeOwned;
use serde_json::{Map, Number, Value};

use crate::types::{JsonObject, JsonValue, ToolCall};

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedToolCallArguments {
    pub arguments: JsonObject,
    pub arguments_error: Option<String>,
}

impl ParsedToolCallArguments {
    pub fn apply_to(self, tool_call: &mut ToolCall) {
        tool_call.arguments = self.arguments;
        tool_call.arguments_error = self.arguments_error;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceArgumentsResult {
    pub delta: Option<String>,
    pub arguments: JsonObject,
}

fn is_valid_escape_char(c: char) -> bool {
    matches!(c, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't')
}

fn is_control_char(c: char) -> bool {
    (c as u32) <= 0x1f
}

fn escape_control_char(c: char) -> String {
    match c {
        '\u{0008}' => "\\b".to_owned(),
        '\u{000c}' => "\\f".to_owned(),
        '\n' => "\\n".to_owned(),
        '\r' => "\\r".to_owned(),
        '\t' => "\\t".to_owned(),
        other => format!("\\u{:04x}", other as u32),
    }
}

/// 修复 JSON 字符串字面量中的裸控制字符与非法反斜杠转义（对照 `utils/json-parse.ts` 的 `repairJson`）。
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len() + 8);
    let mut in_string = false;
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];
        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        if ch == '\\' {
            let Some(&next_ch) = chars.get(index + 1) else {
                repaired.push_str("\\\\");
                index += 1;
                continue;
            };

            if next_ch == 'u' && index + 5 < chars.len() {
                let hex_slice = &chars[index + 2..=index + 5];
                if hex_slice.iter().all(|c| c.is_ascii_hexdigit()) {
                    repaired.push_str("\\u");
                    for &hc in hex_slice {
                        repaired.push(hc);
                    }
                    index += 6;
                    continue;
                }
            }

            if is_valid_escape_char(next_ch) {
                repaired.push('\\');
                repaired.push(next_ch);
                index += 2;
                continue;
            }

            repaired.push_str("\\\\");
            index += 1;
            continue;
        }

        if is_control_char(ch) {
            repaired.push_str(&escape_control_char(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }

    repaired
}

pub fn parse_json_with_repair<T: DeserializeOwned>(json: &str) -> Result<T, serde_json::Error> {
    match serde_json::from_str::<T>(json) {
        Ok(val) => Ok(val),
        Err(first_err) => {
            let repaired = repair_json(json);
            if repaired != json {
                serde_json::from_str::<T>(&repaired)
            } else {
                Err(first_err)
            }
        }
    }
}

/// 解析已完成工具调用的完整 JSON 参数文本。
///
/// 若文本为空，返回空对象 `{}`；若不是合法 JSON 对象，返回空对象并在 `arguments_error` 中附带错误原因与原始文本。
pub fn parse_tool_call_arguments(json: Option<&str>) -> ParsedToolCallArguments {
    let Some(raw) = json else {
        return ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: None,
        };
    };
    if raw.trim().is_empty() {
        return ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: None,
        };
    }

    match parse_json_with_repair::<Value>(raw) {
        Ok(Value::Object(obj)) => ParsedToolCallArguments {
            arguments: obj,
            arguments_error: None,
        },
        Ok(_) => ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: Some(format!(
                "Tool arguments must be a JSON object\n\nReceived arguments:\n{raw}"
            )),
        },
        Err(err) => ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: Some(format!("{err}\n\nReceived arguments:\n{raw}")),
        },
    }
}

/// 解析流式传输中的半截 JSON 文本，始终返回 `JsonValue`（若根为对象则可直接提取 `JsonObject`）。
pub fn parse_streaming_json_value(partial_json: Option<&str>) -> JsonValue {
    let Some(raw) = partial_json else {
        return Value::Object(Map::new());
    };
    if raw.trim().is_empty() {
        return Value::Object(Map::new());
    }
    if let Ok(val) = parse_json_with_repair::<Value>(raw) {
        return val;
    }

    let mut parser = StreamingJsonParser::new();
    parser.append(raw);
    parser
        .snapshot()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// 解析流式传输中的半截 JSON 对象，始终返回合法的 `JsonObject`。
pub fn parse_streaming_json(partial_json: Option<&str>) -> JsonObject {
    match parse_streaming_json_value(partial_json) {
        Value::Object(obj) => obj,
        _ => Map::new(),
    }
}

// =============================================================================
// StreamingJsonParser & ToolCallArgumentsStream (对照 utils/streaming-json.ts)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramePhase {
    Key,
    Colon,
    Value,
    Comma,
}

#[derive(Debug, Clone)]
enum ContainerState {
    Object {
        map: Map<String, Value>,
        current_key: Option<String>,
        phase: FramePhase,
    },
    Array {
        items: Vec<Value>,
        phase: FramePhase,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringTarget {
    Root,
    Key,
    ObjectValue,
    ArrayElement,
}

#[derive(Debug, Clone)]
struct OpenString {
    target: StringTarget,
    text: String,
    escape: String,
    high_surrogate: Option<u16>,
}

/// 容错流式 JSON 增量解析器。
///
/// 按字符单次扫描流式片段，支持未闭合字符串、未闭合转义序列、未闭合对象/数组、
/// 半截布尔/空值/数字字面量及尾随逗号或缺失冒号。
#[derive(Debug, Clone, Default)]
pub struct StreamingJsonParser {
    root: Option<Value>,
    stack: Vec<ContainerState>,
    open_string: Option<OpenString>,
    scalar: String,
    scalar_target: Option<StringTarget>,
    done: bool,
}

impl StreamingJsonParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, piece: &str) {
        for ch in piece.chars() {
            if self.done {
                break;
            }
            self.feed_char(ch);
        }
    }

    pub fn snapshot(&self) -> Option<Value> {
        let mut stack_clone = self.stack.clone();
        let mut root_clone = self.root.clone();

        // 若有未闭合的字符串值，将其当前内容写入打开的容器或根
        if let Some(open) = &self.open_string {
            match open.target {
                StringTarget::Root => {
                    root_clone = Some(Value::String(open.text.clone()));
                }
                StringTarget::ObjectValue => {
                    if let Some(ContainerState::Object {
                        map, current_key, ..
                    }) = stack_clone.last_mut()
                        && let Some(k) = current_key.clone()
                    {
                        map.insert(k, Value::String(open.text.clone()));
                    }
                }
                StringTarget::ArrayElement => {
                    if let Some(ContainerState::Array { items, .. }) = stack_clone.last_mut() {
                        items.push(Value::String(open.text.clone()));
                    }
                }
                StringTarget::Key => {
                    // Key 尚未闭合，不产生新属性
                }
            }
        } else if let Some(target) = self.scalar_target
            && let Some(val) = parse_pending_scalar(&self.scalar)
        {
            place_into(&mut root_clone, &mut stack_clone, target, val);
        }

        while let Some(frame) = stack_clone.pop() {
            let value = match frame {
                ContainerState::Object { map, .. } => Value::Object(map),
                ContainerState::Array { items, .. } => Value::Array(items),
            };
            if let Some(parent) = stack_clone.last_mut() {
                match parent {
                    ContainerState::Object {
                        map,
                        current_key,
                        phase,
                    } => {
                        if let Some(k) = current_key.take() {
                            map.insert(k, value);
                            *phase = FramePhase::Comma;
                        }
                    }
                    ContainerState::Array { items, phase } => {
                        items.push(value);
                        *phase = FramePhase::Comma;
                    }
                }
            } else {
                root_clone = Some(value);
            }
        }

        root_clone
    }

    fn feed_char(&mut self, ch: char) {
        if self.open_string.is_some() {
            self.feed_string_char(ch);
            return;
        }

        if self.scalar_target.is_some() {
            if is_delimiter(ch) {
                self.finish_scalar();
                if self.done {
                    return;
                }
            } else {
                self.scalar.push(ch);
                return;
            }
        }

        if ch.is_whitespace() {
            return;
        }

        match ch {
            ',' => {
                if let Some(top) = self.stack.last_mut() {
                    match top {
                        ContainerState::Object { phase, .. } => {
                            if *phase == FramePhase::Comma {
                                *phase = FramePhase::Key;
                            }
                        }
                        ContainerState::Array { phase, .. } => {
                            if *phase == FramePhase::Comma {
                                *phase = FramePhase::Value;
                            }
                        }
                    }
                }
            }
            ':' => {
                if let Some(ContainerState::Object { phase, .. }) = self.stack.last_mut()
                    && *phase == FramePhase::Colon
                {
                    *phase = FramePhase::Value;
                }
            }
            '"' => {
                if let Some(ContainerState::Object { phase, .. }) = self.stack.last_mut()
                    && (*phase == FramePhase::Key || *phase == FramePhase::Comma)
                {
                    *phase = FramePhase::Key;
                    self.open_string = Some(OpenString {
                        target: StringTarget::Key,
                        text: String::new(),
                        escape: String::new(),
                        high_surrogate: None,
                    });
                    return;
                }
                if let Some(target) = self.prepare_value_slot() {
                    self.open_string = Some(OpenString {
                        target,
                        text: String::new(),
                        escape: String::new(),
                        high_surrogate: None,
                    });
                }
            }
            '{' => {
                if self.prepare_value_slot().is_some() {
                    self.stack.push(ContainerState::Object {
                        map: Map::new(),
                        current_key: None,
                        phase: FramePhase::Key,
                    });
                }
            }
            '[' => {
                if self.prepare_value_slot().is_some() {
                    self.stack.push(ContainerState::Array {
                        items: Vec::new(),
                        phase: FramePhase::Value,
                    });
                }
            }
            '}' | ']' => {
                self.close_container();
            }
            _ => {
                if let Some(target) = self.prepare_value_slot() {
                    self.scalar.clear();
                    self.scalar.push(ch);
                    self.scalar_target = Some(target);
                }
            }
        }
    }

    fn prepare_value_slot(&mut self) -> Option<StringTarget> {
        let Some(top) = self.stack.last_mut() else {
            return if self.root.is_none() {
                Some(StringTarget::Root)
            } else {
                None
            };
        };

        match top {
            ContainerState::Object {
                current_key, phase, ..
            } => {
                if (*phase == FramePhase::Value || *phase == FramePhase::Colon)
                    && current_key.is_some()
                {
                    *phase = FramePhase::Value;
                    Some(StringTarget::ObjectValue)
                } else {
                    None
                }
            }
            ContainerState::Array { phase, .. } => {
                *phase = FramePhase::Value;
                Some(StringTarget::ArrayElement)
            }
        }
    }

    fn close_container(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let value = match frame {
            ContainerState::Object { map, .. } => Value::Object(map),
            ContainerState::Array { items, .. } => Value::Array(items),
        };
        self.commit_value(value);
    }

    fn commit_value(&mut self, value: Value) {
        if let Some(parent) = self.stack.last_mut() {
            match parent {
                ContainerState::Object {
                    map,
                    current_key,
                    phase,
                } => {
                    if let Some(k) = current_key.take() {
                        map.insert(k, value);
                        *phase = FramePhase::Comma;
                    }
                }
                ContainerState::Array { items, phase } => {
                    items.push(value);
                    *phase = FramePhase::Comma;
                }
            }
        } else {
            self.root = Some(value);
            self.done = true;
        }
    }

    fn finish_scalar(&mut self) {
        let token = std::mem::take(&mut self.scalar);
        let Some(target) = self.scalar_target.take() else {
            return;
        };
        if let Some(val) = parse_complete_scalar(&token) {
            place_into(&mut self.root, &mut self.stack, target, val);
            if self.stack.is_empty() && self.root.is_some() {
                self.done = true;
            }
        }
    }

    fn feed_string_char(&mut self, ch: char) {
        let mut open = self.open_string.take().expect("open_string exists");

        if !open.escape.is_empty() {
            if open.escape == "\\" {
                if ch == 'u' {
                    open.escape.push('u');
                    self.open_string = Some(open);
                    return;
                }
                open.escape.clear();
                if let Some(hi) = open.high_surrogate.take() {
                    open.text
                        .push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
                }
                let mapped = match ch {
                    '"' => Some('"'),
                    '\\' => Some('\\'),
                    '/' => Some('/'),
                    'b' => Some('\u{0008}'),
                    'f' => Some('\u{000c}'),
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    _ => None,
                };
                if let Some(decoded) = mapped {
                    open.text.push(decoded);
                } else {
                    open.text.push('\\');
                    open.text.push(ch);
                }
                self.open_string = Some(open);
                return;
            }

            // 处理 \uXXXX
            if ch.is_ascii_hexdigit() {
                open.escape.push(ch);
                if open.escape.len() == 6 {
                    let hex = &open.escape[2..6];
                    let code = u16::from_str_radix(hex, 16).unwrap_or(0xFFFD);
                    open.escape.clear();
                    if (0xD800..=0xDBFF).contains(&code) {
                        if let Some(prev_hi) = open.high_surrogate.replace(code) {
                            open.text
                                .push(char::from_u32(prev_hi as u32).unwrap_or('\u{FFFD}'));
                        }
                    } else if (0xDC00..=0xDFFF).contains(&code) {
                        if let Some(hi) = open.high_surrogate.take() {
                            let combined =
                                0x10000 + (((hi as u32 - 0xD800) << 10) | (code as u32 - 0xDC00));
                            open.text
                                .push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                        } else {
                            open.text.push('\u{FFFD}');
                        }
                    } else {
                        if let Some(hi) = open.high_surrogate.take() {
                            open.text
                                .push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
                        }
                        open.text
                            .push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
                    }
                }
                self.open_string = Some(open);
                return;
            } else {
                // 非法 \u 序列：按字面量保留，并继续处理当前字符
                if let Some(hi) = open.high_surrogate.take() {
                    open.text
                        .push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
                }
                let literal = std::mem::take(&mut open.escape);
                open.text.push_str(&literal);
            }
        }

        if ch == '\\' {
            open.escape.push('\\');
            self.open_string = Some(open);
            return;
        }

        if let Some(hi) = open.high_surrogate.take() {
            open.text
                .push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
        }

        if ch == '"' {
            match open.target {
                StringTarget::Key => {
                    if let Some(ContainerState::Object {
                        current_key, phase, ..
                    }) = self.stack.last_mut()
                    {
                        *current_key = Some(open.text);
                        *phase = FramePhase::Colon;
                    }
                }
                other => {
                    place_into(
                        &mut self.root,
                        &mut self.stack,
                        other,
                        Value::String(open.text),
                    );
                    if self.stack.is_empty() && self.root.is_some() {
                        self.done = true;
                    }
                }
            }
            return;
        }

        open.text.push(ch);
        self.open_string = Some(open);
    }
}

fn place_into(
    root: &mut Option<Value>,
    stack: &mut [ContainerState],
    target: StringTarget,
    value: Value,
) {
    match target {
        StringTarget::Root => {
            *root = Some(value);
        }
        StringTarget::ObjectValue => {
            if let Some(ContainerState::Object {
                map,
                current_key,
                phase,
            }) = stack.last_mut()
                && let Some(k) = current_key.take()
            {
                map.insert(k, value);
                *phase = FramePhase::Comma;
            }
        }
        StringTarget::ArrayElement => {
            if let Some(ContainerState::Array { items, phase }) = stack.last_mut() {
                items.push(value);
                *phase = FramePhase::Comma;
            }
        }
        StringTarget::Key => {}
    }
}

fn is_delimiter(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\t' | '\n' | '\r' | ',' | ':' | '{' | '}' | '[' | ']' | '"'
    )
}

fn parse_complete_scalar(token: &str) -> Option<Value> {
    match token {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        "null" => Some(Value::Null),
        _ => parse_json_number(token),
    }
}

fn parse_pending_scalar(token: &str) -> Option<Value> {
    if token.is_empty() || token.len() > 64 {
        return None;
    }
    if "true".starts_with(token) {
        return Some(Value::Bool(true));
    }
    if "false".starts_with(token) {
        return Some(Value::Bool(false));
    }
    if "null".starts_with(token) {
        return Some(Value::Null);
    }
    if let Some(n) = parse_json_number(token) {
        return Some(n);
    }
    // 处理末尾带有未完成指数符号（如 `1.5e` 或 `1.5e-`）或末尾小数点（如 `12.`）的数字
    let trimmed = token.trim_end_matches(['e', 'E', '+', '-']);
    if trimmed != token
        && !trimmed.is_empty()
        && let Some(n) = parse_json_number(trimmed)
    {
        return Some(n);
    }
    None
}

fn parse_json_number(token: &str) -> Option<Value> {
    if let Ok(i) = token.parse::<i64>() {
        return Some(Value::Number(Number::from(i)));
    }
    if let Ok(u) = token.parse::<u64>() {
        return Some(Value::Number(Number::from(u)));
    }
    if let Ok(f) = token.parse::<f64>()
        && f.is_finite()
    {
        return Number::from_f64(f).map(Value::Number);
    }
    None
}

/// 单个流式工具调用的参数累积器（对照 `ToolCallArgumentsStream`）。
#[derive(Debug, Clone, Default)]
pub struct ToolCallArgumentsStream {
    text: String,
    parser: StreamingJsonParser,
    arguments: JsonObject,
}

impl ToolCallArgumentsStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn append(&mut self, delta: &str) -> JsonObject {
        self.text.push_str(delta);
        self.parser.append(delta);
        self.partial()
    }

    pub fn replace(&mut self, text: &str) -> ReplaceArgumentsResult {
        if let Some(delta) = text.strip_prefix(&self.text) {
            let delta_owned = delta.to_owned();
            let arguments = if delta_owned.is_empty() {
                self.partial()
            } else {
                self.append(&delta_owned)
            };
            return ReplaceArgumentsResult {
                delta: if delta_owned.is_empty() {
                    None
                } else {
                    Some(delta_owned)
                },
                arguments,
            };
        }
        self.text.clear();
        self.parser = StreamingJsonParser::new();
        let arguments = self.append(text);
        ReplaceArgumentsResult {
            delta: None,
            arguments,
        }
    }

    pub fn finish(&self) -> ParsedToolCallArguments {
        parse_tool_call_arguments(Some(&self.text))
    }

    fn partial(&mut self) -> JsonObject {
        match self.parser.snapshot() {
            Some(Value::Object(obj)) => {
                self.arguments = obj.clone();
                obj
            }
            _ => {
                self.arguments.clear();
                Map::new()
            }
        }
    }
}
