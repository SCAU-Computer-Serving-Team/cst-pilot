use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{FontWeight, Overflow, StyleRefinement, rems};
use gpui_kit::assets::IconName;
use gpui_kit::base::Easing;
use gpui_kit::base::text::{
    SelectionFormat, TextView, TextViewMotion, TextViewState, TextViewStyle,
};
use gpui_kit::component::Icon;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, CursorStyle, Entity, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, div, px,
};

use crate::shell::open_url_in_trusted_browser;
use crate::theme::{A11yElementExt as _, DesktopPalette, apply_scale, radius, scale, space};

/// Maintains stable `Entity<TextViewState>` instances keyed by message/entry ID so streaming
/// incremental deltas (`push_str`) and snapshot replacements (`set_text`) never recreate the view
/// or drop active user text selections.
#[derive(Default)]
pub struct MarkdownStreamStore {
    states: BTreeMap<String, (String, Entity<TextViewState>)>,
}

impl MarkdownStreamStore {
    /// Synchronizes `key` with `markdown_source`, calling `push_str` when `markdown_source` extends
    /// the previously committed prefix or `set_text` on initial load / full replacement.
    pub fn sync_entry(
        &mut self,
        key: &str,
        markdown_source: &str,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        if let Some((prev_text, entity)) = self.states.get_mut(key) {
            if prev_text != markdown_source {
                if !prev_text.is_empty()
                    && let Some(delta) = markdown_source.strip_prefix(prev_text.as_str())
                {
                    let delta_owned = delta.to_owned();
                    entity.update(cx, |st, cx| {
                        st.push_str(&delta_owned, cx);
                    });
                } else {
                    let full = markdown_source.to_owned();
                    entity.update(cx, |st, cx| {
                        st.set_text(&full, cx);
                    });
                }
                prev_text.clear();
                prev_text.push_str(markdown_source);
            }
            return entity.clone();
        }

        let initial = markdown_source.to_owned();
        let entity = cx.new(|cx| {
            TextViewState::markdown(&initial, cx)
                .selectable(true)
                .selection_format(SelectionFormat::SourceAcrossStyles)
        });
        self.states
            .insert(key.to_owned(), (markdown_source.to_owned(), entity.clone()));
        entity
    }

    pub fn retain_keys(&mut self, live_keys: &BTreeSet<String>) {
        self.states.retain(|k, _| live_keys.contains(k));
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

pub fn build_markdown_style(palette: &DesktopPalette) -> TextViewStyle {
    let mut table_style = StyleRefinement::default().rounded(radius::ROW);
    table_style.overflow.x = Some(Overflow::Scroll);

    let code_block_style = StyleRefinement::default().pr(px(40.0)).rounded(radius::ROW);

    TextViewStyle::default()
        .with_foreground(palette.text_primary)
        .with_muted_foreground(palette.text_secondary)
        .with_link(palette.accent_ink)
        .with_code_background(palette.bg_surface)
        .with_border(palette.border_subtle)
        .with_paragraph_gap(rems(1.0))
        .with_heading(|level| match level {
            1 | 2 => StyleRefinement::default().font_weight(FontWeight::SEMIBOLD),
            _ => StyleRefinement::default().font_weight(FontWeight::SEMIBOLD),
        })
        .with_table(table_style)
        .with_code_block(code_block_style)
        .with_dark(palette.mode.is_dark())
}

/// Renders a streaming or finalized assistant Markdown block backed by a stable `Entity<TextViewState>`.
pub fn render_assistant_markdown(
    state: &Entity<TextViewState>,
    is_streaming: bool,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let palette_copy = *palette;
    let copied_code_spans: Arc<Mutex<BTreeMap<usize, Instant>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let state_weak = state.downgrade();
    let motion = if is_streaming {
        TextViewMotion::default()
            .with_stream_fade(Duration::from_millis(280))
            .with_stream_fade_stagger(Duration::from_millis(10))
            .with_stream_fade_easing(Easing::EaseOut)
    } else {
        TextViewMotion::default()
    };

    apply_scale(div(), scale::BODY)
        .w_full()
        .min_w_0()
        .text_color(palette.text_primary)
        .child(
            TextView::new(state)
                .style(build_markdown_style(palette))
                .selectable(true)
                .scrollable(false)
                .motion(motion)
                .code_block_actions(move |block, _window, _cx| {
                    let span_start = block.span.as_ref().map(|s| s.start).unwrap_or(0);
                    let copied_recently = copied_code_spans
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&span_start).copied())
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                    let code_str = block.code();
                    let map_click = Arc::clone(&copied_code_spans);
                    let weak_notify = state_weak.clone();

                    let code_copy_id = SharedString::from(format!("md-code-copy-{span_start}"));
                    div()
                        .id(code_copy_id.clone())
                        .test_support()
                        .a11y_button(code_copy_id, "复制代码块")
                        .w(px(32.0))
                        .h(px(32.0))
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |s| s.bg(palette_copy.bg_item_hover))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(code_str.to_string()));
                            if let Ok(mut map) = map_click.lock() {
                                map.insert(span_start, Instant::now());
                            }
                            if let Some(ent) = weak_notify.upgrade() {
                                ent.update(cx, |_, cx| cx.notify());
                            }
                            let map_expire = Arc::clone(&map_click);
                            let weak_expire = weak_notify.clone();
                            cx.spawn(async move |cx| {
                                cx.background_executor().timer(Duration::from_secs(2)).await;
                                if let Ok(mut map) = map_expire.lock() {
                                    map.remove(&span_start);
                                }
                                if let Some(ent) = weak_expire.upgrade() {
                                    ent.update(cx, |_, cx| cx.notify());
                                }
                            })
                            .detach();
                        })
                        .child(
                            Icon::new(if copied_recently {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .size(px(14.0))
                            .text_color(if copied_recently {
                                palette_copy.status_success
                            } else {
                                palette_copy.text_secondary
                            }),
                        )
                })
                .on_link_click(|url, _event, _window, _cx| {
                    open_url_in_trusted_browser(url.as_ref());
                }),
        )
}

/// Renders a `12px/16px` (`$font-caption`) monospace or plain block (`$bg-surface`) with a
/// persistent top-right `32×32px` copy button (`copy` -> 2s `check` swap) and right padding
/// so text is never occluded.
#[allow(clippy::too_many_arguments)]
pub fn render_copyable_text_block<V: 'static>(
    block_id: impl Into<SharedString>,
    text: impl Into<SharedString>,
    monospace: bool,
    mono_family: SharedString,
    copied_recently: bool,
    palette: &DesktopPalette,
    on_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let block_id = block_id.into();
    let text_shared = text.into();
    let copy_btn_id = SharedString::from(format!("{block_id}-copy-btn"));
    let text_for_copy = text_shared.clone();
    let id_for_cb = block_id.clone();

    let mut content_div = apply_scale(div(), scale::CAPTION)
        .w_full()
        .min_w_0()
        .pr(px(36.0))
        .text_color(palette.text_secondary);
    if monospace {
        content_div = content_div.font_family(mono_family);
    }

    div()
        .id(block_id)
        .relative()
        .w_full()
        .min_w_0()
        .p(space::MD)
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_surface)
        .child(content_div.child(text_shared))
        .child(
            div()
                .id(copy_btn_id.clone())
                .test_support()
                .a11y_button(copy_btn_id, "复制文本内容")
                .absolute()
                .top(space::XS)
                .right(space::XS)
                .w(px(32.0))
                .h(px(32.0))
                .rounded(radius::CONTROL)
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(palette.bg_item_hover))
                .on_click(cx.listener(move |view, _, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text_for_copy.to_string()));
                    on_copied(id_for_cb.clone(), window, cx);
                    let _ = view;
                }))
                .child(
                    Icon::new(if copied_recently {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .size(px(14.0))
                    .text_color(if copied_recently {
                        palette.status_success
                    } else {
                        palette.text_secondary
                    }),
                ),
        )
}

/// Helper tracker that remembers recently-copied IDs for 2 seconds (`iconSwap` contract).
#[derive(Default)]
pub struct CopyFeedbackTracker {
    copied_ids: BTreeSet<SharedString>,
}

impl CopyFeedbackTracker {
    pub fn is_copied(&self, id: &str) -> bool {
        self.copied_ids.contains(id)
    }

    pub fn mark_copied<V: 'static>(
        &mut self,
        id: SharedString,
        get_mut: fn(&mut V) -> &mut Self,
        cx: &mut Context<V>,
    ) {
        self.copied_ids.insert(id.clone());
        cx.notify();
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = weak.update(cx, |view, cx| {
                get_mut(view).copied_ids.remove(&id);
                cx.notify();
            });
        })
        .detach();
    }
}

/// Renders an external link pill with explicit pointer hand cursor (`CursorStyle::PointingHand`).
pub fn external_link_cursor() -> CursorStyle {
    CursorStyle::PointingHand
}
