use gpui::EntityInputHandler;
use gpui_kit::component::input::{BufferPoint, InputState, TextareaState};
use gpui_kit::{App, Entity, Focusable as _, Window, px};

fn clamp_char_boundary(text: &str, offset: usize) -> usize {
    let mut clamped = offset.min(text.len());
    while clamped > 0 && !text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

fn byte_offset_to_buffer_point(text: &str, byte_offset: usize) -> BufferPoint {
    let clamped = clamp_char_boundary(text, byte_offset);
    let prefix = &text[..clamped];
    let line = prefix.bytes().filter(|&b| b == b'\n').count();
    let col = clamped - prefix.rfind('\n').map_or(0, |idx| idx + 1);
    BufferPoint::new(line, col)
}

fn is_on_vertical_boundary(st: &TextareaState, text: &str, offset: usize, down: bool) -> bool {
    let clamped = clamp_char_boundary(text, offset);
    let display_rows = st.display_map().display_row_count();
    let has_layout = st.cursor_layout().is_some() || st.text_bounds().is_some();
    if !has_layout || display_rows == 0 {
        return if down {
            !text[clamped..].contains('\n')
        } else {
            !text[..clamped].contains('\n')
        };
    }

    let disp = st
        .display_map()
        .buffer_pos_to_display_pos(byte_offset_to_buffer_point(text, clamped));
    let last_row = display_rows.saturating_sub(1);

    if down {
        if disp.row < last_row {
            return false;
        }
        // On a soft-wrap split where caret sits at end of `last_row - 1` (line_end_affinity),
        // let `TextareaState::down` move down onto `last_row` instead of jumping straight to end.
        if disp.row == last_row
            && last_row > 0
            && disp.col == 0
            && !text[..clamped].ends_with('\n')
            && let (Some((caret, line_h)), Some(text_bounds)) =
                (st.cursor_layout(), st.text_bounds())
        {
            let last_row_top =
                text_bounds.origin.y + st.scroll_offset().y + line_h * (last_row as f32);
            if caret.origin.y + line_h * 0.5 < last_row_top {
                return false;
            }
        }
        true
    } else {
        if disp.row == 0 {
            return true;
        }
        // On a soft-wrap split where caret visually sits at end of row 0, treat it as row 0.
        if disp.row == 1
            && disp.col == 0
            && !text[..clamped].ends_with('\n')
            && let (Some((caret, line_h)), Some(text_bounds)) =
                (st.cursor_layout(), st.text_bounds())
        {
            let row0_top = text_bounds.origin.y + st.scroll_offset().y;
            if caret.origin.y < row0_top + line_h * 0.5 && st.scroll_offset().y >= px(-0.5) {
                return true;
            }
        }
        false
    }
}

/// Ensures `MoveUp` / `MoveDown` on a `TextareaState` jumps synchronously to the start (`0`) when
/// already on the first visual line, or to the end (`len`) when already on the last visual line.
///
/// Must be called from a `capture_action` handler before `TextareaState` runs its own vertical
/// movement. Returns `true` when handled synchronously so the caller calls `cx.stop_propagation()`.
pub(crate) fn handle_textarea_vertical_boundary(
    textarea: &Entity<TextareaState>,
    down: bool,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    if !textarea.read(cx).focus_handle(cx).is_focused(window) {
        return false;
    }
    let composing = textarea.update(cx, |st, cx| {
        EntityInputHandler::marked_text_range(st, window, cx).is_some()
    });
    if composing {
        return false;
    }

    let should_jump = {
        let st = textarea.read(cx);
        let val = st.value();
        let sel = st.selected_range();
        let edge = if sel.is_empty() {
            st.cursor()
        } else if down {
            sel.end
        } else {
            sel.start
        };
        is_on_vertical_boundary(st, &val, edge, down)
    };

    if !should_jump {
        return false;
    }

    textarea.update(cx, |st, cx| {
        let target = if down { st.value().len() } else { 0 };
        st.set_selected_range(target..target, cx);
    });
    true
}

/// Ensures `MoveUp` / `MoveDown` on a single-line `InputState` jumps directly to start (`0`)
/// or end (`len`) when focused and not in IME composition.
pub(crate) fn handle_input_vertical_boundary(
    input: &Entity<InputState>,
    down: bool,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    if !input.read(cx).focus_handle(cx).is_focused(window) {
        return false;
    }
    let composing = input.update(cx, |st, cx| {
        EntityInputHandler::marked_text_range(st, window, cx).is_some()
    });
    if composing {
        return false;
    }
    input.update(cx, |st, cx| {
        let target = if down { st.value().len() } else { 0 };
        st.set_selected_range(target..target, cx);
    });
    true
}
