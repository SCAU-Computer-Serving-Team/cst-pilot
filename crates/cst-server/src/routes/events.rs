use http::{HeaderMap, Method, Response, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::mode::CallerIdentity;
use crate::routes::{
    ApiError, SseOrBytesBody, json_response, parse_query_params, read_json_body, sse_response,
};
use crate::session::attachments::{is_valid_client_id_key, is_valid_session_id_key};
use crate::session::events::MultiplexSubscriptionUpdate;
use crate::state::ServerState;

#[allow(clippy::too_many_arguments)]
pub async fn handle_events_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
    conn_cancel: CancellationToken,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
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
            .filter(|id| is_valid_client_id_key(id))
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
        if !is_valid_client_id_key(client_id) {
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
                        .map_err(|(missing_sid, err)| match (missing_sid, err) {
                            (Some(sid), crate::session::SessionPoolError::NotFound(msg)) => {
                                ApiError::new(StatusCode::NOT_FOUND, "session_not_found", msg)
                                    .with_session_id(sid)
                            }
                            (_, other) => ApiError::from(other),
                        })?;
                    Ok(json!({ "subscribedSessions": subscribed }))
                },
            )
            .await?;
        return Ok(Some(json_response(StatusCode::OK, &result, method)));
    }

    let _ = Value::Null;
    Ok(None)
}
