use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cst_client::KeyboardFocusStyleDto;
use gpui::{
    AnyElement, Bounds, ContentMask, Corners, PathBuilder, Point, ScrollHandle, canvas, fill,
};
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::{App, Hsla, IntoElement, Pixels, SharedString, Styled, Window, point, px, rgba};

use crate::theme::{DesktopPalette, ResolvedTheme, motion};

pub const KBD_GLIDE_DURATION: Duration = Duration::from_millis(180);
pub const KBD_FADE_DURATION: Duration = Duration::from_millis(140);
const POINTER_MUTEX_THRESHOLD_PX: f32 = 4.0;

/// StoryBook Section B interactive corner-bracket stroke color.
pub fn kbd_bracket_color(palette: &DesktopPalette) -> Hsla {
    match palette.mode {
        ResolvedTheme::Light => rgba(0x1111_14B8).into(),
        ResolvedTheme::Dark => rgba(0xF2F2_F4C7).into(),
    }
}

/// StoryBook Section B interactive accent corner-bracket / rail color (`--kbd-bracket-accent`).
pub fn kbd_bracket_accent(palette: &DesktopPalette) -> Hsla {
    match palette.mode {
        ResolvedTheme::Light => rgba(0x1A4B_E0FF).into(),
        ResolvedTheme::Dark => rgba(0x688F_F8FF).into(),
    }
}

/// StoryBook Section B interactive edge-rail fill color.
pub fn kbd_rail_color(palette: &DesktopPalette) -> Hsla {
    match palette.mode {
        ResolvedTheme::Light => rgba(0x1111_11FF).into(),
        ResolvedTheme::Dark => rgba(0xF2F2_F4FF).into(),
    }
}

/// Detects intentional pointer movement (`>= 4px` Manhattan distance) for keyboard/mouse mutual exclusion.
pub fn pointer_moved_past_threshold(
    last_pos: &mut Option<Point<Pixels>>,
    current: Point<Pixels>,
) -> bool {
    match *last_pos {
        None => {
            *last_pos = Some(current);
            false
        }
        Some(prev) => {
            let dx = (f32::from(current.x) - f32::from(prev.x)).abs();
            let dy = (f32::from(current.y) - f32::from(prev.y)).abs();
            if dx + dy >= POINTER_MUTEX_THRESHOLD_PX {
                *last_pos = Some(current);
                true
            } else {
                false
            }
        }
    }
}

/// Renders a crisp `14×14px` vector icon matching Lucide `scan` (`四角`) or `tally-1` (`游标`).
pub fn render_kbd_style_icon(style: KeyboardFocusStyleDto, color: Hsla) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let x = f32::from(bounds.origin.x);
            let y = f32::from(bounds.origin.y);
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height);
            match style {
                KeyboardFocusStyleDto::Brackets => {
                    paint_four_corner_brackets(
                        window,
                        x + 2.0,
                        y + 2.0,
                        (w - 4.0).max(4.0),
                        (h - 4.0).max(4.0),
                        2.0,
                        3.2,
                        1.4,
                        color,
                    );
                }
                KeyboardFocusStyleDto::Rail => {
                    let bar_w = 1.8_f32;
                    let bar_h = (h - 4.0).max(4.0);
                    let bar_bounds = gpui::Bounds {
                        origin: point(px(x + (w - bar_w) * 0.5), px(y + (h - bar_h) * 0.5)),
                        size: gpui::size(px(bar_w), px(bar_h)),
                    };
                    window.paint_quad(
                        fill(bar_bounds, color).corner_radii(Corners::all(px(bar_w * 0.5))),
                    );
                }
            }
        },
    )
    .size(px(14.0))
    .flex_shrink_0()
    .into_any_element()
}

/// Scroll-content-relative row geometry for the gliding keyboard focus indicator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KbdRowTarget {
    pub top: f32,
    pub height: f32,
    pub inset_x: f32,
    pub corner_radius: f32,
}

#[derive(Debug, Clone, Copy)]
struct KbdGlideMemory {
    was_visible: bool,
    top: f32,
    height: f32,
    inset_x: f32,
    corner_radius: f32,
}

impl Default for KbdGlideMemory {
    fn default() -> Self {
        Self {
            was_visible: false,
            top: 0.0,
            height: 40.0,
            inset_x: 0.0,
            corner_radius: 6.0,
        }
    }
}

/// Renders the shared `180ms ease-out` gliding keyboard focus indicator (`四角` or `游标`)
/// as a non-interactive absolute overlay without altering any row's interior background color.
pub fn render_gliding_row_indicator(
    id_prefix: &'static str,
    style: KeyboardFocusStyleDto,
    target: Option<KbdRowTarget>,
    scroll: &ScrollHandle,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let reduce_motion = cx.reduce_motion();
    let mem_state = window.use_keyed_state(
        SharedString::from(format!("{id_prefix}-kbd-mem")),
        cx,
        |_, _| KbdGlideMemory::default(),
    );
    let prev = *mem_state.read(cx);
    let is_visible = target.is_some();
    let snap_pos = !prev.was_visible && is_visible;

    let (goal_top, goal_h, goal_inset_x, goal_radius) = if let Some(t) = target {
        mem_state.update(cx, |m, _| {
            m.was_visible = true;
            m.top = t.top;
            m.height = t.height;
            m.inset_x = t.inset_x;
            m.corner_radius = t.corner_radius;
        });
        (t.top, t.height, t.inset_x, t.corner_radius)
    } else {
        if prev.was_visible {
            mem_state.update(cx, |m, _| {
                m.was_visible = false;
            });
        }
        (prev.top, prev.height, prev.inset_x, prev.corner_radius)
    };

    let pos_duration = if reduce_motion || snap_pos {
        Duration::ZERO
    } else {
        KBD_GLIDE_DURATION
    };
    let fade_duration = if reduce_motion {
        Duration::ZERO
    } else {
        KBD_FADE_DURATION
    };

    let cur_top = transition(
        SharedString::from(format!("{id_prefix}-kbd-y")),
        goal_top,
        Transition::new(pos_duration).ease(motion::ease_out()),
        window,
        cx,
    );
    let cur_h = transition(
        SharedString::from(format!("{id_prefix}-kbd-h")),
        goal_h,
        Transition::new(pos_duration).ease(motion::ease_out()),
        window,
        cx,
    );
    let cur_opacity = transition(
        SharedString::from(format!("{id_prefix}-kbd-op")),
        if is_visible { 1.0_f32 } else { 0.0_f32 },
        Transition::new(fade_duration).ease(motion::ease_out()),
        window,
        cx,
    )
    .clamp(0.0, 1.0);

    if cur_opacity <= 0.005 || cur_h <= 1.0 {
        return None;
    }

    let mut color = match style {
        KeyboardFocusStyleDto::Brackets => kbd_bracket_color(palette),
        KeyboardFocusStyleDto::Rail => kbd_rail_color(palette),
    };
    color.a *= cur_opacity;

    let scroll = scroll.clone();
    Some(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                // Scroll-to-item is resolved during prepaint, after the view renders.
                // The scroll offset follows immediately; only the content-space cursor glides.
                let top = cur_top + f32::from(scroll.offset().y);
                let viewport_height = f32::from(bounds.size.height);
                if top + cur_h <= 0.0 || top >= viewport_height {
                    return;
                }
                let x = f32::from(bounds.origin.x);
                let y = f32::from(bounds.origin.y) + top;
                let width = f32::from(bounds.size.width);
                let clip = Bounds {
                    origin: point(bounds.origin.x - px(4.0), bounds.origin.y - px(3.0)),
                    size: gpui::size(bounds.size.width + px(8.0), bounds.size.height + px(6.0)),
                };
                window.with_content_mask(
                    Some(ContentMask { bounds: clip }),
                    |window| match style {
                        KeyboardFocusStyleDto::Brackets => {
                            paint_four_corner_brackets(
                                window,
                                x + goal_inset_x,
                                y,
                                width - 2.0 * goal_inset_x,
                                cur_h,
                                goal_radius,
                                8.0,
                                1.5,
                                color,
                            );
                        }
                        KeyboardFocusStyleDto::Rail => {
                            let rail_h = (cur_h * 0.56).clamp(16.0, 26.0);
                            let rail = Bounds {
                                origin: point(
                                    px(x + (goal_inset_x + 3.0).max(2.0)),
                                    px(y + (cur_h - rail_h) * 0.5),
                                ),
                                size: gpui::size(px(3.0), px(rail_h)),
                            };
                            window
                                .paint_quad(fill(rail, color).corner_radii(Corners::all(px(1.5))));
                        }
                    },
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn paint_four_corner_brackets(
    window: &mut Window,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius_px: f32,
    arm_px: f32,
    stroke_px: f32,
    color: Hsla,
) {
    if w < 4.0 || h < 4.0 {
        return;
    }
    let half = stroke_px * 0.5;
    let x0 = x + half;
    let y0 = y + half;
    let x1 = (x + w - half).max(x0);
    let y1 = (y + h - half).max(y0);
    let max_arm = ((x1 - x0) * 0.45).min((y1 - y0) * 0.45);
    let arm = arm_px.min(max_arm).max(2.0);
    let r = radius_px.min(arm * 0.78).max(0.5);

    // Top-left corner: (x0, y0 + arm) -> arc around (x0 + r, y0 + r) -> (x0 + arm, y0)
    paint_rounded_corner(
        window,
        stroke_px,
        color,
        (x0, y0 + arm),
        (x0 + r, y0 + r),
        r,
        std::f32::consts::PI,
        std::f32::consts::FRAC_PI_2 * 3.0,
        (x0 + arm, y0),
    );
    // Top-right corner: (x1 - arm, y0) -> arc around (x1 - r, y0 + r) -> (x1, y0 + arm)
    paint_rounded_corner(
        window,
        stroke_px,
        color,
        (x1 - arm, y0),
        (x1 - r, y0 + r),
        r,
        std::f32::consts::FRAC_PI_2 * 3.0,
        std::f32::consts::TAU,
        (x1, y0 + arm),
    );
    // Bottom-right corner: (x1, y1 - arm) -> arc around (x1 - r, y1 - r) -> (x1 - arm, y1)
    paint_rounded_corner(
        window,
        stroke_px,
        color,
        (x1, y1 - arm),
        (x1 - r, y1 - r),
        r,
        0.0,
        std::f32::consts::FRAC_PI_2,
        (x1 - arm, y1),
    );
    // Bottom-left corner: (x0 + arm, y1) -> arc around (x0 + r, y1 - r) -> (x0, y1 - arm)
    paint_rounded_corner(
        window,
        stroke_px,
        color,
        (x0 + arm, y1),
        (x0 + r, y1 - r),
        r,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
        (x0, y1 - arm),
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_rounded_corner(
    window: &mut Window,
    stroke_px: f32,
    color: Hsla,
    start: (f32, f32),
    center: (f32, f32),
    r: f32,
    angle_start: f32,
    angle_end: f32,
    end: (f32, f32),
) {
    let mut builder = PathBuilder::stroke(px(stroke_px));
    builder.move_to(point(px(start.0), px(start.1)));
    let steps = 5;
    for step in 0..=steps {
        let t = (step as f32) / (steps as f32);
        let a = angle_start + (angle_end - angle_start) * t;
        let px_x = center.0 + r * a.cos();
        let px_y = center.1 + r * a.sin();
        builder.line_to(point(px(px_x), px(px_y)));
    }
    builder.line_to(point(px(end.0), px(end.1)));
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Visual tone of a gliding keyboard focus indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdTone {
    Neutral,
    Accent,
}

/// Rail bar placement for `KeyboardFocusStyleDto::Rail` on standalone controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdRailPlacement {
    /// Inside left edge (`x + 2px`), used by checkbox and list rows.
    LeftInside,
    /// Outside left edge (`x - 7px`), used by solid and outline buttons.
    LeftOutside,
    /// Centered below the control (`y + h + 4px`), used by horizontal segmented choices.
    BottomCenter,
}

/// Target descriptor for a non-virtualized control inside a `KbdBoundsTracker` container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KbdControlSpec {
    pub slot: usize,
    pub corner_radius: f32,
    pub tone: KbdTone,
    pub rail: KbdRailPlacement,
}

/// Captures exact post-layout control bounds during `prepaint` so the gliding overlay can read
/// them in the same frame's `paint` pass with zero hardcoded pixel offsets.
#[derive(Clone)]
pub struct KbdBoundsTracker<const N: usize> {
    slots: Rc<RefCell<[Option<Bounds<Pixels>>; N]>>,
}

impl<const N: usize> Default for KbdBoundsTracker<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> KbdBoundsTracker<N> {
    pub fn new() -> Self {
        Self {
            slots: Rc::new(RefCell::new([None; N])),
        }
    }

    pub fn clear(&self) {
        *self.slots.borrow_mut() = [None; N];
    }

    pub fn set(&self, slot: usize, bounds: Bounds<Pixels>) {
        if let Some(cell) = self.slots.borrow_mut().get_mut(slot) {
            *cell = Some(bounds);
        }
    }

    pub fn get(&self, slot: usize) -> Option<Bounds<Pixels>> {
        self.slots.borrow().get(slot).copied().flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RectGeom {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
}

impl RectGeom {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        Self {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            w: a.w + (b.w - a.w) * t,
            h: a.h + (b.h - a.h) * t,
            r: a.r + (b.r - a.r) * t,
        }
    }

    fn approx_eq(self, other: Self) -> bool {
        (self.x - other.x).abs() < 0.25
            && (self.y - other.y).abs() < 0.25
            && (self.w - other.w).abs() < 0.25
            && (self.h - other.h).abs() < 0.25
            && (self.r - other.r).abs() < 0.25
    }
}

fn compute_control_rect(
    style: KeyboardFocusStyleDto,
    rel_x: f32,
    rel_y: f32,
    w: f32,
    h: f32,
    corner_radius: f32,
    rail: KbdRailPlacement,
) -> RectGeom {
    match style {
        KeyboardFocusStyleDto::Brackets => {
            let pad = 3.0;
            RectGeom {
                x: rel_x - pad,
                y: rel_y - pad,
                w: w + pad * 2.0,
                h: h + pad * 2.0,
                r: corner_radius + pad,
            }
        }
        KeyboardFocusStyleDto::Rail => match rail {
            KbdRailPlacement::BottomCenter => {
                let bar_w = (w * 0.36).clamp(16.0, 34.0);
                RectGeom {
                    x: rel_x + (w - bar_w) * 0.5,
                    y: rel_y + h + 4.0,
                    w: bar_w,
                    h: 3.0,
                    r: 1.5,
                }
            }
            KbdRailPlacement::LeftInside => {
                let bar_h = (h * 0.56).clamp(14.0, 22.0);
                RectGeom {
                    x: rel_x + 2.0,
                    y: rel_y + (h - bar_h) * 0.5,
                    w: 3.0,
                    h: bar_h,
                    r: 1.5,
                }
            }
            KbdRailPlacement::LeftOutside => {
                let bar_h = (h * 0.56).clamp(14.0, 22.0);
                RectGeom {
                    x: rel_x - 7.0,
                    y: rel_y + (h - bar_h) * 0.5,
                    w: 3.0,
                    h: bar_h,
                    r: 1.5,
                }
            }
        },
    }
}

struct ControlGlideState {
    initialized: bool,
    was_visible: bool,
    from_rect: RectGeom,
    to_rect: RectGeom,
    pos_start: Instant,
    pos_duration: Duration,
    from_op: f32,
    to_op: f32,
    op_start: Instant,
    op_duration: Duration,
    last_tone: KbdTone,
}

impl Default for ControlGlideState {
    fn default() -> Self {
        let now = Instant::now();
        let zero = RectGeom {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            r: 4.0,
        };
        Self {
            initialized: false,
            was_visible: false,
            from_rect: zero,
            to_rect: zero,
            pos_start: now,
            pos_duration: Duration::ZERO,
            from_op: 0.0,
            to_op: 0.0,
            op_start: now,
            op_duration: Duration::ZERO,
            last_tone: KbdTone::Neutral,
        }
    }
}

impl ControlGlideState {
    fn sample_rect(&self, now: Instant) -> (RectGeom, bool) {
        if self.pos_duration.is_zero() {
            return (self.to_rect, false);
        }
        let raw = (now.saturating_duration_since(self.pos_start).as_secs_f32()
            / self.pos_duration.as_secs_f32())
        .clamp(0.0, 1.0);
        let eased = motion::ease_out()(raw);
        (
            RectGeom::lerp(self.from_rect, self.to_rect, eased),
            raw < 1.0,
        )
    }

    fn sample_op(&self, now: Instant) -> (f32, bool) {
        if self.op_duration.is_zero() {
            return (self.to_op, false);
        }
        let raw = (now.saturating_duration_since(self.op_start).as_secs_f32()
            / self.op_duration.as_secs_f32())
        .clamp(0.0, 1.0);
        let eased = motion::ease_out()(raw);
        let op = (self.from_op + (self.to_op - self.from_op) * eased).clamp(0.0, 1.0);
        (op, raw < 1.0)
    }
}

/// Renders a `180ms ease-out` gliding keyboard focus indicator (`四角` or `游标`) over arbitrary
/// controls inside a container, reading post-layout bounds from `tracker` in the same frame.
pub fn render_gliding_control_indicator<const N: usize>(
    id_prefix: &'static str,
    style: KeyboardFocusStyleDto,
    target: Option<KbdControlSpec>,
    tracker: KbdBoundsTracker<N>,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let reduce_motion = cx.reduce_motion();
    let anim_state = window.use_keyed_state(
        SharedString::from(format!("{id_prefix}-kbd-ctrl-anim")),
        cx,
        |_, _| RefCell::new(ControlGlideState::default()),
    );
    let palette = *palette;

    canvas(
        |_, _, _| (),
        move |container_bounds, _, window, cx| {
            let now = Instant::now();
            let resolved = target.and_then(|spec| {
                let ctrl = tracker.get(spec.slot)?;
                let rel_x = f32::from(ctrl.origin.x) - f32::from(container_bounds.origin.x);
                let rel_y = f32::from(ctrl.origin.y) - f32::from(container_bounds.origin.y);
                let w = f32::from(ctrl.size.width);
                let h = f32::from(ctrl.size.height);
                if w <= 1.0 || h <= 1.0 {
                    return None;
                }
                Some((
                    compute_control_rect(style, rel_x, rel_y, w, h, spec.corner_radius, spec.rail),
                    spec.tone,
                ))
            });

            let mut state = anim_state.read(cx).borrow_mut();
            let (cur_rect, _) = state.sample_rect(now);
            let (cur_op, _) = state.sample_op(now);

            let want_visible = resolved.is_some();
            if let Some((goal_rect, tone)) = resolved {
                state.last_tone = tone;
                let snap = reduce_motion || !state.initialized || !state.was_visible;
                if snap {
                    state.from_rect = goal_rect;
                    state.to_rect = goal_rect;
                    state.pos_start = now;
                    state.pos_duration = Duration::ZERO;
                } else if !state.to_rect.approx_eq(goal_rect) {
                    state.from_rect = cur_rect;
                    state.to_rect = goal_rect;
                    state.pos_start = now;
                    state.pos_duration = KBD_GLIDE_DURATION;
                }
                state.initialized = true;
            }

            if want_visible != state.was_visible {
                state.was_visible = want_visible;
                state.from_op = cur_op;
                state.to_op = if want_visible { 1.0 } else { 0.0 };
                state.op_start = now;
                state.op_duration = if reduce_motion {
                    Duration::ZERO
                } else {
                    KBD_FADE_DURATION
                };
            }

            let (rect, pos_anim) = state.sample_rect(now);
            let (op, op_anim) = state.sample_op(now);
            let tone = state.last_tone;
            drop(state);

            if pos_anim || op_anim {
                window.request_animation_frame();
            }
            if op <= 0.005 || rect.w <= 1.0 || rect.h <= 1.0 {
                return;
            }

            let mut color = match (style, tone) {
                (KeyboardFocusStyleDto::Brackets, KbdTone::Accent)
                | (KeyboardFocusStyleDto::Rail, KbdTone::Accent) => kbd_bracket_accent(&palette),
                (KeyboardFocusStyleDto::Brackets, KbdTone::Neutral) => kbd_bracket_color(&palette),
                (KeyboardFocusStyleDto::Rail, KbdTone::Neutral) => kbd_rail_color(&palette),
            };
            color.a *= op;

            let abs_x = f32::from(container_bounds.origin.x) + rect.x;
            let abs_y = f32::from(container_bounds.origin.y) + rect.y;
            let clip = Bounds {
                origin: point(
                    container_bounds.origin.x - px(16.0),
                    container_bounds.origin.y - px(12.0),
                ),
                size: gpui::size(
                    container_bounds.size.width + px(32.0),
                    container_bounds.size.height + px(28.0),
                ),
            };
            window.with_content_mask(Some(ContentMask { bounds: clip }), |window| match style {
                KeyboardFocusStyleDto::Brackets => {
                    paint_four_corner_brackets(
                        window, abs_x, abs_y, rect.w, rect.h, rect.r, 8.0, 1.5, color,
                    );
                }
                KeyboardFocusStyleDto::Rail => {
                    let bar = Bounds {
                        origin: point(px(abs_x), px(abs_y)),
                        size: gpui::size(px(rect.w), px(rect.h)),
                    };
                    window.paint_quad(fill(bar, color).corner_radii(Corners::all(px(rect.r))));
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_mutex_uses_distance_from_keyboard_activation() {
        let mut origin = Some(point(px(20.0), px(10.0)));
        assert!(!pointer_moved_past_threshold(
            &mut origin,
            point(px(21.0), px(11.0))
        ));
        assert!(pointer_moved_past_threshold(
            &mut origin,
            point(px(22.0), px(12.0))
        ));
    }

    #[test]
    fn control_rect_matches_storybook_brackets_and_rail_geometry() {
        let brackets = compute_control_rect(
            KeyboardFocusStyleDto::Brackets,
            10.0,
            20.0,
            120.0,
            32.0,
            4.0,
            KbdRailPlacement::BottomCenter,
        );
        assert_eq!(brackets.x, 7.0);
        assert_eq!(brackets.y, 17.0);
        assert_eq!(brackets.w, 126.0);
        assert_eq!(brackets.h, 38.0);
        assert_eq!(brackets.r, 7.0);

        let bottom_rail = compute_control_rect(
            KeyboardFocusStyleDto::Rail,
            10.0,
            20.0,
            100.0,
            32.0,
            4.0,
            KbdRailPlacement::BottomCenter,
        );
        assert_eq!(bottom_rail.w, 34.0);
        assert_eq!(bottom_rail.h, 3.0);
        assert_eq!(bottom_rail.y, 56.0);

        let inside_rail = compute_control_rect(
            KeyboardFocusStyleDto::Rail,
            10.0,
            20.0,
            200.0,
            24.0,
            4.0,
            KbdRailPlacement::LeftInside,
        );
        assert_eq!(inside_rail.x, 12.0);
        assert_eq!(inside_rail.w, 3.0);

        let outside_rail = compute_control_rect(
            KeyboardFocusStyleDto::Rail,
            10.0,
            20.0,
            264.0,
            32.0,
            4.0,
            KbdRailPlacement::LeftOutside,
        );
        assert_eq!(outside_rail.x, 3.0);
        assert_eq!(outside_rail.w, 3.0);
    }
}
