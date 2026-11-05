#[cfg(any(windows, test))]
use crate::clipboard::{ClipboardImage, DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES};

/// A clipboard DIB has no BITMAPFILEHEADER. The color table and optional masks
/// precede its pixels; simply prepending a fixed pixel offset corrupts indexed DIBs.
#[cfg(any(windows, test))]
pub(crate) fn image_from_dib(dib: &[u8]) -> Option<ClipboardImage> {
    use image::ImageEncoder;
    let u32_at = |offset: usize| {
        Some(u32::from_le_bytes(
            dib.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    let header = usize::try_from(u32_at(0)?).ok()?;
    if !(40..=124).contains(&header)
        || dib.len() < header
        || dib.len() > DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES
    {
        return None;
    }
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    let colors = if bits <= 8 {
        let used = u32_at(32)?;
        if used == 0 {
            1u32.checked_shl(u32::from(bits))?
        } else {
            used
        }
    } else {
        0
    };
    let masks = if header == 40 {
        match compression {
            3 => 12,
            6 => 16,
            _ => 0,
        }
    } else {
        0
    };
    let pixel_offset = 14usize
        .checked_add(header)?
        .checked_add(masks)?
        .checked_add(usize::try_from(colors).ok()?.checked_mul(4)?)?;
    let size = dib.len().checked_add(14)?;
    if pixel_offset >= size {
        return None;
    }
    let mut bmp = Vec::with_capacity(size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&u32::try_from(size).ok()?.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&u32::try_from(pixel_offset).ok()?.to_le_bytes());
    bmp.extend_from_slice(dib);
    let mut rgba = image::load_from_memory(&bmp).ok()?.to_rgba8();
    if bits == 32 && rgba.pixels().all(|pixel| pixel[3] == 0) {
        for pixel in rgba.pixels_mut() {
            pixel[3] = 255;
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(ClipboardImage {
        bytes: png,
        mime_type: "image/png".to_owned(),
    })
}

#[cfg(windows)]
mod windows {
    use super::{ClipboardImage, DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES, image_from_dib};
    use std::ffi::c_void;
    use std::sync::Mutex;
    type Handle = *mut c_void;
    const CF_BITMAP: u32 = 2;
    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;
    static CLIPBOARD: Mutex<()> = Mutex::new(());

    #[repr(C)]
    #[derive(Default)]
    struct Bitmap {
        kind: i32,
        width: i32,
        height: i32,
        width_bytes: i32,
        planes: u16,
        bits_pixel: u16,
        bits: Handle,
    }
    #[repr(C)]
    #[derive(Default)]
    struct BitmapInfo {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bits: u16,
        compression: u32,
        image_size: u32,
        x: i32,
        y: i32,
        used: u32,
        important: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: Handle) -> i32;
        fn CreateWindowExW(
            extended: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Handle,
            menu: Handle,
            instance: Handle,
            parameter: Handle,
        ) -> Handle;
        fn DestroyWindow(window: Handle) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> Handle;
        fn SetClipboardData(format: u32, memory: Handle) -> Handle;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
        fn GetDC(window: Handle) -> Handle;
        fn ReleaseDC(window: Handle, dc: Handle) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalSize(memory: Handle) -> usize;
        fn GlobalLock(memory: Handle) -> Handle;
        fn GlobalUnlock(memory: Handle) -> i32;
        fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
        fn GlobalFree(memory: Handle) -> Handle;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn DragQueryFileW(drop: Handle, index: u32, path: *mut u16, count: u32) -> u32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetObjectW(object: Handle, size: i32, output: *mut c_void) -> i32;
        fn GetDIBits(
            dc: Handle,
            bitmap: Handle,
            first: u32,
            lines: u32,
            output: *mut c_void,
            info: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
    }
    struct Open;
    impl Open {
        fn acquire() -> Option<Self> {
            Self::with_owner(std::ptr::null_mut())
        }
        fn with_owner(owner: Handle) -> Option<Self> {
            for _ in 0..5 {
                // SAFETY: Reads allow a null owner; writes use a window created on this thread.
                if unsafe { OpenClipboard(owner) } != 0 {
                    return Some(Self);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            None
        }
    }
    impl Drop for Open {
        fn drop(&mut self) {
            /* SAFETY: Only a successful open constructs this guard. */
            unsafe {
                CloseClipboard();
            }
        }
    }

    fn global_bytes(format: u32) -> Option<Vec<u8>> {
        // SAFETY: The clipboard remains open while the borrowed global allocation is locked and copied.
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let size = GlobalSize(handle);
            if size == 0 || size > DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES {
                return None;
            }
            let address = GlobalLock(handle);
            if address.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(address.cast::<u8>(), size).to_vec();
            GlobalUnlock(handle);
            Some(bytes)
        }
    }

    pub(crate) fn read_image() -> Option<ClipboardImage> {
        let _lock = CLIPBOARD.lock().ok()?;
        let _open = Open::acquire()?;
        let png_name: Vec<u16> = "PNG\0".encode_utf16().collect();
        // SAFETY: The format name is terminated UTF-16 and the clipboard is open.
        let png = unsafe { RegisterClipboardFormatW(png_name.as_ptr()) };
        if png != 0
            && let Some(bytes) = global_bytes(png)
        {
            return Some(ClipboardImage {
                bytes,
                mime_type: "image/png".to_owned(),
            });
        }
        for format in [17, 8] {
            if let Some(dib) = global_bytes(format)
                && let Some(image) = image_from_dib(&dib)
            {
                return Some(image);
            }
        }
        // SAFETY: Query the clipboard-owned bitmap without deleting it. The screen DC is released below.
        unsafe {
            let bitmap = GetClipboardData(CF_BITMAP);
            if bitmap.is_null() {
                return None;
            }
            let mut object = Bitmap::default();
            if GetObjectW(
                bitmap,
                std::mem::size_of::<Bitmap>() as i32,
                (&raw mut object).cast(),
            ) == 0
            {
                return None;
            }
            let width = object.width.checked_abs()?;
            let height = object.height.checked_abs()?;
            let size = usize::try_from(width)
                .ok()?
                .checked_mul(usize::try_from(height).ok()?)?
                .checked_mul(4)?;
            if size == 0 || size > DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES {
                return None;
            }
            let mut dib = vec![0u8; 40 + size];
            let mut info = BitmapInfo {
                size: 40,
                width,
                height: -height,
                planes: 1,
                bits: 32,
                image_size: u32::try_from(size).ok()?,
                ..Default::default()
            };
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let lines = GetDIBits(
                dc,
                bitmap,
                0,
                height as u32,
                dib[40..].as_mut_ptr().cast(),
                &raw mut info,
                0,
            );
            ReleaseDC(std::ptr::null_mut(), dc);
            if lines != height {
                return None;
            }
            dib[..40].copy_from_slice(std::slice::from_raw_parts(
                (&raw const info).cast::<u8>(),
                40,
            ));
            image_from_dib(&dib)
        }
    }

    pub(crate) fn read_text() -> Option<String> {
        let _lock = CLIPBOARD.lock().ok()?;
        let _open = Open::acquire()?;
        let bytes = global_bytes(CF_UNICODETEXT)?;
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        let text = String::from_utf16_lossy(&units);
        (!text.is_empty()).then_some(text)
    }

    pub(crate) fn read_paths() -> Option<Vec<String>> {
        let _lock = CLIPBOARD.lock().ok()?;
        let _open = Open::acquire()?;
        // SAFETY: HDROP remains owned by the open clipboard; each path buffer includes its terminator.
        unsafe {
            let drop = GetClipboardData(CF_HDROP);
            if drop.is_null() {
                return None;
            }
            let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
            if count > 4096 {
                return None;
            }
            let mut paths = Vec::with_capacity(count as usize);
            for index in 0..count {
                let length = DragQueryFileW(drop, index, std::ptr::null_mut(), 0);
                if length == 0 || length > 32767 {
                    continue;
                }
                let mut path = vec![0u16; length as usize + 1];
                let actual = DragQueryFileW(drop, index, path.as_mut_ptr(), length + 1);
                if actual == length {
                    paths.push(String::from_utf16_lossy(&path[..length as usize]));
                }
            }
            Some(paths)
        }
    }

    struct OwnerWindow(Handle);
    impl OwnerWindow {
        fn create() -> Option<Self> {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // SAFETY: STATIC is a system window class. HWND_MESSAGE creates an invisible message-only window.
            let window = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    -3isize as Handle,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            (!window.is_null()).then_some(Self(window))
        }
    }
    impl Drop for OwnerWindow {
        fn drop(&mut self) {
            /* SAFETY: The owner is destroyed on its creating thread. */
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    pub(crate) fn set_text(text: &str) -> Result<(), String> {
        if text.contains('\0') {
            return Err("Windows clipboard text cannot contain NUL".to_owned());
        }
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let size = units
            .len()
            .checked_mul(2)
            .filter(|size| *size <= DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES)
            .ok_or("Clipboard text is too large")?;
        let _lock = CLIPBOARD.lock().map_err(|_| "Clipboard lock poisoned")?;
        let owner = OwnerWindow::create().ok_or("Failed to create clipboard owner")?;
        let _open = Open::with_owner(owner.0).ok_or("Clipboard is busy")?;
        // SAFETY: GMEM_MOVEABLE storage is filled while locked. SetClipboardData takes ownership only on success.
        unsafe {
            let handle = GlobalAlloc(0x0002, size);
            if handle.is_null() {
                return Err("Failed to allocate clipboard memory".to_owned());
            }
            let address = GlobalLock(handle);
            if address.is_null() {
                GlobalFree(handle);
                return Err("Failed to lock clipboard memory".to_owned());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), address.cast::<u16>(), units.len());
            GlobalUnlock(handle);
            if EmptyClipboard() == 0 || SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                GlobalFree(handle);
                return Err("Failed to set Windows clipboard text".to_owned());
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::{read_image, read_paths, read_text, set_text};

#[cfg(test)]
mod tests {
    #[test]
    fn dib_bitmap_header_and_pixels_are_converted_to_png() {
        let mut dib = vec![0u8; 48];
        dib[..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib[20..24].copy_from_slice(&8u32.to_le_bytes());
        dib[40..46].copy_from_slice(&[0, 0, 255, 0, 255, 0]);
        let result = super::image_from_dib(&dib).unwrap();
        let decoded = image::load_from_memory(&result.bytes).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0]);
        assert_eq!(decoded.get_pixel(1, 0).0, [0, 255, 0]);
        assert!(super::image_from_dib(&[0; 40]).is_none());
    }
}
