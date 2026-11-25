use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, motion, radius, scale, space,
};

/// Hover-pausable countdown state for transient top banners (`toast` `2600ms` + `pauseOnHover`).
#[derive(Debug, Clone)]
pub struct BannerCountdownState {
    generation: u64,
    timer_epoch: u64,
    remaining: Duration,
    hovered: bool,
    deadline: Option<Instant>,
}

impl Default for BannerCountdownState {
    fn default() -> Self {
        Self::new()
    }
}

impl BannerCountdownState {
    pub fn new() -> Self {
        Self {
            generation: 0,
            timer_epoch: 0,
            remaining: Duration::ZERO,
            hovered: false,
            deadline: None,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    pub fn remaining(&self) -> Duration {
        self.remaining
    }

    /// A fresh banner has its own hover hitbox and full countdown.
    pub fn start(&mut self, now: Instant) -> BannerDeadline {
        self.generation = self.generation.wrapping_add(1);
        self.remaining = motion::BANNER_AUTO_DISMISS;
        self.hovered = false;
        self.resume(now)
    }

    fn resume(&mut self, now: Instant) -> BannerDeadline {
        let at = now + self.remaining;
        self.deadline = Some(at);
        self.timer_epoch = self.timer_epoch.wrapping_add(1);
        BannerDeadline {
            timer_epoch: self.timer_epoch,
            at,
        }
    }

    /// Call only for the currently displayed banner. A pause invalidates the
    /// current deadline; a resume starts a new timer for the unspent time.
    pub fn set_hovered(&mut self, hovered: bool, now: Instant) -> Option<BannerDeadline> {
        if self.hovered == hovered || (self.deadline.is_none() && !self.hovered) {
            return None;
        }
        self.hovered = hovered;
        if hovered {
            let deadline = self.deadline.take().expect("running banner has a deadline");
            self.remaining = deadline.saturating_duration_since(now);
            None
        } else {
            Some(self.resume(now))
        }
    }

    pub fn expire(&mut self, deadline: BannerDeadline, now: Instant) -> bool {
        if self.timer_epoch != deadline.timer_epoch
            || self.deadline != Some(deadline.at)
            || now < deadline.at
        {
            return false;
        }
        self.deadline = None;
        self.remaining = Duration::ZERO;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerDeadline {
    timer_epoch: u64,
    at: Instant,
}

impl BannerDeadline {
    pub fn remaining_at(self, now: Instant) -> Duration {
        self.at.saturating_duration_since(now)
    }
}

/// `WebActive` takeover lock overlay (`cst-pilot-desktop.pen` 「01 · 工作台」 frame `sYHwZ`, card `WO6NQ`, `384px` wide).
#[allow(clippy::too_many_arguments)]
pub fn render_web_active_lock_overlay(
    scrim_progress: f32,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    trap_focus: &FocusHandle,
    switch_back_focus: &FocusHandle,
    on_open_browser: impl Fn(&mut Window, &mut App) + 'static,
    on_switch_back: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_open_browser = Rc::new(on_open_browser);
    let on_switch_back = Rc::new(on_switch_back);

    let scrim = render_frosted_surface(
        "web-active-scrim",
        FrostedGlassSpec::scrim(palette).with_opacity(scrim_progress),
        palette.bg_scrim,
        transparency_enabled,
        scale_factor,
    )
    .opacity(0.01 + 0.99 * scrim_progress)
    .test_support()
    .absolute()
    .top_0()
    .left_0()
    .right_0()
    .bottom_0()
    .flex()
    .items_center()
    .justify_center()
    .occlude();

    let card = render_frosted_surface(
        "web-active-card-wo6nq",
        FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("web-active-lock-overlay", "正在网页端使用")
    .w(layout::MODAL_STANDARD_WIDTH)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_subtle)
    .p(px(20.0))
    .flex()
    .flex_col()
    .gap(space::MD)
    .child(
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        Icon::new(IconName::Globe)
                            .size(px(16.0))
                            .text_color(palette.accent_ink),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("正在网页端使用")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child("当前会话已切换至浏览器继续操作。为防止双端并发修改冲突，桌面端工作区已暂停编辑，会话列表与配置保持实时同步。"),
            ),
    )
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .justify_end()
            .gap(space::SM)
            .child({
                let cb = Rc::clone(&on_open_browser);
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("web-active-open-browser-btn")
                    .test_support()
                    .a11y_button("web-active-open-browser-btn", "打开网页")
                    .tab_index(0)
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_elevated)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, window, cx| cb(window, cx))
                    .child(cjk_inline_label("打开网页"))
            })
            .child({
                let cb = Rc::clone(&on_switch_back);
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("web-active-switch-back-btn")
                    .test_support()
                    .a11y_button("web-active-switch-back-btn", "切回桌面端")
                    .track_focus(switch_back_focus)
                    .tab_index(0)
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.accent_solid)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.accent_hover))
                    .on_click(move |_, window, cx| cb(window, cx))
                    .child(cjk_inline_label("切回桌面端"))
            }),
    );

    scrim.child(
        div()
            .tab_stop(false)
            .child(motion::animate_modal_enter(
                "web-active-card-anim",
                card,
                palette.craft_stack_shadows(),
            ))
            .focus_trap("web-active-focus-trap", trap_focus),
    )
}

/// Workspace top-center `40px` rectangular error bar (`X2gAh`:
/// `top(20px)`, `h(40px)`, `px(16px)`, `rounded(8px)`, `CircleAlert` `16px` `$status-danger` + single-line reason, `H2p2z` frosted glass).
#[allow(clippy::too_many_arguments)]
pub fn render_workspace_error_bar(
    interactive: bool,
    progress: f32,
    message: SharedString,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = single_line_banner_label(&message);
    let card = render_frosted_surface(
        if interactive {
            "workspace-top-error-bar"
        } else {
            "workspace-top-error-bar-exit"
        },
        FrostedGlassSpec::popover(palette).with_opacity(progress),
        palette.bg_elevated,
        transparency_enabled,
        scale_factor,
    )
    .opacity(0.01 + 0.99 * progress)
    .mt(px(-8.0 * (1.0 - progress)))
    .shadow(motion::scale_shadows(
        palette.craft_stack_shadows(),
        progress.powi(3),
    ))
    .h(height::FIELD)
    .max_w(px(640.0))
    .px(space::LG)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_hairline)
    .flex()
    .items_center()
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .gap(space::MD)
            .min_w_0()
            .child(
                Icon::new(IconName::CircleAlert)
                    .size(px(16.0))
                    .text_color(palette.status_danger),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_primary)
                    .min_w_0()
                    .truncate()
                    .child(cjk_inline_label(label)),
            ),
    )
    .map(|el| {
        if interactive {
            el.test_support()
                .a11y_status("workspace-top-error-bar", format!("错误：{message}"))
                .on_hover(on_hover)
                .into_any_element()
        } else {
            el.into_any_element()
        }
    });
    div()
        .absolute()
        .top(space::LG)
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(card)
}

/// Non-blocking top notice banner (`h1BufF`:
/// `top(20px)`, `h(40px)`, `px(16px)`, `rounded(8px)`, `CircleCheck` `16px` `$status-success` + message, `H2p2z` frosted glass).
#[allow(clippy::too_many_arguments)]
pub fn render_top_notice_banner(
    interactive: bool,
    progress: f32,
    message: SharedString,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = single_line_banner_label(&message);
    let card = render_frosted_surface(
        if interactive {
            "top-notice-banner"
        } else {
            "top-notice-banner-exit"
        },
        FrostedGlassSpec::popover(palette).with_opacity(progress),
        palette.bg_elevated,
        transparency_enabled,
        scale_factor,
    )
    .opacity(0.01 + 0.99 * progress)
    .mt(px(-8.0 * (1.0 - progress)))
    .shadow(motion::scale_shadows(
        palette.craft_stack_shadows(),
        progress.powi(3),
    ))
    .h(height::FIELD)
    .max_w(px(480.0))
    .px(space::LG)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_hairline)
    .flex()
    .items_center()
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .gap(space::MD)
            .min_w_0()
            .child(
                Icon::new(IconName::CircleCheck)
                    .size(px(16.0))
                    .text_color(palette.status_success),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_primary)
                    .min_w_0()
                    .truncate()
                    .child(cjk_inline_label(label)),
            ),
    )
    .map(|el| {
        if interactive {
            el.test_support()
                .a11y_status("top-notice-banner", format!("提示：{message}"))
                .on_hover(on_hover)
                .into_any_element()
        } else {
            el.into_any_element()
        }
    });
    div()
        .absolute()
        .top(space::LG)
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(card)
}

fn single_line_banner_label(message: &SharedString) -> SharedString {
    if message.contains(['\r', '\n']) {
        message.replace(['\r', '\n'], " ").into()
    } else {
        message.clone()
    }
}
