pub mod all_sessions;
pub mod branch_tree;
pub mod export_view;

use gpui::EntityInputHandler;
use gpui_kit::component::input::InputState;
use gpui_kit::{App, Entity, Focusable, Window};

pub(crate) fn input_is_composing(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    input.read(cx).focus_handle(cx).is_focused(window)
        && input.update(cx, |state, cx| {
            EntityInputHandler::marked_text_range(state, window, cx).is_some()
        })
}

pub(crate) fn consume_composing_escape(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    // Capture before Input's Escape action unmarks the text and propagates to the shell.
    if input_is_composing(input, window, cx) {
        input.update(cx, |state, cx| {
            EntityInputHandler::unmark_text(state, window, cx)
        });
        cx.stop_propagation();
    }
}
