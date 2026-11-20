pub mod typography;

use cst_client::ThemeModeDto;
use gpui_kit::{
    App, BoxShadow, Global, Hsla, SharedString, Window, WindowAppearance, point, px, rgba,
};

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
    pub const COMPACT_SIDEBAR_BREAKPOINT_WIDTH: f32 = 1120.0;

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
    use std::time::{Duration, Instant};

    use gpui::{Animation, AnimationExt as _, Transformation};
    use gpui_kit::assets::IconName;
    use gpui_kit::base::motion::{Transition, TransitionId, transition};
    use gpui_kit::component::Icon;
    use gpui_kit::{
        App, BoxShadow, Hsla, IntoElement, Pixels, SharedString, Styled, Window, percentage, px,
        size,
    };

    pub const INSTANT: Duration = Duration::ZERO;
    pub const PRESS: Duration = Duration::from_millis(120);
    pub const FAST: Duration = Duration::from_millis(150);
    pub const POPOVER_OPEN: Duration = Duration::from_millis(220);
    pub const WELCOME_ITEM: Duration = Duration::from_millis(220);
    pub const MODAL_OPEN: Duration = Duration::from_millis(240);
    pub const NORMAL: Duration = Duration::from_millis(240);
    pub const SURFACE_CLOSE: Duration = Duration::from_millis(150);
    pub const FLIP_ENTER: Duration = Duration::from_millis(180);
    pub const FLIP_MOVE: Duration = Duration::from_millis(250);
    pub const RESIZE: Duration = Duration::from_millis(300);
    pub const WELCOME_MAX_TOTAL: Duration = Duration::from_millis(300);
    pub const WELCOME_STAGGER_STEP: Duration = Duration::from_millis(40);
    pub const SLOW: Duration = Duration::from_millis(340);
    pub const ICON_SWAP_RESTORE: Duration = Duration::from_millis(1600);
    pub const RUNNING_LIGHT_CYCLE: Duration = Duration::from_millis(2000);
    pub const BANNER_AUTO_DISMISS: Duration = Duration::from_millis(2600);
    pub const TOOLTIP_INITIAL_DELAY: Duration = Duration::from_millis(400);
    pub const TOOLTIP_COOLDOWN: Duration = Duration::from_millis(300);

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
            if t == 0.0 || t == 1.0 {
                return t;
            }
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

    /// Scales box shadow alpha cubically (`d^3`) during fade-in so GPUI drop shadows never bleed through translucent panels.
    pub fn scale_shadows(mut shadows: Vec<BoxShadow>, factor: f32) -> Vec<BoxShadow> {
        let f = factor.clamp(0.0, 1.0);
        for s in &mut shadows {
            s.color.a *= f;
        }
        shadows
    }

    /// Animates a popover panel (`220ms` `ease_out`, fade + subtle `6px` lift, shadow alpha ramp `d^3`, `0ms` in `reduce_motion`).
    pub fn animate_popover_enter<E: IntoElement + Styled + 'static>(
        id: impl Into<gpui::ElementId>,
        el: E,
        shadows: Vec<BoxShadow>,
        slide_y_px: f32,
    ) -> impl IntoElement {
        el.with_animation(
            id,
            Animation::new(POPOVER_OPEN).with_easing(ease_out()),
            move |el, d| {
                let raw = d.clamp(0.0, 1.0);
                let fade = 0.01 + 0.99 * raw;
                el.opacity(fade)
                    .mt(px(slide_y_px * (1.0 - raw)))
                    .shadow(scale_shadows(shadows.clone(), raw * raw * raw))
            },
        )
    }

    /// Animates a modal dialog card (`240ms` `ease_out`, fade + `6px` lift, shadow alpha ramp `d^3`, `0ms` in `reduce_motion`).
    pub fn animate_modal_enter<E: IntoElement + Styled + 'static>(
        id: impl Into<gpui::ElementId>,
        el: E,
        shadows: Vec<BoxShadow>,
    ) -> impl IntoElement {
        el.with_animation(
            id,
            Animation::new(MODAL_OPEN).with_easing(ease_out()),
            move |el, d| {
                let raw = d.clamp(0.0, 1.0);
                let fade = 0.01 + 0.99 * raw;
                let animated = el.opacity(fade).mt(px(6.0 * (1.0 - raw)));
                if shadows.is_empty() {
                    animated
                } else {
                    animated.shadow(scale_shadows(shadows.clone(), raw * raw * raw))
                }
            },
        )
    }

    /// Animates a top banner / toast (`240ms` `ease_out`, `-8px -> 0` top slide + fade + shadow alpha ramp `d^3`, `0ms` in `reduce_motion`).
    pub fn animate_toast_enter<E: IntoElement + Styled + 'static>(
        id: impl Into<gpui::ElementId>,
        el: E,
        shadows: Vec<BoxShadow>,
    ) -> impl IntoElement {
        el.with_animation(
            id,
            Animation::new(NORMAL).with_easing(ease_out()),
            move |el, d| {
                let raw = d.clamp(0.0, 1.0);
                let fade = 0.01 + 0.99 * raw;
                let animated = el.opacity(fade).mt(px(-8.0 * (1.0 - raw)));
                if shadows.is_empty() {
                    animated
                } else {
                    animated.shadow(scale_shadows(shadows.clone(), raw * raw * raw))
                }
            },
        )
    }

    /// Returns an interruptible `Icon` rotated cleanly (`base_down`: `0->180deg` dropdown; `!base_down`: `0->90deg` disclosure).
    pub fn animated_chevron(
        id: impl Into<SharedString>,
        expanded: bool,
        base_down: bool,
        size_px: Pixels,
        color: Hsla,
        window: &mut Window,
        cx: &mut App,
    ) -> Icon {
        let tid: TransitionId = SharedString::from(format!("chev-{}", id.into())).into();
        let target = if expanded {
            if base_down { 0.5 } else { 0.25 }
        } else {
            0.0
        };
        let turns = transition(
            tid,
            target,
            Transition::new(NORMAL).ease(ease_out()),
            window,
            cx,
        );
        Icon::new(if base_down {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .size(size_px)
        .text_color(color)
        .rotate(percentage(turns))
    }

    /// Returns a spring-scaled copy/check icon (`240ms ease_spring`, `0.65 -> 1.0`, zero blur).
    pub fn animated_copy_icon(
        id: impl Into<SharedString>,
        copied: bool,
        size_px: Pixels,
        color: Hsla,
        window: &mut Window,
        cx: &mut App,
    ) -> Icon {
        let tid: TransitionId = SharedString::from(format!("copy-{}", id.into())).into();
        let target = if copied { 1.0 } else { 0.0 };
        let prog = transition(
            tid,
            target,
            Transition::new(NORMAL).ease(ease_spring()),
            window,
            cx,
        );
        let s = 0.65 + 0.35 * if copied { prog } else { 1.0 - prog };
        Icon::new(if copied {
            IconName::Check
        } else {
            IconName::Copy
        })
        .size(size_px)
        .text_color(color)
        .transform(Transformation::scale(size(s, s)))
    }

    /// Returns interruptible spring progress (`0.0 -> 1.0`) for checkbox mark (`340ms` check / `150ms` uncheck).
    pub fn animated_checkbox_mark(
        id: impl Into<SharedString>,
        checked: bool,
        size_px: Pixels,
        color: Hsla,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Icon> {
        let tid: TransitionId = SharedString::from(format!("chk-{}", id.into())).into();
        let dur = if checked { SLOW } else { SURFACE_CLOSE };
        let p = transition(
            tid,
            if checked { 1.0 } else { 0.0 },
            Transition::new(dur).ease(ease_spring()),
            window,
            cx,
        );
        (p > 0.02).then(|| {
            let s = (0.55 + 0.45 * p).max(0.1);
            Icon::new(IconName::Check)
                .size(size_px)
                .text_color(color.opacity(p.clamp(0.0, 1.0)))
                .transform(Transformation::scale(size(s, s)))
        })
    }

    /// The latest page replaces the content immediately. Keep any in-flight
    /// reveal so repeated navigation cannot drop the surface back to near zero.
    #[derive(Default)]
    pub(crate) struct PageSlide(Option<(Instant, f32)>);

    impl PageSlide {
        pub(crate) fn start(&mut self, direction: f32, cx: &App) {
            if cx.reduce_motion() {
                self.0 = None;
                return;
            }
            let now = cx.background_executor().now();
            if self.0.is_none_or(|(start, _)| now - start >= NORMAL) {
                self.0 = Some((now, direction));
            }
        }

        pub(crate) fn sample(&mut self, window: &Window, cx: &App) -> (f32, Pixels) {
            if let Some((start, direction)) = self.0
                && !cx.reduce_motion()
            {
                let elapsed = cx.background_executor().now() - start;
                if elapsed < NORMAL {
                    window.request_animation_frame();
                    let t = ease_out()(elapsed.as_secs_f32() / NORMAL.as_secs_f32());
                    return (0.01 + 0.99 * t, px(8.0 * direction * (1.0 - t)));
                }
            }
            self.0 = None;
            (1.0, px(0.0))
        }
    }

    /// Computes `(opacity, translate_y_px)` for one-shot welcome stagger (`40ms` step, `220ms` item, `<=300ms` total).
    pub fn welcome_stagger_progress(
        index: usize,
        elapsed: Duration,
        reduce_motion: bool,
    ) -> (f32, f32) {
        if reduce_motion {
            return (1.0, 0.0);
        }
        let max_delay = WELCOME_MAX_TOTAL.saturating_sub(WELCOME_ITEM);
        let raw_delay = WELCOME_STAGGER_STEP.saturating_mul(index as u32);
        let delay = raw_delay.min(max_delay);
        if elapsed <= delay {
            return (0.01, 10.0);
        }
        let item_elapsed = elapsed.saturating_sub(delay);
        let raw_t = (item_elapsed.as_secs_f32() / WELCOME_ITEM.as_secs_f32()).clamp(0.0, 1.0);
        let eased = ease_out()(raw_t).clamp(0.0, 1.0);
        (0.01 + 0.99 * eased, 10.0 * (1.0 - eased))
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
    pub bg_active_press: Hsla,
    pub bg_item_active: Hsla,
    pub bg_track_recess: Hsla,
    // Borders, highlights & shadows
    pub border_subtle: Hsla,
    pub border_strong: Hsla,
    pub border_focus: Hsla,
    pub border_hairline: Hsla,
    pub highlight_top: Hsla,
    pub shadow_panel_sm: Hsla,
    pub shadow_panel_lg: Hsla,
    // Text
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub text_welcome: Hsla,
    pub text_on_accent: Hsla,
    // Accent & semantic status
    pub accent_solid: Hsla,
    pub accent_hover: Hsla,
    pub accent_ink: Hsla,
    pub accent_subtle: Hsla,
    pub status_success: Hsla,
    pub status_success_subtle: Hsla,
    pub status_warning: Hsla,
    pub status_warning_subtle: Hsla,
    pub status_danger: Hsla,
    pub status_danger_hover: Hsla,
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
            bg_item_hover: c(0x1111_110C),
            bg_active_press: c(0x1111_1114),
            bg_item_active: c(0xEAEA_EBFF),
            bg_track_recess: c(0x1111_110D),
            border_subtle: c(0xE4E4_E7FF),
            border_strong: c(0xD4D4_D8FF),
            border_focus: c(0xC2C2_C8FF),
            border_hairline: c(0x1111_111A),
            highlight_top: c(0xFFFF_FFF2),
            shadow_panel_sm: c(0x1111_110F),
            shadow_panel_lg: c(0x1111_1117),
            text_primary: c(0x1111_11FF),
            text_secondary: c(0x6B6B_6BFF),
            text_muted: c(0x8A8A_8EFF),
            text_welcome: c(0xFFFF_FFFF),
            text_on_accent: c(0xFFFF_FFFF),
            accent_solid: c(0x1A4B_E0FF),
            accent_hover: c(0x1640_C2FF),
            accent_ink: c(0x1A4B_E0FF),
            accent_subtle: c(0xEEF2_FFFF),
            status_success: c(0x16A3_4AFF),
            status_success_subtle: c(0x16A3_4A1F),
            status_warning: c(0x8C5A_00FF),
            status_warning_subtle: c(0x8C5A_001F),
            status_danger: c(0xDC26_26FF),
            status_danger_hover: c(0xB91C_1CFF),
            status_danger_subtle: c(0xDC26_2624),
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
            bg_item_hover: c(0xF2F2_F40E),
            bg_active_press: c(0xF2F2_F418),
            bg_item_active: c(0x2424_29FF),
            bg_track_recess: c(0x0000_0066),
            border_subtle: c(0x2626_2BFF),
            border_strong: c(0x3434_3AFF),
            border_focus: c(0x4646_4FFF),
            border_hairline: c(0xF2F2_F41F),
            highlight_top: c(0xFFFF_FF1F),
            shadow_panel_sm: c(0x0000_0066),
            shadow_panel_lg: c(0x0000_0080),
            text_primary: c(0xF2F2_F4FF),
            text_secondary: c(0x8585_8BFF),
            text_muted: c(0x6363_69FF),
            text_welcome: c(0xFFFF_FFFF),
            text_on_accent: c(0xFFFF_FFFF),
            accent_solid: c(0x3B6B_F5FF),
            accent_hover: c(0x4E7A_F7FF),
            accent_ink: c(0x688F_F8FF),
            accent_subtle: c(0x1923_42FF),
            status_success: c(0x4CC3_8AFF),
            status_success_subtle: c(0x4CC3_8A29),
            status_warning: c(0xE4A2_47FF),
            status_warning_subtle: c(0xE4A2_4729),
            status_danger: c(0xE548_4DFF),
            status_danger_hover: c(0xCF36_3BFF),
            status_danger_subtle: c(0xE548_4D38),
        }
    }

    pub fn for_mode(mode: ResolvedTheme) -> Self {
        match mode {
            ResolvedTheme::Light => Self::light(),
            ResolvedTheme::Dark => Self::dark(),
        }
    }

    /// Dual-layer outer shadow for the workspace container (`d7oLH` / `k7X8V`).
    pub fn workspace_shadows(&self) -> Vec<BoxShadow> {
        vec![
            BoxShadow {
                color: self.shadow_panel_sm,
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(6.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: self.shadow_panel_lg,
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(24.0),
                spread_radius: px(0.0),
                inset: false,
            },
        ]
    }

    /// Compact elevation (`--craft-lift`) for pills, buttons, and compact special cards.
    pub fn craft_lift_shadows(&self) -> Vec<BoxShadow> {
        vec![
            BoxShadow {
                color: self.highlight_top,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: true,
            },
            BoxShadow {
                color: self.shadow_panel_sm,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(2.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: self.shadow_panel_sm,
                offset: point(px(0.0), px(4.0)),
                blur_radius: px(10.0),
                spread_radius: px(0.0),
                inset: false,
            },
        ]
    }

    /// Three-layer stacked elevation (`--craft-stack` + top highlight) for `ZWAs1` composer, `H2p2z` popovers, `K3x72` modals, and special cards.
    pub fn craft_stack_shadows(&self) -> Vec<BoxShadow> {
        vec![
            BoxShadow {
                color: self.highlight_top,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: true,
            },
            BoxShadow {
                color: self.shadow_panel_sm,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(2.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: self.shadow_panel_sm,
                offset: point(px(0.0), px(6.0)),
                blur_radius: px(14.0),
                spread_radius: px(-2.0),
                inset: false,
            },
            BoxShadow {
                color: self.shadow_panel_lg,
                offset: point(px(0.0), px(18.0)),
                blur_radius: px(36.0),
                spread_radius: px(-6.0),
                inset: false,
            },
        ]
    }

    /// Composer elevation + subtle neutral focus halo (`--focus-shadow` + `--craft-stack`).
    pub fn composer_shadows(&self, focused: bool) -> Vec<BoxShadow> {
        let mut list = self.craft_stack_shadows();
        if focused {
            let halo = match self.mode {
                ResolvedTheme::Light => c(0x1111_140E),
                ResolvedTheme::Dark => c(0xF2F2_F40E),
            };
            list.push(BoxShadow {
                color: halo,
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(8.0),
                spread_radius: px(1.0),
                inset: false,
            });
        }
        list
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

/// Renders the shared full-width `44px` subpage header master `xWJOf` (`cst-pilot-desktop.pen` 「00A · 窗口与导航」):
/// `pl: 24px, pr: 16px`, bottom `1px $border-subtle`, left `16px` icon + `12px` gap + `16/24 @ 600` title,
/// and an optional trailing slot on the right.
pub fn render_subpage_header_xwjof(
    id: impl Into<gpui_kit::ElementId>,
    icon: gpui_kit::assets::IconName,
    title: impl Into<SharedString>,
    trailing: Option<gpui_kit::AnyElement>,
    palette: &DesktopPalette,
) -> impl gpui_kit::IntoElement {
    use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _, TestSupportExt as _};
    let mut bar = gpui_kit::div()
        .id(id)
        .test_support()
        .w_full()
        .h(height::HEADER)
        .flex_shrink_0()
        .pl(space::GROUP)
        .pr(space::LG)
        .bg(palette.bg_workspace)
        .border_b_1()
        .border_color(palette.border_subtle)
        .flex()
        .items_center()
        .justify_between()
        .child(
            gpui_kit::div()
                .flex()
                .items_center()
                .gap(space::MD)
                .min_w_0()
                .child(
                    gpui_kit::component::Icon::new(icon)
                        .size(px(16.0))
                        .text_color(palette.text_secondary),
                )
                .child(
                    apply_scale(gpui_kit::div(), scale::BODY_SEMIBOLD)
                        .text_color(palette.text_primary)
                        .truncate()
                        .child(cjk_inline_label(title)),
                ),
        );
    if let Some(right_el) = trailing {
        bar = bar.child(right_el);
    }
    bar
}

/// Renders the `28px` keyboard shortcut badge master `ZCGS3` (`cst-pilot-desktop.pen` 「00B · 按钮与输入」):
/// `h: 28px, px: 8px, r: 4px`, `$bg-surface` fill + `1px $border-subtle`, `12/16 @ 500` `JetBrains Mono` `$text-primary`.
pub fn render_kbd_zcgs3(
    keys: impl Into<SharedString>,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> gpui_kit::Div {
    use gpui_kit::{ParentElement as _, Styled as _};
    apply_scale(gpui_kit::div(), scale::CAPTION_MEDIUM)
        .h(height::COMPACT)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_surface)
        .font_family(mono_family)
        .text_color(palette.text_primary)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(keys.into())
}
