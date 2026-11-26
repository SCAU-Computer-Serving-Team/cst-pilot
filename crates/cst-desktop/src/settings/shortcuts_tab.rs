use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, SharedString, Styled, TestSupportExt, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, render_kbd_zcgs3, scale, space};

pub fn render_shortcuts_tab(
    mono_family: SharedString,
    palette: &DesktopPalette,
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
        ("Ctrl + V", "粘贴剪贴板截图或文本"),
        ("Esc", "逐级关闭悬浮面板（候选态下仅取消拼音）"),
    ];

    let pref_shortcuts = [("Alt + ↑ / ↓", "上下移动已启用模型顺序")];

    div()
        .id("settings-shortcuts-tab")
        .test_support()
        .a11y_region("settings-shortcuts-tab", "快捷键列表")
        .w_full()
        .flex()
        .flex_col()
        .gap(px(32.0))
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
            "模型偏好",
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
