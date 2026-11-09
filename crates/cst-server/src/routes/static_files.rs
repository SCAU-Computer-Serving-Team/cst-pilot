use std::fs;
use std::io;
use std::path::Path;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use regex::Regex;
use std::sync::OnceLock;

use crate::auth_guard::{WEB_THEME_COOKIE_NAME, extract_cookie, format_web_session_set_cookie};
use crate::discovery::strip_windows_verbatim_prefix;
use crate::mode::ModeController;
use crate::routes::{ApiError, SseOrBytesBody, bytes_response, error_response};

fn content_type_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "otf" => "font/otf",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn is_hashed_asset_path(pathname: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^/assets/[^/]+-[A-Za-z0-9_-]{8,}\.").unwrap());
    re.is_match(pathname)
}

fn theme_from_headers(headers: &HeaderMap) -> &'static str {
    match extract_cookie(headers, WEB_THEME_COOKIE_NAME).as_deref() {
        Some("dark") => "dark",
        Some("system") => "system",
        _ => "light",
    }
}

pub async fn serve_static_or_spa(
    method: &Method,
    pathname: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    static_dir: &Path,
    mode_controller: &ModeController,
) -> Response<SseOrBytesBody> {
    if *method != Method::GET && *method != Method::HEAD {
        let mut resp = error_response(&ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "当前操作暂不可用。",
        ));
        resp.headers_mut()
            .insert(http::header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return resp;
    }

    // Redeem single-use `?ticket=<once>` on `/` if present
    let mut set_cookie_header: Option<String> = None;
    if pathname == "/"
        && let Some(q) = query
    {
        for (key, val) in url::form_urlencoded::parse(q.as_bytes()) {
            if key == "ticket" && !val.is_empty() {
                if let Some(cookie_val) = mode_controller.redeem_ticket(&val).await {
                    let mut response = bytes_response(
                        StatusCode::SEE_OTHER,
                        "text/plain; charset=utf-8",
                        "no-store",
                        0,
                        Bytes::new(),
                    );
                    response
                        .headers_mut()
                        .insert(http::header::LOCATION, HeaderValue::from_static("/"));
                    response.headers_mut().insert(
                        http::header::SET_COOKIE,
                        HeaderValue::from_str(&format_web_session_set_cookie(&cookie_val))
                            .expect("signed session cookie contains only header-safe characters"),
                    );
                    return response;
                } else {
                    return error_response(&ApiError::new(
                        StatusCode::FORBIDDEN,
                        "web_mode_inactive",
                        "Web 启动票据无效或已过期，请从桌面端重新打开 Web 界面",
                    ));
                }
            }
        }
    } else if pathname == "/" && mode_controller.is_standalone_web() {
        let cookie_val = mode_controller.issue_cookie_for_current_epoch().await;
        set_cookie_header = Some(format_web_session_set_cookie(&cookie_val));
    }

    if pathname.contains('\\')
        || pathname.contains('\0')
        || pathname
            .split('/')
            .any(|part| !part.is_empty() && part.starts_with('.'))
    {
        return error_response(&ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "页面不存在。",
        ));
    }

    let Ok(canonical_root) =
        fs::canonicalize(static_dir).map(|p| strip_windows_verbatim_prefix(&p))
    else {
        return error_response(&ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "static_missing",
            "页面文件尚未构建。请先构建 Web 前端。",
        ));
    };

    let rel_trimmed = pathname.trim_start_matches('/');
    let candidate = if rel_trimmed.is_empty() {
        canonical_root.join("index.html")
    } else {
        canonical_root.join(rel_trimmed)
    };

    let reserved_asset = pathname == "/assets"
        || pathname.starts_with("/assets/")
        || pathname == "/fonts"
        || pathname.starts_with("/fonts/");
    let has_ext = Path::new(pathname).extension().is_some();
    let accepts_html = headers
        .get(http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"));

    let resolved_file = match fs::canonicalize(&candidate) {
        Ok(real) => {
            let clean = strip_windows_verbatim_prefix(&real);
            if !clean.starts_with(&canonical_root) {
                return error_response(&ApiError::new(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "页面不存在。",
                ));
            }
            if clean.is_file() { Some(clean) } else { None }
        }
        Err(_) => None,
    };

    let target_file = match resolved_file {
        Some(f) => f,
        None if pathname != "/" && !reserved_asset && !has_ext && accepts_html => {
            let index = canonical_root.join("index.html");
            match fs::canonicalize(&index) {
                Ok(real) => {
                    let clean = strip_windows_verbatim_prefix(&real);
                    if clean.starts_with(&canonical_root) && clean.is_file() {
                        clean
                    } else {
                        return error_response(&ApiError::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "static_missing",
                            "页面文件尚未构建。请先构建 Web 前端。",
                        ));
                    }
                }
                Err(_) => {
                    return error_response(&ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "static_missing",
                        "页面文件尚未构建。请先构建 Web 前端。",
                    ));
                }
            }
        }
        None if pathname == "/" => {
            return error_response(&ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "static_missing",
                "页面文件尚未构建。请先构建 Web 前端。",
            ));
        }
        None => {
            return error_response(&ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "页面不存在。",
            ));
        }
    };

    let ext = target_file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let is_html = ext.eq_ignore_ascii_case("html");
    let raw_bytes = match fs::read(&target_file) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return error_response(&ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "static_missing",
                "页面文件尚未构建。请先构建 Web 前端。",
            ));
        }
        Err(_) => {
            return error_response(&ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "页面暂时无法打开。",
            ));
        }
    };

    let final_bytes = if is_html {
        let theme = theme_from_headers(headers);
        let text = String::from_utf8_lossy(&raw_bytes);
        let replaced = text.replacen("<html", &format!("<html data-theme=\"{theme}\""), 1);
        Bytes::from(replaced.into_bytes())
    } else {
        Bytes::from(raw_bytes)
    };

    let cache_control = if is_html {
        "no-cache"
    } else if is_hashed_asset_path(pathname) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let content_type = content_type_for_extension(ext);
    let body_bytes = if *method == Method::HEAD {
        Bytes::new()
    } else {
        final_bytes.clone()
    };

    let mut resp = bytes_response(
        StatusCode::OK,
        content_type,
        cache_control,
        final_bytes.len(),
        body_bytes,
    );
    if let Some(cookie) = set_cookie_header
        && let Ok(val) = HeaderValue::from_str(&cookie)
    {
        resp.headers_mut().insert(http::header::SET_COOKIE, val);
    }
    resp
}
