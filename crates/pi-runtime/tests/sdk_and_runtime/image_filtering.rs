use pi_agent::AgentMessage;
use pi_ai::{faux::*, *};
use pi_runtime::*;
use pi_session::*;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn dynamic_block_images_reaches_conversation_compaction_and_branch_summary() {
    for summarize_branch in [false, true] {
        for blocked in [false, true] {
            let root = std::env::temp_dir().join(format!("sdk-images-{}", uuid_v7()));
            std::fs::create_dir_all(&root).unwrap();
            let faux = create_faux_provider();
            let captures = Arc::new(Mutex::new(Vec::new()));
            let capture = captures.clone();
            faux.set_responses((0..8).map(|_| {
                let capture = capture.clone();
                FauxResponseStep::Dynamic(Arc::new(move |context, _, _| {
                    capture
                        .lock()
                        .unwrap()
                        .push(serde_json::to_string(context.messages()).unwrap());
                    faux_text_message("## Goal\nSummarized the image-related work.")
                }))
            }));
            let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
                include_builtins: Some(false),
                refresh_on_create: Some(false),
                ..Default::default()
            })
            .await;
            runtime.register_provider(faux.provider.clone());
            let settings = SettingsManager::in_memory();
            settings
                .set_block_images(!blocked, SettingsScope::Global)
                .unwrap();
            let mut manager =
                SessionManager::in_memory(root.to_string_lossy(), None, None).unwrap();
            let mut target = String::new();
            for i in 0..8 {
                manager
                    .append_message(AgentMessage::User(UserMessage {
                        content: UserContent::Blocks(vec![
                            UserContentBlock::Text(TextContent::new(format!(
                                "turn {i} {}",
                                "image discussion ".repeat(200)
                            ))),
                            UserContentBlock::Image(ImageContent {
                                data: "mock-image".into(),
                                mime_type: "image/png".into(),
                            }),
                        ]),
                        timestamp: i,
                    }))
                    .unwrap();
                let id = manager
                    .append_message(AgentMessage::Assistant(faux_text_message(
                        "answer ".repeat(200),
                    )))
                    .unwrap();
                if i == 0 {
                    target = id;
                }
            }
            let created = create_agent_session(CreateAgentSessionOptions {
                cwd: Some(root.clone()),
                agent_dir: Some(root.join("agent")),
                model_runtime: Some(runtime),
                model: Some(faux.model),
                settings_manager: Some(settings.clone()),
                session_manager: Some(manager),
                no_tools: Some(NoToolsMode::All),
                ..Default::default()
            })
            .await
            .unwrap();
            // The callback must read current settings, not construction-time settings.
            settings
                .set_block_images(blocked, SettingsScope::Global)
                .unwrap();
            if summarize_branch {
                created
                    .session
                    .navigate_tree(
                        &target,
                        NavigateTreeOptions {
                            summarize: true,
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
            } else {
                created.session.compact(None).await.unwrap();
            }
            {
                let captures = captures.lock().unwrap();
                assert!(!captures.is_empty());
                for text in captures.iter() {
                    assert_eq!(
                        text.contains(BLOCKED_IMAGE_PLACEHOLDER),
                        blocked,
                        "branch={summarize_branch} blocked={blocked}"
                    );
                    assert!(!text.contains("mock-image")); // summary inputs are serialized text
                }
            }
            captures.lock().unwrap().clear();
            for enabled in [true, false] {
                settings
                    .set_block_images(enabled, SettingsScope::Global)
                    .unwrap();
                created
                    .session
                    .prompt(
                        "inspect this",
                        Some(PromptOptions {
                            images: Some(vec![ImageContent {
                                data: "new-mock-image".into(),
                                mime_type: "image/png".into(),
                            }]),
                            ..Default::default()
                        }),
                    )
                    .await
                    .unwrap();
                let text = captures.lock().unwrap().last().unwrap().clone();
                assert_eq!(text.contains("new-mock-image"), !enabled);
            }
            created.session.shutdown().await.unwrap();
        }
    }
}

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buf = vec![0u8; 58];
    buf[0..2].copy_from_slice(b"BM");
    buf[2..6].copy_from_slice(&(58u32.to_le_bytes()));
    buf[10..14].copy_from_slice(&(54u32.to_le_bytes()));
    buf[14..18].copy_from_slice(&(40u32.to_le_bytes()));
    buf[18..22].copy_from_slice(&(1i32.to_le_bytes()));
    buf[22..26].copy_from_slice(&(1i32.to_le_bytes()));
    buf[26..28].copy_from_slice(&(1u16.to_le_bytes()));
    buf[28..30].copy_from_slice(&(24u16.to_le_bytes()));
    buf[34..38].copy_from_slice(&(4u32.to_le_bytes()));
    buf[56] = 0xFF;
    buf
}

fn create_solid_png(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn inject_jpeg_exif_orientation(jpeg_bytes: &[u8], orientation: u16) -> Vec<u8> {
    assert_eq!(&jpeg_bytes[0..2], &[0xFF, 0xD8]);
    let mut exif_payload = Vec::new();
    exif_payload.extend_from_slice(b"Exif\0\0");
    // Little-endian TIFF header
    exif_payload.extend_from_slice(b"II");
    exif_payload.extend_from_slice(&0x002Au16.to_le_bytes());
    exif_payload.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
    exif_payload.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
    exif_payload.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation tag
    exif_payload.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    exif_payload.extend_from_slice(&1u32.to_le_bytes()); // count = 1
    exif_payload.extend_from_slice(&orientation.to_le_bytes());
    exif_payload.extend_from_slice(&0u16.to_le_bytes()); // padding
    exif_payload.extend_from_slice(&0u32.to_le_bytes()); // next IFD

    let seg_len = (exif_payload.len() + 2) as u16;
    let mut out = Vec::with_capacity(jpeg_bytes.len() + exif_payload.len() + 4);
    out.extend_from_slice(&jpeg_bytes[0..2]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&seg_len.to_be_bytes());
    out.extend_from_slice(&exif_payload);
    out.extend_from_slice(&jpeg_bytes[2..]);
    out
}

#[tokio::test]
async fn image_pipeline_handles_bmp_conversion_exif_orientation_resize_and_tool_result_normalization()
 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;

    // 1. BMP -> PNG conversion with and without autoResize
    let bmp = create_tiny_bmp_1x1_red_24bpp();
    let proc_no_resize = process_image_sync(
        &bmp,
        "image/bmp",
        &ProcessImageOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        },
    );
    assert!(proc_no_resize.ok);
    assert_eq!(proc_no_resize.mime_type, "image/png");
    assert_eq!(
        proc_no_resize.hints,
        vec!["[Image converted from image/bmp to image/png.]"]
    );
    let png_decoded = B64.decode(&proc_no_resize.data).unwrap();
    assert!(png_decoded.starts_with(b"\x89PNG\r\n\x1a\n"));

    // 2. EXIF orientation 6 (rotate 90 CW) on 120x60 JPEG -> becomes 60x120
    let rgb = image::RgbImage::from_pixel(120, 60, image::Rgb([200, 50, 50]));
    let mut raw_jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw_jpeg, 85)
        .encode(&rgb, 120, 60, image::ExtendedColorType::Rgb8)
        .unwrap();
    let exif_jpeg = inject_jpeg_exif_orientation(&raw_jpeg, 6);
    assert_eq!(get_exif_orientation(&exif_jpeg), 6);
    let oriented = resize_image_in_process(&exif_jpeg, "image/jpeg", None).unwrap();
    assert_eq!((oriented.width, oriented.height), (60, 120));

    // 3. Oversized 2400x4800 PNG resized to <= 2000x2000 and annotated in normalize_tool_result_images
    let big_png = create_solid_png(2400, 4800);
    let blocks = vec![
        UserContentBlock::Text(TextContent::new("before")),
        UserContentBlock::Image(ImageContent {
            data: B64.encode(&big_png),
            mime_type: "image/png".into(),
        }),
        UserContentBlock::Text(TextContent::new("after")),
    ];
    let normalized =
        normalize_tool_result_images(blocks, NormalizeToolResultImagesOptions::default()).await;
    assert_eq!(normalized.len(), 4);
    let UserContentBlock::Image(ref norm_img) = normalized[1] else {
        panic!("expected image block");
    };
    let (w, h) = read_image_dimensions(&B64.decode(&norm_img.data).unwrap()).unwrap();
    assert!(w <= 2000 && h <= 2000, "got {w}x{h}");
    let UserContentBlock::Text(ref note) = normalized[2] else {
        panic!("expected dimension note");
    };
    assert!(note.text.contains("original 2400x4800"));

    // 4. Clipboard MIME preference and OSC 52 encoding
    assert_eq!(
        select_preferred_image_mime_type(&[
            "text/plain".into(),
            "image/webp".into(),
            "image/png".into()
        ]),
        Some("image/png".into())
    );
    let osc = encode_osc52_clipboard_sequence("hello");
    assert_eq!(osc, "\x1b]52;c;aGVsbG8=\x07");
}
