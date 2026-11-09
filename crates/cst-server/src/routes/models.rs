use std::collections::HashSet;

use http::{HeaderMap, Method, Response, StatusCode};
use pi_ai::{Model, ModelThinkingLevel, ModelsRefreshOptions, is_thinking_level_supported};
use pi_runtime::{
    ModelRuntime, SettingsManager, SettingsScope, parse_model_thinking_level_cli,
    resolve_model_scope_from_models,
};
use serde_json::{Value, json};

use crate::mode::CallerIdentity;
use crate::routes::{ApiError, SseOrBytesBody, json_response, parse_query_params, read_json_body};
use crate::session::{default_model_reference, explicit_model, require_model_credentials};
use crate::state::ServerState;

const THINKING_ORDER: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
    ModelThinkingLevel::Max,
];

pub fn get_supported_thinking_levels(model: Option<&Model>) -> Vec<ModelThinkingLevel> {
    let Some(m) = model else {
        return vec![ModelThinkingLevel::Off];
    };
    if !m.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    let levels: Vec<ModelThinkingLevel> = THINKING_ORDER
        .into_iter()
        .filter(|&lvl| is_thinking_level_supported(m, lvl))
        .collect();
    if levels.is_empty() {
        vec![ModelThinkingLevel::Off]
    } else {
        levels
    }
}

pub fn home_thinking_level(model: &Model, settings: &SettingsManager) -> ModelThinkingLevel {
    let requested = settings
        .get_model_thinking_level(&model.provider, &model.id)
        .or_else(|| settings.get_default_thinking_level())
        .unwrap_or(ModelThinkingLevel::Medium);
    let levels = get_supported_thinking_levels(Some(model));
    let req_idx = THINKING_ORDER
        .iter()
        .position(|&l| l == requested)
        .unwrap_or(3);
    levels
        .iter()
        .copied()
        .find(|&lvl| THINKING_ORDER.iter().position(|&l| l == lvl).unwrap_or(0) >= req_idx)
        .or_else(|| levels.last().copied())
        .unwrap_or(ModelThinkingLevel::Off)
}

fn describe_model(
    model: &Model,
    runtime: &ModelRuntime,
    ready_providers: &HashSet<String>,
) -> Value {
    let provider_name = runtime
        .models()
        .get_provider(&model.provider)
        .map(|p| p.name().to_owned())
        .unwrap_or_else(|| model.provider.clone());
    let levels: Vec<&'static str> = get_supported_thinking_levels(Some(model))
        .into_iter()
        .map(|lvl| lvl.as_str())
        .collect();
    json!({
        "id": model.id,
        "provider": model.provider,
        "name": model.name,
        "providerName": provider_name,
        "credentialReady": ready_providers.contains(&model.provider),
        "catalogPresent": runtime.get_model(&model.provider, &model.id).is_some(),
        "input": model.input,
        "reasoning": model.reasoning,
        "thinkingLevels": levels,
        "contextWindow": model.context_window,
        "maxTokens": model.max_tokens,
    })
}

pub async fn handle_models_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if pathname == "/api/models/refresh" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        if !body.as_object().is_some_and(|o| o.is_empty()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型刷新参数无效",
            ));
        }
        let result = state
            .write_once(
                caller,
                method.as_str(),
                pathname,
                &body,
                Some(headers),
                || async move {
                    let cstoa_refresh = state.host.auth().refresh(true).await;
                    let all_refresh = state
                        .host
                        .auth()
                        .runtime()
                        .refresh(Some(ModelsRefreshOptions {
                            allow_network: Some(true),
                            providers: None,
                            force: Some(true),
                        }))
                        .await;
                    let aborted = cstoa_refresh.aborted || all_refresh.aborted;
                    let mut failed: Vec<String> = cstoa_refresh
                        .errors
                        .keys()
                        .chain(all_refresh.errors.keys())
                        .cloned()
                        .collect();
                    failed.sort();
                    failed.dedup();

                    let mut event = json!({ "type": "models_changed" });
                    if aborted || !failed.is_empty() {
                        event["warning"] = json!("部分模型目录未能更新，正在使用已有目录。");
                    }
                    state.pool.global_events.publish("state", &event);
                    Ok(json!({
                        "aborted": aborted,
                        "failedProviders": failed,
                    }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if pathname == "/api/models" && (*method == Method::GET || *method == Method::HEAD) {
        let settings = state
            .pool
            .refresh_services()
            .await
            .map_err(ApiError::from)?;
        let runtime = state.host.auth().runtime();
        let params = parse_query_params(query);
        let slot = if let Some(sid) = params.get("sessionId") {
            Some(state.pool.open_saved(sid).await.map_err(ApiError::from)?)
        } else {
            None
        };

        let stats = slot.as_ref().map(|s| s.session().get_session_stats());
        let (def_prov, def_mid) = default_model_reference(&settings);
        let default_model = explicit_model(runtime, &def_prov, &def_mid);
        let selected_model = slot
            .as_ref()
            .map(|s| s.session().model())
            .unwrap_or_else(|| default_model.clone());

        let available = runtime.get_available_models();
        let catalog = runtime.get_all_models();
        let marked_invalid = state
            .pool
            .invalid_auth
            .lock()
            .expect("invalid_auth lock")
            .clone();
        let ready_providers: HashSet<String> = available
            .iter()
            .filter(|m| !marked_invalid.contains(&m.provider))
            .map(|m| m.provider.clone())
            .collect();

        let web_settings = state.web_settings.read().await.map_err(ApiError::from)?;
        let patterns = settings.get_enabled_models().unwrap_or_default();
        let mut enabled: Option<Vec<String>> = None;
        let mut unavailable_enabled: Vec<String> = Vec::new();
        if patterns.is_empty() && web_settings.empty_model_scope {
            enabled = Some(Vec::new());
        } else if !patterns.is_empty() {
            let scope = resolve_model_scope_from_models(&patterns, &catalog);
            let available_ids: HashSet<String> = available
                .iter()
                .map(|m| format!("{}/{}", m.provider, m.id))
                .collect();
            let mut en = Vec::new();
            for scoped in scope.scoped_models {
                let id = format!("{}/{}", scoped.model.provider, scoped.model.id);
                if available_ids.contains(&id) {
                    en.push(id);
                } else {
                    unavailable_enabled.push(id);
                }
            }
            for diag in scope.diagnostics {
                if diag.code == "no-match" {
                    unavailable_enabled.push(diag.pattern);
                }
            }
            enabled = Some(en);
        }

        let selected_levels: Vec<&'static str> =
            get_supported_thinking_levels(Some(&selected_model))
                .into_iter()
                .map(|lvl| lvl.as_str())
                .collect();
        let effective_thinking = slot
            .as_ref()
            .map(|s| s.session().thinking_level())
            .unwrap_or_else(|| home_thinking_level(&selected_model, &settings));

        let mut selected_desc = describe_model(&selected_model, runtime, &ready_providers);
        if let Some(obj) = selected_desc.as_object_mut() {
            obj.insert(
                "thinkingLevel".to_owned(),
                json!(effective_thinking.as_str()),
            );
        }

        let usage_json = stats.as_ref().map(|st| {
            let denom = st.input_tokens + st.cache_read_tokens;
            let hit_rate = if denom > 0 {
                Some((st.cache_read_tokens as f64) / (denom as f64))
            } else {
                None
            };
            json!({
                "tokens": {
                    "input": st.input_tokens,
                    "output": st.output_tokens,
                    "cacheRead": st.cache_read_tokens,
                    "cacheWrite": st.cache_write_tokens,
                    "total": st.total_tokens,
                },
                "cacheHitRate": hit_rate,
            })
        });

        let models_json: Vec<Value> = available
            .iter()
            .map(|m| describe_model(m, runtime, &ready_providers))
            .collect();
        let catalog_json: Vec<Value> = catalog
            .iter()
            .map(|m| describe_model(m, runtime, &ready_providers))
            .collect();

        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({
                "models": models_json,
                "catalog": catalog_json,
                "defaultModel": describe_model(&default_model, runtime, &ready_providers),
                "enabled": enabled,
                "unavailableEnabled": unavailable_enabled,
                "thinkingLevels": selected_levels,
                "contextUsage": stats.and_then(|s| s.context_usage),
                "usage": usage_json,
                "selected": selected_desc,
            }),
            method,
        )));
    }

    if (pathname == "/api/models/select" || pathname == "/api/models/default")
        && *method == Method::POST
    {
        let save_default = pathname == "/api/models/default";
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型选择无效",
            ));
        };
        let allowed_keys: &[&str] = if save_default {
            &["provider", "modelId"]
        } else {
            &["provider", "modelId", "sessionId"]
        };
        if obj.keys().any(|k| !allowed_keys.contains(&k.as_str())) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型选择无效",
            ));
        }
        let Some(provider) = obj
            .get("provider")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型选择无效",
            ));
        };
        let Some(model_id) = obj
            .get("modelId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型选择无效",
            ));
        };
        let session_id = match obj.get("sessionId") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的模型选择无效",
                ));
            }
        };

        let settings = state
            .pool
            .refresh_services()
            .await
            .map_err(ApiError::from)?;
        let runtime = state.host.auth().runtime().clone();
        let Some(model) = runtime.get_model(&provider, &model_id) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型不存在",
            ));
        };

        let lock_key = if save_default {
            "model-default".to_owned()
        } else {
            format!(
                "model-selection:{}",
                session_id.as_deref().unwrap_or("home")
            )
        };

        let result = state
            .write_once(
                caller,
                method.as_str(),
                pathname,
                &body,
                Some(headers),
                || async move {
                    state
                        .locks
                        .serialize(lock_key, || async move {
                            let slot = if let Some(ref sid) = session_id {
                                Some(state.pool.open_saved(sid).await.map_err(ApiError::from)?)
                            } else {
                                None
                            };
                            if save_default {
                                settings
                                    .set_default_model_and_provider(
                                        &model.provider,
                                        &model.id,
                                        SettingsScope::Global,
                                    )
                                    .map_err(|msg| {
                                        ApiError::new(
                                            StatusCode::SERVICE_UNAVAILABLE,
                                            "configuration_unavailable",
                                            msg,
                                        )
                                    })?;
                                state
                                    .pool
                                    .global_events
                                    .publish("state", &json!({ "type": "settings_changed" }));
                            } else if let Some(ref s) = slot {
                                require_model_credentials(
                                    &runtime,
                                    &state.pool.invalid_auth,
                                    &model.provider,
                                    &model.id,
                                )
                                .await
                                .map_err(ApiError::from)?;
                                state
                                    .pool
                                    .set_session_model(&s.id, model.clone())
                                    .await
                                    .map_err(ApiError::from)?;
                            }

                            let provider_name = runtime
                                .models()
                                .get_provider(&model.provider)
                                .map(|p| p.name().to_owned())
                                .unwrap_or_else(|| model.provider.clone());
                            let marked_invalid = state
                                .pool
                                .invalid_auth
                                .lock()
                                .expect("invalid_auth lock")
                                .contains(&model.provider);
                            let cred_ready =
                                !marked_invalid && runtime.has_configured_auth(&model.provider);
                            let levels: Vec<&'static str> =
                                get_supported_thinking_levels(Some(&model))
                                    .into_iter()
                                    .map(|lvl| lvl.as_str())
                                    .collect();
                            let thinking_level = slot
                                .as_ref()
                                .map(|s| s.session().thinking_level())
                                .unwrap_or_else(|| home_thinking_level(&model, &settings));

                            Ok(json!({
                                "provider": model.provider,
                                "id": model.id,
                                "name": model.name,
                                "providerName": provider_name,
                                "credentialReady": cred_ready,
                                "catalogPresent": true,
                                "thinkingLevel": thinking_level.as_str(),
                                "reasoning": model.reasoning,
                                "thinkingLevels": levels,
                            }))
                        })
                        .await
                },
            )
            .await?;

        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if pathname == "/api/models/thinking" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的思考强度无效",
            ));
        };
        let Some(session_id) = obj
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的思考强度无效",
            ));
        };
        let Some(level_str) = obj.get("level").and_then(Value::as_str) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的思考强度无效",
            ));
        };
        let Some(parsed_level) = parse_model_thinking_level_cli(level_str) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的思考强度无效",
            ));
        };

        let slot = state
            .pool
            .open_saved(&session_id)
            .await
            .map_err(ApiError::from)?;
        let supported = get_supported_thinking_levels(Some(&slot.session().model()));
        if !supported.contains(&parsed_level) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的思考强度无效",
            ));
        }

        let result = state
            .write_once(
                caller,
                method.as_str(),
                pathname,
                &body,
                Some(headers),
                || async move {
                    let level = state
                        .pool
                        .set_session_thinking_level(&session_id, parsed_level)
                        .await
                        .map_err(ApiError::from)?;
                    Ok(json!({ "level": level.as_str() }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if pathname == "/api/models/scoped" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型范围无效",
            ));
        };
        let Some(patterns_arr) = obj.get("patterns").and_then(Value::as_array) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型范围无效",
            ));
        };
        if patterns_arr.len() > 100 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型范围无效",
            ));
        }
        let mut patterns = Vec::with_capacity(patterns_arr.len());
        for p in patterns_arr {
            let Some(s) = p.as_str().filter(|s| s.len() <= 200) else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的模型范围无效",
                ));
            };
            patterns.push(s.to_owned());
        }
        let empty_flag = match obj.get("empty") {
            None => false,
            Some(Value::Bool(b)) => *b,
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的模型范围无效",
                ));
            }
        };
        if empty_flag && !patterns.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型范围无效",
            ));
        }

        let result = state
            .write_once(
                caller,
                method.as_str(),
                pathname,
                &body,
                Some(headers),
                || async move {
                    let settings = state.pool.settings_manager();
                    let runtime = state.host.auth().runtime();
                    let all_models = runtime.get_all_models();
                    let scope = resolve_model_scope_from_models(&patterns, &all_models);
                    if !scope.diagnostics.is_empty() {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的模型范围没有匹配项",
                        ));
                    }

                    let available_ids: HashSet<String> = runtime
                        .get_available_models()
                        .into_iter()
                        .map(|m| format!("{}/{}", m.provider, m.id))
                        .collect();
                    let covers_all = !patterns.is_empty()
                        && patterns.len() == available_ids.len()
                        && patterns.iter().all(|id| available_ids.contains(id));

                    let save_res = if covers_all || patterns.is_empty() {
                        settings.set_enabled_models(None, SettingsScope::Global)
                    } else {
                        settings.set_enabled_models(Some(patterns.clone()), SettingsScope::Global)
                    };
                    save_res.map_err(|msg| {
                        ApiError::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "configuration_unavailable",
                            msg,
                        )
                    })?;
                    state
                        .web_settings
                        .save_empty_model_scope(empty_flag)
                        .await
                        .map_err(ApiError::from)?;
                    state
                        .pool
                        .global_events
                        .publish("state", &json!({ "type": "settings_changed" }));

                    let enabled_out: Value = if empty_flag {
                        json!([])
                    } else {
                        match settings.get_enabled_models() {
                            Some(list) if !list.is_empty() => json!(list),
                            _ => Value::Null,
                        }
                    };
                    Ok(json!({ "enabled": enabled_out }))
                },
            )
            .await?;

        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    Ok(None)
}
