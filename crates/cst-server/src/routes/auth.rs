use std::sync::{Arc, Mutex};

use cst_kit::auth::custom_provider::CustomProviderInput;
use http::{HeaderMap, Method, Response, StatusCode};
use pi_ai::{AuthInteraction, AuthPrompt, AuthType, CredentialStore};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::mode::CallerIdentity;
use crate::oauth::auth_type_str;
use crate::routes::{ApiError, SseOrBytesBody, json_response, parse_query_params, read_json_body};
use crate::session::attachments::is_valid_provider_id_key;
use crate::state::ServerState;

#[allow(clippy::too_many_arguments)]
pub async fn handle_auth_route(
    state: &Arc<ServerState>,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
    conn_cancel: CancellationToken,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if pathname == "/api/auth" && (*method == Method::GET || *method == Method::HEAD) {
        state
            .pool
            .refresh_services()
            .await
            .map_err(ApiError::from)?;
        let auth = state.host.auth();
        let runtime = auth.runtime();
        let credentials = runtime
            .credentials()
            .list()
            .await
            .map_err(ApiError::from_config_err)?;
        let custom_ids: std::collections::HashSet<String> = auth
            .custom_providers()
            .map_err(ApiError::from_config_err)?
            .into_iter()
            .collect();
        let invalid_auth = state
            .pool
            .invalid_auth
            .lock()
            .expect("invalid_auth lock")
            .clone();

        let providers: Vec<Value> = runtime
            .models()
            .get_providers()
            .into_iter()
            .filter(|p| {
                let pid = p.id();
                pid == "cstoa" || custom_ids.contains(pid) || !p.get_models().is_empty()
            })
            .map(|p| {
                let pid = p.id();
                let configured = runtime.has_configured_auth(pid);
                let cred_type = credentials
                    .iter()
                    .find(|c| c.provider_id == pid)
                    .map(|c| auth_type_str(c.auth_type));
                let supports_api_key = p.auth().api_key.as_ref().is_some_and(|a| !a.env_only);
                let supports_oauth = p.auth().oauth.is_some();
                json!({
                    "id": pid,
                    "name": p.name(),
                    "custom": custom_ids.contains(pid),
                    "configured": configured,
                    "supportsApiKey": supports_api_key,
                    "supportsOAuth": supports_oauth,
                    "type": cred_type,
                    "requiresLogin": invalid_auth.contains(pid),
                })
            })
            .collect();

        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "providers": providers }),
            method,
        )));
    }

    if pathname == "/api/auth/custom" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let input: CustomProviderInput = serde_json::from_value(body.clone()).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的自定义服务参数无效",
            )
        })?;
        let result = state
            .write_once_keyed(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                "auth-config:all",
                || async move {
                    if state.pool.has_running_session_for_provider(&input.id).await {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "conflict",
                            "此模型服务的会话正在执行，请完成后再修改配置",
                        ));
                    }
                    let saved = state
                        .host
                        .auth()
                        .save_custom_provider_checked(input)
                        .await
                        .map_err(ApiError::from)?;
                    let pid = saved.get("id").and_then(Value::as_str).unwrap_or("");
                    state.pool.global_events.publish(
                        "state",
                        &json!({ "type": "auth_changed", "providerId": pid }),
                    );
                    state.pool.global_events.publish(
                        "state",
                        &json!({ "type": "models_changed", "providerId": pid }),
                    );
                    Ok(saved)
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if let Some(provider_id) = pathname.strip_prefix("/api/auth/custom/") {
        if *method == Method::GET {
            let found = state
                .host
                .auth()
                .custom_provider_checked(provider_id)
                .map_err(ApiError::from)?;
            let Some(val) = found else {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "自定义服务不存在",
                ));
            };
            return Ok(Some(json_response(StatusCode::OK, &val, method)));
        }
        if *method == Method::DELETE {
            let body = read_json_body(raw_body, 128 * 1024)?;
            let Some(obj) = body.as_object() else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "配置版本无效",
                ));
            };
            let Some(revision) = obj
                .get("revision")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "配置版本无效",
                ));
            };
            if obj.len() != 1 {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "配置版本无效",
                ));
            }
            let pid_owned = provider_id.to_owned();
            let result = state
                .write_once_keyed(
                    caller,
                    method.as_str(),
                    pathname,
                    &body,
                    headers,
                    "auth-config:all",
                    || async move {
                        if state
                            .pool
                            .has_running_session_for_provider(&pid_owned)
                            .await
                        {
                            return Err(ApiError::new(
                                StatusCode::CONFLICT,
                                "conflict",
                                "此模型服务的会话正在执行，请完成后再移除",
                            ));
                        }
                        let deleted = state
                            .host
                            .auth()
                            .delete_custom_provider_checked(&pid_owned, &revision)
                            .await
                            .map_err(ApiError::from)?;
                        state.pool.global_events.publish(
                            "state",
                            &json!({ "type": "auth_changed", "providerId": pid_owned }),
                        );
                        state.pool.global_events.publish(
                            "state",
                            &json!({ "type": "models_changed", "providerId": pid_owned }),
                        );
                        Ok(deleted)
                    },
                )
                .await?;
            return Ok(Some(json_response(StatusCode::OK, &result, method)));
        }
    }

    if pathname == "/api/account" && (*method == Method::GET || *method == Method::HEAD) {
        state
            .pool
            .refresh_services()
            .await
            .map_err(ApiError::from)?;
        let params = parse_query_params(query);
        if params.get("refresh").map(String::as_str) == Some("1") {
            state.host.auth().invalidate_account();
        }
        let account = state
            .host
            .auth()
            .account(conn_cancel)
            .await
            .map_err(|msg| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", msg))?;
        return Ok(Some(json_response(StatusCode::OK, &account, method)));
    }

    if pathname == "/api/quota" && (*method == Method::GET || *method == Method::HEAD) {
        state
            .pool
            .refresh_services()
            .await
            .map_err(ApiError::from)?;
        let params = parse_query_params(query);
        let provider_id = params.get("provider").map(String::as_str).unwrap_or("");
        if params.get("refresh").map(String::as_str) == Some("1") {
            state.host.auth().invalidate_provider_account(provider_id);
        }
        let quota = state
            .host
            .auth()
            .quota(provider_id, conn_cancel)
            .await
            .map_err(|msg| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", msg))?;
        return Ok(Some(json_response(StatusCode::OK, &quota, method)));
    }

    // Match `/api/auth/<provider>/(oauth|api-key)/(start|status|cancel|respond)`
    // or `/api/auth/<provider>/(api-key|logout)`
    if let Some(rest) = pathname.strip_prefix("/api/auth/") {
        let segments: Vec<&str> = rest.split('/').collect();
        if segments.len() == 3 {
            let provider_id = segments[0];
            let channel = segments[1];
            let action = segments[2];
            if is_valid_provider_id_key(provider_id)
                && matches!(channel, "oauth" | "api-key")
                && matches!(action, "start" | "status" | "cancel" | "respond")
            {
                let auth_type = if channel == "oauth" {
                    AuthType::OAuth
                } else {
                    AuthType::ApiKey
                };
                let runtime = state.host.auth().runtime().clone();
                let provider_meta = runtime.models().get_provider(provider_id);
                let supported = match auth_type {
                    AuthType::OAuth => provider_meta
                        .as_ref()
                        .is_some_and(|p| p.auth().oauth.is_some()),
                    AuthType::ApiKey => provider_meta
                        .as_ref()
                        .is_some_and(|p| p.auth().api_key.as_ref().is_some_and(|a| !a.env_only)),
                };
                if !supported {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "请求的模型服务不支持此登录方式",
                    ));
                }

                if action == "status" && *method == Method::GET {
                    let status = state
                        .oauth_flows
                        .status(provider_id, Some(auth_type_str(auth_type)));
                    return Ok(Some(json_response(StatusCode::OK, &status, method)));
                }

                if *method != Method::POST {
                    return Ok(None);
                }
                let body = read_json_body(raw_body, 128 * 1024)?;
                let Some(obj) = body.as_object() else {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "请求格式无效",
                    ));
                };

                if action == "start" {
                    if (auth_type == AuthType::OAuth && !obj.is_empty())
                        || obj.keys().any(|k| !matches!(k.as_str(), "key" | "baseUrl"))
                    {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的登录参数无效",
                        ));
                    }
                    let key_opt = match obj.get("key") {
                        None => None,
                        Some(Value::String(k)) if !k.trim().is_empty() && k.len() <= 8192 => {
                            Some(Zeroizing::new(k.clone()))
                        }
                        _ => {
                            return Err(ApiError::new(
                                StatusCode::BAD_REQUEST,
                                "invalid_request",
                                "请求的登录参数无效",
                            ));
                        }
                    };
                    let base_url_opt = match obj.get("baseUrl") {
                        None => None,
                        Some(Value::String(u)) => Some(u.clone()),
                        _ => {
                            return Err(ApiError::new(
                                StatusCode::BAD_REQUEST,
                                "invalid_request",
                                "请求的登录参数无效",
                            ));
                        }
                    };

                    let pid_owned = provider_id.to_owned();
                    let state_for_login = state.clone();
                    let has_inline_key = key_opt.is_some();
                    let result = state
                        .write_once_keyed(
                            caller,
                            method.as_str(),
                            pathname,
                            &body,
                            headers,
                            format!("auth:{pid_owned}"),
                            || async move {
                                let prev =
                                    state_for_login.oauth_flows.status(&pid_owned, None);
                                if prev.get("state").and_then(Value::as_str) == Some("pending")
                                    && prev.get("authType").and_then(Value::as_str)
                                        != Some(auth_type_str(auth_type))
                                {
                                    // ModeController read lock is already dropped; `auth-config:all` is not held here.
                                    state_for_login
                                        .oauth_flows
                                        .cancel_and_wait(&pid_owned)
                                        .await;
                                }

                                let st = state_for_login.clone();
                                let pid_task = pid_owned.clone();
                                let flow = state_for_login.oauth_flows.start_or_reuse(
                                    &pid_owned,
                                    auth_type,
                                    key_opt,
                                    move |interaction| {
                                        Box::pin(async move {
                                            let cancel_tok = interaction
                                                .cancel_token
                                                .clone()
                                                .unwrap_or_default();
                                            if let Some(ref base) = base_url_opt {
                                                let login_interaction = if has_inline_key {
                                                    interaction
                                                } else {
                                                    st.host
                                                        .auth()
                                                        .validate_provider_endpoint(&pid_task, base)
                                                        .map_err(|e| e.message)?;
                                                    let Some(api_auth) = st
                                                        .host
                                                        .auth()
                                                        .runtime()
                                                        .models()
                                                        .get_provider(&pid_task)
                                                        .and_then(|p| p.auth().api_key.clone())
                                                    else {
                                                        return Err(
                                                            "该模型服务不支持 API Key 登录"
                                                                .to_owned(),
                                                        );
                                                    };
                                                    // Wait for user secret with zero config locks or ActivityLease held;
                                                    // `wait_until_ready` returns as soon as this prompt registers.
                                                    let collected = Zeroizing::new(
                                                        interaction
                                                            .prompt_user(AuthPrompt::Secret {
                                                                message: format!(
                                                                    "Enter {}",
                                                                    api_auth.name
                                                                ),
                                                                placeholder: None,
                                                            })
                                                            .await?,
                                                    );
                                                    AuthInteraction {
                                                        cancel_token: Some(cancel_tok.clone()),
                                                        prompt: Arc::new(move |q| {
                                                            let secret = collected.clone();
                                                            Box::pin(async move {
                                                                if matches!(
                                                                    q,
                                                                    AuthPrompt::Secret { .. }
                                                                ) {
                                                                    Ok(secret.as_str().to_owned())
                                                                } else {
                                                                    Err("该模型服务需要其他登录信息"
                                                                        .to_owned())
                                                                }
                                                            })
                                                        }),
                                                        notify: interaction.notify.clone(),
                                                    }
                                                };

                                                // Cancel-aware wait on `auth-config:all` so cancelling this flow never blocks
                                                // behind another provider's custom config edit.
                                                let _cfg_guard = tokio::select! {
                                                    biased;
                                                    _ = cancel_tok.cancelled() => {
                                                        return Err("登录已取消".to_owned());
                                                    }
                                                    guard = st.locks.lock("auth-config:all") => guard,
                                                };
                                                if cancel_tok.is_cancelled() {
                                                    return Err("登录已取消".to_owned());
                                                }
                                                if st
                                                    .pool
                                                    .has_running_session_for_provider(&pid_task)
                                                    .await
                                                {
                                                    return Err("此 Provider 的会话正在执行，请完成后再修改 BaseURL".to_owned());
                                                }
                                                st.host
                                                    .auth()
                                                    .with_provider_endpoint(
                                                        &pid_task,
                                                        base,
                                                        || async {
                                                            st.host
                                                                .auth()
                                                                .login_provider(
                                                                    &pid_task,
                                                                    auth_type,
                                                                    login_interaction,
                                                                )
                                                                .await
                                                                .map(|_| ())
                                                        },
                                                    )
                                                    .await
                                            } else {
                                                st.host
                                                    .auth()
                                                    .login_provider(
                                                        &pid_task,
                                                        auth_type,
                                                        interaction,
                                                    )
                                                    .await
                                                    .map(|_| ())
                                            }
                                        })
                                    },
                                );

                                let mut ready = state_for_login
                                    .oauth_flows
                                    .wait_until_ready(&pid_owned, &flow)
                                    .await?;
                                if let Some(m) = ready.as_object_mut() {
                                    m.insert("started".to_owned(), json!(true));
                                }
                                Ok(ready)
                            },
                        )
                        .await?;

                    return Ok(Some(json_response(StatusCode::ACCEPTED, &result, method)));
                }

                if action == "cancel" {
                    if obj.keys().any(|k| k != "flowId") {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求格式无效",
                        ));
                    }
                    let flow_id = match obj.get("flowId") {
                        None => None,
                        Some(Value::String(s)) => Some(s.clone()),
                        _ => {
                            return Err(ApiError::new(
                                StatusCode::BAD_REQUEST,
                                "invalid_request",
                                "请求格式无效",
                            ));
                        }
                    };
                    let pid_owned = provider_id.to_owned();
                    let result = state
                        .write_once_keyed(
                            caller,
                            method.as_str(),
                            pathname,
                            &body,
                            headers,
                            format!("auth:{pid_owned}"),
                            || async move {
                                let cancelled =
                                    state.oauth_flows.cancel(&pid_owned, flow_id.as_deref());
                                Ok::<_, ApiError>(json!({ "cancelled": cancelled }))
                            },
                        )
                        .await?;
                    return Ok(Some(json_response(StatusCode::OK, &result, method)));
                }

                if action == "respond" {
                    if obj
                        .keys()
                        .any(|k| !matches!(k.as_str(), "flowId" | "promptId" | "value"))
                    {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的授权回答无效",
                        ));
                    }
                    let Some(flow_id) =
                        obj.get("flowId").and_then(Value::as_str).map(str::to_owned)
                    else {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的授权回答无效",
                        ));
                    };
                    let Some(prompt_id) = obj
                        .get("promptId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    else {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的授权回答无效",
                        ));
                    };
                    let Some(val) = obj.get("value").and_then(Value::as_str).map(str::to_owned)
                    else {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的授权回答无效",
                        ));
                    };
                    let pid_owned = provider_id.to_owned();
                    let result = state
                        .write_once_keyed(
                            caller,
                            method.as_str(),
                            pathname,
                            &body,
                            headers,
                            format!("auth:{pid_owned}"),
                            || async move {
                                state
                                    .oauth_flows
                                    .respond(&pid_owned, &flow_id, &prompt_id, &val)?;
                                Ok::<_, ApiError>(json!({ "accepted": true }))
                            },
                        )
                        .await?;
                    return Ok(Some(json_response(StatusCode::OK, &result, method)));
                }
            }
        } else if segments.len() == 2 {
            let provider_id = segments[0];
            let action = segments[1];
            if is_valid_provider_id_key(provider_id) && matches!(action, "api-key" | "logout") {
                let runtime = state.host.auth().runtime().clone();
                let Some(provider_meta) = runtime.models().get_provider(provider_id) else {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "请求的模型服务不存在",
                    ));
                };

                if action == "api-key" && *method == Method::PUT {
                    if provider_meta
                        .auth()
                        .api_key
                        .as_ref()
                        .is_none_or(|a| a.env_only)
                    {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的模型服务不支持 API KEY 登录",
                        ));
                    }
                    let body = read_json_body(raw_body, 128 * 1024)?;
                    let Some(obj) = body.as_object() else {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的密钥或 BaseURL 格式无效",
                        ));
                    };
                    if obj.keys().any(|k| !matches!(k.as_str(), "key" | "baseUrl")) {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的密钥或 BaseURL 格式无效",
                        ));
                    }
                    let Some(key_raw) = obj
                        .get("key")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty() && s.len() <= 8192)
                    else {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的密钥或 BaseURL 格式无效",
                        ));
                    };
                    let key_secret = Zeroizing::new(key_raw.to_owned());
                    let base_url_opt = match obj.get("baseUrl") {
                        None => None,
                        Some(Value::String(u)) => Some(u.clone()),
                        _ => {
                            return Err(ApiError::new(
                                StatusCode::BAD_REQUEST,
                                "invalid_request",
                                "请求的密钥或 BaseURL 格式无效",
                            ));
                        }
                    };

                    let pid_owned = provider_id.to_owned();
                    let st = state.clone();
                    let result = state
                        .write_once_keyed(
                            caller,
                            method.as_str(),
                            pathname,
                            &body,
                            headers,
                            format!("auth:{pid_owned}"),
                            || async move {
                                // Cancel and await any pending interactive/OAuth flow BEFORE acquiring `auth-config:all`,
                                // keeping consistent lock hierarchy (`auth:<provider>` -> `auth-config:all`).
                                st.oauth_flows.cancel_and_wait(&pid_owned).await;

                                let login_action = || async {
                                    let prompts = Arc::new(Mutex::new(0usize));
                                    let secret = key_secret.clone();
                                    let interaction = AuthInteraction::new(
                                        Arc::new(move |question| {
                                            let mut count = prompts.lock().expect("prompts lock");
                                            *count += 1;
                                            let n = *count;
                                            let secret = secret.clone();
                                            Box::pin(async move {
                                                if n != 1
                                                    || !matches!(
                                                        question,
                                                        AuthPrompt::Secret { .. }
                                                    )
                                                {
                                                    return Err(
                                                        "该模型服务需要其他登录信息".to_owned()
                                                    );
                                                }
                                                Ok(secret.as_str().to_owned())
                                            })
                                        }),
                                        Arc::new(|_| {}),
                                    );
                                    st.host
                                        .auth()
                                        .login_provider(&pid_owned, AuthType::ApiKey, interaction)
                                        .await?;
                                    Ok(json!({
                                        "providerId": pid_owned,
                                        "type": "api_key",
                                    }))
                                };

                                let resp_val = if let Some(ref base) = base_url_opt {
                                    let _cfg_guard = st.locks.lock("auth-config:all").await;
                                    if st.pool.has_running_session_for_provider(&pid_owned).await {
                                        return Err(ApiError::new(
                                            StatusCode::CONFLICT,
                                            "conflict",
                                            "此 Provider 的会话正在执行，请完成后再修改 BaseURL",
                                        ));
                                    }
                                    st.host
                                        .auth()
                                        .with_provider_endpoint_checked(
                                            &pid_owned,
                                            base,
                                            login_action,
                                        )
                                        .await
                                        .map_err(ApiError::from)?
                                } else {
                                    login_action().await.map_err(|msg| {
                                        ApiError::new(
                                            StatusCode::BAD_REQUEST,
                                            "invalid_request",
                                            msg,
                                        )
                                    })?
                                };

                                st.pool
                                    .invalid_auth
                                    .lock()
                                    .expect("invalid_auth lock")
                                    .remove(&pid_owned);
                                st.host.auth().invalidate_provider_account(&pid_owned);
                                st.pool.global_events.publish(
                                    "state",
                                    &json!({
                                        "type": "auth_changed",
                                        "providerId": pid_owned,
                                    }),
                                );
                                Ok(resp_val)
                            },
                        )
                        .await?;
                    return Ok(Some(json_response(StatusCode::OK, &result, method)));
                }

                if action == "logout" && *method == Method::POST {
                    let body = read_json_body(raw_body, 128 * 1024)?;
                    if !body.as_object().is_some_and(|o| o.is_empty()) {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求格式无效",
                        ));
                    }
                    let pid_owned = provider_id.to_owned();
                    let st = state.clone();
                    let result = state
                        .write_once_keyed(
                            caller,
                            method.as_str(),
                            pathname,
                            &body,
                            headers,
                            format!("auth:{pid_owned}"),
                            || async move {
                                st.oauth_flows.cancel_and_wait(&pid_owned).await;
                                st.host.auth().logout(&pid_owned).await.map_err(|msg| {
                                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", msg)
                                })?;
                                st.pool
                                    .invalid_auth
                                    .lock()
                                    .expect("invalid_auth lock")
                                    .remove(&pid_owned);
                                st.pool.global_events.publish(
                                    "state",
                                    &json!({
                                        "type": "auth_changed",
                                        "providerId": pid_owned,
                                    }),
                                );
                                Ok(json!({
                                    "providerId": pid_owned,
                                    "type": Value::Null,
                                }))
                            },
                        )
                        .await?;
                    return Ok(Some(json_response(StatusCode::OK, &result, method)));
                }
            }
        }
    }

    Ok(None)
}
