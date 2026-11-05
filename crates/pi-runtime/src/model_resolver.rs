use std::collections::{BTreeMap, BTreeSet};

use pi_ai::{Model, ModelThinkingLevel};
use pi_session::parse_model_thinking_level;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::model_runtime::ModelRuntime;

pub const DEFAULT_THINKING_LEVEL: ModelThinkingLevel = ModelThinkingLevel::Medium;

/// 各内置服务商的默认对话模型映射（对照 `defaultModelPerProvider`）。
pub fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "ant-ling" => Some("Ring-2.6-1T"),
        "anthropic" => Some("claude-opus-4-8"),
        "openai" => Some("gpt-6.1-sol"),
        "openai-codex" => Some("gpt-6.1-sol"),
        "deepseek" => Some("deepseek-v4-pro"),
        "google" => Some("gemini-3.1-pro-preview"),
        "github-copilot" => Some("gpt-6.1-sol"),
        "openrouter" => Some("moonshotai/kimi-k2.6"),
        "vercel-ai-gateway" => Some("zai/glm-5.1"),
        "xai" => Some("grok-4.7"),
        "zai" | "zai-coding-cn" => Some("glm-5.3"),
        "minimax" | "minimax-cn" => Some("MiniMax-M2.7"),
        "moonshotai" | "moonshotai-cn" => Some("kimi-k2.6"),
        "huggingface" => Some("moonshotai/Kimi-K2.6"),
        "opencode" => Some("kimi-k2.6"),
        "opencode-go" => Some("kimi-k3"),
        "kimi-coding" => Some("kimi-for-coding"),
        "qwen-token-plan" | "qwen-token-plan-cn" => Some("qwen3.7-max"),
        "qwen-token-plan-individual" => Some("qwen3.8-max"),
        "xiaomi" | "xiaomi-token-plan-cn" => Some("mimo-v2.6-pro"),
        _ => None,
    }
}

pub const DEFAULT_PROVIDER_PRIORITY: &[&str] = &[
    "ant-ling",
    "anthropic",
    "openai",
    "openai-codex",
    "deepseek",
    "google",
    "github-copilot",
    "openrouter",
    "vercel-ai-gateway",
    "xai",
    "zai",
    "zai-coding-cn",
    "minimax",
    "minimax-cn",
    "moonshotai",
    "moonshotai-cn",
    "huggingface",
    "opencode",
    "opencode-go",
    "kimi-coding",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "xiaomi",
    "xiaomi-token-plan-cn",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModel {
    pub model: Model,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingSuffixMode {
    #[default]
    None,
    Strict,
    Lenient,
}

#[derive(Debug, Clone)]
pub struct ModelReferenceRequest {
    pub text: String,
    pub provider: Option<String>,
    pub thinking_suffix: ThinkingSuffixMode,
    pub fuzzy: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelReferenceTarget {
    pub provider: String,
    pub provider_models: Vec<Model>,
    pub model_text: String,
    pub model_id: String,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelReferenceResult {
    Resolved {
        model: Box<Model>,
        thinking_level: Option<ModelThinkingLevel>,
        warning: Option<String>,
    },
    Ambiguous {
        candidates: Vec<Model>,
        usable_count: usize,
        thinking_level: Option<ModelThinkingLevel>,
        warning: Option<String>,
    },
    NotFound {
        target: Option<ModelReferenceTarget>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InitialModelPolicy {
    /// 通用 Pi SDK 默认选择策略（按 `scoped_models` → `settings` 默认模型 → 已配置认证的服务商默认模型 → 首个可用模型 → `None`）。
    #[default]
    GenericSdk,
    /// 业务固定模型身份策略（如 CST Pilot 固定 `deepseek/deepseek-flash`，即使缺少凭据也保留该模型身份，不自动切往其他服务商）。
    FixedIdentity {
        default_provider: String,
        default_model_id: String,
        preserve_unauthenticated: bool,
    },
}

fn split_thinking_suffix(text: &str) -> Option<(String, ModelThinkingLevel)> {
    let colon_idx = text.rfind(':')?;
    let suffix = &text[colon_idx + 1..];
    let level = parse_model_thinking_level(suffix)?;
    Some((text[..colon_idx].to_owned(), level))
}

fn split_provider_prefix(text: &str, models: &[Model]) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;

    for m in models {
        let prefix = m.provider.trim().to_lowercase();
        if prefix.is_empty() {
            continue;
        }
        if let Some((ref bp, _)) = best
            && bp.len() >= m.provider.len()
        {
            continue;
        }
        // Locate separators in the original text: Unicode case folding can change
        // byte lengths, so offsets from a lowercase copy cannot index the input.
        for (slash_idx, _) in text.match_indices('/') {
            if text[..slash_idx].trim().to_lowercase() == prefix {
                let model_text = text[slash_idx + 1..].trim();
                if !model_text.is_empty() {
                    best = Some((m.provider.clone(), model_text.to_owned()));
                }
                break;
            }
        }
    }
    best
}

fn is_alias_id(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    let bytes = id.as_bytes();
    if bytes.len() > 9 && bytes[bytes.len() - 9] == b'-' {
        let suffix = &bytes[bytes.len() - 8..];
        if suffix.iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    true
}

fn best_fuzzy_match(matches: &[Model]) -> Option<Model> {
    if matches.is_empty() {
        return None;
    }
    let aliases: Vec<Model> = matches
        .iter()
        .filter(|m| is_alias_id(&m.id))
        .cloned()
        .collect();
    let mut pool = if aliases.is_empty() {
        matches.to_vec()
    } else {
        aliases
    };
    pool.sort_by(|a, b| b.id.cmp(&a.id));
    pool.into_iter().next()
}

fn fuzzy_matches(text: &str, models: &[Model]) -> Vec<Model> {
    let lower = text.to_lowercase();
    models
        .iter()
        .filter(|m| m.id.to_lowercase().contains(&lower) || m.name.to_lowercase().contains(&lower))
        .cloned()
        .collect()
}

fn select_reference<F>(
    text: &str,
    pool: &[Model],
    fuzzy: bool,
    usable: &F,
) -> Option<ModelReferenceResult>
where
    F: Fn(&str) -> bool,
{
    let lower_text = text.to_lowercase();
    let raw: Vec<Model> = pool
        .iter()
        .filter(|m| m.id.to_lowercase() == lower_text)
        .cloned()
        .collect();
    let split = split_provider_prefix(text, pool);

    let mut named: Option<Model> = None;
    if let Some((ref prov, ref model_text)) = split {
        let prov_models: Vec<Model> = pool
            .iter()
            .filter(|m| &m.provider == prov)
            .cloned()
            .collect();
        let lower_model_text = model_text.to_lowercase();
        named = prov_models
            .iter()
            .find(|m| m.id.to_lowercase() == lower_model_text)
            .cloned()
            .or_else(|| raw.iter().find(|m| &m.provider == prov).cloned())
            .or_else(|| {
                if fuzzy {
                    best_fuzzy_match(&fuzzy_matches(model_text, &prov_models))
                } else {
                    None
                }
            });
    }

    let others: Vec<Model> = raw
        .into_iter()
        .filter(|m| {
            named
                .as_ref()
                .is_none_or(|n| !(n.provider == m.provider && n.id == m.id))
        })
        .collect();
    let mut candidates = Vec::new();
    if let Some(ref n) = named {
        candidates.push(n.clone());
    }
    candidates.extend(others.iter().cloned());

    if candidates.is_empty() {
        return None;
    }

    let unique_providers: BTreeSet<&str> = candidates.iter().map(|m| m.provider.as_str()).collect();
    if unique_providers.len() == 1 {
        return Some(ModelReferenceResult::Resolved {
            model: Box::new(candidates.remove(0)),
            thinking_level: None,
            warning: None,
        });
    }

    if let Some(ref n) = named
        && usable(&n.provider)
    {
        return Some(ModelReferenceResult::Resolved {
            model: Box::new(n.clone()),
            thinking_level: None,
            warning: None,
        });
    }

    let usable_others: Vec<Model> = others.into_iter().filter(|m| usable(&m.provider)).collect();
    if usable_others.len() == 1 {
        return Some(ModelReferenceResult::Resolved {
            model: Box::new(usable_others.into_iter().next().expect("len 1")),
            thinking_level: None,
            warning: None,
        });
    }
    if usable_others.is_empty()
        && let Some(n) = named
    {
        return Some(ModelReferenceResult::Resolved {
            model: Box::new(n),
            thinking_level: None,
            warning: None,
        });
    }

    Some(ModelReferenceResult::Ambiguous {
        candidates,
        usable_count: usable_others.len(),
        thinking_level: None,
        warning: None,
    })
}

fn select_fuzzy<F>(matches: Vec<Model>, usable: &F) -> Option<ModelReferenceResult>
where
    F: Fn(&str) -> bool,
{
    if matches.is_empty() {
        return None;
    }
    let unique_providers: BTreeSet<&str> = matches.iter().map(|m| m.provider.as_str()).collect();
    let multiple_providers = unique_providers.len() > 1;
    let usable_matches: Vec<Model> = if multiple_providers {
        matches
            .iter()
            .filter(|m| usable(&m.provider))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let pool = if !usable_matches.is_empty() {
        &usable_matches
    } else {
        &matches
    };
    let best = best_fuzzy_match(pool)?;
    let tied: Vec<Model> = pool.iter().filter(|m| m.id == best.id).cloned().collect();
    let tied_providers: BTreeSet<&str> = tied.iter().map(|m| m.provider.as_str()).collect();
    if tied_providers.len() == 1 {
        return Some(ModelReferenceResult::Resolved {
            model: Box::new(best),
            thinking_level: None,
            warning: None,
        });
    }
    let usable_count = if !usable_matches.is_empty() {
        tied.len()
    } else {
        0
    };
    Some(ModelReferenceResult::Ambiguous {
        candidates: tied,
        usable_count,
        thinking_level: None,
        warning: None,
    })
}

fn resolve_text<F>(
    text: &str,
    request: &ModelReferenceRequest,
    pool: &[Model],
    usable: &F,
) -> ModelReferenceResult
where
    F: Fn(&str) -> bool,
{
    if let Some(res) = select_reference(text, pool, request.fuzzy, usable) {
        return res;
    }
    if request.fuzzy
        && let Some(fuzzy_res) = select_fuzzy(fuzzy_matches(text, pool), usable)
    {
        return fuzzy_res;
    }
    if request.thinking_suffix == ThinkingSuffixMode::None {
        return ModelReferenceResult::NotFound { target: None };
    }

    if let Some((base, level)) = split_thinking_suffix(text) {
        let inner = resolve_text(&base, request, pool, usable);
        return match inner {
            ModelReferenceResult::NotFound { .. } => inner,
            ModelReferenceResult::Resolved { model, warning, .. } => {
                ModelReferenceResult::Resolved {
                    model,
                    thinking_level: if warning.is_some() { None } else { Some(level) },
                    warning,
                }
            }
            ModelReferenceResult::Ambiguous {
                candidates,
                usable_count,
                warning,
                ..
            } => ModelReferenceResult::Ambiguous {
                candidates,
                usable_count,
                thinking_level: if warning.is_some() { None } else { Some(level) },
                warning,
            },
        };
    }

    if request.thinking_suffix == ThinkingSuffixMode::Strict {
        return ModelReferenceResult::NotFound { target: None };
    }

    let Some(colon_idx) = text.rfind(':') else {
        return ModelReferenceResult::NotFound { target: None };
    };
    let suffix = &text[colon_idx + 1..];
    let inner = resolve_text(&text[..colon_idx], request, pool, usable);
    let warn_msg = format!(
        "Invalid thinking level \"{suffix}\" in pattern \"{text}\". Using default instead."
    );
    match inner {
        ModelReferenceResult::NotFound { .. } => inner,
        ModelReferenceResult::Resolved { model, .. } => ModelReferenceResult::Resolved {
            model,
            thinking_level: None,
            warning: Some(warn_msg),
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            ..
        } => ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            thinking_level: None,
            warning: Some(warn_msg),
        },
    }
}

/// 解析模型引用字符串（支持 `provider/id`、裸 `id`、含 `:` 或 `/` 的模型 ID、模糊匹配及 `:<thinking>` 后缀）。
pub fn resolve_model_reference<F>(
    request: &ModelReferenceRequest,
    models: &[Model],
    usable: F,
) -> ModelReferenceResult
where
    F: Fn(&str) -> bool,
{
    let text = request.text.trim();
    if text.is_empty() {
        return ModelReferenceResult::NotFound { target: None };
    }
    let pool: Vec<Model> = if let Some(ref prov) = request.provider {
        let lower_prov = prov.to_lowercase();
        models
            .iter()
            .filter(|m| m.provider.to_lowercase() == lower_prov)
            .cloned()
            .collect()
    } else {
        models.to_vec()
    };

    let result = resolve_text(text, request, &pool, &usable);
    if !matches!(result, ModelReferenceResult::NotFound { .. }) {
        return result;
    }

    let split = split_provider_prefix(text, &pool);
    let anchor = if request.provider.is_some() {
        pool.first().cloned()
    } else if let Some((ref p, _)) = split {
        pool.iter().find(|m| &m.provider == p).cloned()
    } else {
        None
    };

    let Some(anchor) = anchor else {
        return result;
    };

    let model_text = split.map(|(_, mt)| mt).unwrap_or_else(|| text.to_owned());
    let suffix = if request.thinking_suffix == ThinkingSuffixMode::None {
        None
    } else {
        split_thinking_suffix(&model_text)
    };
    let mut provider_models = vec![anchor.clone()];
    for m in &pool {
        if m.provider == anchor.provider && m.id != anchor.id {
            provider_models.push(m.clone());
        }
    }

    ModelReferenceResult::NotFound {
        target: Some(ModelReferenceTarget {
            provider: anchor.provider,
            provider_models,
            model_id: suffix
                .as_ref()
                .map_or_else(|| model_text.clone(), |(b, _)| b.clone()),
            thinking_level: suffix.map(|(_, l)| l),
            model_text,
        }),
    }
}

pub fn find_exact_model_reference_match(model_reference: &str, models: &[Model]) -> Option<Model> {
    let res = resolve_model_reference(
        &ModelReferenceRequest {
            text: model_reference.to_owned(),
            provider: None,
            thinking_suffix: ThinkingSuffixMode::None,
            fuzzy: false,
        },
        models,
        |_| true,
    );
    match res {
        ModelReferenceResult::Resolved { model, .. } => Some(*model),
        _ => None,
    }
}

fn glob_to_regex_case_insensitive(pattern: &str) -> Option<Regex> {
    let mut rx = String::from("(?i)^");
    for ch in pattern.chars() {
        match ch {
            '*' => rx.push_str(".*"),
            '?' => rx.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '\\' => {
                rx.push('\\');
                rx.push(ch);
            }
            other => rx.push(other),
        }
    }
    rx.push('$');
    Regex::new(&rx).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScopeDiagnostic {
    pub code: String,
    pub message: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveModelScopeResult {
    pub scoped_models: Vec<ScopedModel>,
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

pub fn resolve_model_scope_from_models(
    patterns: &[String],
    models: &[Model],
) -> ResolveModelScopeResult {
    let mut scoped_models: Vec<ScopedModel> = Vec::new();
    let mut diagnostics: Vec<ModelScopeDiagnostic> = Vec::new();

    let mut add = |model: Model, thinking_level: Option<ModelThinkingLevel>| {
        if !scoped_models
            .iter()
            .any(|s| s.model.provider == model.provider && s.model.id == model.id)
        {
            scoped_models.push(ScopedModel {
                model,
                thinking_level,
            });
        }
    };

    for pattern in patterns {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let thinking = split_thinking_suffix(pattern);
            let glob_pat = thinking
                .as_ref()
                .map_or_else(|| pattern.clone(), |(b, _)| b.clone());
            let thinking_level = thinking.map(|(_, l)| l);

            let exact = resolve_model_reference(
                &ModelReferenceRequest {
                    text: glob_pat.clone(),
                    provider: None,
                    thinking_suffix: ThinkingSuffixMode::None,
                    fuzzy: false,
                },
                models,
                |_| true,
            );
            let matches: Vec<Model> = match exact {
                ModelReferenceResult::Resolved { model, .. } => vec![*model],
                ModelReferenceResult::Ambiguous { candidates, .. } => {
                    let provs: BTreeSet<&str> =
                        candidates.iter().map(|m| m.provider.as_str()).collect();
                    let list = provs.into_iter().collect::<Vec<_>>().join(", ");
                    diagnostics.push(ModelScopeDiagnostic {
                        code: "ambiguous".to_owned(),
                        message: format!(
                            "Pattern \"{pattern}\" matches several providers: {list}. Use provider/id to choose one."
                        ),
                        pattern: pattern.clone(),
                    });
                    candidates
                }
                ModelReferenceResult::NotFound { .. } => {
                    if let Some(re) = glob_to_regex_case_insensitive(&glob_pat) {
                        models
                            .iter()
                            .filter(|m| {
                                let full = format!("{}/{}", m.provider, m.id);
                                re.is_match(&full) || re.is_match(&m.id)
                            })
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    }
                }
            };

            if matches.is_empty() {
                diagnostics.push(ModelScopeDiagnostic {
                    code: "no-match".to_owned(),
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
            }
            for m in matches {
                add(m, thinking_level);
            }
            continue;
        }

        let res = resolve_model_reference(
            &ModelReferenceRequest {
                text: pattern.clone(),
                provider: None,
                thinking_suffix: ThinkingSuffixMode::Lenient,
                fuzzy: true,
            },
            models,
            |_| true,
        );
        match res {
            ModelReferenceResult::NotFound { .. } => {
                diagnostics.push(ModelScopeDiagnostic {
                    code: "no-match".to_owned(),
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
            }
            ModelReferenceResult::Resolved {
                model,
                thinking_level,
                warning,
            } => {
                if let Some(w) = warning {
                    diagnostics.push(ModelScopeDiagnostic {
                        code: "invalid-thinking-level".to_owned(),
                        message: w,
                        pattern: pattern.clone(),
                    });
                }
                add(*model, thinking_level);
            }
            ModelReferenceResult::Ambiguous {
                candidates,
                thinking_level,
                warning,
                ..
            } => {
                if let Some(w) = warning {
                    diagnostics.push(ModelScopeDiagnostic {
                        code: "invalid-thinking-level".to_owned(),
                        message: w,
                        pattern: pattern.clone(),
                    });
                }
                let provs: BTreeSet<&str> =
                    candidates.iter().map(|m| m.provider.as_str()).collect();
                let list = provs.into_iter().collect::<Vec<_>>().join(", ");
                diagnostics.push(ModelScopeDiagnostic {
                    code: "ambiguous".to_owned(),
                    message: format!(
                        "Pattern \"{pattern}\" matches several providers: {list}. Use provider/id to choose one."
                    ),
                    pattern: pattern.clone(),
                });
                for m in candidates {
                    add(m, thinking_level);
                }
            }
        }
    }

    ResolveModelScopeResult {
        scoped_models,
        diagnostics,
    }
}

#[derive(Debug, Clone)]
pub struct FindInitialModelOptions<'a> {
    pub scoped_models: &'a [ScopedModel],
    pub is_continuing: bool,
    pub default_provider: Option<&'a str>,
    pub default_model_id: Option<&'a str>,
    pub default_thinking_level: Option<ModelThinkingLevel>,
    pub model_thinking_levels: &'a BTreeMap<String, ModelThinkingLevel>,
    pub model_runtime: &'a ModelRuntime,
    pub policy: &'a InitialModelPolicy,
}

#[derive(Debug, Clone)]
pub struct InitialModelResult {
    pub model: Option<Model>,
    pub thinking_level: ModelThinkingLevel,
    pub fallback_message: Option<String>,
}

pub fn find_initial_model(options: FindInitialModelOptions<'_>) -> InitialModelResult {
    // 1. Scoped models first (if not continuing)
    if !options.scoped_models.is_empty() && !options.is_continuing {
        let first = &options.scoped_models[0];
        let key = format!("{}/{}", first.model.provider, first.model.id);
        let per_model = options.model_thinking_levels.get(&key).copied();
        return InitialModelResult {
            model: Some(first.model.clone()),
            thinking_level: first
                .thinking_level
                .or(per_model)
                .or(options.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        };
    }

    // 2. FixedIdentity policy (e.g. CST Pilot fixed default model)
    if let InitialModelPolicy::FixedIdentity {
        default_provider,
        default_model_id,
        preserve_unauthenticated,
    } = options.policy
    {
        let prov = options.default_provider.unwrap_or(default_provider);
        let mid = options.default_model_id.unwrap_or(default_model_id);
        if let Some(found) = options.model_runtime.get_model(prov, mid)
            && (*preserve_unauthenticated || options.model_runtime.has_configured_auth(prov))
        {
            let key = format!("{prov}/{mid}");
            let per_model = options.model_thinking_levels.get(&key).copied();
            return InitialModelResult {
                model: Some(found),
                thinking_level: per_model
                    .or(options.default_thinking_level)
                    .unwrap_or(DEFAULT_THINKING_LEVEL),
                fallback_message: None,
            };
        }
    }

    // 3. Saved default from settings if auth is configured
    if let (Some(prov), Some(mid)) = (options.default_provider, options.default_model_id)
        && let Some(found) = options.model_runtime.get_model(prov, mid)
        && options.model_runtime.has_configured_auth(&found.provider)
    {
        let key = format!("{prov}/{mid}");
        let per_model = options.model_thinking_levels.get(&key).copied();
        return InitialModelResult {
            model: Some(found),
            thinking_level: per_model
                .or(options.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        };
    }

    // 4. First available model with configured auth (preferring default_model_for_provider)
    let available = options.model_runtime.get_available_models();
    if !available.is_empty() {
        for &prov in DEFAULT_PROVIDER_PRIORITY {
            if let Some(def_id) = default_model_for_provider(prov)
                && let Some(m) = available
                    .iter()
                    .find(|m| m.provider == prov && m.id == def_id)
            {
                return InitialModelResult {
                    model: Some(m.clone()),
                    thinking_level: options
                        .default_thinking_level
                        .unwrap_or(DEFAULT_THINKING_LEVEL),
                    fallback_message: None,
                };
            }
        }
        return InitialModelResult {
            model: Some(available[0].clone()),
            thinking_level: options
                .default_thinking_level
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        };
    }

    // 5. No model available
    InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL,
        fallback_message: None,
    }
}
