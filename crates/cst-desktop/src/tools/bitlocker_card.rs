use std::time::{SystemTime, UNIX_EPOCH};

use cst_client::ClientHandle;
use cst_client::tool_presenter::{FieldRow, SpecialStatus, SpecialVolumeView};
use gpui::{ClipboardItem, CursorStyle};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};
use serde_json::Value;

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};
use crate::tools::{render_special_details_section, render_tool_notice_line};
use gpui::relative;

pub use super::secret_input::{MAX_RECOVERY_PASSWORD_BYTES, SecretInput, SecretInputEvent};

#[derive(Debug, Clone)]
pub struct PendingBitLockerPrompt {
    pub request_id: String,
    pub kind: String,
    pub drive: String,
    pub prompt: String,
    pub expires_at_ms: u64,
    pub recovery_key_id: Option<String>,
}

impl PendingBitLockerPrompt {
    pub(crate) fn is_expired(&self) -> bool {
        self.expires_at_ms > 0 && now_epoch_ms() >= self.expires_at_ms
    }
}

fn extract_recovery_key_id_from_request(chosen: &Value) -> Option<String> {
    if let Some(direct) = chosen
        .get("keyId")
        .or_else(|| chosen.get("recoveryKeyId"))
        .or_else(|| chosen.get("protectorId"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Some(direct.to_owned());
    }
    if let Some(targets) = chosen.get("targets").and_then(Value::as_array) {
        for vol in targets {
            if let Some(protectors) = vol.get("keyProtectors").and_then(Value::as_array) {
                if let Some(num_pw) = protectors.iter().find(|p| {
                    p.get("type").and_then(Value::as_u64) == Some(3)
                        && p.get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.trim().is_empty())
                }) {
                    return num_pw.get("id").and_then(Value::as_str).map(str::to_owned);
                }
                if let Some(any_prot) = protectors.iter().find_map(|p| {
                    p.get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                }) {
                    return Some(any_prot.to_owned());
                }
            }
        }
    }
    None
}

/// Extracts an active pending BitLocker request explicitly bound to this tool call.
pub fn find_pending_bitlocker_request(
    bitlocker_snapshot: Option<&Value>,
    tool_call_id: &str,
    has_result: bool,
) -> Option<PendingBitLockerPrompt> {
    if has_result {
        return None;
    }
    let arr = bitlocker_snapshot?
        .get("requests")
        .and_then(Value::as_array)?;
    let chosen = arr
        .iter()
        .find(|r| r.get("toolCallId").and_then(Value::as_str) == Some(tool_call_id))?;

    let request_id = chosen
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .or_else(|| {
            chosen
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
        })?
        .to_owned();
    let kind = chosen
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("password")
        .to_owned();
    let drive = chosen
        .get("drive")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            chosen
                .get("targets")
                .and_then(Value::as_array)
                .and_then(|t| t.first())
                .and_then(|v| v.get("drive"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let prompt = chosen
        .get("prompt")
        .or_else(|| chosen.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let expires_at_ms = chosen.get("expiresAt").and_then(Value::as_u64).unwrap_or(0);
    let recovery_key_id = extract_recovery_key_id_from_request(chosen);

    Some(PendingBitLockerPrompt {
        request_id,
        kind,
        drive,
        prompt,
        expires_at_ms,
        recovery_key_id,
    })
}

pub struct BitLockerCardProps<'a> {
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub raw: &'a str,
    pub scope: &'a str,
    pub volumes: &'a [SpecialVolumeView],
    pub fields: &'a [FieldRow],
    pub errors: &'a [String],
    pub pending: Option<PendingBitLockerPrompt>,
    pub secret_input: Option<Entity<SecretInput>>,
    pub details_expanded: bool,
}

/// Renders the standalone BitLocker Controlled Interaction & Result Card (`bitlocker` `unlock / decrypt`,
/// `cst-pilot-desktop.pen` `ABn8j`).
#[allow(clippy::too_many_arguments)]
pub fn render_bitlocker_card<V: 'static>(
    props: BitLockerCardProps<'_>,
    client: ClientHandle,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_details: crate::tools::ToggleDetailsFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    on_error_notice: impl Fn(String, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let now = now_epoch_ms();
    let pending_expired = props
        .pending
        .as_ref()
        .is_some_and(|p| p.expires_at_ms > 0 && now >= p.expires_at_ms);

    if pending_expired && let Some(ref si) = props.secret_input {
        si.update(cx, |s, cx| {
            if !s.is_empty() || !s.is_disabled() {
                s.clear(cx);
                s.set_disabled(true, cx);
            }
            let focus = s.focus_handle(cx);
            if focus.is_focused(window) {
                window.defer(cx, move |window, cx| {
                    if focus.is_focused(window) {
                        window.blur(cx);
                    }
                });
            }
        });
    }

    // Request frame refresh while active 180s countdown is running; freezes at 0 FPS once resolved/expired
    let countdown_display = props.pending.as_ref().and_then(|p| {
        if p.expires_at_ms == 0 {
            return None;
        }
        if p.expires_at_ms > now {
            window.request_animation_frame();
            let rem = p.expires_at_ms.saturating_sub(now).div_ceil(1000);
            let mins = rem / 60;
            let secs = rem % 60;
            let color = if rem <= 30 {
                palette.status_warning
            } else {
                palette.text_secondary
            };
            Some((format!("剩余 {mins:02}:{secs:02}"), color))
        } else {
            Some(("请求已过期".to_owned(), palette.text_secondary))
        }
    });

    let has_pending_ui = props.pending.is_some();

    let is_unknown = !has_pending_ui
        && (props.status == SpecialStatus::Unknown
            || (props.status == SpecialStatus::Success && props.volumes.is_empty()));

    let (status_label, status_color, status_icon) = if has_pending_ui {
        if pending_expired {
            ("已超时", palette.status_danger, IconName::Clock3)
        } else if props.pending.as_ref().is_some_and(|p| p.kind == "password") {
            ("等待输入", palette.text_secondary, IconName::Clock3)
        } else {
            ("等待确认", palette.text_secondary, IconName::Clock3)
        }
    } else if is_unknown {
        (
            "结果未确认",
            palette.status_warning,
            IconName::TriangleAlert,
        )
    } else {
        match props.status {
            SpecialStatus::Pending | SpecialStatus::Running => {
                ("处理中", palette.accent_ink, IconName::LoaderCircle)
            }
            SpecialStatus::Success => {
                if props.volumes.iter().any(|v| v.conversion_code == Some(3)) {
                    ("解密中", palette.status_success, IconName::CircleCheck)
                } else if props.volumes.iter().any(|v| v.conversion_code == Some(2)) {
                    ("加密中", palette.status_success, IconName::CircleCheck)
                } else {
                    ("已完成", palette.status_success, IconName::CircleCheck)
                }
            }
            SpecialStatus::Degraded => {
                ("部分完成", palette.status_warning, IconName::TriangleAlert)
            }
            SpecialStatus::Cancelled => ("已取消", palette.text_muted, IconName::Clock3),
            SpecialStatus::Interrupted => {
                ("已中断", palette.status_warning, IconName::TriangleAlert)
            }
            SpecialStatus::Error => ("执行失败", palette.status_danger, IconName::CircleAlert),
            SpecialStatus::Unknown => (
                "结果未确认",
                palette.status_warning,
                IconName::TriangleAlert,
            ),
        }
    };

    let right_status = div()
        .flex()
        .items_center()
        .gap(space::XS)
        .flex_shrink_0()
        .child(
            Icon::new(status_icon)
                .size(px(12.0))
                .text_color(status_color),
        )
        .child(
            apply_scale(div(), scale::CAPTION)
                .id(SharedString::from(format!(
                    "bitlocker-status-label-{}",
                    props.call_id
                )))
                .test_support()
                .text_color(palette.text_secondary)
                .child(cjk_inline_label(status_label)),
        );

    let header_title = if !props.title.trim().is_empty() {
        props.title.to_owned()
    } else if let Some(ref p) = props.pending
        && !p.drive.is_empty()
    {
        if p.kind == "decrypt" || props.scope == "decrypt" {
            format!("关闭加密 {}", p.drive)
        } else {
            format!("解锁 {}", p.drive)
        }
    } else {
        "BitLocker 卷操作".to_owned()
    };

    let mut card = div()
        .id(SharedString::from(format!(
            "special-bitlocker-card-{}",
            props.call_id
        )))
        .test_support()
        .a11y_region(
            SharedString::from(format!("bitlocker-card-{}", props.call_id)),
            header_title.clone(),
        )
        .w_full()
        .p(space::LG)
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .shadow(palette.craft_stack_shadows())
        .flex()
        .flex_col()
        .gap(space::LG)
        .child(
            div()
                .h(height::COMPACT)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .min_w_0()
                        .child(
                            Icon::new(IconName::LockKeyhole)
                                .size(px(18.0))
                                .text_color(palette.text_primary),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_MEDIUM)
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(header_title)),
                        ),
                )
                .child(right_status),
        );

    // 1. Active Pending Interaction (`password` / `parameter` / `decrypt`)
    if let Some(pending) = props.pending {
        if !pending.prompt.trim().is_empty() {
            card = card.child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child(SharedString::from(pending.prompt.clone())),
            );
        }

        let countdown_el = countdown_display.map(|(txt, col)| {
            apply_scale(div(), scale::CAPTION)
                .font_family(mono_family.clone())
                .text_color(col)
                .flex_shrink_0()
                .child(SharedString::from(txt))
        });

        if pending.kind == "password"
            && let Some(secret_entity) = props.secret_input
        {
            // Optional `24px` Recovery Key ID row (`yD9G3`)
            if let Some(key_id) = pending.recovery_key_id {
                let key_id_copy = key_id.clone();
                card = card.child(
                    div()
                        .w_full()
                        .h(height::INLINE)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::MD)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space::MD)
                                .min_w_0()
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .flex_shrink_0()
                                        .text_color(palette.text_secondary)
                                        .child("密钥 ID"),
                                )
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .truncate()
                                        .font_family(mono_family.clone())
                                        .text_color(palette.text_primary)
                                        .child(SharedString::from(key_id)),
                                ),
                        )
                        .child({
                            let copy_key_btn_id = SharedString::from(format!(
                                "bitlocker-copy-key-id-{}",
                                props.call_id
                            ));
                            let copied = is_copied_fn(copy_key_btn_id.as_ref());
                            let mark_cb = on_mark_copied.clone();
                            let cid_click = copy_key_btn_id.clone();
                            let motion_id = copy_key_btn_id.clone();
                            div()
                                .id(copy_key_btn_id.clone())
                                .test_support()
                                .a11y_button(copy_key_btn_id, "复制密钥 ID")
                                .w(height::INLINE)
                                .h(height::INLINE)
                                .flex_shrink_0()
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(move |_, _, w, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        key_id_copy.clone(),
                                    ));
                                    mark_cb(cid_click.clone(), w, cx);
                                }))
                                .child(motion::animated_copy_icon(
                                    motion_id,
                                    copied,
                                    px(14.0),
                                    if copied {
                                        palette.status_success
                                    } else {
                                        palette.text_secondary
                                    },
                                    window,
                                    cx,
                                ))
                        }),
                );
            }

            let si_submit = secret_entity.clone();
            let si_cancel = secret_entity.clone();

            card = card
                .child(secret_entity)
                // `32px` Action bar (`h1sNW`): left `[主按钮 (px: 16px, accent_solid, "解锁")] + [次按钮 (px: 12px, border_strong, "取消")]` | right `剩余 MM:SS`
                .child(
                    div()
                        .w_full()
                        .h(height::CONTROL)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::MD)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .child(
                                    apply_scale(div(), scale::UI_MEDIUM)
                                        .id(SharedString::from(format!(
                                            "bitlocker-unlock-submit-btn-{}",
                                            props.call_id
                                        )))
                                        .test_support()
                                        .a11y_button(
                                            SharedString::from(format!(
                                                "bitlocker-submit-btn-{}",
                                                props.call_id
                                            )),
                                            "解锁",
                                        )
                                        .h(height::CONTROL)
                                        .px(space::LG)
                                        .rounded(radius::CONTROL)
                                        .bg(palette.accent_solid)
                                        .text_color(palette.text_on_accent)
                                        .opacity(if pending_expired { 0.45 } else { 1.0 })
                                        .flex()
                                        .items_center()
                                        .hover(|s| s.bg(palette.accent_hover))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            si_submit.update(cx, |_, cx| {
                                                cx.emit(SecretInputEvent::SubmitRequested);
                                            });
                                        }))
                                        .child(cjk_inline_label("解锁")),
                                )
                                .child(
                                    apply_scale(div(), scale::UI_MEDIUM)
                                        .id(SharedString::from(format!(
                                            "bitlocker-cancel-btn-{}",
                                            props.call_id
                                        )))
                                        .test_support()
                                        .a11y_button(
                                            SharedString::from(format!(
                                                "bitlocker-cancel-btn-{}",
                                                props.call_id
                                            )),
                                            "取消",
                                        )
                                        .h(height::CONTROL)
                                        .px(space::MD)
                                        .rounded(radius::CONTROL)
                                        .border_1()
                                        .border_color(palette.border_strong)
                                        .bg(palette.bg_elevated)
                                        .text_color(palette.text_primary)
                                        .flex()
                                        .items_center()
                                        .hover(|s| s.bg(palette.bg_item_hover))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            si_cancel.update(cx, |_, cx| {
                                                cx.emit(SecretInputEvent::CancelRequested);
                                            });
                                        }))
                                        .child(cjk_inline_label("取消")),
                                ),
                        )
                        .children(countdown_el),
                )
                // External recovery key lookup link (`x2GW4`)
                .child(
                    div().w_full().flex().items_center().child(
                        apply_scale(div(), scale::CAPTION)
                            .id(SharedString::from(format!(
                                "bitlocker-recovery-link-{}",
                                props.call_id
                            )))
                            .test_support()
                            .a11y_link(
                                SharedString::from(format!(
                                    "bitlocker-recovery-link-{}",
                                    props.call_id
                                )),
                                "查找恢复密钥",
                            )
                            .h(px(20.0))
                            .cursor(CursorStyle::PointingHand)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .text_color(palette.text_secondary)
                            .hover(|s| s.text_color(palette.accent_ink))
                            .on_click(|_, _, _| {
                                crate::shell::open_url_in_trusted_browser(
                                    "https://aka.ms/myrecoverykey",
                                );
                            })
                            .child(cjk_inline_label("查找恢复密钥"))
                            .child(
                                Icon::new(IconName::ExternalLink)
                                    .size(px(12.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
                );
        } else if pending.kind != "password" {
            // `parameter` (AI-prefilled local confirmation) or `decrypt` (turn off encryption confirmation)
            let is_decrypt = pending.kind == "decrypt" || props.scope == "decrypt";
            if is_decrypt {
                card = card.child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .size(px(14.0))
                                .text_color(palette.status_warning),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .flex_1()
                                .min_w_0()
                                .text_color(palette.status_warning)
                                .child("关闭 BitLocker 将对目标卷执行全盘解密，耗时较长期间请勿断电或拔出磁盘。"),
                        ),
                );
            }
            if !props.volumes.is_empty() {
                card = card.child(render_volume_rows(
                    props.volumes,
                    mono_family.clone(),
                    palette,
                ));
            }

            let sid_cancel = props.session_id.to_owned();
            let rid_cancel = pending.request_id.clone();
            let client_cancel = client.clone();
            let err_cancel_cb = on_error_notice.clone();

            let sid_confirm = props.session_id.to_owned();
            let rid_confirm = pending.request_id.clone();
            let client_confirm = client;
            let err_confirm_cb = on_error_notice;

            card = card.child(
                div()
                    .w_full()
                    .h(height::CONTROL)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id(SharedString::from(format!(
                                        "bitlocker-confirm-btn-{}",
                                        props.call_id
                                    )))
                                    .test_support()
                                    .a11y_button(
                                        SharedString::from(format!(
                                            "bitlocker-confirm-btn-{}",
                                            props.call_id
                                        )),
                                        if is_decrypt {
                                            "确认关闭加密"
                                        } else {
                                            "确认执行"
                                        },
                                    )
                                    .h(height::CONTROL)
                                    .px(space::LG)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.accent_solid)
                                    .text_color(palette.text_on_accent)
                                    .opacity(if pending_expired { 0.45 } else { 1.0 })
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.accent_hover))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        if pending_expired {
                                            return;
                                        }
                                        let c = client_confirm.clone();
                                        let s_id = sid_confirm.clone();
                                        let r_id = rid_confirm.clone();
                                        let e_cb = err_confirm_cb.clone();
                                        cx.spawn(async move |weak, cx| {
                                            if let Err(err) =
                                                c.respond_bitlocker_confirm(s_id, r_id, true).await
                                            {
                                                let _ = weak
                                                    .update(cx, |_, cx| e_cb(err.to_string(), cx));
                                            }
                                        })
                                        .detach();
                                    }))
                                    .child(cjk_inline_label(if is_decrypt {
                                        "确认关闭加密"
                                    } else {
                                        "确认执行"
                                    })),
                            )
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id(SharedString::from(format!(
                                        "bitlocker-cancel-btn-{}",
                                        props.call_id
                                    )))
                                    .test_support()
                                    .a11y_button(
                                        SharedString::from(format!(
                                            "bitlocker-cancel-btn-{}",
                                            props.call_id
                                        )),
                                        "取消",
                                    )
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(palette.bg_elevated)
                                    .text_color(palette.text_primary)
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        let c = client_cancel.clone();
                                        let s_id = sid_cancel.clone();
                                        let r_id = rid_cancel.clone();
                                        let e_cb = err_cancel_cb.clone();
                                        cx.spawn(async move |weak, cx| {
                                            if let Err(err) =
                                                c.cancel_bitlocker_request(s_id, r_id).await
                                            {
                                                let _ = weak
                                                    .update(cx, |_, cx| e_cb(err.to_string(), cx));
                                            }
                                        })
                                        .detach();
                                    }))
                                    .child(cjk_inline_label("取消")),
                            ),
                    )
                    .children(countdown_el),
            );
        }

        return card.into_any_element();
    }

    // 2. Settled or Unknown-Review Result Template
    let trimmed_summary = props.summary.trim();
    if !trimmed_summary.is_empty() && trimmed_summary != status_label {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .text_color(palette.text_secondary)
                .child(SharedString::from(trimmed_summary.to_owned())),
        );
    }

    if !props.volumes.is_empty() {
        card = card.child(render_volume_rows(
            props.volumes,
            mono_family.clone(),
            palette,
        ));
    } else if is_unknown && props.errors.is_empty() {
        card = card.child(render_tool_notice_line(
            IconName::TriangleAlert,
            palette.status_warning,
            "操作中断且未采集到目标卷确认状态，请核对磁盘当前锁定与加密情况。",
            palette,
        ));
    }

    if let Some(first_err) = props.errors.first() {
        let (err_icon, err_color) = if is_unknown {
            (IconName::TriangleAlert, palette.status_warning)
        } else {
            (IconName::CircleAlert, palette.status_danger)
        };
        card = card.child(
            render_tool_notice_line(err_icon, err_color, first_err.clone(), palette)
                .id(SharedString::from(format!(
                    "bitlocker-first-error-{}",
                    props.call_id
                )))
                .test_support(),
        );
    }

    let extra_errors = props.errors.get(1..).unwrap_or_default();
    if let Some(details_section) = render_special_details_section(
        SharedString::from(format!("bitlocker-detail-toggle-{}", props.call_id)),
        "BitLocker 详情",
        props.details_expanded,
        None,
        props.fields,
        extra_errors,
        !is_unknown,
        SharedString::from(format!("bitlocker-raw-{}", props.call_id)),
        props.raw,
        None,
        mono_family.clone(),
        palette,
        is_copied_fn,
        on_toggle_details,
        on_mark_copied,
        window,
        cx,
    ) {
        card = card.child(details_section);
    }

    card.into_any_element()
}

/// Frameless flat volume rows (`LbF1q` / `PApm9` / `Wk1TN` — `60px` drive column + right status & 4px progress bar).
fn render_volume_rows(
    volumes: &[SpecialVolumeView],
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut list = div().w_full().flex().flex_col();

    for (idx, vol) in volumes.iter().enumerate() {
        let mut metrics_row = div()
            .min_h(height::INLINE)
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space::MD)
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(vol.conversion.clone())),
            )
            .child(
                apply_scale(div(), scale::CAPTION)
                    .text_color(palette.text_secondary)
                    .child(cjk_inline_label(vol.lock.clone())),
            );

        if !vol.reason.trim().is_empty() {
            metrics_row = metrics_row.child(
                apply_scale(div(), scale::CAPTION)
                    .text_color(palette.text_secondary)
                    .child(cjk_inline_label(vol.reason.clone())),
            );
        }

        if let Some(pct) = vol.percentage {
            metrics_row = metrics_row.child(
                apply_scale(div(), scale::CAPTION)
                    .font_family(mono_family.clone())
                    .text_color(palette.text_secondary)
                    .child(SharedString::from(format!("加密比例 {pct:.0}%"))),
            );
        }

        let progress_pct = vol.percentage.filter(|&pct| {
            matches!(vol.conversion_code, Some(2..=5))
                || (pct > 0.0 && pct < 100.0 && vol.conversion_code != Some(0))
        });

        let mut right_col = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(metrics_row);

        if let Some(pct) = progress_pct {
            let ratio = (pct / 100.0).clamp(0.0, 1.0) as f32;
            right_col = right_col.child(
                div()
                    .w_full()
                    .h(px(4.0))
                    .rounded(radius::FULL)
                    .bg(palette.border_subtle)
                    .overflow_hidden()
                    .child(
                        div()
                            .w(relative(ratio))
                            .h_full()
                            .rounded(radius::FULL)
                            .bg(palette.text_secondary),
                    ),
            );
        }

        let mut row = div()
            .w_full()
            .py(space::SM)
            .flex()
            .items_start()
            .gap(space::LG)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .w(px(60.0))
                    .flex_shrink_0()
                    .font_family(mono_family.clone())
                    .text_color(palette.text_primary)
                    .child(SharedString::from(vol.drive.clone())),
            )
            .child(right_col);

        if idx > 0 {
            row = row.border_t_1().border_color(palette.border_subtle);
        }
        list = list.child(row);
    }

    list
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
