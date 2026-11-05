use std::io::Cursor;

use base64::Engine;
use image::{DynamicImage, ImageFormat, RgbImage};
use pi_runtime::{
    ClipboardImage, ProcessImageOptions, normalize_clipboard_image, process_image_sync,
};

#[test]
fn tiff_and_bmp_are_converted_to_png_without_losing_pixels() {
    for (format, mime) in [
        (ImageFormat::Tiff, "image/tiff"),
        (ImageFormat::Bmp, "image/bmp"),
    ] {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(2, 3, |x, y| {
            image::Rgb([x as u8 * 50, y as u8 * 60, 200])
        }));
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, format).unwrap();
        let raw = encoded.into_inner();
        let clipboard = normalize_clipboard_image(ClipboardImage {
            bytes: raw.clone(),
            mime_type: mime.to_owned(),
        })
        .unwrap();
        assert_eq!(clipboard.mime_type, "image/png");
        assert_eq!(
            image::load_from_memory(&clipboard.bytes).unwrap().to_rgb8(),
            image.to_rgb8()
        );
        for resize in [true, false] {
            let processed = process_image_sync(
                &raw,
                mime,
                &ProcessImageOptions {
                    auto_resize_images: Some(resize),
                    ..Default::default()
                },
            );
            assert!(processed.ok);
            assert_eq!(processed.mime_type, "image/png");
            assert!(processed.conversion_note.is_some());
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&processed.data)
                .unwrap();
            assert_eq!(
                image::load_from_memory(&bytes).unwrap().to_rgb8(),
                image.to_rgb8()
            );
        }
    }
}
