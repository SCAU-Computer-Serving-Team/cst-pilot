use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, BorrowAppContext as _, Bounds, Context,
    DispatchPhase, ElementId, Entity, Global, InteractiveElement as _, IntoElement, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Pixels, Point, Render, ScrollEvent, SharedString,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, WindowId, canvas, div,
    px,
};
use gpui_kit::base::{TooltipOverlay, TooltipRequest, TooltipTransition};
use gpui_kit::{TestSupportExt as _, component::ElementExt};

use super::motion::{FAST, PointerFlag, ease_out};
use super::{DesktopThemeGlobal, apply_scale, radius, scale, space};
use crate::background::{FrostedGlassSpec, render_frosted_surface};

struct DesktopTooltipRegistry {
    windows: HashMap<WindowId, WeakEntity<DesktopTooltips>>,
    _closed: Subscription,
}

impl Global for DesktopTooltipRegistry {}

type AnchorBounds = Cell<Bounds<Pixels>>;

#[derive(Clone)]
struct TooltipAnchor {
    lifetime: Weak<AnchorBounds>,
    bounds: Bounds<Pixels>,
}

/// Owns the shared tooltip group for a single desktop window.
pub struct DesktopTooltips {
    overlay: Entity<TooltipOverlay>,
    owner: Option<TooltipAnchor>,
    pressed_at: Option<Point<Pixels>>,
    pressed_bounds: Option<Bounds<Pixels>>,
    blocked: bool,
    _activation: Subscription,
}

impl DesktopTooltips {
    pub fn install(window: &mut Window, cx: &mut App) -> Entity<Self> {
        if cx.try_global::<DesktopTooltipRegistry>().is_none() {
            let closed = cx.on_window_closed(|cx, id| {
                cx.update_global::<DesktopTooltipRegistry, _>(|registry, _| {
                    registry.windows.remove(&id);
                });
            });
            cx.set_global(DesktopTooltipRegistry {
                windows: HashMap::new(),
                _closed: closed,
            });
        }
        let layer = cx.new(|cx: &mut Context<Self>| Self {
            overlay: create_desktop_tooltip_overlay(cx),
            owner: None,
            pressed_at: None,
            pressed_bounds: None,
            blocked: false,
            _activation: cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.clear_press();
                    this.dismiss(cx);
                }
            }),
        });
        cx.update_global::<DesktopTooltipRegistry, _>(|registry, _| {
            registry
                .windows
                .retain(|_, layer| layer.upgrade().is_some());
            registry
                .windows
                .insert(window.window_handle().window_id(), layer.downgrade());
        });
        layer
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.owner = None;
        self.overlay.update(cx, |overlay, cx| overlay.hide(cx));
    }

    pub(crate) fn set_blocked(&mut self, blocked: bool, cx: &mut Context<Self>) {
        if self.blocked != blocked {
            self.blocked = blocked;
            self.dismiss(cx);
        }
    }

    fn clear_press(&mut self) {
        self.pressed_at = None;
        self.pressed_bounds = None;
    }

    fn reconcile_anchor(&mut self, cx: &mut Context<Self>) {
        if self.owner.as_ref().is_some_and(|owner| {
            owner
                .lifetime
                .upgrade()
                .is_none_or(|anchor| anchor.get() != owner.bounds)
        }) {
            self.dismiss(cx);
        }
    }
}

impl Render for DesktopTooltips {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !window.is_window_active() || !window.is_window_hovered() {
            self.clear_press();
            self.dismiss(cx);
        }
        if self.owner.is_some() {
            // At the end of the frame GPUI drops state for unmounted anchors.
            // This also works for anchors in a retained subtree.
            let layer = cx.weak_entity();
            window.defer(cx, move |_, cx| {
                let _ = layer.update(cx, Self::reconcile_anchor);
            });
        }
        let layer = cx.weak_entity();
        div().child(self.overlay.clone()).child(canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let on_press = layer.clone();
                let on_scroll = layer.clone();
                let on_move = layer.clone();
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = on_press.update(cx, |this, cx| {
                            this.pressed_at = Some(ev.position);
                            this.pressed_bounds = this
                                .owner
                                .as_ref()
                                .map(|owner| owner.bounds)
                                .filter(|bounds| bounds.contains(&ev.position));
                            this.dismiss(cx);
                        });
                    }
                });
                window.on_mouse_event(move |_: &ScrollEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = on_scroll.update(cx, Self::dismiss);
                    }
                });
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = on_move.update(cx, |this, _| {
                            if this.pressed_bounds.map_or_else(
                                || {
                                    this.pressed_at
                                        .is_some_and(|pressed| pressed != ev.position)
                                },
                                |bounds| !bounds.contains(&ev.position),
                            ) {
                                this.clear_press();
                            }
                        });
                    }
                });
            },
        ))
    }
}

pub fn window_tooltip_overlay(window: &Window, cx: &App) -> Option<Entity<DesktopTooltips>> {
    cx.try_global::<DesktopTooltipRegistry>()?
        .windows
        .get(&window.window_handle().window_id())
        .and_then(WeakEntity::upgrade)
}

/// Builds the shared [`TooltipOverlay`] with `storybook.html` enter/switch motion (`150ms` enter / `0ms` adjacent switch).
fn create_desktop_tooltip_overlay(cx: &mut App) -> Entity<TooltipOverlay> {
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
                TooltipTransition::Switch { .. } => div().child(content_view).into_any_element(),
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
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = DesktopThemeGlobal::current(cx);
        let palette = theme.palette;
        if self.frosted {
            let transparency = theme.transparency_enabled;
            let scale_factor = window.scale_factor();
            return render_frosted_surface(
                self.elem_id.clone(),
                FrostedGlassSpec::popover(&palette),
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

/// Installs one GPUI hover listener. Pass the animated hover state here rather
/// than attaching a second `on_hover` to the same element.
pub trait DesktopTooltipExt: StatefulInteractiveElement + ElementExt + Sized {
    fn desktop_tooltip(
        self,
        trigger_key: impl Into<SharedString>,
        text: impl Into<SharedString>,
    ) -> Self {
        self.desktop_tooltip_with_hover(
            SharedString::from(format!("tooltip-{}", trigger_key.into())),
            text.into(),
            false,
            None,
        )
    }

    fn desktop_tooltip_with_hover(
        self,
        tooltip_elem_id: SharedString,
        text: SharedString,
        frosted: bool,
        hover_state: Option<Entity<PointerFlag>>,
    ) -> Self {
        let anchor = Rc::new(RefCell::new(None::<TooltipAnchor>));
        let writer = anchor.clone();
        self.on_prepaint(move |bounds, window, _| {
            let id = window.with_global_id("desktop-tooltip-anchor".into(), |id, _| id.clone());
            let lifetime = window.with_element_state(&id, |state: Option<Rc<AnchorBounds>>, _| {
                let state = state.unwrap_or_default();
                state.set(bounds);
                (Rc::downgrade(&state), state)
            });
            writer.replace(Some(TooltipAnchor { lifetime, bounds }));
        })
        .on_hover(move |hovered, window, cx| {
            if let Some(state) = &hover_state {
                if *hovered {
                    PointerFlag::enter(state, window, cx);
                } else {
                    PointerFlag::set(state, false, cx);
                }
            }
            let Some(layer) = window_tooltip_overlay(window, cx) else {
                return;
            };
            let anchor = anchor.borrow();
            let Some(anchor) = anchor.as_ref() else {
                return;
            };
            layer.update(cx, |this, cx| {
                if *hovered
                    && window.is_window_active()
                    && window.is_window_hovered()
                    && !this.blocked
                {
                    if anchor.lifetime.upgrade().is_none()
                        || this
                            .pressed_at
                            .is_some_and(|point| anchor.bounds.contains(&point))
                    {
                        return;
                    }
                    this.owner = Some(anchor.clone());
                    let elem_id = tooltip_elem_id.clone();
                    let text = text.clone();
                    this.overlay.update(cx, |overlay, cx| {
                        let request = TooltipRequest::new(anchor.bounds, move |_, cx| {
                            cx.new(|_| DesktopTooltipContentView {
                                elem_id: elem_id.clone(),
                                text: text.clone(),
                                frosted,
                            })
                            .into()
                        })
                        .with_offset(space::XS);
                        overlay.request_show(request, window, cx);
                    });
                } else if this
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.lifetime.ptr_eq(&anchor.lifetime))
                {
                    this.overlay
                        .update(cx, |overlay, cx| overlay.request_hide(window, cx));
                }
            });
        })
    }
}

impl<E: StatefulInteractiveElement + ElementExt> DesktopTooltipExt for E {}
