use std::rc::Rc;

use gpui::{FocusHandle, KeyDownEvent};
use gpui_kit::{
    App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, TestSupportExt,
    Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::kbd_focus::{kbd_bracket_color, kbd_rail_color};
use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, render_kbd_zcgs3, scale, space};

pub type ShortcutsKeyDownFn = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;

pub fn render_shortcuts_tab(
    mono_family: SharedString,
    palette: &DesktopPalette,
    focus_handle: &FocusHandle,
    kbd_focused: bool,
    kbd_style: cst_client::KeyboardFocusStyleDto,
    on_key_down: ShortcutsKeyDownFn,
) -> impl IntoElement {
    let global_shortcuts = [
        ("Ctrl + P", "打开命令面板与常见导航操作"),
        ("Ctrl + O", "快速打开指定会话与近期导出"),
        ("Ctrl + N", "新建对话并返回欢迎主页"),
        ("Ctrl + B", "展开或收起左侧会话导航栏"),
        ("Ctrl + ,", "打开设置页面"),
        ("Ctrl + J", "聚焦当前页面或弹窗的输入框"),
    ];

    let chat_shortcuts = [
        ("Enter", "发送当前消息（中文输入法候选态下仅选词）"),
        ("Shift + Enter", "在聊天输入框中换行"),
        ("Alt + M", "打开模型与思考强度选择器"),
        ("行首 /", "唤出斜杠命令与诊断技能补全面板"),
        ("@", "检索当前会话工作目录文件并附加引用"),
        ("Tab", "补全打开时接受候选项；补全关闭时移出输入焦点"),
        ("↑ ↓ ← →", "方向键浏览列表与按钮；输入框首末行跳至起止"),
        ("Ctrl + V", "粘贴剪贴板截图或文本"),
        ("Esc", "逐级关闭浮层与补全、停止当前生成或退出输入态"),
    ];

    let pref_shortcuts = [("Alt + ↑ / ↓", "上下移动已启用模型或排队消息顺序")];

    let focus_bar_color = match kbd_style {
        cst_client::KeyboardFocusStyleDto::Brackets => kbd_bracket_color(palette),
        cst_client::KeyboardFocusStyleDto::Rail => kbd_rail_color(palette),
    };

    div()
        .id("settings-shortcuts-tab")
        .test_support()
        .a11y_region("settings-shortcuts-tab", "快捷键列表")
        .relative()
        .track_focus(focus_handle)
        .on_key_down(move |ev, w, cx| (on_key_down)(ev, w, cx))
        .w_full()
        .flex()
        .flex_col()
        .gap(px(32.0))
        .children(kbd_focused.then(|| {
            div()
                .absolute()
                .top(px(2.0))
                .left(px(-7.0))
                .w(px(3.0))
                .h(px(20.0))
                .rounded(px(1.5))
                .bg(focus_bar_color)
        }))
        .child(shortcut_group_list(
            "全局与窗口",
            &global_shortcuts,
            mono_family.clone(),
            palette,
        ))
        .child(shortcut_group_list(
            "对话与输入",
            &chat_shortcuts,
            mono_family.clone(),
            palette,
        ))
        .child(shortcut_group_list(
            "列表与排序",
            &pref_shortcuts,
            mono_family,
            palette,
        ))
}

fn shortcut_group_list(
    group_title: &'static str,
    rows: &[(&'static str, &'static str)],
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut table = div().w_full().flex().flex_col();

    for (keys, desc) in rows {
        let row = div()
            .w_full()
            .h(px(48.0))
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::LG)
            .child(
                apply_scale(div(), scale::BODY)
                    .min_w_0()
                    .truncate()
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(*desc)),
            )
            .child(render_kbd_zcgs3(*keys, mono_family.clone(), palette));
        table = table.child(row);
    }

    div()
        .flex()
        .flex_col()
        .gap(space::SM)
        .child(
            apply_scale(div(), scale::BODY_SEMIBOLD)
                .text_color(palette.text_primary)
                .child(cjk_inline_label(group_title)),
        )
        .child(table)
}
