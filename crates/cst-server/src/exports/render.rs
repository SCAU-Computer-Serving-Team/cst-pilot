use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine;
use chromiumoxide::cdp::browser_protocol::fetch::{
    self, ContinueRequestParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotReturns;
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use pi_agent::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::{ServiceError, ServiceErrorKind};
use crate::exports::browser::{ExportBrowser, ExportBrowserOptions};
use crate::exports::html::{ExportDocumentSource, OfflineHtml};
use crate::exports::pagination::export_pages;

pub type ExportEmitFn<'a> =
    &'a mut (dyn FnMut(&'static str, Vec<u8>) -> BoxFuture<'a, Result<(), ServiceError>> + Send);

pub trait ExportRenderer: Send + Sync {
    fn render<'a>(
        &'a self,
        snapshot: &'a Value,
        emit: ExportEmitFn<'a>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<(), ServiceError>>;

    fn close(&self) -> BoxFuture<'_, ()>;

    fn set_origin(&self, _origin: &str) {}
}

pub const MEASURE_EXPORT_DOCUMENT_JS: &str = r#"() => {
    const source = document.querySelector(".export-document");
    if (!source) throw new Error("导出正文为空");
    const height = Math.ceil(source.getBoundingClientRect().bottom + scrollY);
    const cuts = [...source.querySelectorAll(".export-message, .markdown > *, tr, .export-tool")].map((node) =>
        Math.ceil(node.getBoundingClientRect().bottom + scrollY + 12),
    );
    for (const node of source.querySelectorAll("pre")) {
        const rect = node.getBoundingClientRect(),
            style = getComputedStyle(node);
        const line = Number.parseFloat(style.lineHeight);
        if (line > 0)
            for (
                let y = rect.top + scrollY + Number.parseFloat(style.paddingTop) + line;
                y < rect.bottom + scrollY;
                y += line
            )
                cuts.push(Math.round(y));
    }
    return { height, cuts };
}"#;

pub const CAPTURE_EXPORT_DOCUMENT_JS: &str = r#"() => {
    const source = document.querySelector(".export-document");
    if (!source) throw new Error("导出正文为空");
    const normalizeWeight = (weight) => (weight === "normal" ? "400" : weight === "bold" ? "700" : weight);
    const key = (family, weight, style) =>
        `${family.replace(/["']/g, "").trim().toLowerCase()}|${normalizeWeight(weight)}|${style}`;
    const loaded = new Set(
        [...document.fonts]
            .filter((font) => font.status === "loaded")
            .map((font) => key(font.family, font.weight, font.style)),
    );
    function serialize(rules) {
        return [...rules]
            .map((rule) => {
                if (
                    rule instanceof CSSFontFaceRule &&
                    !loaded.has(
                        key(
                            rule.style.getPropertyValue("font-family"),
                            rule.style.getPropertyValue("font-weight") || "normal",
                            rule.style.getPropertyValue("font-style") || "normal",
                        ),
                    )
                )
                    return "";
                const nested = Reflect.get(rule, "cssRules");
                if (nested && rule.type !== CSSRule.STYLE_RULE)
                    return `${rule.cssText.slice(0, rule.cssText.indexOf("{") + 1)}${serialize(nested)}}`;
                return rule.cssText;
            })
            .join("\n");
    }
    const styles = [...document.styleSheets].map((sheet) => ({
        css: serialize(sheet.cssRules),
        base: sheet.href ?? location.href,
    }));
    const clone = source.cloneNode(true);
    for (const element of clone.querySelectorAll(
        "script, button, input, select, textarea, .code-copy-feedback, .special-tool-delivery",
    ))
        element.remove();
    for (const element of [clone, ...clone.querySelectorAll("*")]) {
        for (const attribute of [...element.attributes])
            if (attribute.name.toLowerCase().startsWith("on")) element.removeAttribute(attribute.name);
    }
    return { body: clone.outerHTML, styles };
}"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotClip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// Custom CDP `Page.captureScreenshot` command supporting Chromium's `optimizeForSpeed` parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureScreenshotFastParams {
    pub format: String,
    pub capture_beyond_viewport: bool,
    pub clip: ScreenshotClip,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optimize_for_speed: Option<bool>,
}

impl chromiumoxide::Method for CaptureScreenshotFastParams {
    fn identifier(&self) -> chromiumoxide::types::MethodId {
        "Page.captureScreenshot".into()
    }
}

impl chromiumoxide::Command for CaptureScreenshotFastParams {
    type Response = CaptureScreenshotReturns;
}

#[derive(Debug, Deserialize)]
struct MeasuredGeometry {
    height: i64,
    cuts: Vec<f64>,
}

pub struct ChromiumoxideRenderer {
    origin: std::sync::Mutex<String>,
    browser: ExportBrowser,
    html: OfflineHtml,
    fast_encoding: AtomicBool,
}

impl ChromiumoxideRenderer {
    pub fn new(origin: &str, browser_options: ExportBrowserOptions) -> Result<Self, ServiceError> {
        let parsed = Url::parse(origin).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "导出源地址无效",
            )
        })?;
        let clean_origin = parsed.origin().ascii_serialization();
        let html = OfflineHtml::new(&clean_origin)?;
        let browser = ExportBrowser::new(browser_options);
        Ok(Self {
            origin: std::sync::Mutex::new(clean_origin),
            browser,
            html,
            fast_encoding: AtomicBool::new(true),
        })
    }

    pub fn set_fast_encoding(&self, enabled: bool) {
        self.fast_encoding.store(enabled, Ordering::SeqCst);
    }

    pub fn fast_encoding(&self) -> bool {
        self.fast_encoding.load(Ordering::SeqCst)
    }

    async fn wait_for_dataset_state(
        page: &Page,
        expr: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ServiceError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if cancel.is_cancelled() {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_interrupted",
                    "导出已取消",
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ServiceError::new(
                    ServiceErrorKind::Timeout,
                    "export_timeout",
                    "导出页面加载超时，请重试",
                ));
            }
            let val: String = page
                .evaluate(expr)
                .await
                .ok()
                .and_then(|r| r.into_value::<String>().ok())
                .unwrap_or_default();
            if !val.is_empty() {
                return Ok(val);
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    async fn capture_clip(
        &self,
        page: &Page,
        top: u32,
        height: u32,
    ) -> Result<Vec<u8>, ServiceError> {
        let clip = ScreenshotClip {
            x: 0.0,
            y: f64::from(top),
            width: 1240.0,
            height: f64::from(height),
            scale: 1.0,
        };
        let try_fast = self.fast_encoding.load(Ordering::SeqCst);
        let cmd = CaptureScreenshotFastParams {
            format: "png".to_owned(),
            capture_beyond_viewport: false,
            clip: clip.clone(),
            optimize_for_speed: try_fast.then_some(true),
        };
        let encoded: String = match page.execute(cmd).await {
            Ok(resp) => resp.result.data.into(),
            Err(err) => {
                let msg = err.to_string();
                let is_unsupported_param = try_fast
                    && (msg.contains("Invalid parameters")
                        || msg.contains("Unknown parameter")
                        || msg.contains("optimizeForSpeed"));
                if !is_unsupported_param {
                    return Err(ServiceError::new(
                        ServiceErrorKind::Internal,
                        "export_failed",
                        "导出截图生成失败",
                    ));
                }
                self.fast_encoding.store(false, Ordering::SeqCst);
                let fallback = CaptureScreenshotFastParams {
                    format: "png".to_owned(),
                    capture_beyond_viewport: false,
                    clip,
                    optimize_for_speed: None,
                };
                page.execute(fallback)
                    .await
                    .map_err(|_| {
                        ServiceError::new(
                            ServiceErrorKind::Internal,
                            "export_failed",
                            "导出截图生成失败",
                        )
                    })?
                    .result
                    .data
                    .into()
            }
        };

        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Internal,
                    "export_failed",
                    "导出截图解码失败",
                )
            })
    }
}

impl ExportRenderer for ChromiumoxideRenderer {
    fn render<'a>(
        &'a self,
        snapshot: &'a Value,
        emit: ExportEmitFn<'a>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<(), ServiceError>> {
        Box::pin(async move {
            let origin = self.origin.lock().expect("origin lock").clone();
            let format = snapshot
                .get("options")
                .and_then(|o| o.get("format"))
                .and_then(Value::as_str)
                .unwrap_or("png")
                .to_owned();

            enum RenderOutput {
                Html(ExportDocumentSource),
                Png(Vec<Vec<u8>>),
            }

            let snap_clone = snapshot.clone();
            let cancel_for_browser = cancel.clone();
            let render_out = self
                .browser
                .run(cancel_for_browser.clone(), |page, _lease| async move {
                    let mut paused_events = page
                        .event_listener::<EventRequestPaused>()
                        .await
                        .map_err(|_| {
                            ServiceError::new(
                                ServiceErrorKind::Internal,
                                "export_failed",
                                "无法开启导出页面网络隔离",
                            )
                        })?;
                    page.execute(fetch::EnableParams::default())
                        .await
                        .map_err(|_| {
                            ServiceError::new(
                                ServiceErrorKind::Internal,
                                "export_failed",
                                "无法启用导出页面请求拦截",
                            )
                        })?;

                    let page_for_intercept = page.clone();
                    let origin_for_intercept = origin.clone();
                    let intercept_task = tokio::spawn(async move {
                        while let Some(ev) = paused_events.next().await {
                            let allowed = Url::parse(&ev.request.url).is_ok_and(|u| {
                                u.scheme() == "data"
                                    || (ev.request.method.eq_ignore_ascii_case("GET")
                                        && u.origin().ascii_serialization() == origin_for_intercept
                                        && (u.path() == "/_export"
                                            || u.path().starts_with("/assets/")
                                            || u.path().starts_with("/fonts/")))
                            });
                            let p = page_for_intercept.clone();
                            let req_id = ev.request_id.clone();
                            tokio::spawn(async move {
                                if allowed {
                                    let _ = p.execute(ContinueRequestParams::new(req_id)).await;
                                } else {
                                    let _ = p
                                        .execute(FailRequestParams::new(
                                            req_id,
                                            ErrorReason::BlockedByClient,
                                        ))
                                        .await;
                                }
                            });
                        }
                    });

                    let res = async {
                        page.goto(format!("{origin}/_export")).await.map_err(|_| {
                            ServiceError::new(
                                ServiceErrorKind::Internal,
                                "export_document_failed",
                                "导出正文、字体或附图无法加载，请重新生成",
                            )
                        })?;

                        Self::wait_for_dataset_state(
                            &page,
                            r#"document.documentElement.dataset.exportReceiver === "ready" ? "ready" : """#,
                            &cancel_for_browser,
                        )
                        .await?;

                        let post_msg_call = CallFunctionOnParams::builder()
                            .function_declaration(
                                r#"function(value) {
                                    document.documentElement.dataset.exportState = "loading";
                                    window.postMessage({ type: "cst-export-preview", snapshot: value }, location.origin);
                                }"#,
                            )
                            .argument(CallArgument::builder().value(snap_clone).build())
                            .build()
                            .map_err(|_| {
                                ServiceError::new(
                                    ServiceErrorKind::Internal,
                                    "export_document_failed",
                                    "导出快照无法注入页面",
                                )
                            })?;
                        page.evaluate_function(post_msg_call).await.map_err(|_| {
                            ServiceError::new(
                                ServiceErrorKind::Internal,
                                "export_document_failed",
                                "导出快照无法注入页面",
                            )
                        })?;

                        let export_state = Self::wait_for_dataset_state(
                            &page,
                            r#"["ready", "failed"].includes(document.documentElement.dataset.exportState ?? "") ? document.documentElement.dataset.exportState : """#,
                            &cancel_for_browser,
                        )
                        .await?;
                        if export_state != "ready" {
                            return Err(ServiceError::new(
                                ServiceErrorKind::Internal,
                                "export_document_failed",
                                "导出正文、字体或附图无法加载，请重新生成",
                            ));
                        }

                        if format == "html" {
                            let source: ExportDocumentSource = page
                                .evaluate_function(CAPTURE_EXPORT_DOCUMENT_JS)
                                .await
                                .ok()
                                .and_then(|r| r.into_value::<ExportDocumentSource>().ok())
                                .ok_or_else(|| {
                                    ServiceError::new(
                                        ServiceErrorKind::Internal,
                                        "export_document_failed",
                                        "导出正文或样式无法提取",
                                    )
                                })?;
                            return Ok(RenderOutput::Html(source));
                        }

                        let geom: MeasuredGeometry = page
                            .evaluate_function(MEASURE_EXPORT_DOCUMENT_JS)
                            .await
                            .ok()
                            .and_then(|r| r.into_value::<MeasuredGeometry>().ok())
                            .ok_or_else(|| {
                                ServiceError::new(
                                    ServiceErrorKind::InvalidInput,
                                    "export_layout_invalid",
                                    "导出文档没有有效高度",
                                )
                            })?;
                        let parts = export_pages(geom.height, &geom.cuts)?;
                        let mut pngs = Vec::with_capacity(parts.len());
                        for part in parts {
                            if cancel_for_browser.is_cancelled() {
                                return Err(ServiceError::new(
                                    ServiceErrorKind::Unavailable,
                                    "export_interrupted",
                                    "导出已取消",
                                ));
                            }
                            let scroll_js = format!(
                                "window.scrollTo({{ top: {}, behavior: 'instant' }})",
                                part.top
                            );
                            page.evaluate(scroll_js.as_str()).await.map_err(|_| {
                                ServiceError::new(
                                    ServiceErrorKind::Internal,
                                    "export_failed",
                                    "导出分页滚动失败",
                                )
                            })?;
                            let bytes = self.capture_clip(&page, part.top, part.height).await?;
                            pngs.push(bytes);
                        }
                        Ok(RenderOutput::Png(pngs))
                    }
                    .await;

                    intercept_task.abort();
                    res
                })
                .await?;

            match render_out {
                RenderOutput::Html(source) => {
                    let bytes = self.html.render(snapshot, &source, cancel).await?;
                    emit("html", bytes).await?;
                }
                RenderOutput::Png(pngs) => {
                    for png in pngs {
                        if cancel.is_cancelled() {
                            return Err(ServiceError::new(
                                ServiceErrorKind::Unavailable,
                                "export_interrupted",
                                "导出已取消",
                            ));
                        }
                        emit("png", png).await?;
                    }
                }
            }
            Ok(())
        })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.browser.close().await;
            self.html.clear();
        })
    }

    fn set_origin(&self, origin: &str) {
        if let Ok(parsed) = Url::parse(origin) {
            let clean = parsed.origin().ascii_serialization();
            *self.origin.lock().expect("origin lock") = clean.clone();
            let _ = self.html.set_origin(&clean);
        }
    }
}

pub fn create_chromiumoxide_renderer(
    origin: &str,
    work_dir: impl Into<std::path::PathBuf>,
) -> Result<Arc<dyn ExportRenderer>, ServiceError> {
    let renderer = ChromiumoxideRenderer::new(origin, ExportBrowserOptions::new(work_dir))?;
    Ok(Arc::new(renderer))
}
