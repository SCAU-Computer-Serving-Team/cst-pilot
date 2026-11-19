use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use cst_client::tool_presenter::{FieldRow, SpecialStatus, SpecialVolumeView};
use cst_client::{ClientError, ClientHandle};
use gpui::{
    Bounds, ClipboardItem, ElementInputHandler, EntityInputHandler, KeyDownEvent, Pixels, Point,
    Role, TextInputConfiguration, UTF16Selection, canvas,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::input::InputAccessibility;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, div, px,
};
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
};

/// Exact byte capacity for an 8×6 hyphenated BitLocker recovery password
/// (`XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX` = `48 + 7 = 55` bytes).
pub const MAX_RECOVERY_PASSWORD_BYTES: usize = 55;

const HYPHEN_INDICES: [usize; 7] = [6, 13, 20, 27, 34, 41, 48];

fn is_hyphen_slot(idx: usize) -> bool {
    HYPHEN_INDICES.contains(&idx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretInputEvent {
    SubmitRequested,
    CancelRequested,
}

/// Fixed-capacity (55-byte), byte-by-byte zeroizing BitLocker recovery password control.
///
/// Security invariants:
/// 1. Raw bytes live ONLY in `buf: [u8; 55]` — NEVER in `Rope`, `SharedString`, undo history,
///    `ComposerDraft`, `SessionOutbox`, `aria_value`, or logs.
/// 2. Only constant mask glyphs (`•` and `-`) derived from `len` / `is_hyphen` are submitted to GPUI text layout.
/// 3. Backspace, clear, cancel, timeout, submit completion, and `Drop` zeroize affected bytes immediately.
/// 4. Implements `EntityInputHandler` with `accepts_text_input() == false` so Windows IME unbinds while focused
///    and automatically restores Chinese IME when focus returns to `ComposerView`'s `Textarea`.
/// 5. Configures `InputAccessibility::new(Some(Role::PasswordInput)).with_value_exposed(false)`.
pub struct SecretInput {
    buf: [u8; MAX_RECOVERY_PASSWORD_BYTES],
    is_hyphen: [bool; MAX_RECOVERY_PASSWORD_BYTES],
    len: usize,
    focus_handle: FocusHandle,
    accessibility: InputAccessibility,
    disabled: bool,
    error_message: Option<SharedString>,
}

impl EventEmitter<SecretInputEvent> for SecretInput {}

impl Drop for SecretInput {
    fn drop(&mut self) {
        self.zeroize_all();
    }
}

impl Focusable for SecretInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SecretInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let accessibility = InputAccessibility::new(Some(Role::PasswordInput))
            .with_label(Some(SharedString::from("BitLocker 恢复密码")))
            .with_placeholder(Some(SharedString::from(
                "XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX",
            )))
            .with_accessibility_id(Some(SharedString::from("bitlocker-secret-input")))
            .with_value_exposed(false);
        debug_assert!(!accessibility.value_exposed());

        Self {
            buf: [0u8; MAX_RECOVERY_PASSWORD_BYTES],
            is_hyphen: [false; MAX_RECOVERY_PASSWORD_BYTES],
            len: 0,
            focus_handle: cx.focus_handle(),
            accessibility,
            disabled: false,
            error_message: None,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn accessibility(&self) -> &InputAccessibility {
        &self.accessibility
    }

    pub fn error_message(&self) -> Option<&SharedString> {
        self.error_message.as_ref()
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    pub fn set_error_message(
        &mut self,
        msg: Option<impl Into<SharedString>>,
        cx: &mut Context<Self>,
    ) {
        self.error_message = msg.map(Into::into);
        cx.notify();
    }

    /// Immediately overwrites all 55 bytes with `0x00` using `zeroize` volatile writes.
    pub fn zeroize_all(&mut self) {
        self.buf.zeroize();
        self.is_hyphen.zeroize();
        self.len = 0;
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.zeroize_all();
        self.error_message = None;
        cx.notify();
    }

    /// Copies the current buffer into a short-lived `Zeroizing<[u8; 55]>` payload for immediate loopback POST.
    pub fn snapshot_zeroizing_payload(
        &self,
    ) -> (Zeroizing<[u8; MAX_RECOVERY_PASSWORD_BYTES]>, usize) {
        let mut copy = Zeroizing::new([0u8; MAX_RECOVERY_PASSWORD_BYTES]);
        copy[..self.len].copy_from_slice(&self.buf[..self.len]);
        (copy, self.len)
    }

    /// Verifies that every byte past `self.len` inside the internal fixed buffer is `0x00`.
    pub fn is_tail_zeroed(&self) -> bool {
        self.buf[self.len..].iter().all(|&b| b == 0)
    }

    /// Appends a single validated ASCII digit or hyphen with automatic hyphenation at 6-digit boundaries.
    pub fn push_ascii_byte(&mut self, byte: u8, cx: &mut Context<Self>) -> bool {
        if self.disabled || self.len >= MAX_RECOVERY_PASSWORD_BYTES {
            return false;
        }
        if byte.is_ascii_digit() {
            if is_hyphen_slot(self.len) && self.len + 1 < MAX_RECOVERY_PASSWORD_BYTES {
                self.buf[self.len] = b'-';
                self.is_hyphen[self.len] = true;
                self.len += 1;
            }
            if self.len < MAX_RECOVERY_PASSWORD_BYTES {
                self.buf[self.len] = byte;
                self.is_hyphen[self.len] = false;
                self.len += 1;
                self.error_message = None;
                cx.notify();
                return true;
            }
            false
        } else if byte == b'-' {
            if self.len < MAX_RECOVERY_PASSWORD_BYTES
                && self.len > 0
                && !self.is_hyphen[self.len - 1]
            {
                self.buf[self.len] = b'-';
                self.is_hyphen[self.len] = true;
                self.len += 1;
                self.error_message = None;
                cx.notify();
                return true;
            }
            false
        } else {
            self.error_message = Some(SharedString::from("仅允许输入数字与连字符。"));
            cx.notify();
            false
        }
    }

    /// Immediately zeroes the deleted tail byte(s) via `zeroize`.
    pub fn backspace(&mut self, cx: &mut Context<Self>) {
        if self.disabled || self.len == 0 {
            return;
        }
        let last = self.len - 1;
        self.buf[last].zeroize();
        self.is_hyphen[last] = false;
        self.len -= 1;

        // If deleting a digit leaves a trailing auto-hyphen, zeroize that hyphen too so typing stays smooth
        if self.len > 0 && self.is_hyphen[self.len - 1] && is_hyphen_slot(self.len - 1) {
            let h = self.len - 1;
            self.buf[h].zeroize();
            self.is_hyphen[h] = false;
            self.len -= 1;
        }
        self.error_message = None;
        cx.notify();
    }

    /// Validates and pastes recovery password text without exposing raw bytes to `Rope` or `SharedString`.
    pub fn try_paste_str(&mut self, raw_clip: &str, cx: &mut Context<Self>) -> bool {
        if self.disabled {
            return false;
        }
        let trimmed = raw_clip.trim();
        if trimmed.is_empty() {
            return false;
        }
        if !trimmed.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
            self.error_message = Some(SharedString::from(
                "粘贴内容含非法字符，恢复密码仅包含数字与连字符。",
            ));
            cx.notify();
            return false;
        }

        // Case A: 48 pure digits -> auto-format into 55-byte `XXXXXX-...` directly in zeroizing stack array
        if trimmed.len() == 48 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
            self.zeroize_all();
            for b in trimmed.bytes() {
                if is_hyphen_slot(self.len) {
                    self.buf[self.len] = b'-';
                    self.is_hyphen[self.len] = true;
                    self.len += 1;
                }
                self.buf[self.len] = b;
                self.is_hyphen[self.len] = false;
                self.len += 1;
            }
            self.error_message = None;
            cx.notify();
            return true;
        }

        if trimmed.len() > MAX_RECOVERY_PASSWORD_BYTES {
            self.error_message = Some(SharedString::from(
                "粘贴长度超出 55 字符上限，请核对八组六位数字恢复密码。",
            ));
            cx.notify();
            return false;
        }

        self.zeroize_all();
        for (idx, b) in trimmed.bytes().enumerate() {
            self.buf[idx] = b;
            self.is_hyphen[idx] = b == b'-';
            self.len += 1;
        }
        self.error_message = None;
        cx.notify();
        true
    }

    /// Builds a pure mask string (`•` and `-`) solely from `len` and `is_hyphen` — NEVER reading `self.buf`!
    fn masked_display_string(&self) -> SharedString {
        if self.len == 0 {
            return SharedString::default();
        }
        let mut out = String::with_capacity(self.len * 3);
        for idx in 0..self.len {
            if self.is_hyphen[idx] {
                out.push('-');
            } else {
                out.push('•');
            }
        }
        SharedString::from(out)
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        // Strictly forbid Copy & Cut (`Ctrl+C` / `Ctrl+X`)
        if (mods.control || mods.platform) && matches!(key, "c" | "x") {
            cx.stop_propagation();
            return;
        }

        // Validated Paste (`Ctrl+V`)
        if (mods.control || mods.platform) && key == "v" {
            if let Some(clip) = cx.read_from_clipboard()
                && let Some(raw) = clip.text()
            {
                let guarded = Zeroizing::new(raw);
                let _ = self.try_paste_str(guarded.as_str(), cx);
            }
            cx.stop_propagation();
            return;
        }

        if mods.control || mods.alt || mods.platform {
            return;
        }

        match key {
            "backspace" | "delete" => {
                self.backspace(cx);
                cx.stop_propagation();
            }
            "enter" => {
                cx.emit(SecretInputEvent::SubmitRequested);
                cx.stop_propagation();
            }
            "escape" => {
                self.clear(cx);
                cx.emit(SecretInputEvent::CancelRequested);
                cx.stop_propagation();
            }
            _ => {
                let candidate = event.keystroke.key_char.as_deref().unwrap_or(key);
                if candidate.len() == 1
                    && let Some(b) = candidate.bytes().next()
                {
                    let _ = self.push_ascii_byte(b, cx);
                    cx.stop_propagation();
                }
            }
        }
    }
}

impl EntityInputHandler for SecretInput {
    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        // Unbinds OS IME while `SecretInput` owns focus; when focus returns to `Textarea`,
        // `Textarea`'s `EntityInputHandler` (`accepts_text_input() == true`) automatically restores IME!
        false
    }

    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.len..self.len,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn paste(&mut self, item: ClipboardItem, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(raw) = item.text() {
            let guarded = Zeroizing::new(raw);
            let _ = self.try_paste_str(guarded.as_str(), cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let guarded = Zeroizing::new(text.to_owned());
        if guarded.len() == 1 {
            if let Some(b) = guarded.bytes().next() {
                let _ = self.push_ascii_byte(b, cx);
            }
        } else {
            let _ = self.try_paste_str(guarded.as_str(), cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // IME preedit is rejected on secret fields.
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.len)
    }

    fn text_input_configuration(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> TextInputConfiguration {
        TextInputConfiguration {
            autocorrect: false,
            suggestions: false,
            ..TextInputConfiguration::default()
        }
    }
}

impl Render for SecretInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;

        let is_focused = self.focus_handle.is_focused(window);
        let has_error = self.error_message.is_some();
        let masked = self.masked_display_string();

        let border_col = if has_error {
            palette.status_danger
        } else if is_focused {
            palette.accent_solid
        } else {
            palette.border_strong
        };

        let entity_for_paint = cx.entity();
        let fh_for_paint = self.focus_handle.clone();

        // Register `ElementInputHandler` during paint so GPUI forwards `accepts_text_input() == false`
        // to the platform window while focused, unbinding IME cleanly!
        let input_handler_probe = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                window.handle_input(
                    &fh_for_paint,
                    ElementInputHandler::new(bounds, entity_for_paint.clone()),
                    cx,
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let mut field = div()
            .id("bitlocker-secret-input-box")
            .test_support()
            .relative()
            .w_full()
            .h(height::FIELD)
            .px(space::MD)
            .rounded(radius::ROW)
            .border_1()
            .border_color(border_col)
            .bg(palette.bg_workspace)
            .track_focus(&self.focus_handle)
            .tab_index(0)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::SM)
            .on_click(cx.listener(|this, _, window, cx| {
                this.focus_handle.focus(window, cx);
                cx.notify();
            }))
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(input_handler_probe);

        if let Some(role) = self.accessibility.role() {
            field = field.role(role);
        }
        if let Some(lbl) = self.accessibility.label().cloned() {
            field = field.aria_label(lbl);
        }
        if let Some(ph) = self.accessibility.placeholder().cloned() {
            field = field.aria_placeholder(ph);
        }
        if let Some(aid) = self.accessibility.accessibility_id().cloned() {
            field = field.accessibility_id(aid);
        }
        // `self.accessibility.value_exposed()` is strictly `false` -> NEVER call `.aria_value(...)`!
        debug_assert!(!self.accessibility.value_exposed());

        let text_display = if masked.is_empty() {
            apply_scale(div(), scale::CAPTION)
                .font_family(mono_family.clone())
                .text_color(palette.text_muted)
                .truncate()
                .child("输入 48 位数字恢复密码（自动添加连字符）")
        } else {
            apply_scale(div(), scale::UI_MEDIUM)
                .font_family(mono_family.clone())
                .text_color(palette.text_primary)
                .truncate()
                .child(masked)
        };

        let counter = apply_scale(div(), scale::CAPTION)
            .font_family(mono_family)
            .text_color(palette.text_muted)
            .flex_shrink_0()
            .child(SharedString::from(format!(
                "{}/{MAX_RECOVERY_PASSWORD_BYTES}",
                self.len
            )));

        field = field.child(text_display).child(counter);

        let mut wrapper = div().w_full().flex().flex_col().gap(space::XS).child(field);
        if let Some(ref err) = self.error_message {
            wrapper = wrapper.child(
                apply_scale(div(), scale::CAPTION)
                    .id("bitlocker-secret-input-error")
                    .test_support()
                    .text_color(palette.status_danger)
                    .child(err.clone()),
            );
        }
        wrapper
    }
}

#[derive(Debug, Clone)]
pub struct PendingBitLockerPrompt {
    pub request_id: String,
    pub kind: String,
    pub drive: String,
    pub prompt: String,
    pub expires_at_ms: u64,
}

/// Extracts an active pending BitLocker request matching `tool_call_id` (or an untagged pending
/// request when `has_result == false`) from `session.bitlocker` JSON snapshot.
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
    if arr.is_empty() {
        return None;
    }
    let chosen = arr
        .iter()
        .find(|r| r.get("toolCallId").and_then(Value::as_str) == Some(tool_call_id))
        .or_else(|| {
            arr.iter()
                .find(|r| r.get("toolCallId").and_then(Value::as_str).is_none())
        })?;

    let request_id = chosen.get("id").and_then(Value::as_str)?.to_owned();
    let kind = chosen
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("password")
        .to_owned();
    let drive = chosen
        .get("drive")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let prompt = chosen
        .get("prompt")
        .or_else(|| chosen.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let expires_at_ms = chosen.get("expiresAt").and_then(Value::as_u64).unwrap_or(0);

    Some(PendingBitLockerPrompt {
        request_id,
        kind,
        drive,
        prompt,
        expires_at_ms,
    })
}

pub struct BitLockerCardProps<'a> {
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub scope: &'a str,
    pub volumes: &'a [SpecialVolumeView],
    pub fields: &'a [FieldRow],
    pub errors: &'a [String],
    pub pending: Option<PendingBitLockerPrompt>,
    pub secret_input: Option<Entity<SecretInput>>,
    pub details_expanded: bool,
}

/// Renders the standalone BitLocker Controlled Interaction & Result Card (`bitlocker` `unlock / decrypt`,
/// `cst-pilot-desktop.pen` 「07 · 工具与特殊工具」).
/// Covers:
/// 1. `unlock` 180s countdown + `SecretInput` + `400 invalid_bitlocker_input` inline retry
/// 2. AI-supplied parameter local confirmation (`kind == "parameter"`)
/// 3. `decrypt` volume list + risk notice + `"确认解密"` -> per-volume progress -> `FullyDecrypted`
/// 4. Abnormal interruption review (`status == SpecialStatus::Unknown`, never inferring success on empty `volumes`):
///    warning icon + `"结果未确认"` + live volume lock/conversion status + first error + `"详情"` disclosure.
#[allow(clippy::too_many_arguments)]
pub fn render_bitlocker_card<V: 'static>(
    props: BitLockerCardProps<'_>,
    client: ClientHandle,
    mono_family: SharedString,
    palette: &DesktopPalette,
    on_toggle_details: crate::tools::ToggleKeyFn,
    on_error_notice: impl Fn(String, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let now = now_epoch_ms();
    let active_pending = props.pending.filter(|p| {
        if p.expires_at_ms > 0 && now >= p.expires_at_ms {
            if let Some(ref si) = props.secret_input {
                si.update(cx, |s, cx| s.clear(cx));
            }
            false
        } else {
            true
        }
    });

    // Request frame refresh while active 180s countdown is running; freezes at 0 FPS once resolved/expired
    let countdown_secs = active_pending.as_ref().and_then(|p| {
        (p.expires_at_ms > now).then(|| {
            window.request_animation_frame();
            p.expires_at_ms.saturating_sub(now).div_ceil(1000)
        })
    });

    let is_unknown = props.status == SpecialStatus::Unknown
        || (props.status == SpecialStatus::Success
            && props.volumes.is_empty()
            && active_pending.is_none());

    let (status_label, status_color, show_warn_icon) = if active_pending.is_some() {
        ("等待确认", palette.accent_solid, false)
    } else if is_unknown {
        ("结果未确认", palette.status_warning, true)
    } else {
        match props.status {
            SpecialStatus::Pending | SpecialStatus::Running => {
                ("处理中", palette.accent_solid, false)
            }
            SpecialStatus::Success => ("已完成", palette.status_success, false),
            SpecialStatus::Degraded => ("部分完成", palette.status_warning, true),
            SpecialStatus::Cancelled => ("已取消", palette.text_muted, false),
            SpecialStatus::Interrupted => ("已中断", palette.status_warning, true),
            SpecialStatus::Error => ("执行失败", palette.status_danger, true),
            SpecialStatus::Unknown => ("结果未确认", palette.status_warning, true),
        }
    };

    let mut right_status = div().flex().items_center().gap(space::SM);
    if let Some(rem) = countdown_secs {
        right_status = right_status.child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .font_family(mono_family.clone())
                .text_color(if rem <= 30 {
                    palette.status_warning
                } else {
                    palette.text_secondary
                })
                .child(SharedString::from(format!("剩余 {rem} 秒"))),
        );
    }
    if show_warn_icon {
        right_status = right_status.child(
            Icon::new(IconName::TriangleAlert)
                .size(px(14.0))
                .text_color(status_color),
        );
    }
    right_status = right_status.child(
        apply_scale(div(), scale::CAPTION_MEDIUM)
            .id(SharedString::from(format!(
                "bitlocker-status-label-{}",
                props.call_id
            )))
            .test_support()
            .text_color(status_color)
            .child(cjk_inline_label(status_label)),
    );

    let mut card = div()
        .id(SharedString::from(format!(
            "special-bitlocker-card-{}",
            props.call_id
        )))
        .test_support()
        .w_full()
        .p(space::LG)
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .flex()
        .flex_col()
        .gap(space::MD)
        .child(
            div()
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
                            Icon::new(IconName::Lock)
                                .size(px(16.0))
                                .text_color(palette.accent_solid),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(if props.title.trim().is_empty() {
                                    "BitLocker 卷操作"
                                } else {
                                    props.title
                                })),
                        ),
                )
                .child(right_status),
        );

    // 1. Active Pending Interaction (`password` / `parameter` / `decrypt`)
    if let Some(pending) = active_pending {
        if !pending.prompt.trim().is_empty() {
            card = card.child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child(SharedString::from(pending.prompt.clone())),
            );
        }

        if pending.kind == "password"
            && let Some(secret_entity) = props.secret_input
        {
            let _ = window;
            let sid_submit = props.session_id.to_owned();
            let rid_submit = pending.request_id.clone();
            let client_submit = client.clone();
            let si_submit = secret_entity.clone();
            let err_cb = on_error_notice.clone();

            let sid_cancel = props.session_id.to_owned();
            let rid_cancel = pending.request_id.clone();
            let client_cancel = client;
            let si_cancel = secret_entity.clone();
            let err_cancel_cb = on_error_notice;

            card = card.child(secret_entity).child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id(SharedString::from(format!(
                                "bitlocker-cancel-btn-{}",
                                props.call_id
                            )))
                            .test_support()
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
                                si_cancel.update(cx, |s, cx| s.clear(cx));
                                let c = client_cancel.clone();
                                let s_id = sid_cancel.clone();
                                let r_id = rid_cancel.clone();
                                let e_cb = err_cancel_cb.clone();
                                cx.spawn(async move |weak, cx| {
                                    if let Err(err) = c.cancel_bitlocker_request(s_id, r_id).await {
                                        let _ = weak.update(cx, |view, cx| {
                                            let _ = view;
                                            e_cb(err.to_string(), cx);
                                        });
                                    }
                                })
                                .detach();
                            }))
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id(SharedString::from(format!(
                                "bitlocker-unlock-submit-btn-{}",
                                props.call_id
                            )))
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let (payload_buf, payload_len) =
                                    si_submit.read(cx).snapshot_zeroizing_payload();
                                if payload_len == 0 {
                                    si_submit.update(cx, |s, cx| {
                                        s.set_error_message(
                                            Some("请输入八组六位数字恢复密码。"),
                                            cx,
                                        );
                                    });
                                    return;
                                }
                                let c = client_submit.clone();
                                let s_id = sid_submit.clone();
                                let r_id = rid_submit.clone();
                                let si_done = si_submit.clone();
                                let e_cb = err_cb.clone();
                                cx.spawn(async move |weak, cx| {
                                    let res = c
                                        .respond_bitlocker_password(
                                            s_id,
                                            r_id,
                                            payload_buf,
                                            payload_len,
                                        )
                                        .await;
                                    let _ = weak.update(cx, |view, cx| {
                                        let _ = view;
                                        match res {
                                            Ok(_) => {
                                                // Accepted: zeroize immediately!
                                                si_done.update(cx, |s, cx| s.clear(cx));
                                            }
                                            Err(ClientError::BadRequest {
                                                ref code,
                                                ref message,
                                            }) if code == "invalid_bitlocker_input" => {
                                                // Keep `requestId` alive for in-place edit & retry!
                                                let msg = message.clone();
                                                si_done.update(cx, |s, cx| {
                                                    s.set_error_message(Some(msg), cx);
                                                });
                                            }
                                            Err(err) => {
                                                si_done.update(cx, |s, cx| s.clear(cx));
                                                e_cb(err.to_string(), cx);
                                            }
                                        }
                                    });
                                })
                                .detach();
                            }))
                            .child(cjk_inline_label("解锁")),
                    ),
            );
        } else {
            // `parameter` (AI-prefilled local confirmation) or `decrypt` (full-disk decryption confirmation)
            let is_decrypt = pending.kind == "decrypt" || props.scope == "decrypt";
            if is_decrypt {
                card = card.child(
                    apply_scale(div(), scale::CAPTION)
                        .p(space::SM)
                        .rounded(radius::ROW)
                        .bg(palette.bg_surface)
                        .text_color(palette.status_warning)
                        .child("关闭 BitLocker 将对目标卷执行全盘解密，耗时较长期间请勿断电或拔出磁盘。"),
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
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id(SharedString::from(format!(
                                "bitlocker-cancel-btn-{}",
                                props.call_id
                            )))
                            .test_support()
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
                                    if let Err(err) = c.cancel_bitlocker_request(s_id, r_id).await {
                                        let _ = weak.update(cx, |_, cx| e_cb(err.to_string(), cx));
                                    }
                                })
                                .detach();
                            }))
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id(SharedString::from(format!(
                                "bitlocker-confirm-btn-{}",
                                props.call_id
                            )))
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(if is_decrypt {
                                palette.status_danger
                            } else {
                                palette.accent_solid
                            })
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let c = client_confirm.clone();
                                let s_id = sid_confirm.clone();
                                let r_id = rid_confirm.clone();
                                let e_cb = err_confirm_cb.clone();
                                cx.spawn(async move |weak, cx| {
                                    if let Err(err) =
                                        c.respond_bitlocker_confirm(s_id, r_id, true).await
                                    {
                                        let _ = weak.update(cx, |_, cx| e_cb(err.to_string(), cx));
                                    }
                                })
                                .detach();
                            }))
                            .child(cjk_inline_label(if is_decrypt {
                                "确认解密"
                            } else {
                                "确认执行"
                            })),
                    ),
            );
        }

        return card.into_any_element();
    }

    // 2. Settled or Unknown-Review Result Template
    if !props.summary.trim().is_empty() {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .text_color(palette.text_secondary)
                .child(SharedString::from(props.summary.to_owned())),
        );
    }

    if !props.volumes.is_empty() {
        card = card.child(render_volume_rows(
            props.volumes,
            mono_family.clone(),
            palette,
        ));
    } else if is_unknown {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .p(space::SM)
                .rounded(radius::ROW)
                .bg(palette.bg_surface)
                .text_color(palette.status_warning)
                .child("操作中断且未采集到目标卷确认状态，请核对磁盘当前锁定与加密情况。"),
        );
    }

    if let Some(first_err) = props.errors.first() {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .id(SharedString::from(format!(
                    "bitlocker-first-error-{}",
                    props.call_id
                )))
                .test_support()
                .text_color(if is_unknown {
                    palette.status_warning
                } else {
                    palette.status_danger
                })
                .child(SharedString::from(first_err.clone())),
        );
    }

    let has_details = !props.fields.is_empty() || props.errors.len() > 1;
    if has_details {
        let dk = format!("bitlocker-details-{}", props.call_id);
        let dk_click = dk.clone();
        card = card.child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(SharedString::from(format!(
                    "bitlocker-detail-toggle-{}",
                    props.call_id
                )))
                .test_support()
                .h(height::INLINE)
                .flex()
                .items_center()
                .gap(space::XS)
                .text_color(palette.text_secondary)
                .on_click(move |_, w, cx| (on_toggle_details)(dk_click.clone(), w, cx))
                .child(cjk_inline_label("详情"))
                .child(
                    Icon::new(if props.details_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(11.0))
                    .text_color(palette.text_muted),
                ),
        );

        if props.details_expanded {
            let mut detail_col = div()
                .pt(space::SM)
                .border_t_1()
                .border_color(palette.border_subtle)
                .flex()
                .flex_col()
                .gap(space::XS);

            for row in props.fields {
                detail_col = detail_col.child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap(space::MD)
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(row.label.clone())),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family.clone())
                                .text_color(palette.text_primary)
                                .child(SharedString::from(row.value.clone())),
                        ),
                );
            }

            for extra_err in props.errors.iter().skip(1) {
                detail_col = detail_col.child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.status_danger)
                        .child(SharedString::from(extra_err.clone())),
                );
            }
            card = card.child(detail_col);
        }
    }

    card.into_any_element()
}

fn render_volume_rows(
    volumes: &[SpecialVolumeView],
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut list = div()
        .w_full()
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_surface)
        .flex()
        .flex_col();

    for (idx, vol) in volumes.iter().enumerate() {
        let pct_str = vol
            .percentage
            .map(|p| format!("{p:.1}%"))
            .unwrap_or_default();
        let right_text = if pct_str.is_empty() {
            format!("{} · {}", vol.lock, vol.conversion)
        } else {
            format!("{} · {} · {pct_str}", vol.lock, vol.conversion)
        };

        let mut row = div()
            .px(space::MD)
            .py(space::SM)
            .flex()
            .flex_col()
            .gap(space::MICRO)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .child(SharedString::from(vol.drive.clone())),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_secondary)
                            .child(SharedString::from(right_text)),
                    ),
            );

        if !vol.reason.trim().is_empty() {
            row = row.child(
                apply_scale(div(), scale::CAPTION)
                    .text_color(palette.text_muted)
                    .child(SharedString::from(vol.reason.clone())),
            );
        }

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
