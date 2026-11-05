use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;
use pi_agent::{AgentTool, AgentToolResult};
use pi_ai::{ImageContent, JsonValue, Usage, UserContentBlock};
use pi_session::{
    Extension, ExtensionFactory, ExtensionRegistrar, InlineExtension, NestedToolCallOptions,
    SessionEntry, SourceInfo, ToolDefinition, ToolExposure, ToolInfo, ToolLoadout,
    ToolLoadoutChanges, ToolNamespace,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::codemode::{
    CodemodeCall, CodemodeDeclarationItem, CodemodeError, CodemodeErrorKind,
    CodemodeExecuteOptions, CodemodeOutputItem, CodemodeSandbox, CodemodeSandboxOptions,
    CodemodeStoreWrites, CodemodeTool, parse_codemode_source, render_tool_output_type,
    render_tool_sample, render_tool_signature, to_codemode_identifier,
};
use crate::extensions::tool_search::{
    Bm25Ranker, DEFAULT_TOOL_SEARCH_LIMIT, ToolRanker, create_tool_search_document,
};
use crate::settings_manager::SettingsManager;

pub const CODEMODE_TOOL_NAME: &str = "codemode";
pub const CODEMODE_STORE_ENTRY_TYPE: &str = "codemode-store";
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 10_000;
pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_INLINE_BUDGET: usize = 3_000;
pub const DEFAULT_MAX_CODEMODE_WORKERS: usize = 2;
const WORKER_SLOT_WAIT_MS: u64 = 60_000;
const CHARS_PER_TOKEN: usize = 4;

pub const CODEMODE_ONLY_GUIDELINE: &str =
    "Run tools by calling the codemode tool with JavaScript that awaits tools.<name>(args)";

pub const CODEMODE_DESCRIPTION: &str = "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.
- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.
- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`

Globals:
- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.
- `store(key, value)` and `load(key)` keep JSON values across codemode calls.
- `models.getModelsOfType(type, provider?)`, `models.getAvailableOfType(type, provider?)`, `models.getModelOfType(type, provider, id)` discover chat/classifier models. `models.classify(model, context)` runs a classifier; calls are limited to four in flight and their usage is recorded.
- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find unlisted tools, such as MCP tools.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeMode {
    #[default]
    On,
    Only,
}

#[derive(Debug, Clone, Default)]
pub struct CodemodeDescriptionOptions {
    pub namespaces: BTreeMap<String, ToolNamespace>,
    pub deferred: BTreeSet<String>,
    pub inline_budget: Option<usize>,
    pub docs_path: Option<String>,
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

fn tool_sample_from_agent_tool(tool: &AgentTool) -> String {
    let item = CodemodeDeclarationItem {
        name: tool.name.clone(),
        description: Some(tool.description.clone()),
        input_schema: Some(tool.parameters().clone()),
        output_schema: tool.output_schema.clone(),
        signature: None,
    };
    format!("### `{}`\n\n{}", tool.name, render_tool_sample(&item, None))
}

pub fn create_codemode_description(
    tools: &[AgentTool],
    options: &CodemodeDescriptionOptions,
) -> String {
    let mut description = CODEMODE_DESCRIPTION.to_owned();
    if let Some(docs_path) = &options.docs_path {
        description.push_str(&format!(
            "\n- `models`: classifiers and image generation. Read {docs_path} first."
        ));
    }

    let visible: Vec<&AgentTool> = tools
        .iter()
        .filter(|tool| !options.deferred.contains(&tool.name))
        .collect();
    if visible.is_empty() {
        return description;
    }

    struct Group {
        namespace: Option<ToolNamespace>,
        tools: Vec<String>,
        listed: Vec<String>,
    }

    let mut group_order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for tool in visible {
        let ns = options.namespaces.get(&tool.name).cloned();
        let key = ns.as_ref().map(|n| n.name.clone()).unwrap_or_default();
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
            groups.insert(
                key.clone(),
                Group {
                    namespace: ns,
                    tools: Vec::new(),
                    listed: Vec::new(),
                },
            );
        }
        groups
            .get_mut(&key)
            .expect("group exists")
            .tools
            .push(tool_sample_from_agent_tool(tool));
    }

    if let Some(mut remaining) = options.inline_budget {
        type GroupQueue = Vec<(usize, String, usize)>;
        let mut queues: Vec<(String, GroupQueue)> = group_order
            .iter()
            .map(|key| {
                let g = &groups[key];
                let mut entries: Vec<(usize, String, usize)> = g
                    .tools
                    .iter()
                    .enumerate()
                    .map(|(idx, text)| (idx, text.clone(), estimate_tokens(text)))
                    .collect();
                entries.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
                (key.clone(), entries)
            })
            .collect();

        loop {
            let mut added = false;
            for (key, queue) in &mut queues {
                if let Some((_, text, tokens)) = queue.first()
                    && *tokens <= remaining
                {
                    remaining -= *tokens;
                    groups
                        .get_mut(key)
                        .expect("group exists")
                        .listed
                        .push(text.clone());
                    queue.remove(0);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
    } else {
        for g in groups.values_mut() {
            g.listed = g.tools.clone();
        }
    }

    let mut sections: Vec<String> = Vec::new();
    for key in group_order {
        let Some(group) = groups.remove(&key) else {
            continue;
        };
        let Some(ns) = group.namespace else {
            sections.extend(group.listed);
            continue;
        };
        let omitted = group.tools.len().saturating_sub(group.listed.len());
        let suffix = if omitted == 0 {
            ""
        } else if group.listed.is_empty() {
            " (tools not listed)"
        } else {
            " (some tools not listed)"
        };
        let header = if let Some(desc) = ns.description.as_deref().filter(|s| !s.is_empty()) {
            format!("## {}{suffix}\n{desc}", ns.name)
        } else {
            format!("## {}{suffix}", ns.name)
        };
        let mut section_parts = vec![header];
        section_parts.extend(group.listed);
        sections.push(section_parts.join("\n\n"));
    }

    if sections.is_empty() {
        description
    } else {
        format!(
            "{description}\n\nNested tools:\n\n{}",
            sections.join("\n\n")
        )
    }
}

fn declared_tool_note(tool: &AgentTool) -> String {
    let out = render_tool_output_type(tool.output_schema.as_ref());
    let out_type = if out == "unknown" {
        "string".to_owned()
    } else {
        out
    };
    format!(
        "\n\nCodemode: `tools.{}(args)` resolves to `{out_type}`.",
        to_codemode_identifier(&tool.name)
    )
}

fn prepare_codemode_loadout(
    loadout: &ToolLoadout,
    mode: CodemodeMode,
    inline_budget: Option<usize>,
) -> Option<ToolLoadoutChanges> {
    let active_set: BTreeSet<&str> = loadout
        .active_tool_names
        .iter()
        .map(String::as_str)
        .collect();
    let callable_defs: Vec<&ToolDefinition> = loadout
        .all_tools
        .iter()
        .filter(|d| {
            d.name != CODEMODE_TOOL_NAME
                && (matches!(d.exposure, ToolExposure::Codemode | ToolExposure::Deferred)
                    || (d.exposure == ToolExposure::Direct && active_set.contains(d.name.as_str())))
        })
        .collect();

    let callable_names: BTreeSet<&str> = callable_defs.iter().map(|d| d.name.as_str()).collect();
    let mut namespaces = BTreeMap::new();
    let mut deferred = BTreeSet::new();
    for def in &callable_defs {
        if let Some(ns) = &def.namespace {
            namespaces.insert(def.name.clone(), ns.clone());
        }
        if def.exposure == ToolExposure::Deferred {
            deferred.insert(def.name.clone());
        }
    }

    let callable_agent_tools: Vec<AgentTool> = callable_defs
        .iter()
        .map(|d| {
            let mut t = AgentTool::new(
                d.name.clone(),
                d.label.clone(),
                d.description.clone(),
                d.parameters.clone(),
                Arc::new(|_, _, _, _| Box::pin(async { Err("stub".to_owned()) })),
            );
            t.output_schema = d.output_schema.clone();
            t
        })
        .collect();

    let desc_opts = CodemodeDescriptionOptions {
        namespaces,
        deferred,
        inline_budget,
        docs_path: None,
    };

    if mode == CodemodeMode::Only {
        let mut descriptions = BTreeMap::new();
        descriptions.insert(
            CODEMODE_TOOL_NAME.to_owned(),
            create_codemode_description(&callable_agent_tools, &desc_opts),
        );
        let hidden_declarations = loadout
            .declared_tools
            .iter()
            .filter(|t| {
                t.name != CODEMODE_TOOL_NAME
                    && loadout.get_exposure(&t.name) == ToolExposure::Direct
            })
            .map(|t| t.name.clone())
            .collect();
        return Some(ToolLoadoutChanges {
            descriptions,
            hidden_declarations,
        });
    }

    let declared_names: BTreeSet<&str> = loadout
        .declared_tools
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let unlisted: Vec<AgentTool> = callable_agent_tools
        .into_iter()
        .filter(|t| !declared_names.contains(t.name.as_str()))
        .collect();

    let mut descriptions = BTreeMap::new();
    for tool in &loadout.declared_tools {
        if tool.name == CODEMODE_TOOL_NAME {
            descriptions.insert(
                CODEMODE_TOOL_NAME.to_owned(),
                create_codemode_description(&unlisted, &desc_opts),
            );
        } else if callable_names.contains(tool.name.as_str()) {
            descriptions.insert(
                tool.name.clone(),
                format!("{}{}", tool.description, declared_tool_note(tool)),
            );
        }
    }

    Some(ToolLoadoutChanges {
        descriptions,
        hidden_declarations: Vec::new(),
    })
}

fn global_worker_semaphore() -> &'static Arc<Semaphore> {
    static SEM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| {
        let max_workers = std::env::var("PI_CODEMODE_MAX_WORKERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n >= 1)
            .unwrap_or(DEFAULT_MAX_CODEMODE_WORKERS);
        Arc::new(Semaphore::new(max_workers))
    })
}

fn reconstruct_store_from_branch(branch: &[SessionEntry]) -> BTreeMap<String, JsonValue> {
    let mut store = BTreeMap::new();
    for entry in branch {
        if let SessionEntry::Custom(custom) = entry
            && custom.custom_type == CODEMODE_STORE_ENTRY_TYPE
            && let Some(data) = &custom.data
            && let Ok(writes) = serde_json::from_value::<CodemodeStoreWrites>(data.clone())
        {
            for key in writes.delete {
                store.remove(&key);
            }
            for (key, value) in writes.set {
                store.insert(key, value);
            }
        }
    }
    store
}

fn tool_result_to_text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn matches_namespace(ns: &ToolNamespace, wanted: &str) -> bool {
    let normalized = wanted.trim().to_lowercase();
    ns.name.to_lowercase() == normalized
        || ns.aliases.iter().any(|a| a.to_lowercase() == normalized)
        || ns
            .name
            .strip_prefix("mcp__")
            .is_some_and(|s| s.to_lowercase() == normalized)
}

fn build_discovery_globals(all_tools: Vec<ToolInfo>) -> Vec<CodemodeTool> {
    let tools_for_search = all_tools.clone();
    let search_tool = CodemodeTool {
        name: "searchTools".to_owned(),
        js_name: None,
        description: None,
        input_schema: None,
        output_schema: None,
        signature: Some(
            "(query: string, options?: { limit?: number; namespace?: string }): Promise<Array<{ name: string; description: string; declaration: string }>>"
                .to_owned(),
        ),
        spread: true,
        execute: Arc::new(move |args_opt, _cancel| {
            let tools = tools_for_search.clone();
            Box::pin(async move {
                let args_arr = args_opt
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                let query = args_arr
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                if query.trim().is_empty() {
                    return Err("searchTools(query) requires a non-empty query string".to_owned());
                }
                let opts_obj = args_arr.get(1).and_then(|v| v.as_object());
                let limit = opts_obj
                    .and_then(|o| o.get("limit"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(DEFAULT_TOOL_SEARCH_LIMIT);
                let ns_filter = opts_obj
                    .and_then(|o| o.get("namespace"))
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned);

                let candidates: Vec<&ToolInfo> = tools
                    .iter()
                    .filter(|t| {
                        t.name != CODEMODE_TOOL_NAME
                            && matches!(
                                t.exposure,
                                ToolExposure::Direct
                                    | ToolExposure::Codemode
                                    | ToolExposure::Deferred
                            )
                            && ns_filter.as_deref().is_none_or(|wanted| {
                                t.namespace
                                    .as_ref()
                                    .is_some_and(|ns| matches_namespace(ns, wanted))
                            })
                    })
                    .collect();

                let docs: Vec<_> = candidates
                    .iter()
                    .map(|t| {
                        create_tool_search_document(
                            &t.name,
                            &t.description,
                            &t.parameters,
                            t.namespace.as_ref(),
                        )
                    })
                    .collect();
                let matches = Bm25Ranker::default().rank(&query, &docs, limit);
                let out: Vec<JsonValue> = matches
                    .into_iter()
                    .filter_map(|m| {
                        let info = candidates.iter().find(|t| t.name == m.name)?;
                        let decl_item = CodemodeDeclarationItem {
                            name: info.name.clone(),
                            description: Some(info.description.clone()),
                            input_schema: Some(info.parameters.clone()),
                            output_schema: info.output_schema.clone(),
                            signature: None,
                        };
                        Some(json!({
                            "name": to_codemode_identifier(&info.name),
                            "description": info.description,
                            "declaration": format!("declare const tools: {{ {} }};", render_tool_signature(&decl_item, None)),
                        }))
                    })
                    .collect();
                Ok(Some(JsonValue::Array(out)))
            })
        }),
    };

    let tools_for_describe = all_tools.clone();
    let describe_tool = CodemodeTool {
        name: "describeTool".to_owned(),
        js_name: None,
        description: None,
        input_schema: None,
        output_schema: None,
        signature: Some(
            "(name: string): Promise<{ name: string; description: string; declaration: string } | null>"
                .to_owned(),
        ),
        spread: false,
        execute: Arc::new(move |args_opt, _cancel| {
            let tools = tools_for_describe.clone();
            Box::pin(async move {
                let wanted = args_opt
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if wanted.is_empty() {
                    return Err("describeTool(name) requires a tool name string".to_owned());
                }
                let found = tools.iter().find(|t| {
                    t.name != CODEMODE_TOOL_NAME
                        && matches!(
                            t.exposure,
                            ToolExposure::Direct | ToolExposure::Codemode | ToolExposure::Deferred
                        )
                        && (t.name == wanted || to_codemode_identifier(&t.name) == wanted)
                });
                let Some(info) = found else {
                    return Ok(Some(JsonValue::Null));
                };
                let decl_item = CodemodeDeclarationItem {
                    name: info.name.clone(),
                    description: Some(info.description.clone()),
                    input_schema: Some(info.parameters.clone()),
                    output_schema: info.output_schema.clone(),
                    signature: None,
                };
                Ok(Some(json!({
                    "name": to_codemode_identifier(&info.name),
                    "description": info.description,
                    "declaration": format!("declare const tools: {{ {} }};", render_tool_signature(&decl_item, None)),
                })))
            })
        }),
    };

    let tools_for_ns = all_tools;
    let describe_namespace = CodemodeTool {
        name: "describeNamespace".to_owned(),
        js_name: None,
        description: None,
        input_schema: None,
        output_schema: None,
        signature: Some(
            "(name: string): Promise<{ name: string; description?: string; instructions?: string; tools: Array<{ name: string; description: string }> } | null>"
                .to_owned(),
        ),
        spread: false,
        execute: Arc::new(move |args_opt, _cancel| {
            let tools = tools_for_ns.clone();
            Box::pin(async move {
                let wanted = args_opt
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if wanted.is_empty() {
                    return Err("describeNamespace(name) requires a namespace name string".to_owned());
                }
                let mut matched_ns: Option<ToolNamespace> = None;
                let mut ns_tools: Vec<JsonValue> = Vec::new();
                for tool in &tools {
                    if !matches!(tool.exposure, ToolExposure::Direct | ToolExposure::Codemode | ToolExposure::Deferred) { continue; }
                    if let Some(ns) = &tool.namespace
                        && matches_namespace(ns, wanted)
                    {
                        if matched_ns.is_none() {
                            matched_ns = Some(ns.clone());
                        }
                        ns_tools.push(json!({
                            "name": to_codemode_identifier(&tool.name),
                            "description": tool.description,
                        }));
                    }
                }
                let Some(ns) = matched_ns else {
                    return Ok(Some(JsonValue::Null));
                };
                let mut obj = Map::new();
                obj.insert("name".to_owned(), JsonValue::String(ns.name));
                if let Some(desc) = ns.description {
                    obj.insert("description".to_owned(), JsonValue::String(desc));
                }
                if let Some(inst) = ns.instructions {
                    obj.insert("instructions".to_owned(), JsonValue::String(inst));
                }
                obj.insert("tools".to_owned(), JsonValue::Array(ns_tools));
                Ok(Some(JsonValue::Object(obj)))
            })
        }),
    };

    vec![search_tool, describe_tool, describe_namespace]
}

fn save_codemode_image(data: &str, mime_type: &str) -> Option<PathBuf> {
    let ext = match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let path = std::env::temp_dir().join(format!("pi-codemode-{}.{ext}", Uuid::new_v4()));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

fn format_codemode_output(
    output: &[CodemodeOutputItem],
    return_value: Option<&JsonValue>,
    max_output_tokens: u64,
) -> Vec<UserContentBlock> {
    let mut text_items: Vec<String> = Vec::new();
    let mut console_lines: Vec<String> = Vec::new();
    let mut image_blocks: Vec<(Option<PathBuf>, ImageContent)> = Vec::new();

    for item in output {
        match item {
            CodemodeOutputItem::Text { text, console } => {
                if *console {
                    console_lines.push(text.clone());
                } else {
                    text_items.push(text.clone());
                }
            }
            CodemodeOutputItem::Image { data, mime_type } => {
                let saved = save_codemode_image(data, mime_type);
                image_blocks.push((
                    saved,
                    ImageContent {
                        data: data.clone(),
                        mime_type: mime_type.clone(),
                    },
                ));
            }
        }
    }

    if let Some(val) = return_value
        && !val.is_null()
    {
        let rendered = match val {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        if !rendered.is_empty() {
            text_items.push(rendered);
        }
    }

    let mut sections: Vec<String> = Vec::new();
    if text_items.len() == 1 {
        sections.push(text_items.remove(0));
    } else if text_items.len() > 1 {
        let total = text_items.len();
        let combined = text_items
            .into_iter()
            .enumerate()
            .map(|(i, t)| format!("==> text {}/{total} <==\n{t}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(combined);
    }

    if !console_lines.is_empty() {
        sections.push(format!(
            "<console_output>\n{}\n</console_output>",
            console_lines.join("\n")
        ));
    }

    for (saved_path, _) in &image_blocks {
        if let Some(p) = saved_path {
            sections.push(format!("[Saved image to {}]", p.display()));
        }
    }

    let mut combined_text = sections.join("\n\n");
    let max_chars = (max_output_tokens as usize).saturating_mul(CHARS_PER_TOKEN);
    if max_output_tokens == 0 {
        if !combined_text.is_empty() {
            combined_text = "[Output truncated to 0 tokens]".to_owned();
        }
    } else if combined_text.chars().count() > max_chars {
        let truncated: String = combined_text.chars().take(max_chars).collect();
        combined_text = format!("{truncated}\n[Output truncated to {max_output_tokens} tokens]");
    }

    let mut blocks: Vec<UserContentBlock> = Vec::new();
    if !combined_text.is_empty() || image_blocks.is_empty() {
        blocks.push(UserContentBlock::text(if combined_text.is_empty() {
            "(no output)".to_owned()
        } else {
            combined_text
        }));
    }
    for (_, img) in image_blocks {
        blocks.push(UserContentBlock::Image(img));
    }
    blocks
}

fn format_codemode_error(error: &CodemodeError, calls: &[CodemodeCall]) -> String {
    let base = match error.kind {
        CodemodeErrorKind::Script => error
            .stack
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&error.message)
            .to_owned(),
        CodemodeErrorKind::Timeout | CodemodeErrorKind::Aborted | CodemodeErrorKind::Sandbox => {
            error.message.clone()
        }
    };
    if calls.is_empty() {
        base
    } else {
        let summary = calls
            .iter()
            .map(|c| format!("{} ({:?}, {:.0}ms)", c.name, c.status, c.duration_ms))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{base}\nTool calls before failure: {summary}")
    }
}

#[derive(Clone, Default)]
pub struct CodemodeExtensionOptions {
    pub settings_manager: Option<SettingsManager>,
    pub mode: Option<CodemodeMode>,
    pub inline_budget: Option<Option<usize>>,
    pub interrupt_budget: Option<u64>,
}

pub fn create_codemode_tool_definition(options: CodemodeExtensionOptions) -> ToolDefinition {
    let parameters = json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "Raw JavaScript source."
            }
        },
        "required": ["code"],
        "additionalProperties": false
    });

    let opts_for_loadout = options.clone();
    let prepare_loadout = Arc::new(move |loadout: &ToolLoadout| -> Option<ToolLoadoutChanges> {
        let mode = opts_for_loadout.mode.unwrap_or_else(|| {
            opts_for_loadout
                .settings_manager
                .as_ref()
                .and_then(|sm| sm.get_codemode_mode())
                .map(|s| {
                    if s.eq_ignore_ascii_case("only") {
                        CodemodeMode::Only
                    } else {
                        CodemodeMode::On
                    }
                })
                .unwrap_or(CodemodeMode::On)
        });
        let inline_budget = opts_for_loadout
            .inline_budget
            .unwrap_or(Some(DEFAULT_INLINE_BUDGET));
        prepare_codemode_loadout(loadout, mode, inline_budget)
    });

    let interrupt_budget = options.interrupt_budget;
    ToolDefinition::new(
        CODEMODE_TOOL_NAME,
        CODEMODE_TOOL_NAME,
        CODEMODE_DESCRIPTION,
        parameters,
        Arc::new(move |_tool_call_id, args, cancel_token, on_update, ctx| {
            Box::pin(async move {
                let raw_code = args
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let parsed = parse_codemode_source(&raw_code).map_err(|e| e.to_string())?;
                let max_output_tokens = parsed
                    .options
                    .max_output_tokens
                    .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
                let timeout_ms = parsed.options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);

                let sem = global_worker_semaphore().clone();
                let cancel_token = cancel_token.unwrap_or_default();
                let permit = tokio::select! {
                    biased;
                    () = cancel_token.cancelled() => return Err("Codemode execution aborted".to_owned()),
                    permit = tokio::time::timeout(
                        Duration::from_millis(WORKER_SLOT_WAIT_MS), sem.acquire_owned(),
                    ) => permit
                        .map_err(|_| format!("Timed out waiting {WORKER_SLOT_WAIT_MS}ms for an available codemode worker slot"))?
                        .map_err(|e| format!("Codemode worker semaphore closed: {e}"))?,
                };

                let store_snapshot = reconstruct_store_from_branch(&ctx.session_branch());
                let callable_tools = ctx.tools();
                let all_tool_infos = ctx.runtime().get_all_tools().unwrap_or_default();

                let mut sandbox_tools = Vec::with_capacity(callable_tools.len());
                for agent_tool in callable_tools {
                    if agent_tool.name == CODEMODE_TOOL_NAME {
                        continue;
                    }
                    let tool_name = agent_tool.name.clone();
                    let tool_desc = agent_tool.description.clone();
                    let input_schema = agent_tool.parameters().clone();
                    let output_schema = agent_tool.output_schema.clone();
                    let has_output_schema = output_schema.is_some();
                    let ctx_clone = ctx.clone();
                    let on_update_clone = on_update.clone();

                    let mut cm_tool = CodemodeTool::new(
                        tool_name.clone(),
                        move |call_args, call_cancel| {
                            let ctx_inner = ctx_clone.clone();
                            let name_inner = tool_name.clone();
                            let update_cb = on_update_clone.clone();
                            async move {
                                let normalized_args = call_args.unwrap_or_else(|| json!({}));
                                let outcome = ctx_inner
                                    .execute_tool(
                                        &name_inner,
                                        normalized_args,
                                        NestedToolCallOptions {
                                            cancel_token: Some(call_cancel),
                                            on_update: update_cb,
                                        },
                                    )
                                    .await;
                                if outcome.is_error {
                                    if has_output_schema
                                        && let Some(sc) = outcome.result.structured_content
                                    {
                                        return Ok(Some(sc));
                                    }
                                    return Err(tool_result_to_text(&outcome.result));
                                }
                                if has_output_schema
                                    && let Some(sc) = outcome.result.structured_content
                                {
                                    return Ok(Some(sc));
                                }
                                Ok(Some(JsonValue::String(tool_result_to_text(
                                    &outcome.result,
                                ))))
                            }
                        },
                    );
                    cm_tool.description = Some(tool_desc);
                    cm_tool.input_schema = Some(input_schema);
                    cm_tool.output_schema = output_schema;
                    sandbox_tools.push(cm_tool);
                }

                let model_usage = Arc::new(std::sync::Mutex::new(Usage::default()));
                let mut discovery_globals = build_discovery_globals(all_tool_infos);
                discovery_globals.extend(super::codemode_models::model_globals(
                    ctx.model_catalog()?, model_usage.clone(),
                ));
                let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
                    tools: sandbox_tools,
                    globals: discovery_globals,
                    timeout_ms: Some(timeout_ms),
                    memory_limit_bytes: None,
                    max_stack_size: None,
                    interrupt_budget,
                })?;

                let result = sandbox
                    .execute_reserved(
                        &parsed.code,
                        CodemodeExecuteOptions {
                            timeout_ms: Some(timeout_ms),
                            cancel_token: Some(cancel_token),
                            store: store_snapshot,
                        },
                        Some(permit),
                    )
                    .await;

                if !result.ok {
                    let err_text = result
                        .error
                        .as_ref()
                        .map(|e| format_codemode_error(e, &result.calls))
                        .unwrap_or_else(|| "Codemode execution failed".to_owned());
                    let mut failure = AgentToolResult::error_text(err_text);
                    failure.usage = Some(model_usage.lock().expect("model usage lock").clone());
                    return Ok(failure);
                }

                if let Some(writes) = &result.store_writes
                    && (!writes.set.is_empty() || !writes.delete.is_empty())
                    && let Ok(val) = serde_json::to_value(writes)
                {
                    ctx.runtime().append_entry(CODEMODE_STORE_ENTRY_TYPE, Some(val))?;
                }

                let content = format_codemode_output(
                    &result.output,
                    result.value.as_ref(),
                    max_output_tokens,
                );
                Ok(AgentToolResult {
                    content,
                    details: json!({
                        "calls": result.calls,
                    }),
                    structured_content: None,
                    usage: Some(model_usage.lock().expect("model usage lock").clone()),
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    )
    .with_prompt_snippet(
        "Run JavaScript that calls other tools to batch, chain, or filter their results",
    )
    .with_prompt_guidelines(vec![
        "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls.".to_owned(),
    ])
    .with_exposure(ToolExposure::ModelOnly)
    .with_prepare_loadout(prepare_loadout)
}

pub fn create_codemode_extension(options: CodemodeExtensionOptions) -> ExtensionFactory {
    Arc::new(move || {
        let opts = options.clone();
        Ok(Box::new(
            InlineExtension::new("builtin:codemode", move |pi: &mut ExtensionRegistrar| {
                let def = create_codemode_tool_definition(opts.clone()).with_default_active(false);
                pi.register_tool(def);
                Ok(())
            })
            .with_source_info(SourceInfo::builtin("builtin:codemode"))
            .with_replaceable(true),
        ) as Box<dyn Extension>)
    })
}
