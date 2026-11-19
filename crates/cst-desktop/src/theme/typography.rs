use std::borrow::Cow;
use std::fs;
use std::path::Path;

use gpui_kit::{
    App, Div, FontWeight, IntoElement, ParentElement, Pixels, SharedString, Styled, div, px,
};

pub const UI_FONT_FAMILY: &str = "Source Han Sans CN";
pub const FALLBACK_UI_FONT_FAMILY: &str = "Microsoft YaHei UI";
pub const MONO_FONT_FAMILY: &str = "JetBrains Mono";
pub const FALLBACK_MONO_FONT_FAMILY: &str = "Consolas";

/// Optical bottom offset (`$space-micro = 2px`) applied to inline CJK labels aligned alongside icons.
pub const CJK_INLINE_OPTICAL_OFFSET_PX: f32 = 2.0;

#[derive(Debug, Clone, Copy)]
pub struct TypographyScale {
    pub font_size: Pixels,
    pub line_height: Pixels,
    pub weight: FontWeight,
}

/// Five-tier typography hierarchy matching `cst-pilot-desktop.pen` (`Y5ga8O`) and `DESIGN.md`.
pub mod scale {
    use super::{FontWeight, TypographyScale, px};

    /// `$font-hero`: `40/48`, weight `600` (Home welcome hero).
    pub const HERO: TypographyScale = TypographyScale {
        font_size: px(40.0),
        line_height: px(48.0),
        weight: FontWeight::SEMIBOLD,
    };

    /// `$font-title`: `20/28`, weight `600` (Settings section page title).
    pub const TITLE: TypographyScale = TypographyScale {
        font_size: px(20.0),
        line_height: px(28.0),
        weight: FontWeight::SEMIBOLD,
    };

    /// `$font-body`: `16/24`, weight `400` (Conversation prose).
    pub const BODY: TypographyScale = TypographyScale {
        font_size: px(16.0),
        line_height: px(24.0),
        weight: FontWeight::NORMAL,
    };

    /// `$font-body` medium: `16/24`, weight `500` (Chat header `HUkqs`).
    pub const BODY_MEDIUM: TypographyScale = TypographyScale {
        font_size: px(16.0),
        line_height: px(24.0),
        weight: FontWeight::MEDIUM,
    };

    /// `$font-body` semibold: `16/24`, weight `600` (Subpage header `xWJOf`, section headers, modal titles).
    pub const BODY_SEMIBOLD: TypographyScale = TypographyScale {
        font_size: px(16.0),
        line_height: px(24.0),
        weight: FontWeight::SEMIBOLD,
    };

    /// `$font-ui`: `14/20`, weight `400` (Sidebar rows, tables, menus, tool status rows).
    pub const UI: TypographyScale = TypographyScale {
        font_size: px(14.0),
        line_height: px(20.0),
        weight: FontWeight::NORMAL,
    };

    /// `$font-ui` medium: `14/20`, weight `500` (Buttons, active tabs, emphasized UI labels).
    pub const UI_MEDIUM: TypographyScale = TypographyScale {
        font_size: px(14.0),
        line_height: px(20.0),
        weight: FontWeight::MEDIUM,
    };

    /// `$font-caption`: `12/16`, weight `400` (Shortcut badges, elapsed timers, raw tool output).
    pub const CAPTION: TypographyScale = TypographyScale {
        font_size: px(12.0),
        line_height: px(16.0),
        weight: FontWeight::NORMAL,
    };

    /// `$font-caption` medium: `12/16`, weight `500`.
    pub const CAPTION_MEDIUM: TypographyScale = TypographyScale {
        font_size: px(12.0),
        line_height: px(16.0),
        weight: FontWeight::MEDIUM,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontLoadReport {
    pub loaded_cjk: bool,
    pub loaded_mono: bool,
    pub warning: Option<String>,
}

impl FontLoadReport {
    pub fn ui_family(&self) -> &'static str {
        if self.loaded_cjk {
            UI_FONT_FAMILY
        } else {
            FALLBACK_UI_FONT_FAMILY
        }
    }

    pub fn mono_family(&self) -> &'static str {
        if self.loaded_mono {
            MONO_FONT_FAMILY
        } else {
            FALLBACK_MONO_FONT_FAMILY
        }
    }
}

pub fn has_valid_sfnt_magic(bytes: &[u8], expected_magic: &[u8; 4]) -> bool {
    if bytes.len() < 12 || &bytes[0..4] != expected_magic {
        return false;
    }
    let num_tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    num_tables > 0 && bytes.len() >= 12 + num_tables * 16
}

/// Loads bundled `SourceHanSansCN-VF.otf` and `JetBrainsMono-{Regular,Medium}.ttf` from `<kit_root>/assets/fonts/`.
/// Falls back cleanly to system DirectWrite fonts with a non-blocking Chinese warning if missing or corrupted.
pub fn load_bundled_fonts(kit_root: &Path, cx: &App) -> FontLoadReport {
    let fonts_dir = kit_root.join("assets/fonts");
    let mut warnings = Vec::new();

    let cjk_path = fonts_dir.join("SourceHanSansCN-VF.otf");
    let loaded_cjk = match fs::read(&cjk_path) {
        Ok(bytes) if has_valid_sfnt_magic(&bytes, b"OTTO") => {
            cx.text_system().add_fonts(vec![Cow::Owned(bytes)]).is_ok()
        }
        _ => false,
    };
    if !loaded_cjk {
        warnings.push("思源黑体可变字体加载失败，已回退系统默认中文字体");
    }

    let mono_reg = fonts_dir.join("JetBrainsMono-Regular.ttf");
    let mono_med = fonts_dir.join("JetBrainsMono-Medium.ttf");
    let loaded_mono = match (fs::read(&mono_reg), fs::read(&mono_med)) {
        (Ok(reg), Ok(med))
            if has_valid_sfnt_magic(&reg, &[0, 1, 0, 0])
                && has_valid_sfnt_magic(&med, &[0, 1, 0, 0]) =>
        {
            cx.text_system()
                .add_fonts(vec![Cow::Owned(reg), Cow::Owned(med)])
                .is_ok()
        }
        _ => false,
    };
    if !loaded_mono {
        warnings.push("JetBrains Mono 等宽字体加载失败，已回退系统默认等宽字体");
    }

    let warning = if warnings.is_empty() {
        None
    } else {
        Some(format!("{}。", warnings.join("；")))
    };

    FontLoadReport {
        loaded_cjk,
        loaded_mono,
        warning,
    }
}

/// Applies a [`TypographyScale`] to any GPUI [`Div`].
pub fn apply_scale(el: Div, s: TypographyScale) -> Div {
    el.text_size(s.font_size)
        .line_height(s.line_height)
        .font_weight(s.weight)
}

/// Wraps inline Chinese button/navigation text with a `2px` (`$space-micro`) bottom optical offset
/// so CJK glyphs center visually against `14px–16px` stroke icons.
pub fn cjk_inline_label(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pb(px(CJK_INLINE_OPTICAL_OFFSET_PX))
        .child(text.into())
}
