use http::{HeaderMap, Method, Response, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::mode::CallerIdentity;
use crate::routes::{ApiError, SseOrBytesBody, json_response, read_json_body, sse_response};
use crate::settings::DesktopSettingsPatch;
use crate::state::ServerState;

pub async fn handle_app_route(
    state: &ServerState,
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
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(updated).unwrap_or(Value::Null),
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
