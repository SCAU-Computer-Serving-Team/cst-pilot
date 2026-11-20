use std::io::Cursor;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{Image, ImageFormat};
use image::{Rgba, RgbaImage};
use qrcode::QrCode;
use qrcode::types::Color as QrColor;
use serde_json::Value;

/// Pure-Rust in-memory QR code rasterizer (`qrcode` -> `RgbaImage` -> `gpui::Image`).
/// Never transmits `verification_uri_complete` over any external network service.
pub fn rasterize_qr_image_offline(verification_uri_complete: &str) -> Result<Arc<Image>, String> {
    let trimmed = verification_uri_complete.trim();
    if trimmed.is_empty() {
        return Err("授权链接为空，无法生成二维码。".into());
    }

    let code = QrCode::new(trimmed.as_bytes()).map_err(|e| format!("二维码编码失败：{e}"))?;
    let width_modules = code.width();
    let quiet_zone = 3usize;
    let total_modules = width_modules + quiet_zone * 2;
    let scale = (216usize / total_modules.max(1)).max(4);
    let img_px = (total_modules * scale) as u32;

    let mut rgba = RgbaImage::from_pixel(img_px, img_px, Rgba([255, 255, 255, 255]));
    let colors = code.to_colors();

    for my in 0..width_modules {
        for mx in 0..width_modules {
            if colors[my * width_modules + mx] == QrColor::Dark {
                let px_x0 = ((mx + quiet_zone) * scale) as u32;
                let px_y0 = ((my + quiet_zone) * scale) as u32;
                for dy in 0..(scale as u32) {
                    for dx in 0..(scale as u32) {
                        rgba.put_pixel(px_x0 + dx, px_y0 + dy, Rgba([17, 24, 39, 255]));
                    }
                }
            }
        }
    }

    let mut png_bytes = Vec::with_capacity(4096);
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| format!("二维码图像编码失败：{e}"))?;

    Ok(Arc::new(Image::from_bytes(ImageFormat::Png, png_bytes)))
}

#[derive(Clone)]
pub struct ParsedDeviceQrPayload {
    pub flow_id: Option<String>,
    pub verification_uri_complete: String,
    pub expires_at_ms: u64,
}

/// Extracts `verificationUriComplete` (or equivalent OAuth URL) and expiry timestamp from
/// `/api/auth/cstoa/oauth/start` or `/status` JSON without exposing any raw `user_code` text.
pub fn parse_cstoa_oauth_payload(val: &Value) -> Option<ParsedDeviceQrPayload> {
    let flow_id = val
        .get("flowId")
        .or_else(|| val.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let auth_obj = val.get("auth").or_else(|| val.get("device"));
    let device_code = val.get("deviceCode");
    let uri = val
        .get("verificationUriComplete")
        .or_else(|| val.get("verification_uri_complete"))
        .or_else(|| val.get("url"))
        .or_else(|| auth_obj.and_then(|a| a.get("verificationUriComplete")))
        .or_else(|| auth_obj.and_then(|a| a.get("url")))
        .or_else(|| device_code.and_then(|d| d.get("verificationUri")))
        .and_then(Value::as_str)?
        .trim()
        .to_owned();

    if uri.is_empty() {
        return None;
    }

    let now = now_epoch_ms();
    let expires_at_ms = val
        .get("expiresAt")
        .or_else(|| auth_obj.and_then(|a| a.get("expiresAt")))
        .and_then(Value::as_u64)
        .or_else(|| {
            val.get("expiresIn")
                .or_else(|| auth_obj.and_then(|a| a.get("expiresIn")))
                .or_else(|| device_code.and_then(|d| d.get("expiresInSeconds")))
                .and_then(Value::as_u64)
                .map(|secs| now.saturating_add(secs.saturating_mul(1000)))
        })
        .unwrap_or(now + 300_000);

    Some(ParsedDeviceQrPayload {
        flow_id,
        verification_uri_complete: uri,
        expires_at_ms,
    })
}

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
