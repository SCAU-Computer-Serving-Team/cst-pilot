use http::{Method, Response, StatusCode};

use crate::routes::{ApiError, SseOrBytesBody, json_response};
use crate::session::attachments::is_valid_session_id_key;
use crate::state::ServerState;

/// Handles read-only session usage endpoints (`GET /api/sessions/:id/usage`).
pub async fn handle_usage_route(
    state: &ServerState,
    method: &Method,
    pathname: &str,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if *method != Method::GET && *method != Method::HEAD {
        return Ok(None);
    }
    let Some(rest) = pathname.strip_prefix("/api/sessions/") else {
        return Ok(None);
    };
    let Some((id, suffix)) = rest.split_once('/') else {
        return Ok(None);
    };
    if suffix != "usage" {
        return Ok(None);
    }
    if !is_valid_session_id_key(id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "请求的会话 ID 无效",
        ));
    }

    let payload = state
        .pool
        .read_session_usage(id)
        .await
        .map_err(ApiError::from)?;
    let body = serde_json::to_value(payload).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "会话用量序列化失败",
        )
    })?;
    Ok(Some(json_response(StatusCode::OK, &body, method)))
}
