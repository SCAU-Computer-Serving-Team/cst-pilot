use bytes::Bytes;
use http::{HeaderMap, Method, Response, StatusCode};
use pi_runtime::parse_model_thinking_level_cli;
use pi_session::NavigateTreeOptions;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::mode::CallerIdentity;
use crate::routes::models::get_supported_thinking_levels;
use crate::routes::{
    ApiError, MaybeOmitted, SseOrBytesBody, bytes_response, json_response, read_json_body,
    sse_response,
};
use crate::runbook::{
    export_and_maybe_open_runbook, resolve_runbook_file_from_slot, resolve_runbook_plan_from_slot,
    validate_and_open_runbook,
};
use crate::session::attachments::{is_valid_session_id_key, is_valid_sha256_hex};
use crate::session::events::browser_json_value;
use crate::session::inbox::{Delivery, QueueEdit};
use crate::session::interactions::UiRespondError;
use crate::session::receipts::{ReceiptStatus, is_valid_idempotency_key};
use crate::session::{SessionPoolError, default_model_reference, require_model_credentials};
use crate::state::ServerState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSessionRequest {
    #[serde(default)]
    provider: MaybeOmitted<String>,
    #[serde(default)]
    model_id: MaybeOmitted<String>,
    #[serde(default)]
    thinking_level: MaybeOmitted<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_sessions_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
    conn_cancel: CancellationToken,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    // 1. `/api/sessions`, `/api/sessions/search` & `/api/sessions/creation/:requestKey`
    if pathname == "/api/sessions" && (*method == Method::GET || *method == Method::HEAD) {
        let rows = state.pool.list().await.map_err(ApiError::from)?;
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "sessions": rows }),
            method,
        )));
    }

    if pathname == "/api/sessions/search" && (*method == Method::GET || *method == Method::HEAD) {
        let mut q = String::new();
        let mut active_session_id: Option<String> = None;
        let mut title_offset: usize = 0;
        let mut title_limit: usize = 10;
        let mut content_offset: usize = 0;
        let mut content_limit: usize = 10;
        let mut snapshot_token: Option<String> = None;

        if let Some(raw_q) = query {
            for (k, v) in url::form_urlencoded::parse(raw_q.as_bytes()) {
                match k.as_ref() {
                    "q" => q = v.into_owned(),
                    "activeSessionId" => {
                        let s = v.trim();
                        if !s.is_empty() && is_valid_session_id_key(s) {
                            active_session_id = Some(s.to_owned());
                        }
                    }
                    "titleOffset" => {
                        if let Ok(n) = v.parse::<usize>() {
                            title_offset = n;
                        }
                    }
                    "titleLimit" => {
                        if let Ok(n) = v.parse::<usize>() {
                            title_limit = n.clamp(1, 50);
                        }
                    }
                    "contentOffset" => {
                        if let Ok(n) = v.parse::<usize>() {
                            content_offset = n;
                        }
                    }
                    "contentLimit" => {
                        if let Ok(n) = v.parse::<usize>() {
                            content_limit = n.clamp(1, 50);
                        }
                    }
                    "snapshotToken" => {
                        let s = v.trim();
                        if !s.is_empty() && s.len() <= 64 {
                            snapshot_token = Some(s.to_owned());
                        }
                    }
                    _ => {}
                }
            }
        }

        let resp = state
            .pool
            .search_sessions(crate::session::search_index::SearchQueryParams {
                query: &q,
                active_session_id: active_session_id.as_deref(),
                title_offset,
                title_limit,
                content_offset,
                content_limit,
                snapshot_token: snapshot_token.as_deref(),
            })
            .await;
        let body = serde_json::to_value(&resp).unwrap_or_else(|_| json!({}));
        return Ok(Some(json_response(StatusCode::OK, &body, method)));
    }

    if let Some(req_key) = pathname.strip_prefix("/api/sessions/creation/")
        && (*method == Method::GET || *method == Method::HEAD)
    {
        if !is_valid_idempotency_key(req_key) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "去重键无效",
            ));
        }
        let receipt = state
            .pool
            .receipts
            .lookup::<Value>(req_key)
            .await
            .map_err(ApiError::from)?;
        let resp = match receipt {
            None => json!({ "status": "missing" }),
            Some(rec) => match rec.status {
                ReceiptStatus::Started => json!({ "status": "started" }),
                ReceiptStatus::Done => {
                    let sid = rec
                        .result
                        .as_ref()
                        .and_then(|r| r.get("id"))
                        .and_then(Value::as_str);
                    match sid {
                        Some(id) => json!({ "status": "done", "sessionId": id }),
                        None => json!({ "status": "done" }),
                    }
                }
            },
        };
        return Ok(Some(json_response(StatusCode::OK, &resp, method)));
    }

    if pathname == "/api/sessions" && *method == Method::POST {
        let Some(idem_key) = headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .filter(|k| is_valid_idempotency_key(k))
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "缺少有效的去重键",
            ));
        };

        let body = read_json_body(raw_body, 128 * 1024)?;
        if !body.is_object() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的新会话配置无效",
            ));
        }
        let req: CreateSessionRequest = serde_json::from_value(body.clone()).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的新会话配置无效",
            )
        })?;
        if req.provider.is_present() != req.model_id.is_present() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的新会话配置无效",
            ));
        }
        let initial_model = match (req.provider.into_option(), req.model_id.into_option()) {
            (None, None) => None,
            (Some(p), Some(m)) => Some((p, m)),
            _ => unreachable!("checked pair presence"),
        };
        let initial_thinking_str = req.thinking_level.into_option();

        let settings = state
            .pool
            .refresh_services()
            .await
            .map_err(ApiError::from)?;
        let runtime = state.host.auth().runtime().clone();
        if let Some((ref p, ref m)) = initial_model
            && runtime.get_model(p, m).is_none()
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的模型不存在",
            ));
        }

        let (target_prov, target_mid) = initial_model
            .clone()
            .unwrap_or_else(|| default_model_reference(&settings));
        let checked_model = require_model_credentials(
            &runtime,
            &state.pool.invalid_auth,
            &target_prov,
            &target_mid,
        )
        .await
        .map_err(ApiError::from)?;

        let initial_thinking = if let Some(ref lvl_str) = initial_thinking_str {
            let Some(parsed) = parse_model_thinking_level_cli(lvl_str) else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的思考强度无效",
                ));
            };
            let supported = get_supported_thinking_levels(Some(&checked_model));
            if !supported.contains(&parsed) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的思考强度无效",
                ));
            }
            Some(parsed)
        } else {
            None
        };

        let key_for_create = idem_key.clone();
        let result = state
            .write_once(
                caller,
                method.as_str(),
                pathname,
                &body,
                Some(headers),
                || async move {
                    let slot = state
                        .pool
                        .create_with_key(&key_for_create, initial_model, initial_thinking)
                        .await
                        .map_err(ApiError::from)?;
                    state
                        .pool
                        .global_events
                        .publish("state", &json!({ "type": "sessions_changed" }));
                    Ok(json!({ "id": slot.id }))
                },
            )
            .await?;

        let created_id = result
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Err(err) = state.pool.open_saved(&created_id).await {
            if matches!(err, SessionPoolError::NotFound(_)) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "conflict",
                    "创建请求对应的会话已删除",
                ));
            }
            return Err(ApiError::from(err));
        }

        return Ok(Some(json_response(StatusCode::CREATED, &result, method)));
    }

    // 3. `/api/sessions/:id[/...]`
    let Some(rest) = pathname.strip_prefix("/api/sessions/") else {
        return Ok(None);
    };
    let (id, suffix) = match rest.split_once('/') {
        Some((id, s)) => (id, s),
        None => (rest, ""),
    };
    if !is_valid_session_id_key(id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "请求的会话 ID 无效",
        ));
    }

    if suffix == "runbook/export" && *method == Method::POST {
        let body = read_json_body(raw_body, 4096)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定命令方案记录",
            ));
        };
        if obj
            .keys()
            .any(|k| !matches!(k.as_str(), "entryId" | "toolCallId" | "open"))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求参数包含不支持的字段",
            ));
        }
        let Some(entry_id) = obj
            .get("entryId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定命令方案记录",
            ));
        };
        let Some(tool_call_id) = obj
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定命令方案记录",
            ));
        };
        let open = match obj.get("open") {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "open 参数必须为布尔值",
                ));
            }
        };
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    let (runbook_id, version) =
                        resolve_runbook_plan_from_slot(&slot, &entry_id, &tool_call_id)?;
                    export_and_maybe_open_runbook(
                        state.kit_root(),
                        state.artifacts_root(),
                        runbook_id,
                        version,
                        open,
                        &state.open_runbook,
                    )
                    .await
                    .map_err(ApiError::from)
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "runbook/open" && *method == Method::POST {
        let body = read_json_body(raw_body, 4096)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定清单交付记录",
            ));
        };
        if obj.len() != 2 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定清单交付记录",
            ));
        }
        let Some(entry_id) = obj
            .get("entryId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定清单交付记录",
            ));
        };
        let Some(tool_call_id) = obj
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须指定清单交付记录",
            ));
        };
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    let file = resolve_runbook_file_from_slot(&slot, &entry_id, &tool_call_id)?;
                    validate_and_open_runbook(
                        &file,
                        state.kit_root(),
                        state.artifacts_root(),
                        &state.open_runbook,
                    )
                    .await
                    .map_err(ApiError::from)
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "bitlocker" && (*method == Method::GET || *method == Method::HEAD) {
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        let snap = slot.bitlocker.snapshot();
        return Ok(Some(json_response(
            StatusCode::OK,
            &serde_json::to_value(snap).unwrap_or(Value::Null),
            method,
        )));
    }

    if let Some(request_id) = suffix.strip_prefix("bitlocker/")
        && *method == Method::POST
    {
        if !is_valid_session_id_key(request_id) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "BitLocker 请求 ID 无效",
            ));
        }
        let mut body = read_json_body(raw_body, 4096)?;
        struct ZeroizeBitLockerBody<'a>(&'a mut Value);
        impl Drop for ZeroizeBitLockerBody<'_> {
            fn drop(&mut self) {
                use zeroize::Zeroize as _;
                if let Some(obj) = self.0.as_object_mut()
                    && let Some(Value::String(pw)) = obj.get_mut("password")
                {
                    pw.zeroize();
                }
            }
        }
        let body_guard = ZeroizeBitLockerBody(&mut body);
        // Check mode + ActivityGate without persisting to MutationReceipts, then release ModeController read lock.
        let (mode_guard, _activity_lease) = state
            .mode
            .admit_write(caller, Some(state.host.kit().activity_gate()))
            .await
            .map_err(ApiError::from)?
            .into_parts();
        drop(mode_guard);
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        // There is no durable receipt for a sensitive response; consume it under channel admission.
        let _acceptance = state.mode.admit_write(caller, None).await?;
        let respond_res = slot.bitlocker.respond(request_id, body_guard.0);
        drop(body_guard);
        respond_res.map_err(ApiError::from)?;
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "answered": true }),
            method,
        )));
    }

    if suffix.is_empty() && (*method == Method::GET || *method == Method::HEAD) {
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        let snap = slot.full_snapshot().await.map_err(ApiError::from)?;
        return Ok(Some(json_response(StatusCode::OK, &snap, method)));
    }

    if suffix.is_empty() && *method == Method::DELETE {
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &Value::Null,
                headers,
                id,
                || async move {
                    state
                        .pool
                        .delete_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    Ok(json!({ "deleted": true }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix.is_empty() && *method == Method::PATCH {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的会话名称无效",
            ));
        };
        let Some(name) = obj.get("name").and_then(Value::as_str).map(str::to_owned) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的会话名称无效",
            ));
        };
        if obj.len() != 1 || name.trim().is_empty() || name.chars().count() > 160 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的会话名称无效",
            ));
        }
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    let updated_name = state
                        .pool
                        .rename_session(&slot, &name)
                        .map_err(ApiError::from)?;
                    Ok(json!({
                        "id": id_owned,
                        "name": updated_name,
                    }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "fork" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的分支节点无效",
            ));
        };
        let Some(entry_id) = obj
            .get("entryId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的分支节点无效",
            ));
        };
        if obj.len() != 1 || entry_id.trim().is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的分支节点无效",
            ));
        }
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let forked = state
                        .pool
                        .fork(&id_owned, &entry_id)
                        .await
                        .map_err(ApiError::from)?;
                    state
                        .pool
                        .global_events
                        .publish("state", &json!({ "type": "sessions_changed" }));
                    Ok(json!({ "id": forked.id }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::CREATED, &result, method)));
    }

    if suffix == "tree" && (*method == Method::GET || *method == Method::HEAD) {
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        if query.is_some_and(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .any(|(key, value)| key == "format" && value == "flat")
        }) {
            let payload = slot.session().with_session_manager(|sm| {
                let entries: Vec<_> = sm
                    .get_entries()
                    .into_iter()
                    .map(|entry| json!({ "label": sm.get_label(entry.id()), "entry": entry }))
                    .collect();
                json!({ "entries": entries, "leafId": sm.get_leaf_id() })
            });
            return Ok(Some(json_response(StatusCode::OK, &payload, method)));
        }
        let (tree, leaf_id) = slot
            .session()
            .with_session_manager(|sm| (sm.get_tree(), sm.get_leaf_id().map(str::to_owned)));
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({
                "tree": tree,
                "leafId": leaf_id,
            }),
            method,
        )));
    }

    if suffix == "tree/navigate" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的分支导航参数无效",
            ));
        };
        let summarize = match obj.get("summarize") {
            None => false,
            Some(Value::Bool(b)) => *b,
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的总结开关无效",
                ));
            }
        };
        let custom_instructions = match obj.get("customInstructions") {
            None => None,
            Some(Value::String(s)) if s.chars().count() <= 20_000 => Some(s.clone()),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的总结提示无效",
                ));
            }
        };
        let Some(entry_id) = obj
            .get("entryId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的分支节点不存在",
            ));
        };

        let expected_leaf = match obj.get("expectedLeafId") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(id)) if !id.is_empty() => Some(Some(id.as_str())),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的当前分支节点无效",
                ));
            }
        };
        if expected_leaf.is_some() && (summarize || custom_instructions.is_some()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "定位历史分支时不能生成总结",
            ));
        }

        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    if expected_leaf.is_some() {
                        if slot.inbox.has_delivering().await.map_err(ApiError::from)? {
                            return Err(ApiError::new(
                                StatusCode::CONFLICT,
                                "conflict",
                                "会话正在执行",
                            ));
                        }
                    } else if slot.session().is_streaming() {
                        slot.inbox.pause().await.map_err(ApiError::from)?;
                        slot.session().abort().await;
                    }
                    let exists = slot
                        .session()
                        .with_session_manager(|sm| sm.get_entry(&entry_id).is_some());
                    if !exists {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "请求的分支节点不存在",
                        ));
                    }
                    let nav = match expected_leaf {
                        Some(expected) => {
                            slot.session()
                                .navigate_tree_to_leaf(&entry_id, expected)
                                .await
                        }
                        None => {
                            slot.session()
                                .navigate_tree(
                                    &entry_id,
                                    NavigateTreeOptions {
                                        summarize,
                                        custom_instructions,
                                        replace_instructions: false,
                                        label: None,
                                    },
                                )
                                .await
                        }
                    }
                    .map_err(|msg| ApiError::new(StatusCode::CONFLICT, "conflict", msg))?;
                    if nav.cancelled && expected_leaf.is_some() {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "conflict",
                            "历史分支切换已取消",
                        ));
                    }

                    state.pool.search.upsert_from_slot(&slot);

                    slot.events
                        .publish("session", &json!({ "type": "session_tree" }));
                    state.pool.global_events.publish(
                        "state",
                        &json!({
                            "type": "usage_changed",
                            "sessionId": id_owned,
                        }),
                    );
                    state.pool.global_events.publish(
                        "state",
                        &json!({
                            "type": "sessions_changed",
                            "sessionId": id_owned,
                        }),
                    );

                    if nav.cancelled {
                        Ok(json!({
                            "id": id_owned,
                            "entryId": entry_id,
                            "cancelled": true,
                        }))
                    } else {
                        Ok(json!({
                            "id": id_owned,
                            "entryId": entry_id,
                            "cancelled": false,
                            "editorText": nav.editor_text,
                            "summaryEntryId": nav.summary_entry.map(|s| s.id),
                        }))
                    }
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "tree/abort" && *method == Method::POST {
        if let Some(slot) = state.pool.get_loaded(id).await {
            slot.session().request_abort();
        }
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "aborted": true }),
            method,
        )));
    }

    if suffix == "compact" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的压缩提示无效",
            ));
        };
        let instructions = match obj.get("instructions") {
            None => None,
            Some(Value::String(s)) if s.chars().count() <= 20_000 => Some(s.clone()),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的压缩提示无效",
                ));
            }
        };
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    slot.compact(instructions).await.map_err(ApiError::from)?;
                    state.pool.search.upsert_from_slot(&slot);
                    Ok(json!({
                        "id": id_owned,
                        "compacted": true,
                    }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if let Some(rest_ui) = suffix.strip_prefix("ui/")
        && let Some(request_id) = rest_ui.strip_suffix("/response")
        && *method == Method::POST
    {
        if !is_valid_session_id_key(request_id) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "提问 ID 无效",
            ));
        }
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "回答格式无效",
            ));
        };
        if obj.len() != 1 || !obj.contains_key("value") {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "回答格式无效",
            ));
        }
        let val = obj.get("value").cloned().unwrap_or(Value::Null);
        let id_owned = id.to_owned();
        let req_id_owned = request_id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    slot.ui
                        .respond(&req_id_owned, &val)
                        .map_err(|err| match err {
                            UiRespondError::Expired(msg) => {
                                ApiError::new(StatusCode::CONFLICT, "conflict", msg)
                            }
                            UiRespondError::InvalidInput(msg) => {
                                ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", msg)
                            }
                        })?;
                    Ok(json!({ "answered": true }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "events" && *method == Method::GET {
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        let _admission = state
            .mode
            .admit_events(caller, false)
            .await
            .map_err(ApiError::from)?;
        let last_event_id = headers.get("last-event-id").and_then(|v| v.to_str().ok());
        let sse_body = slot
            .events
            .subscribe_single(caller, last_event_id, conn_cancel, None);
        return Ok(Some(sse_response(sse_body)));
    }

    if suffix == "open" && *method == Method::POST {
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &Value::Null,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    Ok(json!({
                        "id": slot.id,
                        "running": slot.session().is_streaming(),
                    }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "close" && *method == Method::POST {
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &Value::Null,
                headers,
                id,
                || async move { Ok(json!({ "closed": true })) },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if let Some(msg_id) = suffix.strip_prefix("messages/")
        && (*method == Method::GET || *method == Method::HEAD)
    {
        if !is_valid_session_id_key(msg_id) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "消息 ID 无效",
            ));
        }
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        let snap = slot.inbox.snapshot().await.map_err(ApiError::from)?;
        let found = snap.items.into_iter().find(|item| item.id == msg_id);
        let out = browser_json_value(&json!({ "item": found }));
        return Ok(Some(json_response(StatusCode::OK, &out, method)));
    }

    if suffix == "messages" && *method == Method::POST {
        let body = read_json_body(raw_body, 17 * 1024 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "消息格式无效",
            ));
        };
        if obj
            .keys()
            .any(|k| !matches!(k.as_str(), "id" | "text" | "delivery" | "images" | "skill"))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "消息字段不受支持",
            ));
        }
        let Some(message_id) = obj.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "消息格式无效",
            ));
        };
        let Some(text) = obj.get("text").and_then(Value::as_str).map(str::to_owned) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "消息格式无效",
            ));
        };
        let delivery = match obj.get("delivery").and_then(Value::as_str) {
            Some("queue") => Delivery::Queue,
            Some("steer") => Delivery::Steer,
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "消息格式无效",
                ));
            }
        };
        let skill = match obj.get("skill") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "消息格式无效",
                ));
            }
        };
        let images_val = obj.get("images").cloned().unwrap_or_else(|| json!([]));

        let id_owned = id.to_owned();
        let item = state
            .locks
            .serialize(format!("session:{id_owned}"), || async move {
                let (mode_guard, _activity_lease) = state
                    .mode
                    .admit_write(caller, Some(state.host.kit().activity_gate()))
                    .await
                    .map_err(ApiError::from)?
                    .into_parts();
                drop(mode_guard);
                state
                    .pool
                    .refresh_services()
                    .await
                    .map_err(ApiError::from)?;
                let slot = state
                    .pool
                    .open_saved(&id_owned)
                    .await
                    .map_err(ApiError::from)?;
                let current_model = slot.session().model();
                require_model_credentials(
                    state.host.auth().runtime(),
                    &state.pool.invalid_auth,
                    &current_model.provider,
                    &current_model.id,
                )
                .await
                .map_err(ApiError::from)?;
                // Preparation can outlast the caller's channel. Keep admission through the
                // durable inbox write, which serves as the acceptance point for this route.
                let _acceptance = state.mode.admit_write(caller, None).await?;
                let refs = state
                    .pool
                    .images
                    .accept(&id_owned, &images_val)
                    .await
                    .map_err(ApiError::from)?;
                slot.inbox
                    .accept(&message_id, &text, delivery, refs, skill)
                    .await
                    .map_err(ApiError::from)
            })
            .await?;

        state.pool.global_events.publish(
            "state",
            &json!({
                "type": "sessions_changed",
                "sessionId": id,
            }),
        );
        return Ok(Some(json_response(
            StatusCode::ACCEPTED,
            &json!({ "item": item }),
            method,
        )));
    }

    if let Some(att_id) = suffix.strip_prefix("attachments/")
        && (*method == Method::GET || *method == Method::HEAD)
    {
        if !is_valid_sha256_hex(att_id) {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "图片不存在",
            ));
        }
        let slot = state.pool.open_saved(id).await.map_err(ApiError::from)?;
        let snap = slot.inbox.snapshot().await.map_err(ApiError::from)?;
        let Some(img_ref) = snap
            .items
            .into_iter()
            .flat_map(|item| item.images)
            .find(|img| img.id == att_id)
        else {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "图片不存在",
            ));
        };
        let bytes = state
            .pool
            .images
            .read(id, &img_ref.id)
            .await
            .map_err(ApiError::from)?;
        let len = bytes.len();
        let body_bytes = if *method == Method::HEAD {
            Bytes::new()
        } else {
            Bytes::from(bytes)
        };
        return Ok(Some(bytes_response(
            StatusCode::OK,
            &img_ref.mime_type,
            "no-store",
            len,
            body_bytes,
        )));
    }

    if suffix == "abort" && *method == Method::POST {
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &Value::Null,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    slot.inbox.pause().await.map_err(ApiError::from)?;
                    slot.session().abort().await;
                    Ok(json!({ "paused": true }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if suffix == "queue/order" && *method == Method::PUT {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "队列排序格式无效",
            ));
        };
        let Some(version) = obj.get("version").and_then(Value::as_u64) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "队列排序格式无效",
            ));
        };
        let Some(ids_arr) = obj.get("ids").and_then(Value::as_array) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "队列排序格式无效",
            ));
        };
        let mut ids = Vec::with_capacity(ids_arr.len());
        for v in ids_arr {
            let Some(s) = v.as_str() else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "队列排序格式无效",
                ));
            };
            ids.push(s.to_owned());
        }
        let id_owned = id.to_owned();
        let result = state
            .write_once_for_session(
                caller,
                method.as_str(),
                pathname,
                &body,
                headers,
                id,
                || async move {
                    let slot = state
                        .pool
                        .open_saved(&id_owned)
                        .await
                        .map_err(ApiError::from)?;
                    let queue = slot
                        .inbox
                        .reorder(&ids, version)
                        .await
                        .map_err(ApiError::from)?;
                    Ok(json!({ "queue": queue }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if let Some(rest_queue) = suffix.strip_prefix("queue/") {
        let (item_id, action_opt) = match rest_queue.split_once('/') {
            Some((iid, act)) => (iid, Some(act)),
            None => (rest_queue, None),
        };
        if !is_valid_session_id_key(item_id) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "队列条目 ID 无效",
            ));
        }
        if matches!(*method, Method::PATCH | Method::DELETE | Method::POST) {
            let body = read_json_body(raw_body, 128 * 1024)?;
            let Some(obj) = body.as_object() else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "缺少队列版本",
                ));
            };
            let Some(version) = obj.get("version").and_then(Value::as_u64) else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "缺少队列版本",
                ));
            };
            let edit = match (method.clone(), action_opt) {
                (Method::DELETE, None) => QueueEdit {
                    remove: true,
                    ..Default::default()
                },
                (Method::POST, Some("retry")) => QueueEdit {
                    retry: true,
                    ..Default::default()
                },
                (Method::POST, Some("steer")) => QueueEdit {
                    delivery: Some(Delivery::Steer),
                    ..Default::default()
                },
                (Method::PATCH, None) => {
                    let Some(text) = obj.get("text").and_then(Value::as_str).map(str::to_owned)
                    else {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "操作无效",
                        ));
                    };
                    QueueEdit {
                        text: Some(text),
                        ..Default::default()
                    }
                }
                _ => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "操作无效",
                    ));
                }
            };

            let id_owned = id.to_owned();
            let item_id_owned = item_id.to_owned();
            let result = state
                .write_once_for_session(
                    caller,
                    method.as_str(),
                    pathname,
                    &body,
                    headers,
                    id,
                    || async move {
                        let slot = state
                            .pool
                            .open_saved(&id_owned)
                            .await
                            .map_err(ApiError::from)?;
                        let queue = slot
                            .inbox
                            .change(&item_id_owned, version, edit)
                            .await
                            .map_err(ApiError::from)?;
                        Ok(json!({ "queue": queue }))
                    },
                )
                .await?;
            return Ok(Some(json_response(StatusCode::OK, &result, method)));
        }
    }

    Ok(None)
}
