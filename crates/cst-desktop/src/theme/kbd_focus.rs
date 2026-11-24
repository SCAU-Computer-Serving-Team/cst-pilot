use std::time::Duration;

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
                    origin: point(bounds.origin.x - px(4.0), bounds.origin.y),
                    size: gpui::size(bounds.size.width + px(8.0), bounds.size.height),
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
}
