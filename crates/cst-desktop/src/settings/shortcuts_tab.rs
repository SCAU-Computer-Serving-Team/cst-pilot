use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, SharedString, Styled, TestSupportExt, div,
};

use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, radius, scale, space};

pub fn render_shortcuts_tab(
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let composer_shortcuts = [
        ("Enter", "发送当前消息（中文输入法候选态下仅选词）"),
        ("Shift + Enter", "在聊天输入框中换行"),
        ("Tab / 行首 /", "唤出斜杠命令与诊断技能补全面板"),
        ("@", "检索当前会话工作目录文件并附加引用"),
        ("Ctrl + V", "粘贴剪贴板截图或文本"),
        ("Escape", "逐级关闭悬浮面板（候选态下仅取消拼音）"),
    ];

    let nav_shortcuts = [
        ("Ctrl + K", "打开全局快捷命令与历史会话面板"),
        ("Ctrl + N", "新建对话并返回欢迎主页"),
        ("Ctrl + B", "展开或收起左侧会话导航栏"),
        ("Ctrl + ,", "打开设置页面"),
        (
            "Ctrl + O / Shift + Ctrl + O",
            "在分支树视图中正反向切换五档过滤",
        ),
        ("Alt + ↑ / Alt + ↓", "在模型偏好中上下移动已启用模型顺序"),
    ];

    div()
        .id("settings-shortcuts-tab")
        .test_support()
        .w_full()
        .flex()
        .flex_col()
        .gap(space::GROUP)
        .child(shortcut_group_card(
            "聊天输入与补全面板",
            &composer_shortcuts,
            mono_family.clone(),
            palette,
        ))
        .child(shortcut_group_card(
            "会话与分支树导航",
            &nav_shortcuts,
            mono_family,
            palette,
        ))
}

fn shortcut_group_card(
    group_title: &'static str,
    rows: &[(&'static str, &'static str)],
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut table = div()
        .w_full()
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_surface)
        .flex()
        .flex_col();

    for (idx, (keys, desc)) in rows.iter().enumerate() {
        let mut row = div()
            .px(space::LG)
            .py(space::MD)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::LG)
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(*desc)),
            )
            .child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .px(space::SM)
                    .py(space::MICRO)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .bg(palette.bg_elevated)
                    .font_family(mono_family.clone())
                    .text_color(palette.text_secondary)
                    .flex_shrink_0()
                    .child(SharedString::from((*keys).to_owned())),
            );
        if idx > 0 {
            row = row.border_t_1().border_color(palette.border_subtle);
        }
        table = table.child(row);
    }

    div()
        .flex()
        .flex_col()
        .gap(space::SM)
        .child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .text_color(palette.text_secondary)
                .child(group_title),
        )
        .child(table)
}
