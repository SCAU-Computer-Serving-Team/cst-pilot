use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use icu_normalizer::ComposingNormalizer;
use pi_agent::AgentToolResult;
use pi_ai::{JsonValue, UserContentBlock};
use pi_session::{
    Extension, ExtensionFactory, ExtensionRegistrar, InlineExtension, SourceInfo, ToolDefinition,
    ToolExposure, ToolInfo, ToolNamespace,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use unicode_segmentation::UnicodeSegmentation;

pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
pub const DEFAULT_TOOL_SEARCH_LIMIT: usize = 8;

pub const TOOL_SEARCH_DESCRIPTION: &str = "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search`.";

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "that", "the", "this", "to", "with",
];

const NAME_WEIGHT: f64 = 3.0;
const NAMESPACE_WEIGHT: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchDocument {
    pub name: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchMatch {
    pub name: String,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchResultTool {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchToolDetails {
    pub loaded: Vec<String>,
}

pub trait ToolRanker: Send + Sync {
    fn rank(
        &self,
        query: &str,
        documents: &[ToolSearchDocument],
        limit: usize,
    ) -> Vec<ToolSearchMatch>;
}

pub trait ToolSearchInventory: Send + Sync {
    fn get_all_tools(&self) -> Vec<ToolInfo>;
    fn get_active_tools(&self) -> Vec<String>;
    fn set_active_tools(&self, tool_names: Vec<String>);
}

#[derive(Clone, Default)]
pub struct ToolSearchToolOptions {
    pub tools: Option<Arc<dyn ToolSearchInventory>>,
}

fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.binary_search(&word).is_ok()
}

fn stem(term: &str) -> String {
    if term.len() > 4 && term.ends_with("ies") {
        let mut s = term[..term.len() - 3].to_owned();
        s.push('y');
        return s;
    }
    if term.len() > 4
        && (term.ends_with("ches")
            || term.ends_with("shes")
            || term.ends_with("sses")
            || term.ends_with("xes")
            || term.ends_with("zes"))
    {
        return term[..term.len() - 2].to_owned();
    }
    if term.len() > 3 && term.ends_with('s') && !term.ends_with("ss") {
        return term[..term.len() - 1].to_owned();
    }
    term.to_owned()
}

fn is_word_medial_connector(c: char) -> bool {
    matches!(
        c,
        '\u{00B7}'
            | '\u{0387}'
            | '\u{055F}'
            | '\u{05F3}'
            | '\u{05F4}'
            | '\u{2018}'
            | '\u{2019}'
            | '\u{2024}'
            | '\u{2027}'
            | '\u{203F}'
            | '\u{2040}'
            | '\u{2054}'
            | '\u{FE13}'
            | '\u{FE33}'
            | '\u{FE34}'
            | '\u{FE4D}'
            | '\u{FE4E}'
            | '\u{FE4F}'
            | '\u{FF07}'
            | '\u{FF1A}'
            | '\u{FF3F}'
    )
}

/// Unicode word segmentation with identifier boundaries and English stop words/singular forms.
pub fn tokenize(text: &str) -> Vec<String> {
    let normalizer = ComposingNormalizer::new_nfc();
    let chars: Vec<char> = normalizer.normalize_iter(text.chars()).collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut camel_split = String::with_capacity(chars.len() + 8);
    for i in 0..chars.len() {
        let c = chars[i];
        camel_split.push(c);
        if i + 1 < chars.len() {
            let next = chars[i + 1];
            let lower_to_upper =
                (c.is_ascii_lowercase() || c.is_ascii_digit()) && next.is_ascii_uppercase();
            let acronym_to_word = c.is_ascii_uppercase()
                && next.is_ascii_uppercase()
                && i + 2 < chars.len()
                && chars[i + 2].is_ascii_lowercase();
            if lower_to_upper || acronym_to_word {
                camel_split.push(' ');
            }
        }
    }

    let lowered = camel_split.to_lowercase();
    let cleaned: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii() {
                if c.is_ascii_lowercase() || c.is_ascii_digit() {
                    c
                } else {
                    ' '
                }
            } else if c.is_whitespace() || is_word_medial_connector(c) {
                ' '
            } else {
                c
            }
        })
        .collect();

    let is_pure_ascii = cleaned
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b' ');

    let raw_words: Vec<&str> = if is_pure_ascii {
        cleaned.split(' ').filter(|w| !w.is_empty()).collect()
    } else {
        cleaned
            .split_whitespace()
            .flat_map(|chunk| chunk.unicode_words())
            .filter(|w| !w.is_empty())
            .collect()
    };

    raw_words
        .into_iter()
        .filter(|word| !is_stop_word(word))
        .map(|word| {
            if word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            {
                stem(word)
            } else {
                word.to_owned()
            }
        })
        .collect()
}

fn collect_schema_text(schema: &JsonValue, parts: &mut Vec<String>) {
    let Some(obj) = schema.as_object() else {
        return;
    };
    if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
        parts.push(desc.to_owned());
    }
    if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
        for (name, prop_schema) in props {
            parts.push(name.clone());
            collect_schema_text(prop_schema, parts);
        }
    }
    if let Some(items) = obj.get("items") {
        collect_schema_text(items, parts);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(variants) = obj.get(key).and_then(|v| v.as_array()) {
            for variant in variants {
                collect_schema_text(variant, parts);
            }
        }
    }
}

pub fn create_tool_search_document(
    name: &str,
    description: &str,
    parameters: &JsonValue,
    namespace: Option<&ToolNamespace>,
) -> ToolSearchDocument {
    let mut parts = vec![description.to_owned()];
    collect_schema_text(parameters, &mut parts);
    let text = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let namespace_text = Some(
        namespace
            .map(|ns| {
                format!(
                    "{} {} {}",
                    ns.name,
                    ns.description.as_deref().unwrap_or(""),
                    ns.instructions.as_deref().unwrap_or("")
                )
            })
            .unwrap_or_default(),
    );
    ToolSearchDocument {
        name: name.to_owned(),
        text,
        namespace_text,
    }
}

pub fn create_tool_search_document_from_info(tool: &ToolInfo) -> ToolSearchDocument {
    create_tool_search_document(
        &tool.name,
        &tool.description,
        &tool.parameters,
        tool.namespace.as_ref(),
    )
}

fn normalize_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn is_exact_name_query(query: &str, name: &str) -> bool {
    let normalized = normalize_name(name);
    normalized == query || normalized.ends_with(&format!("__{query}"))
}

struct IndexedDocument {
    name: String,
    name_terms: BTreeSet<String>,
    local_name_terms: Vec<String>,
    text_counts: BTreeMap<String, usize>,
    text_length: usize,
    namespace_terms: BTreeSet<String>,
}

fn index_document(document: &ToolSearchDocument) -> IndexedDocument {
    let mut text_counts = BTreeMap::new();
    for term in tokenize(&document.text) {
        *text_counts.entry(term).or_insert(0) += 1;
    }
    let text_length = text_counts.values().sum();
    let last_segment = document.name.rsplit("__").next().unwrap_or(&document.name);
    let mut local_name_terms = Vec::new();
    let mut seen_local = BTreeSet::new();
    for term in tokenize(last_segment) {
        if seen_local.insert(term.clone()) {
            local_name_terms.push(term);
        }
    }
    IndexedDocument {
        name: document.name.clone(),
        name_terms: tokenize(&document.name).into_iter().collect(),
        local_name_terms,
        text_counts,
        text_length,
        namespace_terms: tokenize(document.namespace_text.as_deref().unwrap_or(""))
            .into_iter()
            .collect(),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Bm25Ranker {
    pub k1: f64,
    pub b: f64,
}

impl Default for Bm25Ranker {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

impl Bm25Ranker {
    pub fn new(k1: f64, b: f64) -> Self {
        Self { k1, b }
    }
}

impl ToolRanker for Bm25Ranker {
    fn rank(
        &self,
        query: &str,
        documents: &[ToolSearchDocument],
        limit: usize,
    ) -> Vec<ToolSearchMatch> {
        let mut query_terms = Vec::new();
        let mut query_term_set = BTreeSet::new();
        for term in tokenize(query) {
            if query_term_set.insert(term.clone()) {
                query_terms.push(term);
            }
        }
        if query_terms.is_empty() || documents.is_empty() || limit == 0 {
            return Vec::new();
        }

        let indexed: Vec<IndexedDocument> = documents.iter().map(index_document).collect();
        let total_len: usize = indexed.iter().map(|d| d.text_length).sum();
        let average_length = if total_len == 0 {
            1.0
        } else {
            (total_len as f64) / (indexed.len() as f64)
        };

        let doc_count = indexed.len() as f64;
        let mut idf = BTreeMap::new();
        for term in &query_terms {
            let frequency = indexed
                .iter()
                .filter(|doc| {
                    doc.name_terms.contains(term)
                        || doc.text_counts.contains_key(term)
                        || doc.namespace_terms.contains(term)
                })
                .count() as f64;
            let val = (1.0 + (doc_count - frequency + 0.5) / (frequency + 0.5)).ln();
            idf.insert(term.clone(), val);
        }

        let full_name_bonus: f64 = idf.values().sum();
        let exact_query = normalize_name(query);

        struct CandidateMatch {
            name: String,
            score: f64,
            covered: usize,
            exact: bool,
        }

        let mut matches = Vec::new();
        for document in &indexed {
            let norm = self.k1
                * (1.0 - self.b + (self.b * (document.text_length as f64)) / average_length);
            let mut score = 0.0;
            let mut covered = 0usize;
            for term in &query_terms {
                let count = *document.text_counts.get(term).unwrap_or(&0) as f64;
                let weight = (if document.name_terms.contains(term) {
                    NAME_WEIGHT
                } else {
                    0.0
                }) + (if document.namespace_terms.contains(term) {
                    NAMESPACE_WEIGHT
                } else {
                    0.0
                }) + if count > 0.0 {
                    (count * (self.k1 + 1.0)) / (count + norm)
                } else {
                    0.0
                };
                if weight > 0.0 {
                    covered += 1;
                }
                score += idf.get(term).copied().unwrap_or(0.0) * weight;
            }
            if score == 0.0 {
                continue;
            }
            if !document.local_name_terms.is_empty()
                && document
                    .local_name_terms
                    .iter()
                    .all(|term| query_term_set.contains(term))
            {
                score += full_name_bonus;
            }
            matches.push(CandidateMatch {
                name: document.name.clone(),
                score,
                covered,
                exact: is_exact_name_query(&exact_query, &document.name),
            });
        }

        matches.sort_by(|a, b| {
            b.exact
                .cmp(&a.exact)
                .then_with(|| b.covered.cmp(&a.covered))
                .then_with(|| b.score.total_cmp(&a.score))
        });

        matches
            .into_iter()
            .take(limit)
            .map(|m| ToolSearchMatch {
                name: m.name,
                score: m.score,
            })
            .collect()
    }
}

pub fn is_searchable_exposure(exposure: ToolExposure) -> bool {
    matches!(exposure, ToolExposure::Codemode | ToolExposure::Deferred)
}

fn search_and_load_with_lists(
    all_tools: Vec<ToolInfo>,
    active: Vec<String>,
    query: &str,
    limit: usize,
) -> (Vec<ToolSearchResultTool>, Option<Vec<String>>) {
    let active_set: BTreeSet<&str> = active.iter().map(String::as_str).collect();
    let candidates: Vec<ToolInfo> = all_tools
        .into_iter()
        .filter(|tool| {
            is_searchable_exposure(tool.exposure) && !active_set.contains(tool.name.as_str())
        })
        .collect();
    let documents: Vec<ToolSearchDocument> = candidates
        .iter()
        .map(create_tool_search_document_from_info)
        .collect();
    let matches = Bm25Ranker::default().rank(query, &documents, limit);
    let updated_active = if matches.is_empty() {
        None
    } else {
        let mut next = active;
        next.extend(matches.iter().map(|m| m.name.clone()));
        Some(next)
    };
    let result_tools = matches
        .into_iter()
        .map(|m| {
            let description = candidates
                .iter()
                .find(|t| t.name == m.name)
                .map(|t| t.description.clone())
                .unwrap_or_default();
            ToolSearchResultTool {
                name: m.name,
                description,
            }
        })
        .collect();
    (result_tools, updated_active)
}

pub fn create_tool_search_tool_definition(options: ToolSearchToolOptions) -> ToolDefinition {
    let parameters = json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Search query for deferred tools."
            },
            "limit": {
                "type": "number",
                "description": format!("Maximum number of tools to return. Defaults to {DEFAULT_TOOL_SEARCH_LIMIT}.")
            }
        },
        "required": ["query"],
        "additionalProperties": false
    });

    let custom_inventory = options.tools;
    ToolDefinition::new(
        TOOL_SEARCH_TOOL_NAME,
        TOOL_SEARCH_TOOL_NAME,
        TOOL_SEARCH_DESCRIPTION,
        parameters,
        Arc::new(move |_tool_call_id, args, _cancel, _on_update, ctx| {
            let custom_inventory = custom_inventory.clone();
            Box::pin(async move {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                if query.trim().is_empty() {
                    return Err("query must not be empty".to_owned());
                }

                let limit = match args.get("limit") {
                    None | Some(JsonValue::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
                    Some(val) => {
                        let Some(num) = val.as_f64() else {
                            return Err("limit must be a positive integer".to_owned());
                        };
                        if !num.is_finite() || num.fract() != 0.0 || num <= 0.0 {
                            return Err("limit must be a positive integer".to_owned());
                        }
                        num as usize
                    }
                };

                let tools = if let Some(inv) = custom_inventory {
                    let (found, next_active) = search_and_load_with_lists(
                        inv.get_all_tools(),
                        inv.get_active_tools(),
                        &query,
                        limit,
                    );
                    if let Some(next) = next_active {
                        inv.set_active_tools(next);
                    }
                    found
                } else {
                    let runtime = ctx.runtime();
                    let all_tools = runtime.get_all_tools().unwrap_or_default();
                    let active = runtime.get_requested_tools()?;
                    let (found, next_active) =
                        search_and_load_with_lists(all_tools, active, &query, limit);
                    if let Some(next) = next_active {
                        let _ = runtime.set_active_tools(next);
                    }
                    found
                };

                let text = if tools.is_empty() {
                    "No matching tools found.".to_owned()
                } else {
                    let lines = tools
                        .iter()
                        .map(|tool| {
                            let first_line = tool.description.trim().lines().next().unwrap_or("");
                            format!("- {}: {}", tool.name, first_line)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "Loaded {} tool{}. They are available from your next call:\n{}",
                        tools.len(),
                        if tools.len() == 1 { "" } else { "s" },
                        lines
                    )
                };

                let loaded: Vec<String> = tools.into_iter().map(|t| t.name).collect();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::text(text)],
                    details: json!({ "loaded": loaded }),
                    structured_content: None,
                    usage: None,
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    )
    .with_prompt_snippet("Search for tools that are not loaded yet and load the matches")
    .with_exposure(ToolExposure::ModelOnly)
}

pub fn create_tool_search_extension() -> ExtensionFactory {
    Arc::new(|| {
        Ok(Box::new(
            InlineExtension::new("builtin:tool-search", |pi: &mut ExtensionRegistrar| {
                let def = create_tool_search_tool_definition(ToolSearchToolOptions::default())
                    .with_default_active(false);
                pi.register_tool(def);
                Ok(())
            })
            .with_source_info(SourceInfo::builtin("builtin:tool-search"))
            .with_replaceable(true),
        ) as Box<dyn Extension>)
    })
}
