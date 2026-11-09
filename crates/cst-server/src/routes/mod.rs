pub mod app;
pub mod auth;
pub mod lifecycle;
pub mod models;
pub mod sessions;
pub mod static_files;

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body::{Body, Frame};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth_guard::{AuthGuardError, authenticate_api_request, validate_host_and_origin};
use crate::error::{MutationError, ServiceError, ServiceErrorKind};
use crate::locks::StorageError;
use crate::mode::ModeRejection;
use crate::session::SessionPoolError;
use crate::session::attachments::AttachmentError;
use crate::session::events::{SseStreamBody, browser_json_string};
use crate::session::inbox::InboxError;
use crate::session::interactions::BitLockerRespondError;
use crate::session::receipts::ReceiptError;
use crate::session::slot::CompactSessionError;
use crate::settings::SettingsError;
use crate::state::ServerState;

pub enum SseOrBytesBody {
    Bytes(Full<Bytes>),
    Sse(SseStreamBody),
}

impl Body for SseOrBytesBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            Self::Bytes(full) => Pin::new(full).poll_frame(cx),
            Self::Sse(sse) => Pin::new(sse).poll_frame(cx),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn from_config_err(msg: String) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "configuration_unavailable",
            msg,
        )
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        let status = match error.kind {
            ServiceErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
            ServiceErrorKind::Conflict => StatusCode::CONFLICT,
            ServiceErrorKind::NotFound => StatusCode::NOT_FOUND,
            ServiceErrorKind::AccessDenied => StatusCode::FORBIDDEN,
            ServiceErrorKind::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ServiceErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ServiceErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, error.code, error.message)
    }
}

impl From<MutationError<ApiError>> for ApiError {
    fn from(error: MutationError<ApiError>) -> Self {
        match error {
            MutationError::InvalidKey => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "缺少有效的去重键",
            ),
            MutationError::Admission(error) => error.into(),
            MutationError::Receipt(error) => error.into(),
            MutationError::Action(error) => error,
        }
    }
}

impl From<AuthGuardError> for ApiError {
    fn from(err: AuthGuardError) -> Self {
        Self {
            status: err.status,
            code: err.code,
            message: err.message,
        }
    }
}

impl From<ModeRejection> for ApiError {
    fn from(rej: ModeRejection) -> Self {
        let status = StatusCode::from_u16(rej.status_code()).unwrap_or(StatusCode::CONFLICT);
        Self::new(status, rej.code(), rej.message())
    }
}

impl From<StorageError> for ApiError {
    fn from(err: StorageError) -> Self {
        let status =
            StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Self::new(status, err.code(), err.message)
    }
}

impl From<SessionPoolError> for ApiError {
    fn from(err: SessionPoolError) -> Self {
        let status = StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::BAD_REQUEST);
        Self::new(status, err.code(), err.message().to_owned())
    }
}

impl From<InboxError> for ApiError {
    fn from(err: InboxError) -> Self {
        match err {
            InboxError::InvalidInput(m) => Self::new(StatusCode::BAD_REQUEST, "invalid_request", m),
            InboxError::Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            InboxError::Corrupted(m) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_unavailable",
                m,
            ),
            InboxError::Storage(s) => Self::from(s),
        }
    }
}

impl From<AttachmentError> for ApiError {
    fn from(err: AttachmentError) -> Self {
        match err {
            AttachmentError::InvalidInput(m) => {
                Self::new(StatusCode::BAD_REQUEST, "invalid_request", m)
            }
            AttachmentError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, "not_found", "图片不存在")
            }
            AttachmentError::Storage(s) => Self::from(s),
        }
    }
}

impl From<ReceiptError> for ApiError {
    fn from(err: ReceiptError) -> Self {
        match err {
            ReceiptError::InvalidKey(m) => Self::new(StatusCode::BAD_REQUEST, "invalid_request", m),
            ReceiptError::Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            ReceiptError::Corrupted(m) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_unavailable",
                m,
            ),
            ReceiptError::Storage(s) => Self::from(s),
            ReceiptError::Action(m) => Self::new(StatusCode::BAD_REQUEST, "invalid_request", m),
        }
    }
}

impl From<SettingsError> for ApiError {
    fn from(err: SettingsError) -> Self {
        let status = StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::BAD_REQUEST);
        Self::new(status, err.code(), err.message().to_owned())
    }
}

impl From<CompactSessionError> for ApiError {
    fn from(err: CompactSessionError) -> Self {
        let status = StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::CONFLICT);
        Self::new(status, err.code(), err.message().to_owned())
    }
}

impl From<BitLockerRespondError> for ApiError {
    fn from(err: BitLockerRespondError) -> Self {
        let status = StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::BAD_REQUEST);
        Self::new(status, err.code(), err.message())
    }
}

pub fn json_response(
    status: StatusCode,
    value: &Value,
    method: &Method,
) -> Response<SseOrBytesBody> {
    let serialized = browser_json_string(value);
    let bytes = Bytes::from(serialized.into_bytes());
    let len = bytes.len();
    let body_bytes = if *method == Method::HEAD {
        Bytes::new()
    } else {
        bytes
    };
    bytes_response(
        status,
        "application/json; charset=utf-8",
        "no-store",
        len,
        body_bytes,
    )
}

pub fn error_response(err: &ApiError) -> Response<SseOrBytesBody> {
    let body = json!({
        "error": {
            "code": err.code,
            "message": err.message,
        }
    });
    json_response(err.status, &body, &Method::GET)
}

pub fn bytes_response(
    status: StatusCode,
    content_type: &str,
    cache_control: &str,
    content_length: usize,
    body: Bytes,
) -> Response<SseOrBytesBody> {
    let mut resp = Response::new(SseOrBytesBody::Bytes(Full::new(body)));
    *resp.status_mut() = status;
    let headers = resp.headers_mut();
    if let Ok(ct) = HeaderValue::from_str(content_type) {
        headers.insert(http::header::CONTENT_TYPE, ct);
    }
    if let Ok(cc) = HeaderValue::from_str(cache_control) {
        headers.insert(http::header::CACHE_CONTROL, cc);
    }
    if let Ok(cl) = HeaderValue::from_str(&content_length.to_string()) {
        headers.insert(http::header::CONTENT_LENGTH, cl);
    }
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(http::header::CONNECTION, HeaderValue::from_static("close"));
    resp
}

pub fn sse_response(sse_body: SseStreamBody) -> Response<SseOrBytesBody> {
    let mut resp = Response::new(SseOrBytesBody::Sse(sse_body));
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("keep-alive"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    resp
}

pub fn read_json_body(raw_body: &[u8], max_bytes: usize) -> Result<Value, ApiError> {
    if raw_body.len() > max_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "请求体超出允许体积上限",
        ));
    }
    if raw_body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(raw_body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", "请求须为有效 JSON"))
}

pub fn parse_query_params(query: Option<&str>) -> HashMap<String, String> {
    let Some(q) = query else {
        return HashMap::new();
    };
    url::form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

async fn collect_request_body(mut body: Incoming, max_bytes: usize) -> Result<Bytes, ApiError> {
    let mut buf = Vec::new();
    while let Some(frame_res) = body.frame().await {
        let frame = frame_res.map_err(|_| {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", "请求体读取失败")
        })?;
        if let Ok(data) = frame.into_data() {
            if buf.len().saturating_add(data.len()) > max_bytes {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_request",
                    "请求体超出允许体积上限",
                ));
            }
            buf.extend_from_slice(&data);
        }
    }
    Ok(Bytes::from(buf))
}

pub async fn handle_http_request(
    state: Arc<ServerState>,
    req: Request<Incoming>,
    conn_cancel: CancellationToken,
) -> Result<Response<SseOrBytesBody>, Infallible> {
    let method = req.method().clone();
    let raw_path = req.uri().path().to_owned();
    let query = req.uri().query().map(str::to_owned);
    let headers: HeaderMap = req.headers().clone();

    let port = state.port();
    if let Err(err) = validate_host_and_origin(&method, &headers, port) {
        return Ok(error_response(&ApiError::from(err)));
    }

    let Ok(decoded_cow) = percent_decode_str(&raw_path).decode_utf8() else {
        return Ok(error_response(&ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "页面地址无效。",
        )));
    };
    let pathname = decoded_cow.into_owned();

    if pathname != "/api" && !pathname.starts_with("/api/") {
        return Ok(static_files::serve_static_or_spa(
            &method,
            &pathname,
            query.as_deref(),
            &headers,
            state.static_dir(),
            &state.mode,
        )
        .await);
    }

    let caller = match authenticate_api_request(
        &method,
        &pathname,
        &headers,
        port,
        state.app_token(),
        &state.mode,
    )
    .await
    {
        Ok(c) => c,
        Err(err) => return Ok(error_response(&ApiError::from(err))),
    };

    let is_messages_post = method == Method::POST
        && pathname.starts_with("/api/sessions/")
        && pathname.ends_with("/messages");
    let max_body_limit = if is_messages_post {
        17 * 1024 * 1024
    } else {
        128 * 1024
    };

    let raw_body = match collect_request_body(req.into_body(), max_body_limit).await {
        Ok(b) => b,
        Err(err) => return Ok(error_response(&err)),
    };

    let route_res = Box::pin(dispatch_api_route(
        &state,
        caller,
        &method,
        &pathname,
        query.as_deref(),
        &headers,
        &raw_body,
        conn_cancel,
    ))
    .await;

    match route_res {
        Ok(Some(resp)) => Ok(resp),
        Ok(None) => Ok(error_response(&ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "接口不存在",
        ))),
        Err(err) => Ok(error_response(&err)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_api_route(
    state: &Arc<ServerState>,
    caller: crate::mode::CallerIdentity,
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    raw_body: &[u8],
    conn_cancel: CancellationToken,
) -> Result<Option<Response<SseOrBytesBody>>, ApiError> {
    if pathname == "/api/app" || pathname.starts_with("/api/app/") {
        return Box::pin(app::handle_app_route(
            state,
            method,
            pathname,
            headers,
            raw_body,
            conn_cancel,
        ))
        .await;
    }

    if let Some(resp) = Box::pin(lifecycle::handle_lifecycle_route(
        state,
        caller,
        method,
        pathname,
        query,
        headers,
        raw_body,
        conn_cancel.clone(),
    ))
    .await?
    {
        return Ok(Some(resp));
    }

    if let Some(resp) = Box::pin(models::handle_models_route(
        state, caller, method, pathname, query, headers, raw_body,
    ))
    .await?
    {
        return Ok(Some(resp));
    }

    if let Some(resp) = Box::pin(auth::handle_auth_route(
        state,
        caller,
        method,
        pathname,
        query,
        headers,
        raw_body,
        conn_cancel.clone(),
    ))
    .await?
    {
        return Ok(Some(resp));
    }

    if let Some(resp) = Box::pin(sessions::handle_sessions_and_exports_route(
        state,
        caller,
        method,
        pathname,
        headers,
        raw_body,
        conn_cancel,
    ))
    .await?
    {
        return Ok(Some(resp));
    }

    Ok(None)
}
