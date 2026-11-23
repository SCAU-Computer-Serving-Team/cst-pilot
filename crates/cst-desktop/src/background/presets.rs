use std::io::Cursor;
use std::sync::OnceLock;

use gpui_kit::{Hsla, rgba};
use image::ImageFormat;

use crate::theme::{DesktopPalette, ResolvedTheme};

const BLUE_HOUR_LIGHT_PNG: &[u8] = include_bytes!("../../../../design/asset/blue-hour-palette.png");
const BLUE_HOUR_DARK_PNG: &[u8] =
    include_bytes!("../../../../design/asset/blue-hour-dark-palette.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkyPreset {
    /// Home welcome canvas (`p2HWHP`) — `blue-hour.glsl` + `blue-hour-palette.png`.
    HomeLight,
    /// Home welcome canvas (`PuezS`) — `blue-hour.glsl` + `blue-hour-dark-palette.png`.
    HomeDark,
    /// Login `864×800` left canvas (`tM7YQ` / `jzHSt`) — `blue-sky.glsl`.
    LoginLight,
    /// Login `864×800` left canvas (`jiQRy` / `k6JMV`) — `night-sky.glsl`.
    LoginDark,
}

impl SkyPreset {
    pub fn home_for_theme(mode: ResolvedTheme) -> Self {
        match mode {
            ResolvedTheme::Light => Self::HomeLight,
            ResolvedTheme::Dark => Self::HomeDark,
        }
    }

    pub fn login_for_theme(mode: ResolvedTheme) -> Self {
        match mode {
            ResolvedTheme::Light => Self::LoginLight,
            ResolvedTheme::Dark => Self::LoginDark,
        }
    }

    /// Fallback multi-stop linear gradient `(top, bottom)` for GPUs below D3D Feature Level `11_0`.
    pub fn fallback_gradient_stops(self) -> (Hsla, Hsla) {
        match self {
            // Both stops (and the colors between them) meet 4.5:1 against the white heading,
            // including when a short or resized window moves the heading down the gradient.
            Self::HomeLight => (rgba(0x1E2A_6CFF).into(), rgba(0x6072_ABFF).into()),
            Self::HomeDark => (rgba(0x1319_2BFF).into(), rgba(0x0D0D_11FF).into()),
            Self::LoginLight => (rgba(0xE6F2_FFFF).into(), rgba(0x80B3_FFFF).into()),
            Self::LoginDark => (rgba(0x4A5A_A0FF).into(), rgba(0x1C1C_1CFF).into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrostedGlassSpec {
    pub blur_px: f32,
    pub refract_px: f32,
    pub vibrancy: f32,
    pub grain: f32,
    pub tint_rgb: [f32; 3],
    pub tint_opacity: f32,
    pub specular: f32,
    pub corner_radius_px: f32,
    pub opacity: f32,
}

impl FrostedGlassSpec {
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    #[doc(hidden)]
    pub fn popover_h2p2z(palette: &DesktopPalette) -> Self {
        Self::popover(palette)
    }

    #[doc(hidden)]
    pub fn modal_k3x72(palette: &DesktopPalette, corner_radius_px: f32) -> Self {
        Self::modal(palette, corner_radius_px)
    }

    #[doc(hidden)]
    pub fn scrim_dz0fp(palette: &DesktopPalette) -> Self {
        Self::scrim(palette)
    }

    /// Tier 2 unmasked popover / menu / top error notice (`H2p2z`).
    pub fn popover(palette: &DesktopPalette) -> Self {
        Self {
            blur_px: 10.0,
            refract_px: 0.0,
            vibrancy: 1.06,
            grain: 0.0,
            tint_rgb: palette.bg_glass_rgb,
            tint_opacity: 0.92,
            specular: 0.16,
            corner_radius_px: 8.0,
            opacity: 1.0,
        }
    }

    /// Tier 3 masked modal dialog / command palette (`K3x72`).
    pub fn modal(palette: &DesktopPalette, corner_radius_px: f32) -> Self {
        Self {
            blur_px: 10.0,
            refract_px: 0.0,
            vibrancy: 1.06,
            grain: 0.0,
            tint_rgb: palette.bg_glass_rgb,
            tint_opacity: 0.95,
            specular: 0.12,
            corner_radius_px,
            opacity: 1.0,
        }
    }

    pub fn workspace_scrim(palette: &DesktopPalette) -> Self {
        Self {
            corner_radius_px: f32::from(crate::theme::radius::ROW),
            ..Self::scrim(palette)
        }
    }

    /// Full-window blurred modal backdrop scrim (`DZ0Fp`).
    pub fn scrim(palette: &DesktopPalette) -> Self {
        Self {
            blur_px: 14.0,
            refract_px: 0.0,
            vibrancy: 1.0,
            grain: 0.0,
            tint_rgb: palette.bg_scrim_rgb,
            tint_opacity: palette.bg_scrim_opacity,
            specular: 0.0,
            corner_radius_px: 0.0,
            opacity: 1.0,
        }
    }
}

/// 80-byte `vec4`-aligned constant buffer for `blue-sky.glsl` and `night-sky.glsl`.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct SkyUniforms {
    pub slot0: [f32; 4], // [origin_x, origin_y, res_w, res_h]
    pub slot1: [f32; 4], // [viewport_w, viewport_h, opacity, u_time]
    pub slot2: [f32; 4], // [u_speed, u_scale, u_distortion, u_swirl]
    pub slot3: [f32; 4], // [u_direction, u_grain, 0, 0]
    pub slot4: [f32; 4], // [r_tl, r_tr, r_br, r_bl] physical corner radii
}

impl SkyUniforms {
    pub fn for_preset(
        preset: SkyPreset,
        origin: [f32; 2],
        resolution: [f32; 2],
        viewport: [f32; 2],
        opacity: f32,
        time_sec: f32,
        corner_radii_phys: [f32; 4],
    ) -> Self {
        let (speed, scale, distortion, swirl, direction, grain) = match preset {
            SkyPreset::LoginLight | SkyPreset::HomeLight => (22.0, 72.0, 48.0, 38.0, 0.0, 4.0),
            SkyPreset::LoginDark | SkyPreset::HomeDark => (14.0, 42.0, 46.0, 28.0, 0.0, 8.0),
        };
        Self {
            slot0: [origin[0], origin[1], resolution[0], resolution[1]],
            slot1: [viewport[0], viewport[1], opacity, time_sec],
            slot2: [speed, scale, distortion, swirl],
            slot3: [direction, grain, 0.0, 0.0],
            slot4: corner_radii_phys,
        }
    }
}

/// 96-byte `vec4`-aligned constant buffer for `blue-hour.glsl`.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct BlueHourUniforms {
    pub slot0: [f32; 4], // [origin_x, origin_y, res_w, res_h]
    pub slot1: [f32; 4], // [viewport_w, viewport_h, opacity, u_time]
    pub slot2: [f32; 4], // [u_speed, u_phase, u_count, u_height]
    pub slot3: [f32; 4], // [u_focus, u_position, u_blend, u_facets]
    pub slot4: [f32; 4], // [u_grain, 0, 0, 0]
    pub slot5: [f32; 4], // [r_tl, r_tr, r_br, r_bl] physical corner radii
}

impl BlueHourUniforms {
    pub fn new(
        origin: [f32; 2],
        resolution: [f32; 2],
        viewport: [f32; 2],
        opacity: f32,
        time_sec: f32,
        corner_radii_phys: [f32; 4],
    ) -> Self {
        Self {
            slot0: [origin[0], origin[1], resolution[0], resolution[1]],
            slot1: [viewport[0], viewport[1], opacity, time_sec],
            slot2: [30.0, 20.75, 11.0, 60.0],
            slot3: [50.0, 50.0, 65.0, 35.0],
            slot4: [0.015, 0.0, 0.0, 0.0],
            slot5: corner_radii_phys,
        }
    }
}

/// 96-byte `vec4`-aligned constant buffer for `frosted-glass.glsl`.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct FrostedGlassUniforms {
    pub slot0: [f32; 4], // [origin_x, origin_y, res_w, res_h]
    pub slot1: [f32; 4], // [viewport_w, viewport_h, opacity, 0]
    pub slot2: [f32; 4], // [u_blur, u_refract, u_vibrancy, u_grain]
    pub slot3: [f32; 4], // [u_tint.r, u_tint.g, u_tint.b, u_tintOpacity]
    pub slot4: [f32; 4], // [u_specular, cap_offset_x, cap_offset_y, corner_radius_px]
    pub slot5: [f32; 4], // [cap_w, cap_h, alloc_w, alloc_h]
}

pub fn decode_palette_256x1(light: bool) -> &'static [[u8; 4]; 256] {
    static LIGHT_PALETTE: OnceLock<[[u8; 4]; 256]> = OnceLock::new();
    static DARK_PALETTE: OnceLock<[[u8; 4]; 256]> = OnceLock::new();

    if light {
        LIGHT_PALETTE.get_or_init(|| decode_png_row(BLUE_HOUR_LIGHT_PNG))
    } else {
        DARK_PALETTE.get_or_init(|| decode_png_row(BLUE_HOUR_DARK_PNG))
    }
}

fn decode_png_row(png_bytes: &[u8]) -> [[u8; 4]; 256] {
    let mut out = [[255u8; 4]; 256];
    if let Ok(img) = image::load(Cursor::new(png_bytes), ImageFormat::Png) {
        let rgba = img.to_rgba8();
        let w = rgba.width().max(1);
        for (i, slot) in out.iter_mut().enumerate() {
            let x = ((i as u32) * w / 256).min(w - 1);
            *slot = rgba.get_pixel(x, 0).0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_home_fallback_meets_white_heading_contrast() {
        let (top, bottom) = SkyPreset::HomeLight.fallback_gradient_stops();
        for color in [top, bottom] {
            let rgb = color.to_rgb();
            let linear = |c: f32| {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            let luminance =
                0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b);
            assert_eq!(rgb.a, 1.0);
            assert!(1.05 / (luminance + 0.05) >= 4.5);
        }
    }
}
