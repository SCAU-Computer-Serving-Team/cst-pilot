use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

use crate::exports::{EXPORT_CSP, build_export_preview_snapshot, parse_export_options};
use crate::mode::CallerIdentity;
use crate::routes::{
    ApiError, SseOrBytesBody, bytes_response, json_response, parse_query_params, read_json_body,
};
use crate::session::attachments::is_valid_session_id_key;
use crate::state::ServerState;

pub async fn handle_exports_route(
    state: &ServerState,
    caller: CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if let Some(rest) = pathname.strip_prefix("/api/exports/") {
        let parts: Vec<&str> = rest.split('/').collect();
        let export_id = parts[0];
        if export_id.is_empty() {
            return Ok(None);
        }
        if parts.len() == 1
            && export_id == "recent"
            && (*method == Method::GET || *method == Method::HEAD)
        {
            let params = parse_query_params(query);
            let limit = match params.get("limit") {
                Some(raw) => match raw.trim().parse::<usize>() {
                    Ok(v) if v >= 1 => v.min(50),
                    _ => {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "limit 参数无效",
                        ));
                    }
                },
                None => 20,
            };
            let list = state
                .exports
                .list_recent(limit)
                .await
                .map_err(ApiError::from)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &json!({ "exports": list }),
                method,
            )));
        }
        if parts.len() == 1 && (*method == Method::GET || *method == Method::HEAD) {
            let job = state.exports.get(export_id).await.map_err(ApiError::from)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &serde_json::to_value(job).unwrap_or(Value::Null),
                method,
            )));
        }
        if parts.len() == 2 && parts[1] == "open" && *method == Method::POST {
            let body = read_json_body(raw_body, 4096)?;
            if !body.as_object().is_some_and(|o| o.is_empty()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "请求只需导出 ID，不接受目录路径",
                ));
            }
            let eid = export_id.to_owned();
            let result = state
                .write_once(
                    caller,
                    method.as_str(),
                    pathname,
                    &body,
                    Some(headers),
                    || async move {
                        let target = state
                            .exports
                            .open_target(&eid)
                            .await
                            .map_err(ApiError::from)?;
                        (state.open_export_directory)(target).await.map_err(|_| {
                            ApiError::new(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "export_launch_failed",
                                "无法启动文件管理器，请重试或按显示路径手动打开",
                            )
                        })?;
                        Ok(json!({ "launched": true }))
                    },
                )
                .await?;
            return Ok(Some(json_response(StatusCode::OK, &result, method)));
        }
        if parts.len() == 3
            && parts[1] == "files"
            && (*method == Method::GET || *method == Method::HEAD)
        {
            let index: usize = parts[2].parse().map_err(|_| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "export_file_missing",
                    "导出文件不存在",
                )
            })?;
            let file = state
                .exports
                .file(export_id, index)
                .await
                .map_err(ApiError::from)?;
            let len = file.data.len();
            let body_bytes = if *method == Method::HEAD {
                Bytes::new()
            } else {
                Bytes::from(file.data)
            };
            let mut resp = bytes_response(
                StatusCode::OK,
                file.content_type,
                "no-store",
                len,
                body_bytes,
            );
            let encoded_name = utf8_percent_encode(&file.name, NON_ALPHANUMERIC);
            if let Ok(cd) =
                HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{encoded_name}"))
            {
                resp.headers_mut()
                    .insert(http::header::CONTENT_DISPOSITION, cd);
            }
            resp.headers_mut().insert(
                "content-security-policy",
                HeaderValue::from_static(EXPORT_CSP),
            );
            return Ok(Some(resp));
        }
        return Ok(None);
    }

    if let Some(rest) = pathname.strip_prefix("/api/sessions/")
        && let Some((id, suffix)) = rest.split_once('/')
        && matches!(suffix, "exports" | "exports/preview")
    {
        if !is_valid_session_id_key(id) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求的会话ID无效",
            ));
        }
        if *method == Method::POST {
            let is_preview = suffix == "exports/preview";
            let body = read_json_body(raw_body, 128 * 1024)?;
            let options = parse_export_options(&body)?;
            let id_owned = id.to_owned();
            if is_preview {
                let slot = state
                    .pool
                    .open_saved(&id_owned)
                    .await
                    .map_err(ApiError::from)?;
                let title = slot.display_title();
                let snap = build_export_preview_snapshot(&slot, &title, &options)?;
                return Ok(Some(json_response(StatusCode::OK, &snap, method)));
            }
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
                        let title = slot.display_title();
                        let snap = build_export_preview_snapshot(&slot, &title, &options)?;
                        let job = state.exports.create(&snap).await.map_err(ApiError::from)?;
                        Ok(serde_json::to_value(job).unwrap_or(Value::Null))
                    },
                )
                .await?;
            return Ok(Some(json_response(StatusCode::ACCEPTED, &result, method)));
        }

        if suffix == "exports" && (*method == Method::GET || *method == Method::HEAD) {
            let list = state.exports.list(id).await.map_err(ApiError::from)?;
            return Ok(Some(json_response(
                StatusCode::OK,
                &json!({ "exports": list }),
                method,
            )));
        }
    }

    Ok(None)
}
