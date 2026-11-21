use std::rc::Rc;

use cst_client::{ClientError, ClientHandle};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    App, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};
use serde_json::Value;

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleQueueItem {
    pub id: String,
    pub text: String,
    pub status: String,
    pub delivery: String,
    pub uncertain: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQueueSnapshot {
    pub version: u64,
    pub paused: bool,
    pub items: Vec<VisibleQueueItem>,
}

impl ParsedQueueSnapshot {
    /// Parses `session.queue` JSON and filters `visibleQueueItems` identically to `conversation-parts.ts`.
    pub fn from_json(raw: Option<&Value>) -> Self {
        let Some(obj) = raw.and_then(Value::as_object) else {
            return Self::default();
        };
        let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
        let paused = obj.get("paused").and_then(Value::as_bool).unwrap_or(false);
        let mut items = Vec::new();
        if let Some(arr) = obj.get("items").and_then(Value::as_array) {
            for item in arr {
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let delivery = item
                    .get("delivery")
                    .and_then(Value::as_str)
                    .unwrap_or("queue")
                    .to_owned();
                let uncertain = item
                    .get("uncertain")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let visible = status == "failed"
                    || (status == "delivering" && uncertain)
                    || (status == "pending" && delivery != "direct");
                if !visible {
                    continue;
                }
                let Some(id) = item
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                items.push(VisibleQueueItem {
                    id: id.to_owned(),
                    text: item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    status,
                    delivery,
                    uncertain,
                    error: item.get("error").and_then(Value::as_str).map(str::to_owned),
                });
            }
        }
        Self {
            version,
            paused,
            items,
        }
    }

    pub fn reorderable_ids(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| i.status == "pending" && i.delivery == "queue")
            .map(|i| i.id.clone())
            .collect()
    }
}

pub(super) enum QueueMutation {
    Steer(String),
    Retry(String),
    Delete(String),
    Reorder(Vec<String>),
    Edit(String, String),
}

impl QueueMutation {
    pub(super) async fn send(
        self,
        client: ClientHandle,
        sid: String,
        version: u64,
    ) -> Result<Value, ClientError> {
        match self {
            Self::Steer(id) => client.steer_queue_item(sid, id, version).await,
            Self::Retry(id) => client.retry_queue_item(sid, id, version).await,
            Self::Delete(id) => client.delete_queue_item(sid, id, version).await,
            Self::Reorder(ids) => client.reorder_queue(sid, version, ids).await,
            Self::Edit(id, text) => client.edit_queue_item(sid, id, version, text).await,
        }
    }

    pub(super) fn project(&self, mut queue: ParsedQueueSnapshot) -> ParsedQueueSnapshot {
        match self {
            Self::Reorder(ids) => {
                let reordered: Vec<_> = ids
                    .iter()
                    .filter_map(|id| queue.items.iter().find(|item| &item.id == id).cloned())
                    .collect();
                for (slot, item) in queue
                    .items
                    .iter_mut()
                    .filter(|i| i.status == "pending" && i.delivery == "queue")
                    .zip(reordered)
                {
                    *slot = item;
                }
            }
            Self::Delete(id) => queue.items.retain(|item| &item.id != id),
            _ => {}
        }
        queue
    }
}

pub type QueueItemActionFn = Rc<dyn Fn(String, u64, &mut Window, &mut App)>;
pub type QueueReorderFn = Rc<dyn Fn(u64, Vec<String>, &mut Window, &mut App)>;
pub type QueueBeginEditFn = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
pub type QueueCommitEditFn = Rc<dyn Fn(String, u64, &mut Window, &mut App)>;
pub type QueueCancelEditFn = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct QueueBarCallbacks {
    pub on_steer: QueueItemActionFn,
    pub on_retry: QueueItemActionFn,
    pub on_delete: QueueItemActionFn,
    pub on_reorder: QueueReorderFn,
    pub on_begin_edit: QueueBeginEditFn,
    pub on_commit_edit: QueueCommitEditFn,
    pub on_cancel_edit: QueueCancelEditFn,
}

/// Renders the temporary pending-message `QueueBar` (`cst-pilot-desktop.pen` 「01 · 工作台」 `rnehA`)
/// inside the top of the composer container, supporting reorder (`version`), `↑ 立即` steer, inline edit, and delete.
#[allow(clippy::too_many_arguments)]
pub fn render_queue_bar(
    queue: &ParsedQueueSnapshot,
    session_id: &str,
    editing_item_id: Option<&str>,
    edit_input_state: Option<&Entity<InputState>>,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
    cb: QueueBarCallbacks,
) -> Option<gpui_kit::AnyElement> {
    if queue.items.is_empty() {
        return None;
    }

    let failed_item = queue.items.iter().find(|i| i.status == "failed");
    let has_uncertain = queue.items.iter().any(|i| i.uncertain);
    let queued_ids = queue.reorderable_ids();
    let version = queue.version;

    let header_text = if let Some(f) = failed_item {
        match f.error.as_deref().filter(|s| !s.is_empty()) {
            Some(reason) => format!("投递失败 · {reason}"),
            None => "投递失败 · 请检查模型服务".to_owned(),
        }
    } else if has_uncertain {
        "上次投递结果待核对 · 请核对对话后再补充消息，不会自动重发".to_owned()
    } else if queue.paused {
        format!("排队已暂停（{} 条）· 发送新消息后继续", queue.items.len())
    } else {
        format!("排队 · {}", queue.items.len())
    };

    let header_color = if failed_item.is_some() {
        palette.status_danger
    } else if has_uncertain || queue.paused {
        palette.status_warning
    } else {
        palette.text_secondary
    };

    let mut list = div()
        .id("composer-queue-bar-rneha")
        .test_support()
        .a11y_region("composer-queue-bar-rneha", "排队消息栏")
        .pb(space::XS)
        .border_b_1()
        .border_color(palette.border_subtle)
        .flex()
        .flex_col();

    list = list.child(
        apply_scale(div(), scale::CAPTION_MEDIUM)
            .h(height::INLINE)
            .flex()
            .items_center()
            .text_color(header_color)
            .child(SharedString::from(header_text)),
    );

    let flip_scope = SharedString::from(session_id.to_owned());
    for (index, item) in queue.items.iter().enumerate() {
        let iid = item.id.clone();
        let is_editing = editing_item_id == Some(iid.as_str());

        if is_editing
            && matches!(item.status.as_str(), "pending" | "failed")
            && let Some(input_entity) = edit_input_state
        {
            let commit_cb = Rc::clone(&cb.on_commit_edit);
            let cancel_cb = Rc::clone(&cb.on_cancel_edit);
            let iid_save = iid.clone();
            let edit_input_id = SharedString::from(format!("queue-edit-input-{iid}"));
            let save_btn_id = SharedString::from(format!("queue-save-btn-{iid}"));
            let cancel_btn_id = SharedString::from(format!("queue-cancel-edit-btn-{iid}"));

            list = list.child(
                div()
                    .id(SharedString::from(format!("queue-row-edit-{iid}")))
                    .h(height::CONTROL)
                    .border_t_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(input_entity)
                                .accessibility_id(edit_input_id)
                                .aria_label("编辑排队消息"),
                        ),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(save_btn_id.clone())
                            .test_support()
                            .a11y_button(save_btn_id, "保存排队消息")
                            .tab_index(0)
                            .h(height::INLINE)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .on_click(move |_, w, cx| {
                                (commit_cb)(iid_save.clone(), version, w, cx);
                            })
                            .child(cjk_inline_label("保存")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(cancel_btn_id.clone())
                            .test_support()
                            .a11y_button(cancel_btn_id, "取消编辑排队消息")
                            .tab_index(0)
                            .h(height::INLINE)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .on_click(move |_, w, cx| (cancel_cb)(w, cx))
                            .child(cjk_inline_label("取消")),
                    ),
            );
            continue;
        }

        let display_label = if item.text.trim().is_empty() {
            SharedString::from("图片消息")
        } else {
            SharedString::from(item.text.clone())
        };

        let can_reorder = item.status == "pending" && item.delivery == "queue";
        let pos_in_queue = queued_ids.iter().position(|x| x == &iid);

        let mut actions = div().flex().items_center().gap(space::XS).flex_shrink_0();

        if can_reorder
            && queued_ids.len() > 1
            && let Some(pos) = pos_in_queue
        {
            if pos > 0 {
                let reorder_up = Rc::clone(&cb.on_reorder);
                let mut next_ids = queued_ids.clone();
                next_ids.swap(pos, pos - 1);
                let up_id = SharedString::from(format!("queue-move-up-{iid}"));
                actions = actions.child(
                    div()
                        .id(up_id.clone())
                        .test_support()
                        .a11y_button(up_id, "上移排队消息")
                        .tab_index(0)
                        .w(height::INLINE)
                        .h(height::INLINE)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| {
                            (reorder_up)(version, next_ids.clone(), w, cx);
                        })
                        .child(
                            Icon::new(IconName::ChevronUp)
                                .size(px(13.0))
                                .text_color(palette.text_muted),
                        ),
                );
            }
            if pos + 1 < queued_ids.len() {
                let reorder_down = Rc::clone(&cb.on_reorder);
                let mut next_ids = queued_ids.clone();
                next_ids.swap(pos, pos + 1);
                let down_id = SharedString::from(format!("queue-move-down-{iid}"));
                actions = actions.child(
                    div()
                        .id(down_id.clone())
                        .test_support()
                        .a11y_button(down_id, "下移排队消息")
                        .tab_index(0)
                        .w(height::INLINE)
                        .h(height::INLINE)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| {
                            (reorder_down)(version, next_ids.clone(), w, cx);
                        })
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(px(13.0))
                                .text_color(palette.text_muted),
                        ),
                );
            }
        }

        if item.status == "pending" || item.status == "failed" {
            let is_failed = item.status == "failed";
            let action_fn = if is_failed {
                Rc::clone(&cb.on_retry)
            } else {
                Rc::clone(&cb.on_steer)
            };
            let iid_act = iid.clone();
            let iid_edit = iid.clone();
            let raw_text_edit = item.text.clone();
            let edit_fn = Rc::clone(&cb.on_begin_edit);
            let iid_del = iid.clone();
            let del_fn = Rc::clone(&cb.on_delete);
            let pri_id = SharedString::from(if is_failed {
                format!("queue-retry-btn-{iid}")
            } else {
                format!("queue-steer-btn-{iid}")
            });
            let edit_id = SharedString::from(format!("queue-edit-btn-{iid}"));
            let del_id = SharedString::from(format!("queue-delete-btn-{iid}"));

            actions = actions
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id(pri_id.clone())
                        .test_support()
                        .tab_index(0)
                        .a11y_button(
                            pri_id,
                            if is_failed {
                                "重试排队消息"
                            } else {
                                "立即插队发送"
                            },
                        )
                        .h(height::INLINE)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .bg(palette.bg_workspace)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .gap(space::MICRO)
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| {
                            (action_fn)(iid_act.clone(), version, w, cx);
                        })
                        .child(
                            Icon::new(if is_failed {
                                IconName::RotateCcw
                            } else {
                                IconName::ArrowUp
                            })
                            .size(px(12.0))
                            .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label(if is_failed {
                            "重试"
                        } else {
                            "立即"
                        })),
                )
                .child(
                    div()
                        .id(edit_id.clone())
                        .test_support()
                        .a11y_button(edit_id, "编辑排队消息")
                        .tab_index(0)
                        .w(height::INLINE)
                        .h(height::INLINE)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| {
                            (edit_fn)(iid_edit.clone(), raw_text_edit.clone(), w, cx);
                        })
                        .child(
                            Icon::new(IconName::Pencil)
                                .size(px(14.0))
                                .text_color(palette.text_secondary),
                        ),
                )
                .child(
                    div()
                        .id(del_id.clone())
                        .test_support()
                        .a11y_button(del_id, "删除排队消息")
                        .tab_index(0)
                        .w(height::INLINE)
                        .h(height::INLINE)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| {
                            (del_fn)(iid_del.clone(), version, w, cx);
                        })
                        .child(
                            Icon::new(IconName::Trash)
                                .size(px(14.0))
                                .text_color(palette.text_secondary),
                        ),
                );
        }

        let flip_y = motion::flip_row_offset_y(
            "composer-queue",
            &iid,
            &flip_scope,
            height::CONTROL * index as f32,
            window,
            cx,
        );
        list = list.child(
            div()
                .id(SharedString::from(format!("queue-item-row-{iid}")))
                .test_support()
                .relative()
                .top(flip_y)
                .h(height::CONTROL)
                .border_t_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::UI)
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(space::TIGHT)
                        .child(
                            Icon::new(IconName::GripVertical)
                                .size(px(13.0))
                                .text_color(palette.text_muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(display_label)),
                        ),
                )
                .child(actions),
        );
    }

    Some(list.into_any_element())
}
