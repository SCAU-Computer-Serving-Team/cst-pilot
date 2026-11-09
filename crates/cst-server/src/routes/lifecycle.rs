use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth_guard::WEB_THEME_COOKIE_NAME;
use crate::mode::{CallerIdentity, ChannelMode};
use crate::routes::{
    ApiError, SseOrBytesBody, json_response, parse_query_params, read_json_body, sse_response,
};
use crate::session::attachments::is_valid_session_id_key;
use crate::session::events::MultiplexSubscriptionUpdate;
use crate::settings::ThemeMode;
use crate::state::ServerState;

#[allow(clippy::too_many_arguments)]
pub async fn handle_lifecycle_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
    conn_cancel: CancellationToken,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if pathname == "/api/health" && (*method == Method::GET || *method == Method::HEAD) {
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({
                "connected": true,
                "stopping": state.mode.is_stopping(),
            }),
            method,
        )));
    }

    if pathname == "/api/state" && (*method == Method::GET || *method == Method::HEAD) {
        let mode_snap = state.mode.mode_snapshot().await;
        let is_browser_in_app_mode = matches!(caller, CallerIdentity::Web { .. })
            && mode_snap.mode == ChannelMode::AppActive;
        let (sessions_json, stage) = if is_browser_in_app_mode {
            (json!([]), "app_active")
        } else {
            let rows = state.pool.list().await.map_err(ApiError::from)?;
            (serde_json::to_value(rows).unwrap_or(json!([])), "ready")
        };
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({
                "version": state.kit_version(),
                "sessions": sessions_json,
                "stage": stage,
                "channelMode": mode_snap.mode.as_str(),
                "connected": true,
                "commands": [
                    "/compact",
                    "/fork",
                    "/tree",
                    "/skill:bitlocker",
                    "/skill:disk",
                    "/skill:driver",
                    "/skill:eventlog",
                    "/skill:ls",
                    "/skill:runbook",
                    "/skill:startup",
                    "/skill:sys",
                ],
            }),
            method,
        )));
    }

    if pathname == "/api/lifecycle" && (*method == Method::GET || *method == Method::HEAD) {
        let stopping = state.mode.is_stopping();
        let sessions = state.pool.lifecycle_snapshot(stopping).await;
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({
                "stopping": stopping,
                "sessions": sessions,
            }),
            method,
        )));
    }

    if pathname == "/api/lifecycle/telemetry" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let reason = body.get("reason").and_then(Value::as_str).unwrap_or("");
        if !matches!(reason, "pagehide" | "pageshow" | "online") {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_hint",
                "无效的遥测提示",
            ));
        }
        if !state.mode.is_stopping()
            && let Some(telemetry) = state.host.telemetry()
        {
            telemetry.hint();
        }
        return Ok(Some(json_response(
            StatusCode::ACCEPTED,
            &json!({ "accepted": true }),
            method,
        )));
    }

    if pathname == "/api/lifecycle/exit" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        if body.get("confirm").and_then(Value::as_str) != Some("stop") {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求须明确确认停止任务",
            ));
        }
        state.execute_shutdown().await?;
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "exited": true }),
            method,
        )));
    }

    if state.mode.is_stopping() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "stopping",
            "程序正在退出，请勿继续提交操作",
        ));
    }

    if pathname == "/api/events" && *method == Method::GET {
        let _admission = state
            .mode
            .admit_events(caller, true)
            .await
            .map_err(ApiError::from)?;
        let last_event_id = headers.get("last-event-id").and_then(|v| v.to_str().ok());
        let sse_body =
            state
                .pool
                .global_events
                .subscribe_single(caller, last_event_id, conn_cancel, None);
        return Ok(Some(sse_response(sse_body)));
    }

    if pathname == "/api/events/stream" && *method == Method::GET {
        let params = parse_query_params(query);
        let Some(client_id) = params
            .get("clientId")
            .filter(|id| is_valid_session_id_key(id))
        else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "缺少有效的聚合流 clientId",
            ));
        };
        let _admission = state
            .mode
            .admit_events(caller, false)
            .await
            .map_err(ApiError::from)?;
        let last_event_id = headers.get("last-event-id").and_then(|v| v.to_str().ok());
        let sse_body =
            state
                .pool
                .multiplex_hub
                .connect(caller, client_id, last_event_id, conn_cancel);
        return Ok(Some(sse_response(sse_body)));
    }

    if let Some(rest) = pathname.strip_prefix("/api/events/stream/")
        && let Some(client_id) = rest.strip_suffix("/subscriptions")
        && *method == Method::PUT
    {
        if !is_valid_session_id_key(client_id) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "聚合流 clientId 无效",
            ));
        }
        let body = read_json_body(raw_body, 128 * 1024)?;
        let update: MultiplexSubscriptionUpdate =
            serde_json::from_value(body.clone()).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的聚合流订阅参数无效",
                )
            })?;
        for sid in &update.sessions {
            if !is_valid_session_id_key(sid) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "订阅的会话 ID 格式无效",
                ));
            }
        }
        let client_id_owned = client_id.to_owned();
        let result = state
            .write_once(
                caller,
                method.as_str(),
                pathname,
                &body,
                Some(headers),
                || async move {
                    let subscribed = state
                        .pool
                        .update_multiplex_subscriptions(caller, &client_id_owned, update)
                        .await
                        .map_err(ApiError::from)?;
                    Ok(json!({ "subscribedSessions": subscribed }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    if pathname == "/api/settings" {
        if *method == Method::GET {
            let settings = state.web_settings.read().await.map_err(ApiError::from)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &json!({ "theme": settings.theme }),
                method,
            )));
        }
        if *method == Method::PATCH {
            let body = read_json_body(raw_body, 128 * 1024)?;
            let Some(obj) = body.as_object() else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的主题设置无效",
                ));
            };
            let Some(theme_str) = obj.get("theme").and_then(Value::as_str) else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的主题设置无效",
                ));
            };
            let Some(theme) = ThemeMode::parse(theme_str) else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的主题设置无效",
                ));
            };
            if obj.len() != 1 {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的主题设置无效",
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
                        let saved = state
                            .web_settings
                            .save_theme(theme)
                            .await
                            .map_err(ApiError::from)?;
                        state
                            .pool
                            .global_events
                            .publish("state", &json!({ "type": "settings_changed" }));
                        Ok(json!({ "theme": saved.theme }))
                    },
                )
                .await?;
            let mut resp = json_response(StatusCode::OK, &result, method);
            let cookie = format!(
                "{WEB_THEME_COOKIE_NAME}={}; Path=/; SameSite=Strict",
                theme.as_str()
            );
            if let Ok(val) = HeaderValue::from_str(&cookie) {
                resp.headers_mut().insert(http::header::SET_COOKIE, val);
            }
            return Ok(Some(resp));
        }
    }

    if pathname == "/api/files" && *method == Method::GET {
        let params = parse_query_params(query);
        let q = params
            .get("query")
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let target_cwd = if let Some(sid) = params.get("sessionId") {
            if !is_valid_session_id_key(sid) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的会话ID无效",
                ));
            }
            let slot = state.pool.open_saved(sid).await.map_err(ApiError::from)?;
            let raw = slot
                .session()
                .with_session_manager(|sm| sm.get_cwd().to_owned());
            std::path::PathBuf::from(raw)
        } else {
            state.pool.default_cwd().to_path_buf()
        };
        let all = state.file_cache.list(&target_cwd);
        let total = all.len();
        let matches: Vec<Value> = all
            .into_iter()
            .filter(|p| q.is_empty() || p.to_lowercase().contains(&q))
            .take(50)
            .map(|path| {
                let dir = path
                    .rfind('/')
                    .map(|idx| path[..idx].to_owned())
                    .unwrap_or_default();
                json!({
                    "path": path,
                    "dir": dir,
                })
            })
            .collect();
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({
                "total": total,
                "files": matches,
            }),
            method,
        )));
    }

    Ok(None)
}
