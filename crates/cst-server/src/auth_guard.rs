use http::{HeaderMap, Method, StatusCode};
use serde::Serialize;
use subtle::ConstantTimeEq;

use crate::mode::{CallerIdentity, ChannelMode, ModeController, ModeRejection};

pub const WEB_SESSION_COOKIE_NAME: &str = "cst-web-session";
pub const WEB_THEME_COOKIE_NAME: &str = "cst-theme";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiErrorBody {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthGuardError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl AuthGuardError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn to_body(&self) -> ApiErrorBody {
        ApiErrorBody {
            error: ApiErrorDetail {
                code: self.code.to_owned(),
                message: self.message.clone(),
            },
        }
    }
}

impl From<ModeRejection> for AuthGuardError {
    fn from(rejection: ModeRejection) -> Self {
        let status = StatusCode::from_u16(rejection.status_code()).unwrap_or(StatusCode::CONFLICT);
        Self::new(status, rejection.code(), rejection.message())
    }
}

/// Validates loopback `Host` (`127.0.0.1:<port>`) and same-origin `Origin` (`http://127.0.0.1:<port>`).
pub fn validate_host_and_origin(
    method: &Method,
    headers: &HeaderMap,
    expected_port: u16,
) -> Result<(), AuthGuardError> {
    if *method == Method::OPTIONS {
        return Err(AuthGuardError::new(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "拒绝 CORS 预检请求",
        ));
    }

    let expected_host = format!("127.0.0.1:{expected_port}");
    let host = headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if host != expected_host {
        return Err(AuthGuardError::new(
            StatusCode::FORBIDDEN,
            "invalid_host",
            "仅允许通过本机 127.0.0.1 端口访问服务",
        ));
    }

    let expected_origin = format!("http://127.0.0.1:{expected_port}");
    if let Some(origin_val) = headers.get(http::header::ORIGIN) {
        let origin = origin_val.to_str().unwrap_or("");
        if origin != expected_origin {
            return Err(AuthGuardError::new(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "拒绝跨站来源请求",
            ));
        }
    }

    Ok(())
}

pub fn verify_bearer_token(headers: &HeaderMap, expected_app_token: &str) -> bool {
    let Some(raw) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(token) = raw.strip_prefix("Bearer ") else {
        return false;
    };
    if token.len() != expected_app_token.len() {
        return false;
    }
    bool::from(token.as_bytes().ct_eq(expected_app_token.as_bytes()))
}

pub fn is_app_client_header(headers: &HeaderMap) -> bool {
    headers
        .get("x-cst-client")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("app"))
}

pub fn has_web_request_header(headers: &HeaderMap) -> bool {
    headers
        .get("x-cst-web-request")
        .and_then(|v| v.to_str().ok())
        == Some("1")
}

pub fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(http::header::COOKIE) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        for pair in raw.split(';') {
            let trimmed = pair.trim();
            if let Some((k, v)) = trimmed.split_once('=')
                && k.trim() == name
            {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

pub fn format_web_session_set_cookie(cookie_value: &str) -> String {
    format!("{WEB_SESSION_COOKIE_NAME}={cookie_value}; Path=/; HttpOnly; SameSite=Strict")
}

/// Resolves caller identity and enforces entry-level channel mode rules for `/api/*` and `/api/app/*`.
pub async fn authenticate_api_request(
    method: &Method,
    pathname: &str,
    headers: &HeaderMap,
    expected_port: u16,
    app_token: &str,
    mode_controller: &ModeController,
) -> Result<CallerIdentity, AuthGuardError> {
    validate_host_and_origin(method, headers, expected_port)?;

    let is_app_route = pathname == "/api/app" || pathname.starts_with("/api/app/");
    let is_app_caller = is_app_route
        || is_app_client_header(headers)
        || headers.contains_key(http::header::AUTHORIZATION);

    if is_app_caller {
        if !verify_bearer_token(headers, app_token) {
            return Err(AuthGuardError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "软件端访问令牌无效",
            ));
        }
        if is_app_route {
            return Ok(CallerIdentity::App);
        }
        let is_readonly_method = *method == Method::GET || *method == Method::HEAD;
        let allow_in_web = is_readonly_method && is_app_readonly_allowed_in_web_mode(pathname);
        mode_controller
            .verify_caller_access(CallerIdentity::App, !is_readonly_method, allow_in_web)
            .await?;
        return Ok(CallerIdentity::App);
    }

    // Browser / Web caller
    let is_readonly_method = *method == Method::GET || *method == Method::HEAD;
    if !is_readonly_method {
        let expected_origin = format!("http://127.0.0.1:{expected_port}");
        let origin_ok = headers
            .get(http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            == Some(expected_origin.as_str());
        if !origin_ok || !has_web_request_header(headers) {
            return Err(AuthGuardError::new(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "Web 写请求必须携带同源 Origin 与 X-CST-Web-Request: 1",
            ));
        }
    }

    let state = mode_controller.mode_snapshot().await;
    if is_readonly_method && (pathname == "/api/health" || pathname == "/api/state") {
        // `/api/health` and `/api/state` are accessible to browser even in `AppActive` (returns `stage: "app_active"`).
        return Ok(CallerIdentity::Web {
            cookie_epoch: state.web_cookie_epoch,
        });
    }

    if state.mode != ChannelMode::WebActive {
        return Err(AuthGuardError::from(ModeRejection::WebModeInactive));
    }

    let cookie_epoch = if mode_controller.is_standalone_web() {
        match extract_cookie(headers, WEB_SESSION_COOKIE_NAME)
            .and_then(|raw| mode_controller.verify_cookie(&raw))
        {
            Some(epoch) => epoch,
            None => state.web_cookie_epoch,
        }
    } else {
        let raw = extract_cookie(headers, WEB_SESSION_COOKIE_NAME)
            .ok_or_else(|| AuthGuardError::from(ModeRejection::WebModeInactive))?;
        mode_controller
            .verify_cookie(&raw)
            .ok_or_else(|| AuthGuardError::from(ModeRejection::WebModeInactive))?
    };

    let caller = CallerIdentity::Web { cookie_epoch };
    mode_controller
        .verify_caller_access(caller, !is_readonly_method, false)
        .await?;
    Ok(caller)
}

fn is_app_readonly_allowed_in_web_mode(pathname: &str) -> bool {
    matches!(
        pathname,
        "/api/events"
            | "/api/models"
            | "/api/auth"
            | "/api/settings"
            | "/api/account"
            | "/api/quota"
            | "/api/health"
            | "/api/state"
            | "/api/lifecycle"
    ) || pathname.starts_with("/api/auth/custom/")
        || (pathname.starts_with("/api/auth/") && pathname.ends_with("/status"))
}
