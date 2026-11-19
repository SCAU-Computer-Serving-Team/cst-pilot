use std::sync::Arc;
use std::time::Duration;

use cst_kit::update::service::UpdateControlError;
use cst_os::{ProcessIdentity, RestartUi, normalize_canonical_path_for_hash, query_process_image_path};
use http::{HeaderMap, Method, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth_guard::is_app_client_header;
use crate::mode::{CallerIdentity, ModeRejection};
use crate::routes::{ApiError, SseOrBytesBody, json_response, read_json_body, sse_response};
use crate::settings::DesktopSettingsPatch;
use crate::state::ServerState;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateCancelRequest {
    pub operation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateLaunchRequest {
    pub app_pid: u32,
    pub app_created: u64,
    pub ui: Option<String>,
}

fn map_update_error(err: UpdateControlError) -> ApiError {
    match err {
        UpdateControlError::Busy(msg) => ApiError::new(StatusCode::CONFLICT, "update_busy", msg),
        UpdateControlError::NotAvailable(msg) => {
            ApiError::new(StatusCode::CONFLICT, "update_not_available", msg)
        }
        UpdateControlError::NotReady(msg) => {
            ApiError::new(StatusCode::CONFLICT, "update_not_ready", msg)
        }
        UpdateControlError::Conflict(msg) => ApiError::new(StatusCode::CONFLICT, "conflict", msg),
        UpdateControlError::Failed(msg) => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "update_failed", msg)
        }
    }
}

fn verify_app_parent_identity(
    state: &ServerState,
    app_pid: u32,
    app_created: u64,
) -> Result<ProcessIdentity, ApiError> {
    if state.mode.active_app_connections() == 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parent_process",
            "软件端未建立活跃保活连接，无法发起升级交接",
        ));
    }
    let captured = ProcessIdentity::capture(app_pid)
        .map_err(|msg| ApiError::new(StatusCode::BAD_REQUEST, "invalid_parent_process", msg))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_parent_process",
                "升级父进程不存在或已退出",
            )
        })?;
    if captured.created != app_created {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parent_process",
            "升级父进程创建时间不匹配",
        ));
    }
    let actual_path = query_process_image_path(app_pid)
        .map_err(|msg| ApiError::new(StatusCode::BAD_REQUEST, "invalid_parent_process", msg))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_parent_process",
                "升级父进程路径无法核验",
            )
        })?;
    let expected_path = state.kit_root().join("cst-pilot.exe");
    let actual_norm = normalize_canonical_path_for_hash(&actual_path).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parent_process",
            "升级父进程路径无效",
        )
    })?;
    let expected_norm = normalize_canonical_path_for_hash(&expected_path).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parent_process",
            "工具包主程序 cst-pilot.exe 不存在",
        )
    })?;
    if actual_norm != expected_norm {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parent_process",
            "升级父进程不属于当前工具包 cst-pilot.exe",
        ));
    }
    Ok(captured)
}

pub async fn handle_app_route(
    state: &Arc<ServerState>,
    method: &Method,
    pathname: &str,
    headers: &HeaderMap,
    raw_body: &[u8],
    conn_cancel: CancellationToken,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if pathname == "/api/app/events" && *method == Method::GET {
        let app_guard = state
            .mode
            .register_app_connection()
            .map_err(ApiError::from)?;
        let last_event_id = headers.get("last-event-id").and_then(|v| v.to_str().ok());
        let sse_body = state.pool.global_events.subscribe_single(
            CallerIdentity::App,
            last_event_id,
            conn_cancel,
            Some(Box::new(move || {
                drop(app_guard);
            })),
        );
        return Ok(Some(sse_response(sse_body)));
    }

    if pathname == "/api/app/web/open" && *method == Method::POST {
        let _activity = state
            .host
            .kit()
            .activity_gate()
            .enter()
            .map_err(|msg| ApiError::from(ModeRejection::Updating(msg)))?;
        let port = state.port();
        let (open_res, _ticket) = state.mode.open_web(port).await.map_err(ApiError::from)?;
        state.pool.global_events.publish(
            "state",
            &json!({
                "type": "channel_mode_changed",
                "mode": "web_active",
            }),
        );
        return Ok(Some(json_response(
            StatusCode::OK,
            &serde_json::to_value(open_res).unwrap_or(Value::Null),
            method,
        )));
    }

    if pathname == "/api/app/web/close" && *method == Method::POST {
        let _activity = state
            .host
            .kit()
            .activity_gate()
            .enter()
            .map_err(|msg| ApiError::from(ModeRejection::Updating(msg)))?;
        let _new_epoch = state.mode.close_web().await.map_err(ApiError::from)?;
        state.pool.global_events.publish(
            "state",
            &json!({
                "type": "channel_mode_changed",
                "mode": "app_active",
            }),
        );
        state.pool.revoke_web_connections().await;
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "mode": "app_active" }),
            method,
        )));
    }

    if pathname == "/api/app/settings" {
        if *method == Method::GET {
            let data = state
                .desktop_settings
                .read()
                .await
                .map_err(ApiError::from)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(data).unwrap_or(Value::Null),
                method,
            )));
        }
        if *method == Method::PATCH {
            let _activity = state
                .host
                .kit()
                .activity_gate()
                .enter()
                .map_err(|msg| ApiError::from(ModeRejection::Updating(msg)))?;
            let body_val = read_json_body(raw_body, 128 * 1024)?;
            let patch: DesktopSettingsPatch = serde_json::from_value(body_val).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的桌面端设置无效",
                )
            })?;
            let updated = state
                .desktop_settings
                .patch(patch)
                .await
                .map_err(ApiError::from)?;
            state.pool.global_events.publish(
                "state",
                &json!({
                    "type": "settings_changed",
                    "scope": "desktop",
                }),
            );
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(updated).unwrap_or(Value::Null),
                method,
            )));
        }
    }

    if pathname.starts_with("/api/app/update/") {
        if !is_app_client_header(headers) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "软件端更新控制接口须携带 X-CST-Client: app",
            ));
        }

        if pathname == "/api/app/update/status" && (*method == Method::GET || *method == Method::HEAD) {
            let status = state.host.updates().status();
            let last_report = state.host.updates().last_report().ok().flatten();
            return Ok(Some(json_response(
                StatusCode::OK,
                &json!({
                    "currentVersion": state.kit_version(),
                    "status": status,
                    "lastReport": last_report,
                }),
                method,
            )));
        }

        if pathname == "/api/app/update/check" && *method == Method::POST {
            let body = read_json_body(raw_body, 128 * 1024)?;
            if !body.as_object().is_some_and(|o| o.is_empty()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的升级检查参数无效",
                ));
            }
            let status = state
                .host
                .updates()
                .check_for_app(CancellationToken::new())
                .await
                .map_err(map_update_error)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(status).unwrap_or(Value::Null),
                method,
            )));
        }

        if pathname == "/api/app/update/prepare" && *method == Method::POST {
            let body = read_json_body(raw_body, 128 * 1024)?;
            if !body.as_object().is_some_and(|o| o.is_empty()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的升级准备参数无效",
                ));
            }
            let status = state
                .host
                .updates()
                .prepare_for_app()
                .await
                .map_err(map_update_error)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(status).unwrap_or(Value::Null),
                method,
            )));
        }

        if pathname == "/api/app/update/cancel" && *method == Method::POST {
            let body = read_json_body(raw_body, 128 * 1024)?;
            let req: UpdateCancelRequest = serde_json::from_value(body).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的升级取消参数无效",
                )
            })?;
            let status = state
                .host
                .updates()
                .cancel_prepare(req.operation_id.as_deref())
                .await
                .map_err(map_update_error)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(status).unwrap_or(Value::Null),
                method,
            )));
        }

        if pathname == "/api/app/update/launch" && *method == Method::POST {
            let body = read_json_body(raw_body, 128 * 1024)?;
            let req: UpdateLaunchRequest = serde_json::from_value(body).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的升级启动参数无效",
                )
            })?;
            let restart_ui = match req.ui.as_deref() {
                None | Some("desktop") => RestartUi::Desktop,
                Some("tui") => RestartUi::Tui,
                Some(_) => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "不支持的重启界面类型",
                    ));
                }
            };
            let app_proc = verify_app_parent_identity(state, req.app_pid, req.app_created)?;
            if !state.host.updates().is_ready() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "update_not_ready",
                    "升级包尚未准备就绪",
                ));
            }
            let sessions = state.pool.lifecycle_snapshot(false).await;
            if state.exports.active_count() > 0
                || sessions
                    .iter()
                    .any(|s| s.running || s.pending_inbox_count > 0)
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "session_busy_for_update",
                    "仍有运行中会话、未决收件箱或活动导出任务，请结束后重试升级",
                ));
            }
            let server_proc = ProcessIdentity::current().map_err(|msg| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "server_error", msg)
            })?;
            let mut parents = vec![app_proc];
            if parents[0].pid != server_proc.pid {
                parents.push(server_proc);
            }
            let receipt = state
                .host
                .updates()
                .launch(parents, restart_ui, CancellationToken::new())
                .await
                .map_err(|msg| {
                    if msg.contains("会话") {
                        ApiError::new(StatusCode::CONFLICT, "session_busy_for_update", msg)
                    } else {
                        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "update_failed", msg)
                    }
                })?;

            let state_for_exit = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let _ = state_for_exit.execute_shutdown().await;
            });

            return Ok(Some(json_response(
                StatusCode::OK,
                &json!({ "receipt": receipt }),
                method,
            )));
        }
    }

    if pathname == "/api/app/exit" && *method == Method::POST {
        state.execute_shutdown().await?;
        return Ok(Some(json_response(
            StatusCode::OK,
            &json!({ "exited": true }),
            method,
        )));
    }

    Ok(None)
}
