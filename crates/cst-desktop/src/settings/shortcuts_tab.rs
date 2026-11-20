use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, SharedString, Styled, TestSupportExt, div, px,
};

use crate::theme::{
    A11yElementExt as _, DesktopPalette, apply_scale, cjk_inline_label, radius, render_kbd_zcgs3,
    scale, space,
};

pub fn render_shortcuts_tab(
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let global_shortcuts = [
        ("Ctrl + K", "打开全局快捷命令与历史会话面板"),
        ("Ctrl + N", "新建对话并返回欢迎主页"),
        ("Ctrl + B", "展开或收起左侧会话导航栏"),
        ("Ctrl + ,", "打开设置页面"),
    ];

    let chat_shortcuts = [
        ("Enter", "发送当前消息（中文输入法候选态下仅选词）"),
        ("Shift + Enter", "在聊天输入框中换行"),
        ("Tab / 行首 /", "唤出斜杠命令与诊断技能补全面板"),
        ("@", "检索当前会话工作目录文件并附加引用"),
        ("Ctrl + V", "粘贴剪贴板截图或文本"),
        ("Esc", "逐级关闭悬浮面板（候选态下仅取消拼音）"),
    ];

    let tree_pref_shortcuts = [
        ("Ctrl + O", "在分支树视图中正向切换五档过滤"),
        ("Ctrl + Shift + O", "在分支树视图中反向切换五档过滤"),
        ("Alt + ↑ / ↓", "在模型偏好中上下移动已启用模型顺序"),
    ];

    div()
        .id("settings-shortcuts-tab")
        .test_support()
        .a11y_region("settings-shortcuts-tab", "快捷键列表")
        .w_full()
        .flex()
        .flex_col()
        .gap(space::GROUP)
        .child(
            apply_scale(div(), scale::TITLE)
                .text_color(palette.text_primary)
                .child(cjk_inline_label("快捷键")),
        )
        .child(shortcut_group_card(
            "全局与窗口",
            &global_shortcuts,
            mono_family.clone(),
            palette,
        ))
        .child(shortcut_group_card(
            "对话与输入",
            &chat_shortcuts,
            mono_family.clone(),
            palette,
        ))
        .child(shortcut_group_card(
            "分支树与模型偏好",
            &tree_pref_shortcuts,
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
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_workspace)
        .flex()
        .flex_col();

    for (idx, (keys, desc)) in rows.iter().enumerate() {
        let mut row = div()
            .w_full()
            .h(px(48.0))
            .px(space::LG)
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
            apply_scale(div(), scale::BODY_SEMIBOLD)
                .text_color(palette.text_primary)
                .child(cjk_inline_label(group_title)),
        )
        .child(table)
}
