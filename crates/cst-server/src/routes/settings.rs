use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use serde_json::{Value, json};

use crate::auth_guard::WEB_THEME_COOKIE_NAME;
use crate::mode::CallerIdentity;
use crate::routes::{ApiError, SseOrBytesBody, json_response, parse_query_params, read_json_body};
use crate::session::attachments::is_valid_session_id_key;
use crate::settings::ThemeMode;
use crate::state::ServerState;

pub async fn handle_settings_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
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
                        state.pool.global_events.publish(
                            "state",
                            &json!({
                                "type": "settings_changed",
                                "scope": "web",
                            }),
                        );
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

    if pathname == "/api/ui-hints" && (*method == Method::GET || *method == Method::HEAD) {
        let hints = state.ui_hints.read().await;
        return Ok(Some(json_response(
            StatusCode::OK,
            &serde_json::to_value(hints).unwrap_or(Value::Null),
            method,
        )));
    }

    if pathname == "/api/ui-hints/missing-key-prompt/claim" && *method == Method::POST {
        let body = read_json_body(raw_body, 128 * 1024)?;
        let Some(obj) = body.as_object() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的界面提示参数无效",
            ));
        };
        if obj.keys().any(|k| k != "clientLastPromptedAtMs") {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的界面提示参数无效",
            ));
        }
        let client_last = match obj.get("clientLastPromptedAtMs") {
            None => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求的界面提示时间戳无效",
                )
            })?),
        };
        let (_web_admission, _app_lease) =
            match caller {
                CallerIdentity::Web { .. } => (
                    Some(
                        state
                            .mode
                            .admit_write(caller, Some(state.host.kit().activity_gate()))
                            .await
                            .map_err(ApiError::from)?,
                    ),
                    None,
                ),
                CallerIdentity::App => {
                    if state.mode.is_stopping() {
                        return Err(ApiError::from(crate::mode::ModeRejection::Stopping));
                    }
                    let lease =
                        state.host.kit().activity_gate().enter().map_err(|msg| {
                            ApiError::from(crate::mode::ModeRejection::Updating(msg))
                        })?;
                    (None, Some(lease))
                }
            };
        let claimed = state
            .ui_hints
            .claim_missing_key_prompt(client_last, pi_ai::now_ms())
            .await
            .map_err(ApiError::from)?;
        return Ok(Some(json_response(
            StatusCode::OK,
            &serde_json::to_value(claimed).unwrap_or(Value::Null),
            method,
        )));
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
