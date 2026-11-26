pub mod kbd_focus;
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

    pub const SIDEBAR_WIDTH: Pixels = px(248.0);
    pub const TOPBAR_HEIGHT: Pixels = px(40.0);
    pub const SHELL_FRAME_INSET: Pixels = px(8.0);
    pub const CENTER_COLUMN_MAX_WIDTH: Pixels = px(816.0);
    pub const SUBPAGE_HORIZONTAL_INSET: Pixels = px(64.0);
    pub const MODAL_STANDARD_WIDTH: Pixels = px(384.0);
    pub const PALETTE_MODAL_WIDTH: Pixels = px(540.0);
    pub const LOGIN_SHADER_WIDTH: Pixels = px(864.0);
}

mod tooltip;

/// Motion curves & durations strictly aligned with `design/storybook.html`.
pub mod motion {
    use std::time::{Duration, Instant};

    use gpui::{
        Animation, AnimationExt as _, AnyElement, ElementId, Entity, InteractiveElement as _,
        MouseButton, ParentElement as _, PathBuilder, Rgba, StatefulInteractiveElement,
        Subscription, Transformation, canvas, div,
    };
    use gpui_kit::assets::IconName;
    use gpui_kit::base::motion::{Transition, TransitionId, transition};
    use gpui_kit::component::Icon;
    use gpui_kit::{
        App, BoxShadow, Hsla, IntoElement, Pixels, SharedString, Styled, Window, percentage, point,
        px, size,
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
    pub const SPINNER_CYCLE: Duration = Duration::from_millis(850);
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

    /// Applies single-signal interruptible modal motion driven by [`surface_presence`] (`0.0 <-> 1.0`).
    /// Uses integer-pixel relative offset (`ADR-008` ClearType boundary) and cubic `p^3` effective shadow ramp
    /// (`p^2` shadow alpha × container `opacity(p)`).
    pub fn apply_modal_motion<E: Styled>(el: E, progress: f32, shadows: Vec<BoxShadow>) -> E {
        let p = progress.clamp(0.0, 1.0);
        let dy = (6.0 * (1.0 - p)).round();
        let el = el.relative().top(px(dy)).opacity(0.01 + 0.99 * p);
        if shadows.is_empty() {
            el
        } else {
            el.shadow(scale_shadows(shadows, p * p))
        }
    }

    /// Applies single-signal interruptible popover motion driven by [`surface_presence`] (`0.0 <-> 1.0`).
    /// Uses integer-pixel relative offset (`ADR-008` ClearType boundary) and cubic `p^3` effective shadow ramp.
    pub fn apply_popover_motion<E: Styled>(
        el: E,
        progress: f32,
        slide_y_px: f32,
        shadows: Vec<BoxShadow>,
    ) -> E {
        let p = progress.clamp(0.0, 1.0);
        let dy = (slide_y_px * (1.0 - p)).round();
        let el = el.relative().top(px(dy)).opacity(0.01 + 0.99 * p);
        if shadows.is_empty() {
            el
        } else {
            el.shadow(scale_shadows(shadows, p * p))
        }
    }

    /// Compatibility one-shot popover entrance helper retained for unmigrated call sites.
    /// Prefer [`surface_presence`] + [`apply_popover_motion`] for interruptible open/close cycles.
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

    /// One-shot upward glide (`240ms` `ease_out`, `+14px -> 0` + fade `0.01 -> 1.0`) for newly
    /// mounted special diagnostic cards. Keyed via `window.use_keyed_state` so subsequent
    /// streaming updates never restart the entrance; `0ms` when `reduce_motion` or `!animate`.
    pub fn animate_card_enter(
        id: impl Into<SharedString>,
        el: AnyElement,
        animate: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let key = id.into();
        let reduce_motion = cx.reduce_motion();
        let now = cx.background_executor().now();
        let state_id = SharedString::from(format!("special-card-enter-{key}"));
        let state = window.use_keyed_state(state_id, cx, |_, _| {
            if animate && !reduce_motion {
                Some(now)
            } else {
                None
            }
        });
        if reduce_motion {
            state.update(cx, |started, _| *started = None);
            return el;
        }
        let started_opt = *state.read(cx);
        let Some(started) = started_opt else {
            return el;
        };
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= NORMAL {
            state.update(cx, |s, _| *s = None);
            return el;
        }
        window.request_animation_frame();
        let raw = (elapsed.as_secs_f32() / NORMAL.as_secs_f32()).clamp(0.0, 1.0);
        let t = ease_out()(raw);
        div()
            .w_full()
            .relative()
            .top(px(14.0 * (1.0 - t)))
            .opacity(0.01 + 0.99 * t)
            .child(el)
            .into_any_element()
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

    /// Continuous `850ms` linear spinner (`storybook.html` `.spin-icon`) using GPU SVG rotation.
    /// Freezes at `0 FPS` when `!animate_allowed`, `reduce_motion`, unfocused, minimized, or scrolled offscreen.
    pub fn animated_spinner(
        id: impl Into<SharedString>,
        size_px: Pixels,
        color: Hsla,
        animate_allowed: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let vp = window.viewport_size();
        let minimized = f32::from(vp.width) <= 1.0 || f32::from(vp.height) <= 1.0;
        let should_spin = animate_allowed
            && !cx.reduce_motion()
            && window.is_window_active()
            && window.is_visible()
            && !minimized;
        if !should_spin {
            return Icon::new(IconName::LoaderCircle)
                .size(size_px)
                .text_color(color)
                .into_any_element();
        }
        let state_id = SharedString::from(format!("spinner-start-{}", id.into()));
        let started_at =
            window.use_keyed_state(state_id, cx, |_, cx| cx.background_executor().now());
        let elapsed_ms = cx
            .background_executor()
            .now()
            .saturating_duration_since(*started_at.read(cx))
            .as_millis();
        let cycle_ms = SPINNER_CYCLE.as_millis();
        let turns = (elapsed_ms % cycle_ms) as f32 / cycle_ms as f32;
        div()
            .relative()
            .size(size_px)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                canvas(
                    |bounds, window, _| {
                        if bounds.intersects(&window.content_mask().bounds) {
                            window.request_animation_frame();
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                Icon::new(IconName::LoaderCircle)
                    .size(size_px)
                    .text_color(color)
                    .rotate(percentage(turns)),
            )
            .into_any_element()
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

    /// Animates a modal backdrop scrim fade-in (`240ms` `ease_out`, `0ms` in `reduce_motion`).
    pub fn animate_scrim_enter<E: IntoElement + Styled + 'static>(
        id: impl Into<gpui::ElementId>,
        el: E,
    ) -> impl IntoElement {
        el.with_animation(
            id,
            Animation::new(MODAL_OPEN).with_easing(ease_out()),
            move |el, d| {
                let raw = d.clamp(0.0, 1.0);
                el.opacity(0.01 + 0.99 * raw)
            },
        )
    }

    /// Samples an interruptible `0.0 <-> 1.0` surface presence transition (`open_dur` open / `SURFACE_CLOSE` close).
    pub fn surface_presence(
        id: impl Into<TransitionId>,
        open: bool,
        open_dur: Duration,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        transition(
            id,
            if open { 1.0_f32 } else { 0.0 },
            Transition::new(if open { open_dur } else { SURFACE_CLOSE }).ease(ease_out()),
            window,
            cx,
        )
        .clamp(0.0, 1.0)
    }

    /// Keyed `120ms ease_out` press progress (`0.0 <-> 1.0`) for button press tint feedback.
    pub fn animated_press_progress(
        id: impl Into<SharedString>,
        pressed: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        let tid: TransitionId = SharedString::from(format!("press-bg-{}", id.into())).into();
        transition(
            tid,
            if pressed { 1.0_f32 } else { 0.0 },
            Transition::new(PRESS).ease(ease_out()),
            window,
            cx,
        )
        .clamp(0.0, 1.0)
    }

    /// Renders an interruptible vector `checkStroke` path animation (`340ms ease_spring` check / `150ms ease_out` uncheck) via GPUI `canvas` + `PathBuilder::stroke`.
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
        let t = if checked {
            Transition::new(dur).ease(ease_spring())
        } else {
            Transition::new(dur).ease(ease_out())
        };
        let p = transition(tid, if checked { 1.0 } else { 0.0 }, t, window, cx).clamp(0.0, 1.0);
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

    struct CollapseState {
        height: Pixels,
        body_focus: gpui::FocusHandle,
    }

    /// Context adapter allowing `animated_collapse*` to accept either `&mut App` or `&mut Context<T>`.
    pub trait CollapseAppCx {
        fn as_app(&self) -> &App;
        fn as_app_mut(&mut self) -> &mut App;
    }

    impl CollapseAppCx for App {
        fn as_app(&self) -> &App {
            self
        }

        fn as_app_mut(&mut self) -> &mut App {
            self
        }
    }

    impl<T: 'static> CollapseAppCx for gpui::Context<'_, T> {
        fn as_app(&self) -> &App {
            self
        }

        fn as_app_mut(&mut self) -> &mut App {
            self
        }
    }

    /// Measures the unconstrained body even at rest, so nested and streaming
    /// content can resize. Only the measured space remains during the exit.
    pub fn animated_collapse<C: CollapseAppCx>(
        id: impl Into<SharedString>,
        open: bool,
        window: &mut Window,
        cx: &mut C,
        build_content: impl FnOnce(&mut Window, &mut C) -> AnyElement,
    ) -> Option<AnyElement> {
        animated_collapse_impl(id, open, false, window, cx, |_, window, cx| {
            build_content(window, cx)
        })
    }

    /// Interruptible collapse (`240ms` open / `150ms` close) that preserves a visual-only
    /// (`interactive = false`) snapshot of the content while closing so collapsed panels
    /// clip and fade smoothly instead of flashing a blank gap.
    pub fn animated_collapse_with_exit<C: CollapseAppCx>(
        id: impl Into<SharedString>,
        open: bool,
        window: &mut Window,
        cx: &mut C,
        build_content: impl FnOnce(bool, &mut Window, &mut C) -> AnyElement,
    ) -> Option<AnyElement> {
        animated_collapse_impl(id, open, true, window, cx, build_content)
    }

    fn animated_collapse_impl<C: CollapseAppCx>(
        id: impl Into<SharedString>,
        open: bool,
        render_inert_on_exit: bool,
        window: &mut Window,
        cx: &mut C,
        build_content: impl FnOnce(bool, &mut Window, &mut C) -> AnyElement,
    ) -> Option<AnyElement> {
        use gpui_kit::component::ElementExt as _;

        let key = id.into();
        let state_id = SharedString::from(format!("collapse-measure-{key}"));
        let state = window.use_keyed_state(state_id, cx.as_app_mut(), |_, cx| CollapseState {
            height: px(0.0),
            body_focus: cx.focus_handle(),
        });
        let height = state.read(cx.as_app()).height;
        let body_focus = state.read(cx.as_app()).body_focus.clone();
        let duration = if open { NORMAL } else { SURFACE_CLOSE };
        let progress = transition(
            SharedString::from(format!("collapse-{key}")),
            if open { 1.0_f32 } else { 0.0 },
            Transition::new(duration).ease(ease_out()),
            window,
            cx.as_app_mut(),
        )
        .clamp(0.0, 1.0);

        if !open {
            if body_focus.contains_focused(window, cx.as_app()) {
                window.blur(cx.as_app_mut());
            }
            if progress <= 0.001 || height <= px(0.0) {
                return None;
            }
            if render_inert_on_exit {
                let inert_content = build_content(false, window, cx);
                return Some(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .flex_shrink_0()
                        .h(height * progress)
                        .overflow_hidden()
                        .opacity(0.01 + 0.99 * progress)
                        .child(div().w_full().flex_shrink_0().child(inert_content))
                        .into_any_element(),
                );
            }
            return Some(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .h(height * progress)
                    .into_any_element(),
            );
        }

        let settled = progress >= 0.999;
        let reveal = progress.max(0.01);
        let content = build_content(true, window, cx);
        let body = div()
            .id(key)
            .track_focus(&body_focus)
            .w_full()
            .flex_shrink_0()
            .on_prepaint(move |bounds, window, cx| {
                if state.read(cx).height != bounds.size.height {
                    state.update(cx, |state, _| state.height = bounds.size.height);
                    if !settled {
                        window.request_animation_frame();
                    }
                }
            })
            .child(content);
        let wrapper = div().w_full().flex().flex_col().flex_shrink_0().child(body);
        Some(if settled {
            wrapper.into_any_element()
        } else {
            let clip_h = if height > px(0.0) {
                (height * reveal).max(px(1.0))
            } else {
                px(1.0)
            };
            wrapper
                .max_h(clip_h)
                .overflow_hidden()
                .opacity(0.01 + 0.99 * progress)
                .into_any_element()
        })
    }

    pub struct PointerFlag {
        active: bool,
        _activation: Subscription,
    }

    impl PointerFlag {
        pub(crate) fn keyed(
            id: impl Into<ElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (bool, Entity<Self>) {
            let state =
                window.use_keyed_state(id, cx, |window, cx: &mut gpui::Context<Self>| Self {
                    active: false,
                    _activation: cx.observe_window_activation(window, |this, window, cx| {
                        if !window.is_window_active() && this.active {
                            this.active = false;
                            cx.notify();
                        }
                    }),
                });
            if !window.is_window_active() || !window.is_window_hovered() {
                Self::set(&state, false, cx);
            }
            (state.read(cx).active, state)
        }

        pub(crate) fn set(state: &Entity<Self>, active: bool, cx: &mut App) {
            if state.read(cx).active != active {
                state.update(cx, |this, cx| {
                    this.active = active;
                    cx.notify();
                });
            }
        }

        pub(crate) fn enter(state: &Entity<Self>, window: &Window, cx: &mut App) {
            Self::set(
                state,
                window.is_window_active() && window.is_window_hovered(),
                cx,
            );
        }
    }

    pub(crate) fn icon_press_state(
        id: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> (bool, Entity<PointerFlag>) {
        PointerFlag::keyed(
            SharedString::from(format!("icon-press-state-{id}")),
            window,
            cx,
        )
    }

    pub(crate) trait IconPressExt: StatefulInteractiveElement {
        fn on_icon_press(self, state: Entity<PointerFlag>) -> Self {
            let down = state.clone();
            let up = state.clone();
            let motion = state.clone();
            self.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                PointerFlag::enter(&down, window, cx);
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                PointerFlag::set(&up, false, cx)
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                PointerFlag::set(&state, false, cx)
            })
            .on_mouse_move(move |event, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    PointerFlag::set(&motion, false, cx);
                }
            })
        }
    }

    impl<E: StatefulInteractiveElement> IconPressExt for E {}

    #[cfg(all(test, windows))]
    #[gpui::test]
    fn icon_press_clears_on_native_pointer_leave_and_deactivation(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, Context, InputEvent as _, MouseDownEvent, Render};
        use gpui_kit::test::TestWindowExt as _;
        use std::{cell::Cell, rc::Rc};

        struct Probe(Rc<Cell<bool>>);
        impl Render for Probe {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let (pressed, state) = icon_press_state("press-probe", window, cx);
                self.0.set(pressed);
                div()
                    .size_full()
                    .child(div().id("press-probe").size(px(40.0)).on_icon_press(state))
            }
        }

        let pressed = Rc::new(Cell::new(false));
        let win = cx.add_window(|_, _| Probe(pressed.clone()));
        let draw = |cx: &mut gpui::TestAppContext| {
            cx.update_window(*win, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };
        let down = |cx: &mut gpui::TestAppContext| {
            cx.update_window(*win, |_, window, cx| {
                window.render_frame(cx);
                window.dispatch_event(
                    MouseDownEvent {
                        button: MouseButton::Left,
                        position: point(px(20.0), px(20.0)),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();
            draw(cx);
            assert!(pressed.get());
        };
        cx.simulate_window_pointer_state(*win, true, true);
        down(cx);
        cx.simulate_window_pointer_state(*win, true, false);
        draw(cx);
        assert!(!pressed.get());

        cx.simulate_window_pointer_state(*win, true, true);
        down(cx);
        cx.simulate_window_pointer_state(*win, false, false);
        // Reactivate before a redraw; the activation observer must still reset the flag.
        cx.simulate_window_pointer_state(*win, true, true);
        draw(cx);
        assert!(!pressed.get());
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

    /// Linearly interpolates two `Hsla` colors in premultiplied RGBA space so blending opaque dark
    /// surfaces with low-alpha light overlays (e.g. `bg_item_active` <-> `bg_active_press`) never flashes white.
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
        let out_a = a.a + (b.a - a.a) * t;
        if out_a <= 1e-5 {
            return gpui_kit::transparent_black();
        }
        let pr = a.r * a.a + (b.r * b.a - a.r * a.a) * t;
        let pg = a.g * a.a + (b.g * b.a - a.g * a.a) * t;
        let pb = a.b * a.a + (b.b * b.a - a.b * a.a) * t;
        Hsla::from(Rgba {
            r: (pr / out_a).clamp(0.0, 1.0),
            g: (pg / out_a).clamp(0.0, 1.0),
            b: (pb / out_a).clamp(0.0, 1.0),
            a: out_a.clamp(0.0, 1.0),
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

    struct FlipRowState {
        scope: SharedString,
        y: Pixels,
        anim: Option<(Instant, Pixels)>,
    }

    /// FLIP for fixed-stride rows. Use the unanimated list-local position so
    /// scrolling, geometry and the previous transform cannot enter the delta.
    /// A filter or session change establishes a new baseline.
    pub fn flip_row_offset_y(
        list_id: &'static str,
        item_key: &str,
        scope: &SharedString,
        y: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Pixels {
        flip_row_offset_y_with_duration(list_id, item_key, scope, y, FLIP_MOVE, window, cx)
    }

    pub fn flip_row_offset_y_with_duration(
        list_id: &'static str,
        item_key: &str,
        scope: &SharedString,
        y: Pixels,
        duration: Duration,
        window: &mut Window,
        cx: &mut App,
    ) -> Pixels {
        let state = window.with_element_namespace(list_id, |window| {
            window.use_keyed_state(SharedString::from(item_key.to_owned()), cx, |_, _| {
                FlipRowState {
                    scope: scope.clone(),
                    y,
                    anim: None,
                }
            })
        });
        let now = cx.background_executor().now();
        let reduce_motion = cx.reduce_motion();
        let dur_secs = duration.as_secs_f32().max(0.001);
        let fast_curve = duration <= FAST;
        let offset = state.update(cx, |state, _| {
            let mut offset = state.anim.map_or(px(0.0), |(start, delta)| {
                let elapsed = now.saturating_duration_since(start);
                let progress = (elapsed.as_secs_f32() / dur_secs).min(1.0);
                let eased = if fast_curve {
                    ease_out()(progress)
                } else {
                    ease_in_out()(progress)
                };
                delta * (1.0 - eased)
            });
            if reduce_motion || state.scope != *scope {
                offset = px(0.0);
                state.anim = None;
            } else if state.y != y {
                let delta = state.y + offset - y;
                state.anim =
                    (delta.abs() >= px(1.0) && delta.abs() <= px(480.0)).then_some((now, delta));
                offset = state.anim.map_or(px(0.0), |(_, delta)| delta);
            } else if offset.abs() < px(0.01) {
                offset = px(0.0);
                state.anim = None;
            }
            state.scope = scope.clone();
            state.y = y;
            offset
        });
        if offset != px(0.0) {
            window.request_animation_frame();
        }
        offset
    }

    pub use super::tooltip::{DesktopTooltipExt, DesktopTooltips, window_tooltip_overlay};

    /// Segment of an inline status label partitioned into stationary text vs contiguous ASCII digits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LabelSegment<'a> {
        Static(&'a str),
        Digits(&'a str),
    }

    /// Partitions `text` into alternating `Static` and contiguous ASCII `Digits` (`0..=9`) segments.
    pub fn split_label_digits(text: &str) -> Vec<LabelSegment<'_>> {
        let mut out = Vec::new();
        let bytes = text.as_bytes();
        let mut idx = 0usize;
        while idx < bytes.len() {
            let start = idx;
            let is_digit = bytes[idx].is_ascii_digit();
            idx += 1;
            while idx < bytes.len() && bytes[idx].is_ascii_digit() == is_digit {
                idx += 1;
            }
            let slice = &text[start..idx];
            if is_digit {
                out.push(LabelSegment::Digits(slice));
            } else {
                out.push(LabelSegment::Static(slice));
            }
        }
        out
    }

    /// Minimal transition plan between two inline status labels.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DigitDiffPlan {
        /// Strings are identical; zero motion.
        Unchanged,
        /// Non-digit static skeleton changed (e.g. `工作中 52s` -> `已工作 52.3s`); cross-slide whole label.
        FullSwap,
        /// Static skeleton is identical; only changed decimal places roll independently.
        /// Each tuple is `(digit_run_idx, right_place_idx, prev_digit, next_digit, roll_up)`.
        DigitsOnly(Vec<(usize, usize, Option<char>, char, bool)>),
    }

    /// Computes the minimal rolling diff between `prev` and `next`.
    ///
    /// Algorithm specification:
    /// 1. Split both strings into alternating `Static` and `Digits` runs via [`split_label_digits`].
    /// 2. If run count, run kinds, or any `Static` slice differs, return [`DigitDiffPlan::FullSwap`].
    /// 3. For each `Digits` run, compare right-to-left by decimal place value (`place = 0` is ones,
    ///    `place = 1` is tens, ...). Unchanged places stay stationary.
    /// 4. Roll direction is upward (`roll_up = true`) when numeric value increases or digit count grows,
    ///    and downward (`roll_up = false`) when numeric value decreases.
    pub fn plan_label_digit_diff(prev: &str, next: &str) -> DigitDiffPlan {
        if prev == next {
            return DigitDiffPlan::Unchanged;
        }
        let prev_segs = split_label_digits(prev);
        let next_segs = split_label_digits(next);
        if prev_segs.len() != next_segs.len() {
            return DigitDiffPlan::FullSwap;
        }
        let mut digit_run_idx = 0usize;
        let mut changes = Vec::new();
        for (p, n) in prev_segs.iter().zip(next_segs.iter()) {
            match (p, n) {
                (LabelSegment::Static(ps), LabelSegment::Static(ns)) => {
                    if ps != ns {
                        return DigitDiffPlan::FullSwap;
                    }
                }
                (LabelSegment::Digits(pd), LabelSegment::Digits(nd)) => {
                    if pd != nd {
                        let roll_up = match nd.len().cmp(&pd.len()) {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Less => false,
                            std::cmp::Ordering::Equal => nd >= pd,
                        };
                        let p_bytes = pd.as_bytes();
                        let n_bytes = nd.as_bytes();
                        for (pos, &nb) in n_bytes.iter().enumerate() {
                            let place = n_bytes.len() - 1 - pos;
                            let prev_digit = p_bytes
                                .len()
                                .checked_sub(1 + place)
                                .map(|idx| p_bytes[idx] as char);
                            let next_digit = nb as char;
                            if prev_digit != Some(next_digit) {
                                changes.push((
                                    digit_run_idx,
                                    place,
                                    prev_digit,
                                    next_digit,
                                    roll_up,
                                ));
                            }
                        }
                    }
                    digit_run_idx += 1;
                }
                _ => return DigitDiffPlan::FullSwap,
            }
        }
        if changes.is_empty() {
            DigitDiffPlan::FullSwap
        } else {
            DigitDiffPlan::DigitsOnly(changes)
        }
    }

    #[derive(Clone, Copy)]
    struct DigitSlotSwap {
        prev: Option<char>,
        roll_up: bool,
        started_at: Instant,
    }

    struct StatusLabelState {
        current: SharedString,
        full_swap: Option<(SharedString, Instant)>,
        digit_swaps: std::collections::BTreeMap<(usize, usize), DigitSlotSwap>,
    }

    #[derive(Clone, Copy)]
    struct ActiveDigitSwap {
        prev: Option<char>,
        progress: f32,
        dir: f32,
    }

    type ActiveDigitSwaps = std::collections::BTreeMap<(usize, usize), ActiveDigitSwap>;

    /// Digit-aware inline status label with `RunningLight` per-grapheme highlight sweep.
    ///
    /// When only numbers change (e.g. `工作中 52s` -> `工作中 53s` or `59s` -> `60s`), stationary
    /// characters stay pinned while ONLY the changed decimal places roll vertically (`180ms ease_out`,
    /// `±6px`). When the static skeleton changes (e.g. `工作中 52s` -> `已工作 52.3s`), performs a single
    /// whole-label cross-slide.
    #[allow(clippy::too_many_arguments)]
    pub fn animated_status_label(
        id: impl Into<SharedString>,
        text: impl Into<SharedString>,
        sweep_allowed: bool,
        sweep_started_at: Instant,
        base_color: Hsla,
        highlight_color: Hsla,
        typography: super::TypographyScale,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let key = id.into();
        let next_text = text.into();
        let state_id = SharedString::from(format!("status-label-swap-{key}"));
        let initial_text = next_text.clone();
        let state = window.use_keyed_state(state_id, cx, |_, _| StatusLabelState {
            current: initial_text,
            full_swap: None,
            digit_swaps: std::collections::BTreeMap::new(),
        });
        let now = cx.background_executor().now();
        let reduce_motion = cx.reduce_motion();
        let swap_duration = FLIP_ENTER;

        let (active_full_swap, active_digits): (Option<(SharedString, f32)>, ActiveDigitSwaps) =
            state.update(cx, |st, _| {
                if st.current != next_text {
                    if reduce_motion {
                        st.current = next_text.clone();
                        st.full_swap = None;
                        st.digit_swaps.clear();
                    } else {
                        match plan_label_digit_diff(st.current.as_ref(), next_text.as_ref()) {
                            DigitDiffPlan::Unchanged => {}
                            DigitDiffPlan::FullSwap => {
                                let prev = std::mem::replace(&mut st.current, next_text.clone());
                                st.full_swap = Some((prev, now));
                                st.digit_swaps.clear();
                            }
                            DigitDiffPlan::DigitsOnly(changes) => {
                                st.current = next_text.clone();
                                st.full_swap = None;
                                for (run_idx, place, prev_digit, _next_digit, roll_up) in changes {
                                    st.digit_swaps.insert(
                                        (run_idx, place),
                                        DigitSlotSwap {
                                            prev: prev_digit,
                                            roll_up,
                                            started_at: now,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                if reduce_motion {
                    st.full_swap = None;
                    st.digit_swaps.clear();
                    return (None, ActiveDigitSwaps::new());
                }

                let full = if let Some((prev, started)) = st.full_swap.clone() {
                    let elapsed = now.saturating_duration_since(started);
                    if elapsed < swap_duration {
                        let raw =
                            (elapsed.as_secs_f32() / swap_duration.as_secs_f32()).clamp(0.0, 1.0);
                        Some((prev, ease_out()(raw)))
                    } else {
                        st.full_swap = None;
                        None
                    }
                } else {
                    None
                };

                let mut digits = ActiveDigitSwaps::new();
                st.digit_swaps.retain(|&(run_idx, place), swap| {
                    let elapsed = now.saturating_duration_since(swap.started_at);
                    if elapsed < swap_duration {
                        let raw =
                            (elapsed.as_secs_f32() / swap_duration.as_secs_f32()).clamp(0.0, 1.0);
                        let dir = if swap.roll_up { 1.0 } else { -1.0 };
                        digits.insert(
                            (run_idx, place),
                            ActiveDigitSwap {
                                prev: swap.prev,
                                progress: ease_out()(raw),
                                dir,
                            },
                        );
                        true
                    } else {
                        false
                    }
                });

                (full, digits)
            });

        let vp = window.viewport_size();
        let minimized = f32::from(vp.width) <= 1.0 || f32::from(vp.height) <= 1.0;
        let should_sweep = sweep_allowed
            && !reduce_motion
            && window.is_window_active()
            && window.is_visible()
            && !minimized;
        let phase = if should_sweep {
            let cycle_ms = RUNNING_LIGHT_CYCLE.as_millis();
            (sweep_started_at.elapsed().as_millis() % cycle_ms) as f32 / cycle_ms as f32
        } else {
            0.0
        };

        let render_line =
            |line_text: &str, enable_sweep: bool, digit_anims: &ActiveDigitSwaps| -> AnyElement {
                let total_glyphs = crate::workbench::turn_segments::light_glyphs(line_text).len();
                if !enable_sweep && digit_anims.is_empty() {
                    return super::apply_scale(div(), typography)
                        .pb(super::space::MICRO)
                        .relative()
                        .flex()
                        .items_center()
                        .text_color(base_color)
                        .child(SharedString::from(line_text.to_owned()))
                        .into_any_element();
                }

                let mut row = super::apply_scale(div(), typography)
                    .pb(super::space::MICRO)
                    .relative()
                    .flex()
                    .items_center();

                if enable_sweep {
                    row = row.child(
                        canvas(
                            |bounds, window, _| {
                                if bounds.intersects(&window.content_mask().bounds) {
                                    #[cfg(feature = "test-support")]
                                    crate::ux_probe::record_running_light_frame();
                                    window.request_animation_frame();
                                }
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    );
                }

                let mut glyph_idx = 0usize;
                let mut digit_run_idx = 0usize;
                for seg in split_label_digits(line_text) {
                    match seg {
                        LabelSegment::Static(text_part) => {
                            if enable_sweep {
                                for g in crate::workbench::turn_segments::light_glyphs(text_part) {
                                    let col = crate::workbench::turn_segments::glyph_blend_color(
                                        base_color,
                                        highlight_color,
                                        glyph_idx,
                                        total_glyphs,
                                        phase,
                                    );
                                    row = row.child(
                                        div()
                                            .text_color(col)
                                            .child(SharedString::from(g.to_owned())),
                                    );
                                    glyph_idx += 1;
                                }
                            } else {
                                let part_glyphs =
                                    crate::workbench::turn_segments::light_glyphs(text_part).len();
                                row = row.child(
                                    div()
                                        .text_color(base_color)
                                        .child(SharedString::from(text_part.to_owned())),
                                );
                                glyph_idx += part_glyphs;
                            }
                        }
                        LabelSegment::Digits(digits_str) => {
                            let d_bytes = digits_str.as_bytes();
                            for (pos, &b) in d_bytes.iter().enumerate() {
                                let ch = b as char;
                                let place = d_bytes.len() - 1 - pos;
                                let col = if enable_sweep {
                                    crate::workbench::turn_segments::glyph_blend_color(
                                        base_color,
                                        highlight_color,
                                        glyph_idx,
                                        total_glyphs,
                                        phase,
                                    )
                                } else {
                                    base_color
                                };
                                if let Some(&ActiveDigitSwap {
                                    prev: prev_opt,
                                    progress,
                                    dir,
                                }) = digit_anims.get(&(digit_run_idx, place))
                                {
                                    let mut slot = div()
                                        .relative()
                                        .overflow_hidden()
                                        .flex()
                                        .items_center()
                                        .child(
                                            div()
                                                .relative()
                                                .top(px(6.0 * dir * (1.0 - progress)))
                                                .opacity(0.01 + 0.99 * progress)
                                                .text_color(col)
                                                .child(SharedString::from(ch.to_string())),
                                        );
                                    if let Some(prev_ch) = prev_opt {
                                        let exit_op = (1.0 - progress * 1.5).clamp(0.0, 1.0);
                                        if exit_op > 0.01 {
                                            slot = slot.child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .top(px(-6.0 * dir * progress))
                                                    .opacity(exit_op)
                                                    .text_color(col)
                                                    .child(SharedString::from(prev_ch.to_string())),
                                            );
                                        }
                                    }
                                    row = row.child(slot);
                                } else {
                                    row = row.child(
                                        div()
                                            .text_color(col)
                                            .child(SharedString::from(ch.to_string())),
                                    );
                                }
                                glyph_idx += 1;
                            }
                            digit_run_idx += 1;
                        }
                    }
                }
                row.into_any_element()
            };

        if !active_digits.is_empty() || active_full_swap.is_some() {
            window.request_animation_frame();
        }

        let current_el = render_line(next_text.as_ref(), should_sweep, &active_digits);
        let Some((prev_text, progress)) = active_full_swap else {
            return current_el;
        };

        let empty_digits = std::collections::BTreeMap::new();
        let prev_el = render_line(prev_text.as_ref(), false, &empty_digits);
        let exit_op = (1.0 - progress * 1.5).clamp(0.0, 1.0);
        let mut wrap = div()
            .relative()
            .overflow_hidden()
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .top(px(6.0 * (1.0 - progress)))
                    .opacity(0.01 + 0.99 * progress)
                    .child(current_el),
            );
        if exit_op > 0.01 {
            wrap = wrap.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(-6.0 * progress))
                    .opacity(exit_op)
                    .whitespace_nowrap()
                    .child(prev_el),
            );
        }
        wrap.into_any_element()
    }

    /// Interruptible horizontal page slide (`240ms ease_out`, `±8px`, `0ms` in `reduce_motion`).
    /// Rapid consecutive triggers restart smoothly from the current opacity floor without flashing to zero.
    #[derive(Default)]
    pub(crate) struct PageSlide(Option<(Instant, f32, f32)>);

    impl PageSlide {
        pub(crate) fn start(&mut self, direction: f32, cx: &App) {
            if cx.reduce_motion() {
                self.0 = None;
                return;
            }
            let now = cx.background_executor().now();
            let start_opacity = match self.0 {
                Some((prev_start, _, prev_floor))
                    if now.saturating_duration_since(prev_start) < NORMAL =>
                {
                    let elapsed = now.saturating_duration_since(prev_start);
                    let t =
                        ease_out()((elapsed.as_secs_f32() / NORMAL.as_secs_f32()).clamp(0.0, 1.0));
                    (prev_floor + (1.0 - prev_floor) * t).clamp(0.25, 0.72)
                }
                _ => 0.01,
            };
            self.0 = Some((now, direction, start_opacity));
        }

        pub(crate) fn sample(&mut self, window: &Window, cx: &App) -> (f32, Pixels) {
            if let Some((start, direction, floor)) = self.0
                && !cx.reduce_motion()
            {
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(start);
                if elapsed < NORMAL {
                    window.request_animation_frame();
                    let t =
                        ease_out()((elapsed.as_secs_f32() / NORMAL.as_secs_f32()).clamp(0.0, 1.0));
                    return (floor + (1.0 - floor) * t, px(8.0 * direction * (1.0 - t)));
                }
            }
            self.0 = None;
            (1.0, px(0.0))
        }
    }

    /// Interruptible gentle content reveal (e.g. Session A -> B message surface, header title glide, export preview).
    #[derive(Default)]
    pub(crate) struct SurfaceReveal(Option<(Instant, Duration, f32, f32)>);

    impl SurfaceReveal {
        pub(crate) fn start(&mut self, duration: Duration, dy_px: f32, floor: f32, cx: &App) {
            if cx.reduce_motion() || duration.is_zero() {
                self.0 = None;
                return;
            }
            let now = cx.background_executor().now();
            let next_floor = match self.0 {
                Some((prev_start, prev_dur, _, prev_floor))
                    if now.saturating_duration_since(prev_start) < prev_dur =>
                {
                    let elapsed = now.saturating_duration_since(prev_start);
                    let t = ease_out()(
                        (elapsed.as_secs_f32() / prev_dur.as_secs_f32()).clamp(0.0, 1.0),
                    );
                    let current = prev_floor + (1.0 - prev_floor) * t;
                    (current * 0.65).clamp(floor, 0.72)
                }
                _ => floor.clamp(0.01, 0.99),
            };
            self.0 = Some((now, duration, dy_px, next_floor));
        }

        pub(crate) fn clear(&mut self) {
            self.0 = None;
        }

        pub(crate) fn sample(&mut self, window: &Window, cx: &App) -> (f32, Pixels) {
            if let Some((start, dur, dy_px, floor)) = self.0
                && !cx.reduce_motion()
            {
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(start);
                if elapsed < dur {
                    window.request_animation_frame();
                    let t = ease_out()((elapsed.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0));
                    return (floor + (1.0 - floor) * t, px(dy_px * (1.0 - t)));
                }
            }
            self.0 = None;
            (1.0, px(0.0))
        }
    }

    fn eased_window(
        elapsed: Duration,
        delay: Duration,
        duration: Duration,
        curve: impl Fn(f32) -> f32,
    ) -> f32 {
        if elapsed <= delay {
            return 0.0;
        }
        let local = elapsed.saturating_sub(delay);
        if local >= duration || duration.is_zero() {
            return 1.0;
        }
        curve((local.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum RouteTransitionKind {
        /// Home -> Workspace (`Session` / `Settings` / `Account` / `AllSessions`):
        /// Sky curtain slides UP (`0..240ms`), destination reveals smoothly underneath (`60..300ms`).
        HomeToWorkspace,
        /// Workspace -> Home:
        /// Sky sweeps DOWN (`0..240ms`), BrandArcs/Greeting/Composer settle in (`50..300ms`).
        WorkspaceToHome,
        /// Hierarchical drill-in (`Chat -> Tree`, `Chat -> Export`): `240ms ease_out`, `+8px -> 0`.
        SlideForward,
        /// Hierarchical return (`Tree -> Chat`, `Export -> Chat`): `240ms ease_out`, `-8px -> 0`.
        SlideBackward,
        /// Sibling workspace pages (`Chat <-> Settings/Account/AllSessions`, `Any <-> Login`): `220ms ease_out`, `+6px -> 0`.
        CrossFade,
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct HomeExitCurtainSample {
        pub(crate) sky_progress: f32,
        pub(crate) mark_opacity: f32,
        pub(crate) mark_dy: Pixels,
        pub(crate) greeting_opacity: f32,
        pub(crate) greeting_dy: Pixels,
        pub(crate) footer_opacity: f32,
        pub(crate) footer_dy: Pixels,
        pub(crate) frozen_sky_time: f32,
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct HomeEnterSample {
        pub(crate) sky_progress: f32,
        pub(crate) arcs_opacity: f32,
        pub(crate) arcs_dy: f32,
        pub(crate) title_opacity: f32,
        pub(crate) title_dy: f32,
        pub(crate) composer_opacity: f32,
        pub(crate) composer_dy: f32,
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct ChatEnterSample {
        pub(crate) header_opacity: f32,
        pub(crate) header_dy: Pixels,
        pub(crate) messages_opacity: f32,
        pub(crate) messages_dy: Pixels,
        pub(crate) composer_opacity: f32,
        pub(crate) composer_dy: Pixels,
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct RouteTransitionSample {
        pub(crate) container_opacity: f32,
        pub(crate) container_dx: Pixels,
        pub(crate) container_dy: Pixels,
        pub(crate) home_exit_curtain: Option<HomeExitCurtainSample>,
        pub(crate) home_enter: Option<HomeEnterSample>,
        pub(crate) chat_enter: Option<ChatEnterSample>,
    }

    impl Default for RouteTransitionSample {
        fn default() -> Self {
            Self {
                container_opacity: 1.0,
                container_dx: px(0.0),
                container_dy: px(0.0),
                home_exit_curtain: None,
                home_enter: None,
                chat_enter: None,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct ActiveRouteTransition {
        started: Instant,
        kind: RouteTransitionKind,
        floor_opacity: f32,
        frozen_sky_time: f32,
    }

    /// Choreographed workspace route transition aligned with web's full-coverage transitions
    /// and desktop `storybook.html` tokens (`FAST=150ms`, `OPEN=220ms`, `NORMAL=240ms`, `RESIZE=300ms`,
    /// `ease_out`/`ease_in_out`, interruptible, `0ms` in `reduce_motion`).
    #[derive(Default)]
    pub(crate) struct RouteTransition(Option<ActiveRouteTransition>);

    impl RouteTransition {
        pub(crate) fn start(&mut self, kind: RouteTransitionKind, frozen_sky_time: f32, cx: &App) {
            if cx.reduce_motion() {
                self.0 = None;
                return;
            }
            let now = cx.background_executor().now();
            let floor_opacity = match self.0 {
                Some(prev)
                    if now.saturating_duration_since(prev.started)
                        < Self::duration_for(prev.kind) =>
                {
                    let prev_dur = Self::duration_for(prev.kind);
                    let elapsed = now.saturating_duration_since(prev.started);
                    let t = ease_out()(
                        (elapsed.as_secs_f32() / prev_dur.as_secs_f32()).clamp(0.0, 1.0),
                    );
                    let current = prev.floor_opacity + (1.0 - prev.floor_opacity) * t;
                    current.clamp(0.25, 0.72)
                }
                _ => 0.01,
            };
            self.0 = Some(ActiveRouteTransition {
                started: now,
                kind,
                floor_opacity,
                frozen_sky_time,
            });
        }

        fn duration_for(kind: RouteTransitionKind) -> Duration {
            match kind {
                RouteTransitionKind::HomeToWorkspace | RouteTransitionKind::WorkspaceToHome => {
                    Duration::from_millis(300)
                }
                RouteTransitionKind::SlideForward | RouteTransitionKind::SlideBackward => NORMAL,
                RouteTransitionKind::CrossFade => POPOVER_OPEN,
            }
        }

        pub(crate) fn sample(
            &mut self,
            dest_is_chat: bool,
            window: &Window,
            cx: &App,
        ) -> RouteTransitionSample {
            let Some(active) = self.0 else {
                return RouteTransitionSample::default();
            };
            if cx.reduce_motion() {
                self.0 = None;
                return RouteTransitionSample::default();
            }
            let total = Self::duration_for(active.kind);
            let elapsed = cx
                .background_executor()
                .now()
                .saturating_duration_since(active.started);
            if elapsed >= total {
                self.0 = None;
                return RouteTransitionSample::default();
            }
            window.request_animation_frame();

            match active.kind {
                RouteTransitionKind::HomeToWorkspace => {
                    let sky_p = eased_window(elapsed, Duration::ZERO, NORMAL, ease_in_out());
                    let mark_p = eased_window(
                        elapsed,
                        Duration::ZERO,
                        Duration::from_millis(180),
                        ease_in_out(),
                    );
                    let foot_p = eased_window(elapsed, Duration::ZERO, FAST, ease_in_out());
                    let home_exit_curtain = (elapsed < NORMAL).then_some(HomeExitCurtainSample {
                        sky_progress: sky_p,
                        mark_opacity: (1.0 - mark_p).clamp(0.0, 1.0),
                        mark_dy: px(-24.0 * mark_p),
                        greeting_opacity: (1.0 - mark_p).clamp(0.0, 1.0),
                        greeting_dy: px(-12.0 * mark_p),
                        footer_opacity: (1.0 - foot_p).clamp(0.0, 1.0),
                        footer_dy: px(12.0 * foot_p),
                        frozen_sky_time: active.frozen_sky_time,
                    });

                    if dest_is_chat {
                        let hdr_t = eased_window(
                            elapsed,
                            Duration::from_millis(60),
                            POPOVER_OPEN,
                            ease_out(),
                        );
                        let msg_t = eased_window(
                            elapsed,
                            Duration::from_millis(90),
                            Duration::from_millis(190),
                            ease_out(),
                        );
                        let cmp_t =
                            eased_window(elapsed, Duration::from_millis(60), NORMAL, ease_out());
                        RouteTransitionSample {
                            container_opacity: 1.0,
                            container_dx: px(0.0),
                            container_dy: px(0.0),
                            home_exit_curtain,
                            home_enter: None,
                            chat_enter: Some(ChatEnterSample {
                                header_opacity: 0.01 + 0.99 * hdr_t,
                                header_dy: px(-8.0 * (1.0 - hdr_t)),
                                messages_opacity: 0.01 + 0.99 * msg_t,
                                messages_dy: px(6.0 * (1.0 - msg_t)),
                                composer_opacity: 0.01 + 0.99 * cmp_t,
                                composer_dy: px(12.0 * (1.0 - cmp_t)),
                            }),
                        }
                    } else {
                        let body_t = eased_window(
                            elapsed,
                            Duration::from_millis(80),
                            POPOVER_OPEN,
                            ease_out(),
                        );
                        RouteTransitionSample {
                            container_opacity: 0.01 + 0.99 * body_t,
                            container_dx: px(0.0),
                            container_dy: px(8.0 * (1.0 - body_t)),
                            home_exit_curtain,
                            home_enter: None,
                            chat_enter: None,
                        }
                    }
                }
                RouteTransitionKind::WorkspaceToHome => {
                    let sky_p = eased_window(elapsed, Duration::ZERO, NORMAL, ease_out());
                    let arcs_t =
                        eased_window(elapsed, Duration::from_millis(50), NORMAL, ease_out());
                    let title_t = eased_window(
                        elapsed,
                        Duration::from_millis(65),
                        Duration::from_millis(230),
                        ease_out(),
                    );
                    let comp_t =
                        eased_window(elapsed, Duration::from_millis(80), POPOVER_OPEN, ease_out());
                    RouteTransitionSample {
                        container_opacity: 1.0,
                        container_dx: px(0.0),
                        container_dy: px(0.0),
                        home_exit_curtain: None,
                        home_enter: Some(HomeEnterSample {
                            sky_progress: sky_p,
                            arcs_opacity: 0.01 + 0.99 * arcs_t,
                            arcs_dy: -24.0 * (1.0 - arcs_t),
                            title_opacity: 0.01 + 0.99 * title_t,
                            title_dy: -12.0 * (1.0 - title_t),
                            composer_opacity: 0.01 + 0.99 * comp_t,
                            composer_dy: 12.0 * (1.0 - comp_t),
                        }),
                        chat_enter: None,
                    }
                }
                RouteTransitionKind::SlideForward | RouteTransitionKind::SlideBackward => {
                    let dir = if active.kind == RouteTransitionKind::SlideForward {
                        1.0
                    } else {
                        -1.0
                    };
                    let t = eased_window(elapsed, Duration::ZERO, NORMAL, ease_out());
                    RouteTransitionSample {
                        container_opacity: active.floor_opacity + (1.0 - active.floor_opacity) * t,
                        container_dx: px(8.0 * dir * (1.0 - t)),
                        container_dy: px(0.0),
                        home_exit_curtain: None,
                        home_enter: None,
                        chat_enter: None,
                    }
                }
                RouteTransitionKind::CrossFade => {
                    let t = eased_window(elapsed, Duration::ZERO, POPOVER_OPEN, ease_out());
                    RouteTransitionSample {
                        container_opacity: active.floor_opacity + (1.0 - active.floor_opacity) * t,
                        container_dx: px(0.0),
                        container_dy: px(6.0 * (1.0 - t)),
                        home_exit_curtain: None,
                        home_enter: None,
                        chat_enter: None,
                    }
                }
            }
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

    /// Dual-layer outer shadow for the workspace container (`d7oLH`).
    /// Dark mode uses the recessed `$bg-workspace` well + `1px $border-subtle` ring without a
    /// heavy dark outer blur that causes 8-bit banding on `#141416` `$bg-shell`.
    pub fn workspace_shadows(&self) -> Vec<BoxShadow> {
        if self.mode == ResolvedTheme::Dark {
            return Vec::new();
        }
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
        let (contact, mid) = match self.mode {
            ResolvedTheme::Light => (self.shadow_panel_sm, self.shadow_panel_sm),
            ResolvedTheme::Dark => (c(0x0000_0038), c(0x0000_0047)),
        };
        vec![
            BoxShadow {
                color: self.highlight_top,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: true,
            },
            BoxShadow {
                color: contact,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(3.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: mid,
                offset: point(px(0.0), px(4.0)),
                blur_radius: px(12.0),
                spread_radius: px(0.0),
                inset: false,
            },
        ]
    }

    /// Three-layer stacked elevation (`--craft-stack` + top highlight) for `ZWAs1` composer, `H2p2z` popovers, `K3x72` modals, and special cards.
    pub fn craft_stack_shadows(&self) -> Vec<BoxShadow> {
        match self.mode {
            ResolvedTheme::Light => vec![
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
            ],
            // Dark mode uses a monotonic alpha ramp across overlapping blurs to avoid a hard
            // contact-shadow collar step over bright shader/surface regions.
            ResolvedTheme::Dark => vec![
                BoxShadow {
                    color: self.highlight_top,
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(0.0),
                    inset: true,
                },
                BoxShadow {
                    color: c(0x0000_003D),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(3.0),
                    spread_radius: px(0.0),
                    inset: false,
                },
                BoxShadow {
                    color: c(0x0000_0047),
                    offset: point(px(0.0), px(6.0)),
                    blur_radius: px(14.0),
                    spread_radius: px(-1.0),
                    inset: false,
                },
                BoxShadow {
                    color: c(0x0000_0061),
                    offset: point(px(0.0), px(16.0)),
                    blur_radius: px(34.0),
                    spread_radius: px(-4.0),
                    inset: false,
                },
            ],
        }
    }

    /// Subtle single-layer flat shadow for small dropdowns, context menus, and tooltips
    /// (no glossy top specular highlight or multi-layer stack).
    pub fn flat_popover_shadows(&self) -> Vec<BoxShadow> {
        let color = match self.mode {
            ResolvedTheme::Light => c(0x1111_1114),
            ResolvedTheme::Dark => c(0x0000_0048),
        };
        vec![BoxShadow {
            color,
            offset: point(px(0.0), px(4.0)),
            blur_radius: px(12.0),
            spread_radius: px(0.0),
            inset: false,
        }]
    }

    /// Composer elevation aligned with `ZTzRO` / `ZWAs1`: subtle grounding shadow without
    /// stacking an extra blurred focus ring on top of `border_focus`.
    pub fn composer_shadows(&self, _focused: bool) -> Vec<BoxShadow> {
        match self.mode {
            ResolvedTheme::Light => vec![
                BoxShadow {
                    color: self.shadow_panel_sm,
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(2.0),
                    spread_radius: px(0.0),
                    inset: false,
                },
                BoxShadow {
                    color: self.shadow_panel_lg,
                    offset: point(px(0.0), px(8.0)),
                    blur_radius: px(20.0),
                    spread_radius: px(-4.0),
                    inset: false,
                },
            ],
            ResolvedTheme::Dark => vec![
                BoxShadow {
                    color: c(0x0000_0030),
                    offset: point(px(0.0), px(2.0)),
                    blur_radius: px(6.0),
                    spread_radius: px(0.0),
                    inset: false,
                },
                BoxShadow {
                    color: c(0x0000_004A),
                    offset: point(px(0.0), px(10.0)),
                    blur_radius: px(22.0),
                    spread_radius: px(-4.0),
                    inset: false,
                },
            ],
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
        kit_theme.colors.popover = palette.bg_glass;
        kit_theme.colors.popover_foreground = palette.text_primary;
        kit_theme.colors.danger = palette.status_danger;
        kit_theme.colors.danger_foreground = palette.text_on_accent;
    }

    {
        let base_theme = gpui_kit::base::Theme::global_mut(cx);
        base_theme.appearance = match resolved {
            ResolvedTheme::Light => gpui_kit::base::ThemeAppearance::Light,
            ResolvedTheme::Dark => gpui_kit::base::ThemeAppearance::Dark,
        };
        base_theme.tokens.typography.sans = ui_font.clone();
        base_theme.tokens.typography.mono = mono_font.clone();
        base_theme.tokens.colors.background = palette.bg_workspace;
        base_theme.tokens.colors.surface = palette.bg_workspace;
        base_theme.tokens.colors.foreground = palette.text_primary;
        base_theme.tokens.colors.muted_foreground = palette.text_secondary;
        base_theme.tokens.colors.border = palette.border_subtle;
        base_theme.tokens.colors.primary = palette.accent_solid;
        base_theme.tokens.colors.primary_foreground = palette.text_on_accent;
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

pub type SubpageBackFn = std::rc::Rc<dyn Fn(&mut gpui_kit::Window, &mut gpui_kit::App)>;

/// Renders the shared full-width `44px` subpage header master `xWJOf` (`cst-pilot-desktop.pen` 「00A · 窗口与导航」):
/// - Center track (`max_w: 816px`, `px: 64px` outer inset): `deTmS` (`16px` icon + `12px` gap + `16/24 @ 600` title),
///   strictly aligned with the `816px` centered page content column below (e.g., `常规` tab in `q8TgB`) at every window width.
/// - Left back button (`28×28`, `arrow-left`) anchors at `left: 24px` without perturbing center track symmetry.
/// - Optional `trailing` control anchors at `right: 16px`.
#[allow(clippy::too_many_arguments)]
pub fn render_subpage_header_xwjof(
    id: impl Into<gpui_kit::ElementId>,
    icon: gpui_kit::assets::IconName,
    title: impl Into<SharedString>,
    trailing: Option<gpui_kit::AnyElement>,
    on_back: Option<SubpageBackFn>,
    palette: &DesktopPalette,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) -> impl gpui_kit::IntoElement {
    use crate::accessibility::A11yElementExt as _;
    use crate::theme::motion::{DesktopTooltipExt as _, IconPressExt as _};
    use gpui_kit::{
        InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _, Styled as _,
        TestSupportExt as _,
    };

    let header_id: gpui_kit::ElementId = id.into();
    let back_id_str = SharedString::from(format!("{header_id}-back-btn"));
    let track_id_str = SharedString::from(format!("{header_id}-title-track"));

    let back_slot = if let Some(back_cb) = on_back {
        let (pressed, press_state) = motion::icon_press_state(back_id_str.as_ref(), window, cx);
        let (hovered, hs) = motion::PointerFlag::keyed(format!("hover-{back_id_str}"), window, cx);
        let hover_t = motion::animated_state_progress(back_id_str.clone(), hovered, window, cx);
        let press_t = motion::animated_press_progress(back_id_str.clone(), pressed, window, cx);
        let bg = motion::lerp_hsla(
            motion::lerp_hsla(
                gpui_kit::transparent_black(),
                palette.bg_item_hover,
                hover_t,
            ),
            palette.bg_active_press,
            press_t,
        );
        let fg = motion::lerp_hsla(palette.text_secondary, palette.text_primary, hover_t);
        Some(
            gpui_kit::div()
                .id(back_id_str.clone())
                .test_support()
                .a11y_button(back_id_str.clone(), "返回")
                .desktop_tooltip_with_hover(
                    SharedString::from(format!("tooltip-{back_id_str}")),
                    "返回".into(),
                    false,
                    Some(hs),
                )
                .tab_index(0)
                .w(height::COMPACT)
                .h(height::COMPACT)
                .rounded(radius::CONTROL)
                .bg(bg)
                .flex()
                .items_center()
                .justify_center()
                .on_icon_press(press_state)
                .on_click(move |_, w, cx| (back_cb)(w, cx))
                .child(motion::animated_icon_press(
                    back_id_str,
                    pressed,
                    gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ArrowLeft)
                        .size(px(16.0))
                        .text_color(fg),
                    window,
                    cx,
                )),
        )
    } else {
        None
    };

    let mut bar = gpui_kit::div()
        .id(header_id)
        .test_support()
        .relative()
        .w_full()
        .h(height::HEADER)
        .flex_shrink_0()
        .px(layout::SUBPAGE_HORIZONTAL_INSET)
        .border_b_1()
        .border_color(palette.border_subtle)
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui_kit::div()
                .id(track_id_str)
                .test_support()
                .w_full()
                .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
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
                ),
        );

    if let Some(back_el) = back_slot {
        bar = bar.child(
            gpui_kit::div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(space::GROUP)
                .flex()
                .items_center()
                .child(back_el),
        );
    }

    if let Some(right_el) = trailing {
        bar = bar.child(
            gpui_kit::div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(space::LG)
                .flex()
                .items_center()
                .child(right_el),
        );
    }
    bar
}

/// Renders the `28px` keyboard shortcut badge master `ZCGS3` (`cst-pilot-desktop.pen` 「00B · 按钮与输入」):
/// `h: 28px, px: 8px, r: 4px`, `$bg-surface` fill + `1px $border-subtle`, `12/16 @ 500` `Source Han Sans CN` `$text-primary`.
pub fn render_kbd_zcgs3(
    keys: impl Into<SharedString>,
    _mono_family: SharedString,
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
        .text_color(palette.text_primary)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(keys.into())
}

#[cfg(test)]
mod tests {
    use super::DesktopPalette;

    #[test]
    fn popover_surface_keeps_contrast_against_menu_row_hover_in_both_themes() {
        for palette in [DesktopPalette::light(), DesktopPalette::dark()] {
            assert_ne!(palette.bg_glass, palette.bg_item_active);
            assert_ne!(palette.bg_glass, palette.status_danger_subtle);
        }
    }
}
