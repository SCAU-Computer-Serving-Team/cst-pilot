use http::{Method, Response, StatusCode};
use serde_json::{Value, json};

use crate::mode::{CallerIdentity, ChannelMode};
use crate::routes::{ApiError, SseOrBytesBody, json_response, read_json_body};
use crate::state::ServerState;

fn state_commands() -> Vec<String> {
    let mut skills: Vec<&str> = cst_kit::SKILL_NAMES.to_vec();
    skills.sort_unstable();
    let mut commands = vec![
        "/compact".to_owned(),
        "/fork".to_owned(),
        "/tree".to_owned(),
    ];
    commands.extend(skills.into_iter().map(|skill| format!("/skill:{skill}")));
    commands
}

pub async fn handle_lifecycle_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    raw_body: &[u8],
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
                "multiplexStream": true,
                "connected": true,
                "commands": state_commands(),
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

    Ok(None)
}
