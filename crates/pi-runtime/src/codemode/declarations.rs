use std::collections::{BTreeMap, BTreeSet};

use pi_ai::JsonValue;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::codemode::source::to_codemode_identifier;

const INDENT: &str = "  ";
pub const DEFAULT_INPUT_SCHEMA_MAX_CHARS: usize = 16_000;
const MAX_REF_EXPANSIONS: usize = 32;
const MAX_TAG_CHARS: usize = 80;

const TAG_KEYWORDS: &[&str] = &[
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
];

fn is_lower_bound(keyword: &str) -> bool {
    matches!(
        keyword,
        "minimum" | "exclusiveMinimum" | "minLength" | "minItems"
    )
}

fn is_upper_bound(keyword: &str) -> bool {
    matches!(
        keyword,
        "maximum" | "exclusiveMaximum" | "maxLength" | "maxItems"
    )
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

fn split_line_terminators(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (idx, c) = chars[i];
        if is_line_terminator(c) {
            result.push(&s[start..idx]);
            if c == '\r' && i + 1 < chars.len() && chars[i + 1].1 == '\n' {
                i += 1;
            }
            start = chars[i].0 + chars[i].1.len_utf8();
        }
        i += 1;
    }
    result.push(&s[start..]);
    result
}

fn escape_line_terminators(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_line_terminator(c) {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDeclarationItem {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RenderDeclarationsOptions {
    pub tools: Vec<CodemodeDeclarationItem>,
    pub globals: Vec<CodemodeDeclarationItem>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SchemaToTypeOptions {
    pub max_chars: Option<usize>,
}

pub fn render_declarations(options: &RenderDeclarationsOptions) -> String {
    let mut sections = Vec::new();
    if !options.tools.is_empty() {
        let members: Vec<String> = options
            .tools
            .iter()
            .map(|tool| {
                format!(
                    "{}{}{}",
                    doc_comment(tool.description.as_deref(), INDENT),
                    INDENT,
                    render_tool_signature(tool, None)
                )
            })
            .collect();
        sections.push(format!(
            "declare const tools: {{\n{}\n}};",
            members.join("\n")
        ));
    }

    let mut namespace_order: Vec<String> = Vec::new();
    let mut namespaces: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for global in &options.globals {
        if let Some(dot) = global.name.find('.') {
            let ns = global.name[..dot].to_owned();
            let member_name = &global.name[dot + 1..];
            if !namespaces.contains_key(&ns) {
                namespace_order.push(ns.clone());
            }
            namespaces
                .entry(ns)
                .or_default()
                .push(render_global(member_name, global, INDENT));
        } else {
            sections.push(render_global(
                &format!("declare function {}", global.name),
                global,
                "",
            ));
        }
    }
    for ns in namespace_order {
        if let Some(members) = namespaces.remove(&ns) {
            sections.push(format!(
                "declare const {ns}: {{\n{}\n}};",
                members.join("\n")
            ));
        }
    }

    sections.join("\n\n")
}

pub fn render_tool_signature(
    tool: &CodemodeDeclarationItem,
    input_max_chars: Option<usize>,
) -> String {
    let input = match &tool.input_schema {
        None => "unknown".to_owned(),
        Some(schema) => schema_to_type(
            schema,
            SchemaToTypeOptions {
                max_chars: Some(input_max_chars.unwrap_or(DEFAULT_INPUT_SCHEMA_MAX_CHARS)),
            },
        ),
    };
    let output = render_tool_output_type(tool.output_schema.as_ref());
    format!(
        "{}(args: {input}): Promise<{output}>;",
        to_codemode_identifier(&tool.name)
    )
}

pub fn render_tool_sample(
    tool: &CodemodeDeclarationItem,
    input_max_chars: Option<usize>,
) -> String {
    let declaration = format!(
        "declare const tools: {{ {} }};",
        render_tool_signature(tool, input_max_chars)
    );
    let desc = tool.description.as_deref().unwrap_or("").trim();
    format!("{desc}\n\ncodemode tool declaration:\n```ts\n{declaration}\n```")
}

pub fn render_tool_output_type(schema: Option<&JsonValue>) -> String {
    match schema {
        None => "unknown".to_owned(),
        Some(s) => schema_to_type(s, SchemaToTypeOptions::default()),
    }
}

fn render_global(head: &str, global: &CodemodeDeclarationItem, indent: &str) -> String {
    if let Some(sig) = &global.signature {
        return format!(
            "{}{indent}{head}{sig};",
            doc_comment(global.description.as_deref(), indent)
        );
    }
    let input = global.input_schema.as_ref().map_or_else(
        || "unknown".to_owned(),
        |s| schema_to_type(s, SchemaToTypeOptions::default()),
    );
    let output = global.output_schema.as_ref().map_or_else(
        || "unknown".to_owned(),
        |s| schema_to_type(s, SchemaToTypeOptions::default()),
    );
    format!(
        "{}{indent}{head}(args: {input}): Promise<{output}>;",
        doc_comment(global.description.as_deref(), indent)
    )
}

fn doc_comment(description: Option<&str>, indent: &str) -> String {
    let text = description.unwrap_or("").trim();
    if text.is_empty() {
        return String::new();
    }
    let escaped = text.replace("*/", "*\\/");
    let lines: Vec<&str> = escaped.lines().collect();
    if lines.len() == 1 {
        return format!("{indent}/** {} */\n", lines[0]);
    }
    let body = lines
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                format!("{indent} *")
            } else {
                format!("{indent} * {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{indent}/**\n{body}\n{indent} */\n")
}

fn property_key(name: &str) -> String {
    if is_identifier(name) {
        name.to_owned()
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""))
    }
}

fn union_types(types: Vec<String>) -> String {
    let mut unique = Vec::new();
    let mut seen = BTreeSet::new();
    for t in types {
        if t == "unknown" {
            return "unknown".to_owned();
        }
        if seen.insert(t.clone()) {
            unique.push(t);
        }
    }
    if unique.is_empty() {
        "never".to_owned()
    } else {
        unique.join(" | ")
    }
}

pub fn schema_to_type(schema: &JsonValue, options: SchemaToTypeOptions) -> String {
    let render = |tags: bool| {
        let mut ctx = SchemaContext {
            root: schema,
            resolving: BTreeSet::new(),
            expansions: 0,
            tags,
        };
        to_type(schema, &mut ctx)
    };
    let rendered = render(true);
    let Some(max_chars) = options.max_chars else {
        return rendered;
    };
    if rendered.chars().count() <= max_chars {
        return rendered;
    }
    let untagged = render(false);
    if untagged.chars().count() <= max_chars {
        untagged
    } else {
        "unknown".to_owned()
    }
}

struct SchemaContext<'a> {
    root: &'a JsonValue,
    resolving: BTreeSet<String>,
    expansions: usize,
    tags: bool,
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex_str, 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_owned())
}

fn resolve_ref<'a>(ref_str: &str, root: &'a JsonValue) -> Option<&'a JsonValue> {
    if ref_str != "#" && !ref_str.starts_with("#/") {
        return None;
    }
    let mut current = root;
    if ref_str == "#" {
        return match current {
            Value::Bool(_) | Value::Object(_) => Some(current),
            _ => None,
        };
    }
    for segment in ref_str[2..].split('/').filter(|s| !s.is_empty()) {
        let key = percent_decode(segment)
            .replace("~1", "/")
            .replace("~0", "~");
        let obj = current.as_object()?;
        current = obj.get(&key)?;
    }
    match current {
        Value::Bool(_) | Value::Object(_) => Some(current),
        _ => None,
    }
}

fn to_type(schema: &JsonValue, context: &mut SchemaContext<'_>) -> String {
    match schema {
        Value::Bool(true) => return "unknown".to_owned(),
        Value::Bool(false) => return "never".to_owned(),
        Value::Object(_) => {}
        _ => return "unknown".to_owned(),
    }
    let obj = schema.as_object().expect("checked object");

    if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()) {
        if context.resolving.contains(ref_str) || context.expansions >= MAX_REF_EXPANSIONS {
            return "unknown".to_owned();
        }
        let Some(target) = resolve_ref(ref_str, context.root) else {
            return "unknown".to_owned();
        };
        context.expansions += 1;
        context.resolving.insert(ref_str.to_owned());
        let res = to_type(target, context);
        context.resolving.remove(ref_str);
        return res;
    }

    if let Some(const_val) = obj.get("const") {
        return serde_json::to_string(const_val).unwrap_or_else(|_| "unknown".to_owned());
    }
    if let Some(enum_vals) = obj.get("enum").and_then(|v| v.as_array()) {
        let types = enum_vals
            .iter()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "unknown".to_owned()))
            .collect();
        return union_types(types);
    }

    let variants = obj
        .get("anyOf")
        .and_then(|v| v.as_array())
        .or_else(|| obj.get("oneOf").and_then(|v| v.as_array()));
    if let Some(variants) = variants {
        let types = variants.iter().map(|v| to_type(v, context)).collect();
        return union_types(types);
    }

    if let Some(all_of) = obj.get("allOf").and_then(|v| v.as_array()) {
        let parts: Vec<String> = all_of
            .iter()
            .map(|part| to_type(part, context))
            .filter(|part| part != "unknown")
            .collect();
        if parts.is_empty() {
            return "unknown".to_owned();
        }
        return parts
            .into_iter()
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" & ");
    }

    if let Some(type_arr) = obj.get("type").and_then(|v| v.as_array()) {
        let types = type_arr
            .iter()
            .map(|entry| {
                let mut cloned = obj.clone();
                cloned.insert("type".to_owned(), entry.clone());
                to_type(&Value::Object(cloned), context)
            })
            .collect();
        return union_types(types);
    }

    match obj.get("type").and_then(|v| v.as_str()) {
        Some("string") => "string".to_owned(),
        Some("number" | "integer") => "number".to_owned(),
        Some("boolean") => "boolean".to_owned(),
        Some("null") => "null".to_owned(),
        Some("array") => array_type(obj, context),
        Some("object") => object_type(obj, context),
        None => {
            if obj.contains_key("properties")
                || obj.contains_key("additionalProperties")
                || obj.contains_key("required")
            {
                object_type(obj, context)
            } else if obj.contains_key("items") || obj.contains_key("prefixItems") {
                array_type(obj, context)
            } else {
                "unknown".to_owned()
            }
        }
        Some(_) => "unknown".to_owned(),
    }
}

fn array_type(obj: &Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    if let Some(items) = obj.get("items")
        && !items.is_array()
    {
        return format!("Array<{}>", to_type(items, context));
    }
    let tuple = obj
        .get("prefixItems")
        .and_then(|v| v.as_array())
        .or_else(|| obj.get("items").and_then(|v| v.as_array()));
    if let Some(tuple) = tuple
        && !tuple.is_empty()
    {
        let items: Vec<String> = tuple.iter().map(|item| to_type(item, context)).collect();
        return format!("[{}]", items.join(", "));
    }
    "unknown[]".to_owned()
}

fn description_of(property: &Value) -> String {
    property
        .as_object()
        .and_then(|o| o.get("description"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_owned()
}

fn format_number_value(v: &Value) -> Option<(f64, String)> {
    let n = v.as_f64()?;
    let text = if let Some(i) = v.as_i64() {
        i.to_string()
    } else if let Some(u) = v.as_u64() {
        u.to_string()
    } else if n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        v.to_string()
    };
    Some((n, text))
}

fn constraint_tags(property: &Value) -> String {
    let Some(prop_obj) = property.as_object() else {
        return String::new();
    };
    let variants = prop_obj
        .get("anyOf")
        .and_then(|v| v.as_array())
        .or_else(|| prop_obj.get("oneOf").and_then(|v| v.as_array()));
    let non_null: Vec<&Value> = variants
        .map(|arr| {
            arr.iter()
                .filter(|v| {
                    v.as_object()
                        .and_then(|o| o.get("type"))
                        .and_then(|t| t.as_str())
                        != Some("null")
                })
                .collect()
        })
        .unwrap_or_default();
    let branch_obj = if non_null.len() == 1 {
        non_null[0].as_object()
    } else {
        None
    };

    let raw_branches: Vec<&Map<String, Value>> = match branch_obj {
        Some(b) => vec![prop_obj, b],
        None => vec![prop_obj],
    };

    let schemas: Vec<Map<String, Value>> = raw_branches
        .into_iter()
        .map(|s| {
            let mut m = s.clone();
            if s.get("exclusiveMinimum") == Some(&Value::Bool(true)) {
                if let Some(min_val) = m.remove("minimum") {
                    m.insert("exclusiveMinimum".to_owned(), min_val);
                } else {
                    m.remove("exclusiveMinimum");
                }
            }
            if s.get("exclusiveMaximum") == Some(&Value::Bool(true)) {
                if let Some(max_val) = m.remove("maximum") {
                    m.insert("exclusiveMaximum".to_owned(), max_val);
                } else {
                    m.remove("exclusiveMaximum");
                }
            }
            m
        })
        .collect();

    let mut tags = Vec::new();
    let is_integer = schemas.iter().any(|s| match s.get("type") {
        Some(Value::String(t)) => t == "integer",
        Some(Value::Array(arr)) => arr.iter().any(|v| v.as_str() == Some("integer")),
        _ => false,
    });
    if is_integer {
        tags.push("@integer".to_owned());
    }

    for &keyword in TAG_KEYWORDS {
        if is_lower_bound(keyword) || is_upper_bound(keyword) {
            let nums: Vec<(f64, String)> = schemas
                .iter()
                .filter_map(|s| s.get(keyword).and_then(format_number_value))
                .collect();
            if nums.is_empty() {
                continue;
            }
            let chosen = if is_lower_bound(keyword) {
                nums.into_iter()
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, s)| s)
            } else {
                nums.into_iter()
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, s)| s)
            };
            if let Some(text) = chosen {
                tags.push(format!("@{keyword} {text}"));
            }
            continue;
        }

        let mut seen = BTreeSet::new();
        for s in &schemas {
            let Some(val) = s.get(keyword) else {
                continue;
            };
            let text_opt = if let Some((_, num_str)) = format_number_value(val) {
                Some(num_str)
            } else {
                val.as_str().map(ToOwned::to_owned)
            };
            if let Some(text) = text_opt
                && seen.insert(text.clone())
            {
                tags.push(format!("@{keyword} {text}"));
            }
        }
    }

    if schemas
        .iter()
        .any(|s| s.get("uniqueItems") == Some(&Value::Bool(true)))
    {
        tags.push("@uniqueItems".to_owned());
    }
    if let Some(def_val) = prop_obj.get("default")
        && !def_val.is_null()
        && let Ok(json_str) = serde_json::to_string(def_val)
    {
        tags.push(format!("@default {json_str}"));
    }
    if prop_obj.get("deprecated") == Some(&Value::Bool(true)) {
        tags.push("@deprecated".to_owned());
    }

    tags.into_iter()
        .filter(|tag| tag.chars().count() <= MAX_TAG_CHARS)
        .map(|tag| escape_line_terminators(&tag))
        .collect::<Vec<_>>()
        .join(" ")
}

fn object_type(obj: &Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    let empty_props = Map::new();
    let properties = obj
        .get("properties")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty_props);
    let required: BTreeSet<&str> = obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut names: Vec<&String> = properties.keys().collect();
    names.sort();

    let mut members: Vec<String> = names
        .iter()
        .map(|name| {
            let optional = if required.contains(name.as_str()) {
                ""
            } else {
                "?"
            };
            let prop_type = to_type(&properties[*name], context);
            format!("{}{optional}: {prop_type};", property_key(name))
        })
        .collect();

    match obj.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => {
            members.push("[key: string]: unknown;".to_owned());
        }
        Some(other) => {
            let t = to_type(other, context);
            members.push(format!("[key: string]: {t};"));
        }
        None => {
            if names.is_empty() {
                members.push("[key: string]: unknown;".to_owned());
            }
        }
    }

    if members.is_empty() {
        return "{}".to_owned();
    }

    let comments: Vec<Vec<String>> = names
        .iter()
        .map(|name| {
            let prop = &properties[*name];
            let desc = description_of(prop);
            let mut lines: Vec<String> = split_line_terminators(&desc)
                .into_iter()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            if context.tags {
                let tags = constraint_tags(prop);
                if !tags.is_empty() {
                    lines.push(tags);
                }
            }
            lines
        })
        .collect();

    if !comments.iter().any(|c| !c.is_empty()) {
        return format!("{{ {} }}", members.join(" "));
    }

    let mut lines = vec!["{".to_owned()];
    for (idx, comment) in comments.iter().enumerate() {
        for line in comment {
            lines.push(format!("{INDENT}// {line}"));
        }
        let indented_member = members[idx].replace('\n', &format!("\n{INDENT}"));
        lines.push(format!("{INDENT}{indented_member}"));
    }
    for member in members.into_iter().skip(names.len()) {
        lines.push(format!("{INDENT}{member}"));
    }
    lines.push("}".to_owned());
    lines.join("\n")
}
