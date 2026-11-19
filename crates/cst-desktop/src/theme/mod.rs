pub mod typography;

use cst_client::ThemeModeDto;
use gpui_kit::{App, Global, Hsla, SharedString, Window, WindowAppearance, px, rgba};

pub use typography::{
    CJK_INLINE_OPTICAL_OFFSET_PX, FontLoadReport, TypographyScale, apply_scale, cjk_inline_label,
    has_valid_sfnt_magic, load_bundled_fonts, scale,
};

/// Seven-step spacing ladder (`2 / 4 / 6 / 8 / 12 / 16 / 24px`).
pub mod space {
    use gpui_kit::{Pixels, px};

    pub const MICRO: Pixels = px(2.0);
    pub const XS: Pixels = px(4.0);
    pub const TIGHT: Pixels = px(6.0);
    pub const SM: Pixels = px(8.0);
    pub const MD: Pixels = px(12.0);
    pub const LG: Pixels = px(16.0);
    pub const GROUP: Pixels = px(24.0);
}

/// Four border-radius tokens matching `cst-pilot-desktop.pen`.
pub mod radius {
    use gpui_kit::{Pixels, px};

    pub const CONTROL: Pixels = px(4.0);
    pub const ROW: Pixels = px(8.0);
    pub const SPECIAL: Pixels = px(12.0);
    pub const FULL: Pixels = px(9999.0);
}

/// Five fixed box heights (`24 / 28 / 32 / 40 / 44px`).
pub mod height {
    use gpui_kit::{Pixels, px};

    pub const INLINE: Pixels = px(24.0);
    pub const COMPACT: Pixels = px(28.0);
    pub const CONTROL: Pixels = px(32.0);
    pub const FIELD: Pixels = px(40.0);
    pub const HEADER: Pixels = px(44.0);
}

/// Layout dimension constants from `cst-pilot-desktop.pen`.
pub mod layout {
    use gpui_kit::{Pixels, px};

    pub const DEFAULT_WINDOW_WIDTH: f32 = 1280.0;
    pub const DEFAULT_WINDOW_HEIGHT: f32 = 800.0;
    pub const MIN_WINDOW_WIDTH: f32 = 1024.0;
    pub const MIN_WINDOW_HEIGHT: f32 = 640.0;

    pub const SIDEBAR_WIDTH: Pixels = px(232.0);
    pub const TOPBAR_HEIGHT: Pixels = px(40.0);
    pub const SHELL_FRAME_INSET: Pixels = px(8.0);
    pub const CENTER_COLUMN_MAX_WIDTH: Pixels = px(816.0);
    pub const MODAL_STANDARD_WIDTH: Pixels = px(384.0);
    pub const PALETTE_MODAL_WIDTH: Pixels = px(520.0);
    pub const LOGIN_SHADER_WIDTH: Pixels = px(864.0);
}

/// Motion curves & durations strictly aligned with `design/storybook.html`.
pub mod motion {
    use std::time::Duration;

    pub const POPOVER_OPEN: Duration = Duration::from_millis(220);
    pub const MODAL_OPEN: Duration = Duration::from_millis(240);
    pub const SURFACE_CLOSE: Duration = Duration::from_millis(150);

    pub fn cubic_bezier(
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    ) -> impl Fn(f32) -> f32 + Copy + 'static {
        let (cx, cy) = (3.0 * x1, 3.0 * y1);
        let (bx, by) = (3.0 * (x2 - x1) - cx, 3.0 * (y2 - y1) - cy);
        let (ax, ay) = (1.0 - cx - bx, 1.0 - cy - by);
        move |t: f32| {
            let t = t.clamp(0.0, 1.0);
            let sample_x = |s: f32| ((ax * s + bx) * s + cx) * s;
            let sample_y = |s: f32| ((ay * s + by) * s + cy) * s;
            let slope_x = |s: f32| (3.0 * ax * s + 2.0 * bx) * s + cx;
            let mut s = t;
            let mut converged = false;
            for _ in 0..8 {
                let err = sample_x(s) - t;
                if err.abs() < 1e-6 {
                    converged = true;
                    break;
                }
                let sl = slope_x(s);
                if sl.abs() < 1e-6 {
                    break;
                }
                s = (s - err / sl).clamp(0.0, 1.0);
            }
            if !converged {
                let (mut lo, mut hi) = (0.0, 1.0);
                s = t;
                for _ in 0..24 {
                    let x = sample_x(s);
                    if (x - t).abs() < 1e-6 {
                        break;
                    }
                    if x < t {
                        lo = s;
                    } else {
                        hi = s;
                    }
                    s = (lo + hi) * 0.5;
                }
            }
            sample_y(s)
        }
    }

    pub fn ease_out() -> impl Fn(f32) -> f32 + Copy + 'static {
        cubic_bezier(0.22, 1.0, 0.36, 1.0)
    }

    pub fn ease_in_out() -> impl Fn(f32) -> f32 + Copy + 'static {
        cubic_bezier(0.45, 0.0, 0.55, 1.0)
    }

    pub fn ease_spring() -> impl Fn(f32) -> f32 + Copy + 'static {
        cubic_bezier(0.22, 1.12, 0.36, 1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedTheme {
    Light,
    Dark,
}

impl ResolvedTheme {
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// Complete 29-token semantic palette strictly matching `design/cst-pilot-desktop.pen` (`Y5ga8O`)
/// and `src/test/pen-color-subset.test.mjs`.
#[derive(Debug, Clone, Copy)]
pub struct DesktopPalette {
    pub mode: ResolvedTheme,
    // Surfaces
    pub bg_shell: Hsla,
    pub bg_workspace: Hsla,
    pub bg_elevated: Hsla,
    pub bg_glass: Hsla,
    pub bg_glass_rgb: [f32; 3],
    pub bg_scrim: Hsla,
    pub bg_scrim_rgb: [f32; 3],
    pub bg_scrim_opacity: f32,
    pub bg_modal: Hsla,
    pub bg_surface: Hsla,
    pub bg_special: Hsla,
    pub bg_item_hover: Hsla,
    pub bg_item_active: Hsla,
    pub bg_track_recess: Hsla,
    // Borders & highlights
    pub border_subtle: Hsla,
    pub border_strong: Hsla,
    pub border_hairline: Hsla,
    pub highlight_top: Hsla,
    // Text
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub text_on_accent: Hsla,
    // Accent & semantic status
    pub accent_solid: Hsla,
    pub accent_hover: Hsla,
    pub accent_subtle: Hsla,
    pub status_success: Hsla,
    pub status_success_subtle: Hsla,
    pub status_warning: Hsla,
    pub status_warning_subtle: Hsla,
    pub status_danger: Hsla,
    pub status_danger_subtle: Hsla,
}

fn c(hex_rgba: u32) -> Hsla {
    rgba(hex_rgba).into()
}

impl DesktopPalette {
    pub fn light() -> Self {
        Self {
            mode: ResolvedTheme::Light,
            bg_shell: c(0xF4F4_F4FF),
            bg_workspace: c(0xFFFF_FFFF),
            bg_elevated: c(0xFFFF_FFFF),
            bg_glass: c(0xFFFF_FFFF),
            bg_glass_rgb: [1.0, 1.0, 1.0],
            bg_scrim: c(0x1111_142E),
            bg_scrim_rgb: [17.0 / 255.0, 17.0 / 255.0, 20.0 / 255.0],
            bg_scrim_opacity: 0.18,
            bg_modal: c(0xFFFF_FFF2),
            bg_surface: c(0xF4F4_F5FF),
            bg_special: c(0xFCFC_FCFF),
            bg_item_hover: c(0xEAEA_EB80),
            bg_item_active: c(0xEAEA_EBFF),
            bg_track_recess: c(0xE4E4_E7FF),
            border_subtle: c(0x0000_0012),
            border_strong: c(0x0000_0024),
            border_hairline: c(0x0000_0014),
            highlight_top: c(0xFFFF_FFCC),
            text_primary: c(0x1111_11FF),
            text_secondary: c(0x6B6B_6BFF),
            text_muted: c(0x8A8A_8EFF),
            text_on_accent: c(0xFFFF_FFFF),
            accent_solid: c(0x1A4B_E0FF),
            accent_hover: c(0x153E_C0FF),
            accent_subtle: c(0x1A4B_E014),
            status_success: c(0x16A3_4AFF),
            status_success_subtle: c(0x16A3_4A1A),
            status_warning: c(0x8C5A_00FF),
            status_warning_subtle: c(0x8C5A_001A),
            status_danger: c(0xDC26_26FF),
            status_danger_subtle: c(0xDC26_261A),
        }
    }

    pub fn dark() -> Self {
        Self {
            mode: ResolvedTheme::Dark,
            bg_shell: c(0x1414_16FF),
            bg_workspace: c(0x0D0D_0FFF),
            bg_elevated: c(0x2424_29FF),
            bg_glass: c(0x1818_1CFF),
            bg_glass_rgb: [24.0 / 255.0, 24.0 / 255.0, 28.0 / 255.0],
            bg_scrim: c(0x0404_0661),
            bg_scrim_rgb: [4.0 / 255.0, 4.0 / 255.0, 6.0 / 255.0],
            bg_scrim_opacity: 0.38,
            bg_modal: c(0x1818_1CF2),
            bg_surface: c(0x1C1C_20FF),
            bg_special: c(0x1212_15FF),
            bg_item_hover: c(0x2424_2980),
            bg_item_active: c(0x2424_29FF),
            bg_track_recess: c(0x2727_2AFF),
            border_subtle: c(0xFFFF_FF12),
            border_strong: c(0xFFFF_FF24),
            border_hairline: c(0xFFFF_FF1A),
            highlight_top: c(0xFFFF_FF14),
            text_primary: c(0xF2F2_F4FF),
            text_secondary: c(0x8585_8BFF),
            text_muted: c(0x6363_69FF),
            text_on_accent: c(0xFFFF_FFFF),
            accent_solid: c(0x3B6B_F5FF),
            accent_hover: c(0x4E7B_FFFF),
            accent_subtle: c(0x3B6B_F524),
            status_success: c(0x4CC3_8AFF),
            status_success_subtle: c(0x4CC3_8A24),
            status_warning: c(0xE4A2_47FF),
            status_warning_subtle: c(0xE4A2_4724),
            status_danger: c(0xE548_4DFF),
            status_danger_subtle: c(0xE548_4D24),
        }
    }

    pub fn for_mode(mode: ResolvedTheme) -> Self {
        match mode {
            ResolvedTheme::Light => Self::light(),
            ResolvedTheme::Dark => Self::dark(),
        }
    }
}

pub fn resolve_theme_mode(pref: ThemeModeDto, appearance: WindowAppearance) -> ResolvedTheme {
    match pref {
        ThemeModeDto::Light => ResolvedTheme::Light,
        ThemeModeDto::Dark => ResolvedTheme::Dark,
        ThemeModeDto::System => match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ResolvedTheme::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => ResolvedTheme::Light,
        },
    }
}

/// Global desktop theme state stored on `gpui::App`.
#[derive(Clone)]
pub struct DesktopThemeGlobal {
    pub preference: ThemeModeDto,
    pub palette: DesktopPalette,
    pub ui_font_family: SharedString,
    pub mono_font_family: SharedString,
    pub transparency_enabled: bool,
}

impl Global for DesktopThemeGlobal {}

impl DesktopThemeGlobal {
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_else(|| Self {
            preference: ThemeModeDto::Light,
            palette: DesktopPalette::light(),
            ui_font_family: SharedString::from(typography::FALLBACK_UI_FONT_FAMILY),
            mono_font_family: SharedString::from(typography::FALLBACK_MONO_FONT_FAMILY),
            transparency_enabled: true,
        })
    }
}

/// Checks Windows `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\EnableTransparency`.
/// Defaults to `true` unless the user explicitly turned off Windows transparency effects (`0`).
pub fn query_system_transparency_enabled() -> bool {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
        use windows::core::PCWSTR;

        let subkey: Vec<u16> =
            OsStr::new(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
                .encode_wide()
                .chain([0])
                .collect();
        let value_name: Vec<u16> = OsStr::new("EnableTransparency")
            .encode_wide()
            .chain([0])
            .collect();
        let mut val: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value_name.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut val as *mut u32).cast()),
                Some(&mut size),
            )
        };
        if status.is_ok() {
            return val != 0;
        }
        true
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Synchronizes CST Pilot's semantic palette and font families into both `DesktopThemeGlobal`
/// and `gpui_kit::component::theme::Theme`.
pub fn apply_desktop_theme(
    pref: ThemeModeDto,
    window: Option<&mut Window>,
    fonts: Option<&FontLoadReport>,
    cx: &mut App,
) -> DesktopPalette {
    let appearance = window
        .as_ref()
        .map(|w| w.appearance())
        .unwrap_or(WindowAppearance::Light);
    let resolved = resolve_theme_mode(pref, appearance);
    let palette = DesktopPalette::for_mode(resolved);

    let prev = cx.try_global::<DesktopThemeGlobal>().cloned();
    let ui_font = fonts
        .map(|f| SharedString::from(f.ui_family()))
        .or_else(|| prev.as_ref().map(|p| p.ui_font_family.clone()))
        .unwrap_or_else(|| SharedString::from(typography::FALLBACK_UI_FONT_FAMILY));
    let mono_font = fonts
        .map(|f| SharedString::from(f.mono_family()))
        .or_else(|| prev.as_ref().map(|p| p.mono_font_family.clone()))
        .unwrap_or_else(|| SharedString::from(typography::FALLBACK_MONO_FONT_FAMILY));
    let transparency_enabled = query_system_transparency_enabled();

    let kit_mode = match resolved {
        ResolvedTheme::Light => gpui_kit::component::theme::ThemeMode::Light,
        ResolvedTheme::Dark => gpui_kit::component::theme::ThemeMode::Dark,
    };
    gpui_kit::component::theme::Theme::change(kit_mode, window, cx);

    {
        let kit_theme = gpui_kit::component::theme::Theme::global_mut(cx);
        kit_theme.font_family = ui_font.clone();
        kit_theme.mono_font_family = mono_font.clone();
        kit_theme.font_size = px(14.0);
        kit_theme.mono_font_size = px(12.0);
        kit_theme.radius = radius::CONTROL;
        kit_theme.radius_lg = radius::ROW;
        kit_theme.colors.background = palette.bg_workspace;
        kit_theme.colors.foreground = palette.text_primary;
        kit_theme.colors.muted_foreground = palette.text_secondary;
        kit_theme.colors.border = palette.border_subtle;
        kit_theme.colors.input = palette.border_strong;
        kit_theme.colors.primary = palette.accent_solid;
        kit_theme.colors.primary_hover = palette.accent_hover;
        kit_theme.colors.primary_foreground = palette.text_on_accent;
        kit_theme.colors.popover = palette.bg_elevated;
        kit_theme.colors.popover_foreground = palette.text_primary;
        kit_theme.colors.danger = palette.status_danger;
        kit_theme.colors.danger_foreground = palette.text_on_accent;
    }

    cx.set_global(DesktopThemeGlobal {
        preference: pref,
        palette,
        ui_font_family: ui_font,
        mono_font_family: mono_font,
        transparency_enabled,
    });

    palette
}
