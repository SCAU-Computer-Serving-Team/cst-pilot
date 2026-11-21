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
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gpui::{
        Animation, AnimationExt as _, AnyElement, AppContext as _, BorrowAppContext as _, Bounds,
        ContentMask, Element, ElementId, Entity, Global, GlobalElementId, InspectorElementId,
        InteractiveElement as _, LayoutId, MouseButton, Overflow, ParentElement as _, PathBuilder,
        Render, Rgba, StatefulInteractiveElement, Style, Transformation, WeakEntity, WindowId,
        canvas, div, relative,
    };
    use gpui_kit::assets::IconName;
    use gpui_kit::base::motion::{Transition, TransitionId, transition};
    use gpui_kit::base::{TooltipOverlay, TooltipRequest, TooltipTransition};
    use gpui_kit::component::Icon;
    use gpui_kit::{
        App, BoxShadow, Hsla, IntoElement, Pixels, SharedString, Styled, TestSupportExt as _,
        Window, percentage, point, px, size,
    };

    use crate::background::{FrostedGlassSpec, render_frosted_surface};
    use crate::theme::{DesktopThemeGlobal, apply_scale, radius, scale, space};

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

    /// Renders an interruptible vector `checkStroke` path animation (`340ms` check / `150ms` uncheck) via GPUI `canvas` + `PathBuilder::stroke`.
    pub fn animated_checkbox_mark(
        id: impl Into<SharedString>,
        checked: bool,
        size_px: Pixels,
        color: Hsla,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let tid: TransitionId = SharedString::from(format!("chk-{}", id.into())).into();
        let dur = if checked { SLOW } else { SURFACE_CLOSE };
        let p = transition(
            tid,
            if checked { 1.0 } else { 0.0 },
            Transition::new(dur).ease(ease_out()),
            window,
            cx,
        )
        .clamp(0.0, 1.0);
        (p > 0.01).then(|| {
            canvas(
                move |_, _, _| {},
                move |bounds, _, window, _| {
                    let w = f32::from(bounds.size.width);
                    let h = f32::from(bounds.size.height);
                    if w <= 0.0 || h <= 0.0 {
                        return;
                    }
                    let p0 = point(
                        bounds.origin.x + px(w * 0.16),
                        bounds.origin.y + px(h * 0.54),
                    );
                    let p1 = point(
                        bounds.origin.x + px(w * 0.41),
                        bounds.origin.y + px(h * 0.78),
                    );
                    let p2 = point(
                        bounds.origin.x + px(w * 0.85),
                        bounds.origin.y + px(h * 0.24),
                    );
                    let split = 0.34_f32;
                    let stroke_w = px((w * 0.16).clamp(1.4, 2.2));
                    let mut builder = PathBuilder::stroke(stroke_w);
                    builder.move_to(p0);
                    if p <= split {
                        let t = (p / split).clamp(0.0, 1.0);
                        let mid = point(p0.x + (p1.x - p0.x) * t, p0.y + (p1.y - p0.y) * t);
                        builder.line_to(mid);
                    } else {
                        let t = ((p - split) / (1.0 - split)).clamp(0.0, 1.0);
                        let end = point(p1.x + (p2.x - p1.x) * t, p1.y + (p2.y - p1.y) * t);
                        builder.line_to(p1);
                        builder.line_to(end);
                    }
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }
                },
            )
            .size(size_px)
            .into_any_element()
        })
    }

    #[derive(Clone, Copy, Default)]
    struct CollapseHeightState {
        height: Option<Pixels>,
    }

    struct MeasuredCollapse {
        id: ElementId,
        opening: bool,
        progress: f32,
        child: AnyElement,
    }

    impl MeasuredCollapse {
        fn target_height(&self, natural: Pixels) -> Pixels {
            let scaled = natural * self.progress;
            if self.opening {
                scaled.max(natural.min(px(32.0)))
            } else {
                scaled
            }
        }
    }

    impl gpui::IntoElement for MeasuredCollapse {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for MeasuredCollapse {
        type RequestLayoutState = LayoutId;
        type PrepaintState = Bounds<Pixels>;

        fn id(&self) -> Option<ElementId> {
            Some(self.id.clone())
        }

        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            global_id: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            let child_layout_id = self.child.request_layout(window, cx);
            let cached = global_id.and_then(|gid| {
                window.with_element_state(gid, |state: Option<CollapseHeightState>, _| {
                    let s = state.unwrap_or_default();
                    (s.height, s)
                })
            });
            let mut style = Style::default();
            style.size.width = relative(1.0).into();
            style.overflow.y = Overflow::Hidden;
            if let Some(h) = cached {
                style.size.height = self.target_height(h).into();
            }
            let layout_id = window.request_layout(style, [child_layout_id], cx);
            (layout_id, child_layout_id)
        }

        fn prepaint(
            &mut self,
            global_id: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            child_layout_id: &mut Self::RequestLayoutState,
            window: &mut Window,
            cx: &mut App,
        ) -> Self::PrepaintState {
            let child_bounds = window.layout_bounds(*child_layout_id);
            if let Some(gid) = global_id {
                let changed =
                    window.with_element_state(gid, |state: Option<CollapseHeightState>, _| {
                        let mut s = state.unwrap_or_default();
                        let next_h = child_bounds.size.height;
                        let changed = next_h > px(0.0) && s.height != Some(next_h);
                        if next_h > px(0.0) {
                            s.height = Some(next_h);
                        }
                        (changed, s)
                    });
                if changed {
                    window.request_animation_frame();
                }
            }
            let clip_h = bounds
                .size
                .height
                .min(self.target_height(child_bounds.size.height));
            let clip_bounds =
                Bounds::new(bounds.origin, size(bounds.size.width, clip_h.max(px(1.0))));
            window.with_content_mask(
                Some(ContentMask {
                    bounds: clip_bounds,
                }),
                |window| {
                    self.child.prepaint(window, cx);
                },
            );
            clip_bounds
        }

        fn paint(
            &mut self,
            _: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            clip_bounds: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: *clip_bounds,
                }),
                |window| {
                    self.child.paint(window, cx);
                },
            );
        }
    }

    /// Reversible measured height `collapse` transition (`240ms` open / `150ms` close, `ease_out`, `0ms` in `reduce_motion`).
    /// Fully unmounts content once closed (`p <= 0.001`) and drops clipping once settled open (`p >= 0.999`).
    pub fn animated_collapse<C>(
        id: impl Into<SharedString>,
        open: bool,
        window: &mut Window,
        cx: &mut C,
        build_content: impl FnOnce(&mut Window, &mut C) -> AnyElement,
    ) -> Option<AnyElement>
    where
        C: std::ops::DerefMut<Target = App>,
    {
        let key = id.into();
        let tid: TransitionId = SharedString::from(format!("collapse-{key}")).into();
        let dur = if open { NORMAL } else { SURFACE_CLOSE };
        let p = transition(
            tid,
            if open { 1.0_f32 } else { 0.0 },
            Transition::new(dur).ease(ease_out()),
            window,
            cx,
        )
        .clamp(0.0, 1.0);
        if !open && p <= 0.001 {
            return None;
        }
        let content = build_content(window, cx);
        if open && p >= 0.999 {
            return Some(content);
        }
        let reveal_p = if open { p.max(0.15) } else { p };
        let reveal_id = ElementId::Name(SharedString::from(format!("collapse-reveal-{key}")));
        Some(
            div()
                .w_full()
                .opacity(0.01 + 0.99 * reveal_p)
                .child(MeasuredCollapse {
                    id: reveal_id,
                    opening: open,
                    progress: reveal_p,
                    child: content,
                })
                .into_any_element(),
        )
    }

    /// Keyed pressed-state flag for icon buttons so `press` `scale(0.92)` animates smoothly (`120ms ease_out`).
    pub fn icon_press_state(
        id: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<bool> {
        let key = ElementId::Name(SharedString::from(format!(
            "icon-press-state-{}",
            id.into()
        )));
        window.use_keyed_state(key, cx, |_, _| false)
    }

    /// Applies `120ms ease_out` `scale(1.0 -> 0.92)` directly to an SVG `Icon` on press (`0ms` in `reduce_motion`).
    pub fn animated_icon_press(
        id: impl Into<SharedString>,
        pressed: bool,
        icon: Icon,
        window: &mut Window,
        cx: &mut App,
    ) -> Icon {
        let tid: TransitionId = SharedString::from(format!("icon-press-{}", id.into())).into();
        let p = transition(
            tid,
            if pressed { 1.0_f32 } else { 0.0 },
            Transition::new(PRESS).ease(ease_out()),
            window,
            cx,
        )
        .clamp(0.0, 1.0);
        if p <= 0.001 {
            icon
        } else {
            let s = 1.0 - 0.08 * p;
            icon.transform(Transformation::scale(size(s, s)))
        }
    }

    /// Keyed `150ms ease_out` state progress (`0.0 <-> 1.0`) for hover/focus color and shadow interpolation.
    pub fn animated_state_progress(
        id: impl Into<SharedString>,
        active: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        let tid: TransitionId = SharedString::from(format!("state-{}", id.into())).into();
        transition(
            tid,
            if active { 1.0_f32 } else { 0.0 },
            Transition::new(FAST).ease(ease_out()),
            window,
            cx,
        )
        .clamp(0.0, 1.0)
    }

    /// Linearly interpolates two `Hsla` colors in RGBA space so transparent blends never sweep hues.
    pub fn lerp_hsla(from: Hsla, to: Hsla, t: f32) -> Hsla {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return from;
        }
        if t >= 1.0 {
            return to;
        }
        let a = Rgba::from(from);
        let b = Rgba::from(to);
        Hsla::from(Rgba {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: a.a + (b.a - a.a) * t,
        })
    }

    /// Interpolates focus/elevation shadow lists smoothly over `t in [0, 1]`.
    pub fn lerp_shadows(from: &[BoxShadow], to: &[BoxShadow], t: f32) -> Vec<BoxShadow> {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return from.to_vec();
        }
        if t >= 1.0 {
            return to.to_vec();
        }
        if from.is_empty() {
            return scale_shadows(to.to_vec(), t);
        }
        if to.is_empty() {
            return scale_shadows(from.to_vec(), 1.0 - t);
        }
        let len = from.len().max(to.len());
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            match (from.get(i), to.get(i)) {
                (Some(a), Some(b)) => out.push(BoxShadow {
                    color: lerp_hsla(a.color, b.color, t),
                    offset: point(
                        a.offset.x + (b.offset.x - a.offset.x) * t,
                        a.offset.y + (b.offset.y - a.offset.y) * t,
                    ),
                    blur_radius: a.blur_radius + (b.blur_radius - a.blur_radius) * t,
                    spread_radius: a.spread_radius + (b.spread_radius - a.spread_radius) * t,
                    inset: a.inset,
                }),
                (Some(a), None) => {
                    let mut s = a.clone();
                    s.color.a *= 1.0 - t;
                    out.push(s);
                }
                (None, Some(b)) => {
                    let mut s = b.clone();
                    s.color.a *= t;
                    out.push(s);
                }
                (None, None) => {}
            }
        }
        out
    }

    #[derive(Clone, Copy, Default)]
    pub struct FlipRowState {
        last_y: Option<f32>,
        last_epoch: u64,
        anim: Option<(Instant, f32)>,
    }

    /// Tracks vertical `FLIP` (`250ms ease_out`) reorder deltas for list rows without triggering on normal scroll.
    /// Callers bump `reorder_epoch` only when list order changes (e.g. `Alt+Up/Down`, queue move, session reorder).
    pub fn sample_flip_offset_y(
        list_id: &str,
        item_key: &str,
        reorder_epoch: u64,
        window: &mut Window,
        cx: &mut App,
    ) -> (Pixels, Entity<FlipRowState>, u64) {
        let state_id = ElementId::Name(SharedString::from(format!("flip-{list_id}-{item_key}")));
        let state = window.use_keyed_state(state_id, cx, |_, _| FlipRowState {
            last_y: None,
            last_epoch: reorder_epoch,
            anim: None,
        });
        let snap = *state.read(cx);
        let offset = if cx.reduce_motion() {
            if snap.anim.is_some() {
                state.update(cx, |s, _| s.anim = None);
            }
            px(0.0)
        } else if let Some((start, delta_y)) = snap.anim {
            let elapsed = cx
                .background_executor()
                .now()
                .saturating_duration_since(start);
            if elapsed < FLIP_MOVE {
                window.request_animation_frame();
                let t =
                    ease_out()((elapsed.as_secs_f32() / FLIP_MOVE.as_secs_f32()).clamp(0.0, 1.0));
                px(delta_y * (1.0 - t))
            } else {
                state.update(cx, |s, _| s.anim = None);
                px(0.0)
            }
        } else {
            px(0.0)
        };
        (offset, state, reorder_epoch)
    }

    /// Updates a `FLIP` row's measured `origin.y` during prepaint; triggers animation only when `reorder_epoch` advanced.
    pub fn record_flip_bounds_y(
        state: &Entity<FlipRowState>,
        reorder_epoch: u64,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let new_y = f32::from(bounds.origin.y);
        let reduce_motion = cx.reduce_motion();
        let now = cx.background_executor().now();
        let mut needs_frame = false;
        state.update(cx, |s, _| {
            if let Some(prev_y) = s.last_y
                && reorder_epoch != s.last_epoch
            {
                let delta = prev_y - new_y;
                if !reduce_motion && delta.abs() >= 1.0 && delta.abs() <= 480.0 {
                    s.anim = Some((now, delta));
                    needs_frame = true;
                } else {
                    s.anim = None;
                }
            }
            s.last_y = Some(new_y);
            s.last_epoch = reorder_epoch;
        });
        if needs_frame {
            window.request_animation_frame();
        }
    }

    #[derive(Default)]
    struct DesktopTooltipRegistry {
        overlays: HashMap<WindowId, WeakEntity<TooltipOverlay>>,
    }

    impl Global for DesktopTooltipRegistry {}

    /// Registers a window's shared [`TooltipOverlay`] so all icon buttons and warning tooltips share one cooldown group.
    pub fn register_window_tooltip_overlay(
        window: &Window,
        overlay: &Entity<TooltipOverlay>,
        cx: &mut App,
    ) {
        if cx.try_global::<DesktopTooltipRegistry>().is_none() {
            cx.set_global(DesktopTooltipRegistry::default());
        }
        cx.update_global::<DesktopTooltipRegistry, _>(|reg, _| {
            reg.overlays.retain(|_, weak| weak.upgrade().is_some());
            reg.overlays
                .insert(window.window_handle().window_id(), overlay.downgrade());
        });
    }

    /// Returns the registered [`TooltipOverlay`] for `window`, if mounted.
    pub fn window_tooltip_overlay(window: &Window, cx: &App) -> Option<Entity<TooltipOverlay>> {
        let reg = cx.try_global::<DesktopTooltipRegistry>()?;
        reg.overlays
            .get(&window.window_handle().window_id())
            .and_then(WeakEntity::upgrade)
    }

    /// Builds the shared [`TooltipOverlay`] with `storybook.html` enter/switch motion (`150ms` enter / `200ms` slide).
    pub fn create_desktop_tooltip_overlay(cx: &mut App) -> Entity<TooltipOverlay> {
        cx.new(|_| {
            TooltipOverlay::new().render_with(|content_view, transition, _window, cx| {
                if cx.reduce_motion() {
                    return div().child(content_view).into_any_element();
                }
                match transition {
                    TooltipTransition::Enter { epoch } => div()
                        .child(content_view)
                        .with_animation(
                            ElementId::NamedInteger("desktop-tooltip-enter".into(), epoch as u64),
                            Animation::new(FAST).with_easing(ease_out()),
                            |el, d| {
                                let t = d.clamp(0.0, 1.0);
                                el.opacity(0.01 + 0.99 * t).mt(px(4.0 * (1.0 - t)))
                            },
                        )
                        .into_any_element(),
                    TooltipTransition::Switch {
                        epoch,
                        previous,
                        current,
                    } => {
                        let same_row = (current.origin.y - previous.origin.y).abs() < px(12.0);
                        if !same_row {
                            return div().child(content_view).into_any_element();
                        }
                        let dx = f32::from(current.center().x - previous.center().x)
                            .clamp(-240.0, 240.0);
                        div()
                            .child(content_view)
                            .with_animation(
                                ElementId::NamedInteger(
                                    "desktop-tooltip-switch".into(),
                                    epoch as u64,
                                ),
                                Animation::new(Duration::from_millis(200))
                                    .with_easing(ease_in_out()),
                                move |el, d| {
                                    let t = d.clamp(0.0, 1.0);
                                    el.ml(px(-dx * (1.0 - t)))
                                },
                            )
                            .into_any_element()
                    }
                }
            })
        })
    }

    struct DesktopTooltipContentView {
        elem_id: SharedString,
        text: SharedString,
        frosted: bool,
    }

    impl Render for DesktopTooltipContentView {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let theme = DesktopThemeGlobal::current(cx);
            let palette = theme.palette;
            if self.frosted {
                let transparency = theme.transparency_enabled;
                let scale_factor = window.scale_factor();
                return render_frosted_surface(
                    self.elem_id.clone(),
                    FrostedGlassSpec::popover_h2p2z(&palette),
                    palette.bg_elevated,
                    transparency,
                    scale_factor,
                )
                .test_support()
                .max_w(px(320.0))
                .px(space::SM)
                .py(space::XS)
                .rounded(radius::CONTROL)
                .border_1()
                .border_color(palette.border_hairline)
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .relative()
                        .text_color(palette.text_primary)
                        .child(self.text.clone()),
                )
                .into_any_element();
            }

            div()
                .id(self.elem_id.clone())
                .test_support()
                .max_w(px(280.0))
                .px(space::SM)
                .py(space::XS)
                .rounded(radius::CONTROL)
                .bg(palette.bg_elevated)
                .border_1()
                .border_color(palette.border_hairline)
                .shadow(palette.craft_lift_shadows())
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.text_primary)
                        .child(self.text.clone()),
                )
                .into_any_element()
        }
    }

    /// Attaches a tooltip backed by the window's shared [`TooltipOverlay`] (`400ms` delay / `300ms` cooldown group).
    /// Also optionally updates a keyed `Entity<bool>` hover state inside the single GPUI `on_hover` listener.
    pub trait DesktopTooltipExt:
        StatefulInteractiveElement + gpui_kit::component::ElementExt + Sized
    {
        fn desktop_tooltip(
            self,
            trigger_key: impl Into<SharedString>,
            text: impl Into<SharedString>,
        ) -> Self {
            let key = trigger_key.into();
            let elem_id = SharedString::from(format!("tooltip-{key}"));
            self.desktop_tooltip_with_hover(elem_id, text.into(), false, None)
        }

        fn desktop_tooltip_with_hover(
            self,
            tooltip_elem_id: SharedString,
            text: SharedString,
            frosted: bool,
            hover_state: Option<Entity<bool>>,
        ) -> Self {
            let bounds_cell: Rc<RefCell<Bounds<Pixels>>> = Rc::new(RefCell::new(Bounds::default()));
            let bounds_writer = bounds_cell.clone();
            let bounds_reader = bounds_cell.clone();
            self.on_prepaint(move |bounds, _, _| {
                *bounds_writer.borrow_mut() = bounds;
            })
            .on_hover(move |hovered, window, cx| {
                if let Some(ref hs) = hover_state {
                    hs.update(cx, |s, cx| {
                        if *s != *hovered {
                            *s = *hovered;
                            cx.notify();
                        }
                    });
                }
                let Some(overlay) = window_tooltip_overlay(window, cx) else {
                    return;
                };
                if *hovered {
                    let trigger_bounds = *bounds_reader.borrow();
                    let elem_id = tooltip_elem_id.clone();
                    let label = text.clone();
                    overlay.update(cx, |o, cx| {
                        let req = TooltipRequest::new(trigger_bounds, move |_, cx| {
                            let elem_id = elem_id.clone();
                            let label = label.clone();
                            cx.new(|_| DesktopTooltipContentView {
                                elem_id,
                                text: label,
                                frosted,
                            })
                            .into()
                        })
                        .with_offset(space::XS);
                        o.request_show(req, window, cx);
                    });
                } else {
                    overlay.update(cx, |o, cx| {
                        o.request_hide(window, cx);
                    });
                }
            })
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                if let Some(overlay) = window_tooltip_overlay(window, cx) {
                    overlay.update(cx, |o, cx| o.hide(cx));
                }
            })
        }
    }

    impl<E: StatefulInteractiveElement + gpui_kit::component::ElementExt> DesktopTooltipExt for E {}

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
