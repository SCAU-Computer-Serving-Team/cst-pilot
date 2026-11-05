use std::io::Cursor;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageEncoder};
use pi_ai::{ImageContent, TextContent, UserContentBlock};
use serde::{Deserialize, Serialize};

pub const SUPPORTED_IMAGE_MIME_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];

pub const DEFAULT_MAX_IMAGE_WIDTH: u32 = 2000;
pub const DEFAULT_MAX_IMAGE_HEIGHT: u32 = 2000;
/// 4.5 MiB of Base64 payload (headroom below Anthropic's 5 MiB limit).
pub const DEFAULT_MAX_IMAGE_BYTES: usize = 4_718_592;
pub const DEFAULT_JPEG_QUALITY: u8 = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageResizeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jpeg_quality: Option<u8>,
}

impl Default for ImageResizeOptions {
    fn default() -> Self {
        Self {
            max_width: Some(DEFAULT_MAX_IMAGE_WIDTH),
            max_height: Some(DEFAULT_MAX_IMAGE_HEIGHT),
            max_bytes: Some(DEFAULT_MAX_IMAGE_BYTES),
            jpeg_quality: Some(DEFAULT_JPEG_QUALITY),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizedImage {
    pub data: String,
    pub mime_type: String,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub was_resized: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessImageOptions {
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<ImageResizeOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedImage {
    pub ok: bool,
    pub data: String,
    pub mime_type: String,
    pub dimension_note: Option<String>,
    pub conversion_note: Option<String>,
    pub hints: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NormalizeToolResultImagesOptions {
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<ImageResizeOptions>,
}

/// Extracts EXIF orientation (1..=8) from JPEG or WebP bytes. Returns 1 if absent or invalid.
pub fn get_exif_orientation(bytes: &[u8]) -> u8 {
    // JPEG: starts with FF D8
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut offset = 2usize;
        while offset + 4 <= bytes.len() {
            if bytes[offset] != 0xFF {
                break;
            }
            let marker = bytes[offset + 1];
            if marker == 0xFF {
                offset += 1;
                continue;
            }
            if marker == 0xDA || marker == 0xD9 {
                break;
            }
            if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
                offset += 2;
                continue;
            }
            let seg_len = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
            if seg_len < 2 || offset + 2 + seg_len > bytes.len() {
                break;
            }
            // APP1 (FF E1) with "Exif\0\0"
            if marker == 0xE1
                && seg_len >= 8
                && offset + 10 <= bytes.len()
                && &bytes[offset + 4..offset + 10] == b"Exif\0\0"
            {
                let tiff_end = offset + 2 + seg_len;
                if let Some(orient) = read_tiff_orientation(bytes, offset + 10, tiff_end) {
                    return orient;
                }
            }
            offset += 2 + seg_len;
        }
        return 1;
    }

    // WebP: RIFF....WEBP
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let mut offset = 12usize;
        while offset + 8 <= bytes.len() {
            let fourcc = &bytes[offset..offset + 4];
            let chunk_size = u32::from_le_bytes([
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]) as usize;
            let data_start = offset + 8;
            if data_start + chunk_size > bytes.len() {
                break;
            }
            if fourcc == b"EXIF" {
                let has_exif_header = chunk_size >= 6
                    && data_start + 6 <= bytes.len()
                    && &bytes[data_start..data_start + 6] == b"Exif\0\0";
                let tiff_start = if has_exif_header {
                    data_start + 6
                } else {
                    data_start
                };
                if let Some(orient) =
                    read_tiff_orientation(bytes, tiff_start, data_start + chunk_size)
                {
                    return orient;
                }
            }
            // Chunks are padded to even sizes
            offset = data_start + chunk_size + (chunk_size & 1);
        }
    }

    1
}

fn read_tiff_orientation(bytes: &[u8], tiff_start: usize, tiff_end: usize) -> Option<u8> {
    if tiff_end > bytes.len() || tiff_start + 8 > tiff_end {
        return None;
    }
    let slice = &bytes[tiff_start..tiff_end];
    let little = match &slice[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |pos: usize| -> Option<u16> {
        let b = slice.get(pos..pos + 2)?;
        Some(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let read_u32 = |pos: usize| -> Option<u32> {
        let b = slice.get(pos..pos + 4)?;
        Some(if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };

    if read_u16(2)? != 0x002A {
        return None;
    }
    let ifd_offset = read_u32(4)? as usize;
    let count = read_u16(ifd_offset)? as usize;
    for i in 0..count {
        let entry_offset = ifd_offset.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        if entry_offset + 12 > slice.len() {
            break;
        }
        if read_u16(entry_offset)? == 0x0112 {
            let value = read_u16(entry_offset + 8)?;
            if (1..=8).contains(&value) {
                return Some(value as u8);
            }
            return None;
        }
    }
    None
}

pub fn apply_exif_orientation(img: DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }
    let scale = if result.width > 0 {
        f64::from(result.original_width) / f64::from(result.width)
    } else {
        1.0
    };
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]",
        result.original_width, result.original_height, result.width, result.height, scale
    ))
}

fn encode_png_bytes(img: &DynamicImage) -> Option<Vec<u8>> {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut buf = Vec::new();
    let encoder = PngEncoder::new(&mut buf);
    encoder
        .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(buf)
}

fn encode_jpeg_bytes(img: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut buf = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder
        .encode(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(buf)
}

struct EncodedCandidate {
    data: String,
    encoded_size: usize,
    mime_type: String,
}

fn encode_candidate(bytes: &[u8], mime_type: &str) -> EncodedCandidate {
    let data = BASE64_STANDARD.encode(bytes);
    let encoded_size = data.len();
    EncodedCandidate {
        data,
        encoded_size,
        mime_type: mime_type.to_owned(),
    }
}

/// Converts an image in Base64 to PNG Base64.
pub fn convert_to_png_sync(base64_data: &str, _mime_type: &str) -> Option<ImageContent> {
    let bytes = BASE64_STANDARD.decode(base64_data.trim()).ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?;
    let oriented = apply_exif_orientation(decoded, get_exif_orientation(&bytes));
    let png_bytes = encode_png_bytes(&oriented)?;
    Some(ImageContent {
        data: BASE64_STANDARD.encode(&png_bytes),
        mime_type: "image/png".to_owned(),
    })
}

pub async fn convert_to_png(base64_data: String, mime_type: String) -> Option<ImageContent> {
    tokio::task::spawn_blocking(move || convert_to_png_sync(&base64_data, &mime_type))
        .await
        .ok()
        .flatten()
}

/// Resizes an image in-process following the 4-step adaptive strategy.
pub fn resize_image_in_process(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<&ImageResizeOptions>,
) -> Option<ResizedImage> {
    let max_width = options
        .and_then(|o| o.max_width)
        .unwrap_or(DEFAULT_MAX_IMAGE_WIDTH)
        .max(1);
    let max_height = options
        .and_then(|o| o.max_height)
        .unwrap_or(DEFAULT_MAX_IMAGE_HEIGHT)
        .max(1);
    let max_bytes = options
        .and_then(|o| o.max_bytes)
        .unwrap_or(DEFAULT_MAX_IMAGE_BYTES);
    let jpeg_quality = options
        .and_then(|o| o.jpeg_quality)
        .unwrap_or(DEFAULT_JPEG_QUALITY);

    let input_base64_size = input_bytes.len().div_ceil(3) * 4;
    let raw_image = image::load_from_memory(input_bytes).ok()?;
    let orientation = get_exif_orientation(input_bytes);
    let image = apply_exif_orientation(raw_image, orientation);
    let (original_width, original_height) = image.dimensions();

    if orientation == 1
        && original_width <= max_width
        && original_height <= max_height
        && input_base64_size < max_bytes
    {
        return Some(ResizedImage {
            data: BASE64_STANDARD.encode(input_bytes),
            mime_type: if mime_type.is_empty() {
                "image/png".to_owned()
            } else {
                mime_type.to_owned()
            },
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    let mut target_width = original_width;
    let mut target_height = original_height;
    if target_width > max_width {
        target_height = ((u64::from(target_height) * u64::from(max_width)
            + u64::from(target_width) / 2)
            / u64::from(target_width)) as u32;
        target_width = max_width;
    }
    if target_height > max_height {
        target_width = ((u64::from(target_width) * u64::from(max_height)
            + u64::from(target_height) / 2)
            / u64::from(target_height)) as u32;
        target_height = max_height;
    }
    target_width = target_width.max(1);
    target_height = target_height.max(1);

    let mut quality_steps = Vec::new();
    for q in [jpeg_quality, 85, 70, 55, 40] {
        if !quality_steps.contains(&q) {
            quality_steps.push(q);
        }
    }

    let mut current_width = target_width;
    let mut current_height = target_height;

    loop {
        let resized = if current_width == original_width && current_height == original_height {
            image.clone()
        } else {
            image.resize_exact(current_width, current_height, FilterType::Lanczos3)
        };

        let png_cand = encode_png_bytes(&resized).map(|b| encode_candidate(&b, "image/png"));
        let mut jpeg_cands: Vec<EncodedCandidate> = quality_steps
            .iter()
            .filter_map(|&q| {
                encode_jpeg_bytes(&resized, q).map(|b| encode_candidate(&b, "image/jpeg"))
            })
            .collect();

        let first_jpeg = if !jpeg_cands.is_empty() {
            Some(jpeg_cands.remove(0))
        } else {
            None
        };

        let mut ordered = Vec::new();
        match (png_cand, first_jpeg) {
            (Some(p), Some(j)) => {
                if j.encoded_size < p.encoded_size {
                    ordered.push(j);
                    ordered.push(p);
                } else {
                    ordered.push(p);
                    ordered.push(j);
                }
            }
            (Some(p), None) => ordered.push(p),
            (None, Some(j)) => ordered.push(j),
            (None, None) => {}
        }
        ordered.extend(jpeg_cands);

        for candidate in ordered {
            if candidate.encoded_size < max_bytes {
                let was_resized =
                    current_width != original_width || current_height != original_height;
                return Some(ResizedImage {
                    data: candidate.data,
                    mime_type: candidate.mime_type,
                    original_width,
                    original_height,
                    width: current_width,
                    height: current_height,
                    was_resized,
                });
            }
        }

        if current_width == 1 && current_height == 1 {
            break;
        }
        let next_width = if current_width == 1 {
            1
        } else {
            ((u64::from(current_width) * 3) / 4).max(1) as u32
        };
        let next_height = if current_height == 1 {
            1
        } else {
            ((u64::from(current_height) * 3) / 4).max(1) as u32
        };
        if next_width == current_width && next_height == current_height {
            break;
        }
        current_width = next_width;
        current_height = next_height;
    }

    None
}

pub fn resize_image_sync(img: &ImageContent, options: Option<&ImageResizeOptions>) -> ResizedImage {
    let Ok(bytes) = BASE64_STANDARD.decode(img.data.trim()) else {
        return ResizedImage {
            data: img.data.clone(),
            mime_type: img.mime_type.clone(),
            original_width: 0,
            original_height: 0,
            width: 0,
            height: 0,
            was_resized: false,
        };
    };
    resize_image_in_process(&bytes, &img.mime_type, options).unwrap_or_else(|| ResizedImage {
        data: img.data.clone(),
        mime_type: img.mime_type.clone(),
        original_width: 0,
        original_height: 0,
        width: 0,
        height: 0,
        was_resized: false,
    })
}

pub async fn resize_image(img: ImageContent, options: Option<ImageResizeOptions>) -> ResizedImage {
    tokio::task::spawn_blocking(move || resize_image_sync(&img, options.as_ref()))
        .await
        .unwrap_or_else(|_| ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_owned(),
            original_width: 0,
            original_height: 0,
            width: 0,
            height: 0,
            was_resized: false,
        })
}

pub fn process_image_sync(
    bytes: &[u8],
    mime_type: &str,
    options: &ProcessImageOptions,
) -> ProcessedImage {
    let auto_resize = options.auto_resize_images.unwrap_or(true);
    let mut base64_data = BASE64_STANDARD.encode(bytes);
    let mut resolved_mime = mime_type.to_owned();
    let mut conversion_note: Option<String> = None;

    if !SUPPORTED_IMAGE_MIME_TYPES.contains(&resolved_mime.as_str()) {
        if let Some(converted) = convert_to_png_sync(&base64_data, &resolved_mime) {
            conversion_note = Some(format!(
                "[Image converted from {resolved_mime} to {}.]",
                converted.mime_type
            ));
            base64_data = converted.data;
            resolved_mime = converted.mime_type;
        } else if bytes.starts_with(b"BM") {
            return ProcessedImage {
                ok: false,
                data: String::new(),
                mime_type: resolved_mime.clone(),
                dimension_note: None,
                conversion_note: None,
                hints: Vec::new(),
                message: Some(format!(
                    "[Image could not be converted from {resolved_mime} to image/png and was omitted.]"
                )),
            };
        }
    }

    if !auto_resize {
        let hints = conversion_note.iter().cloned().collect();
        return ProcessedImage {
            ok: true,
            data: base64_data,
            mime_type: resolved_mime,
            dimension_note: None,
            conversion_note,
            hints,
            message: None,
        };
    }

    let resized = resize_image_sync(
        &ImageContent {
            data: base64_data,
            mime_type: resolved_mime,
        },
        options.resize_options.as_ref(),
    );
    let dimension_note = format_dimension_note(&resized);
    let mut hints = Vec::new();
    if let Some(ref c) = conversion_note {
        hints.push(c.clone());
    }
    if let Some(ref d) = dimension_note {
        hints.push(d.clone());
    }

    ProcessedImage {
        ok: true,
        data: resized.data,
        mime_type: resized.mime_type,
        dimension_note,
        conversion_note,
        hints,
        message: None,
    }
}

pub async fn process_image(
    bytes: Vec<u8>,
    mime_type: String,
    options: ProcessImageOptions,
) -> ProcessedImage {
    tokio::task::spawn_blocking(move || process_image_sync(&bytes, &mime_type, &options))
        .await
        .unwrap_or_else(|e| ProcessedImage {
            ok: false,
            data: String::new(),
            mime_type: "image/png".to_owned(),
            dimension_note: None,
            conversion_note: None,
            hints: Vec::new(),
            message: Some(format!("[Image processing panicked: {e}]")),
        })
}

/// Normalizes all image blocks in tool result content (converts unsupported formats to PNG and resizes oversized images).
pub fn normalize_tool_result_images_sync(
    content: Vec<UserContentBlock>,
    options: &NormalizeToolResultImagesOptions,
) -> Vec<UserContentBlock> {
    if !content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Image(_)))
    {
        return content;
    }

    let proc_opts = ProcessImageOptions {
        auto_resize_images: options.auto_resize_images,
        resize_options: options.resize_options.clone(),
    };
    let mut normalized = Vec::with_capacity(content.len());
    let mut changed = false;

    for block in &content {
        let UserContentBlock::Image(img) = block else {
            normalized.push(block.clone());
            continue;
        };

        let Ok(raw_bytes) = BASE64_STANDARD.decode(img.data.trim()) else {
            normalized.push(block.clone());
            continue;
        };

        let processed = process_image_sync(&raw_bytes, &img.mime_type, &proc_opts);
        if !processed.ok {
            normalized.push(block.clone());
            continue;
        }

        if processed.data != img.data
            || processed.mime_type != img.mime_type
            || !processed.hints.is_empty()
        {
            changed = true;
        }

        normalized.push(UserContentBlock::Image(ImageContent {
            data: processed.data,
            mime_type: processed.mime_type,
        }));
        if !processed.hints.is_empty() {
            normalized.push(UserContentBlock::Text(TextContent::new(
                processed.hints.join("\n"),
            )));
        }
    }

    if changed { normalized } else { content }
}

pub async fn normalize_tool_result_images(
    content: Vec<UserContentBlock>,
    options: NormalizeToolResultImagesOptions,
) -> Vec<UserContentBlock> {
    if !content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Image(_)))
    {
        return content;
    }
    tokio::task::spawn_blocking(move || normalize_tool_result_images_sync(content, &options))
        .await
        .unwrap_or_default()
}

/// Helper to read image dimensions from PNG/JPEG/GIF/WebP bytes.
pub fn read_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.into_dimensions().ok()
}
