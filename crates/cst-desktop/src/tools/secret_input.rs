use std::ops::Range;

use gpui::{
    Bounds, ClipboardItem, ElementInputHandler, EntityInputHandler, KeyDownEvent, Pixels, Point,
    Role, TextInputConfiguration, UTF16Selection, canvas,
};
use gpui_kit::base::input::InputAccessibility;
use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, div,
};
use zeroize::{Zeroize, Zeroizing};

use crate::theme::{DesktopThemeGlobal, apply_scale, height, radius, scale, space};

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
        Self::with_accessibility_id("bitlocker-secret-input", cx)
    }

    pub fn with_accessibility_id(id: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        let accessibility = InputAccessibility::new(Some(Role::PasswordInput))
            .with_label(Some(SharedString::from("BitLocker 恢复密码")))
            .with_placeholder(Some(SharedString::from(
                "XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX",
            )))
            .with_accessibility_id(Some(id.into()))
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

    pub(super) fn is_disabled(&self) -> bool {
        self.disabled
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
        if trimmed.bytes().all(|b| b.is_ascii_digit()) {
            for b in trimmed.bytes() {
                if !self.push_ascii_byte(b, cx) {
                    break;
                }
            }
            self.error_message = None;
            cx.notify();
            return true;
        }
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
        if self.disabled {
            return;
        }
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
        if self.disabled {
            return;
        }
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
            palette.border_focus
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
            .rounded(radius::CONTROL)
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

        let mut wrapper = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .text_color(palette.text_secondary)
                    .child("恢复密码（48 位）"),
            )
            .child(field);
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
