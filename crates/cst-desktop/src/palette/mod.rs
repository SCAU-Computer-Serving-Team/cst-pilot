use cst_client::SessionSummary;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, TestSupportExt, Window, div, px,
};

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, layout, radius, scale, space,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteCommandTarget {
    SlashExport,
    SlashTree,
    SlashCompact,
    SlashFork,
    OpenSession(String),
    NewSession,
    OpenSettings,
    OpenWeb,
}

#[derive(Debug, Clone)]
pub enum CommandPaletteEvent {
    Execute(PaletteCommandTarget),
    Dismiss,
}

/// Global `520px` `Ctrl+K` modal command palette (`cst-pilot-desktop.pen` 「01 · 工作台」 `X9p2ZR` -> `I70mz`,
/// `0ms` instant open, built on `gpui_kit::component::command::Command`).
pub struct CommandPaletteModal {
    command_state: Entity<CommandState>,
    prior_focus: Option<FocusHandle>,
    sessions: Vec<SessionSummary>,
    has_active_session: bool,
}

impl EventEmitter<CommandPaletteEvent> for CommandPaletteModal {}

impl CommandPaletteModal {
    pub fn new(
        sessions: Vec<SessionSummary>,
        has_active_session: bool,
        prior_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let command_state = cx.new(|cx| CommandState::new(window, cx));
        command_state.update(cx, |st, cx| {
            st.focus_handle(cx).focus(window, cx);
        });
        Self {
            command_state,
            prior_focus,
            sessions,
            has_active_session,
        }
    }

    pub fn command_state(&self) -> &Entity<CommandState> {
        &self.command_state
    }

    pub fn sync_context(&mut self, sessions: Vec<SessionSummary>, has_active_session: bool) {
        self.sessions = sessions;
        self.has_active_session = has_active_session;
    }

    pub fn prior_focus(&self) -> Option<FocusHandle> {
        self.prior_focus.clone()
    }

    fn build_groups_and_targets(&self) -> (Vec<CommandGroup>, Vec<Vec<PaletteCommandTarget>>) {
        let mut groups = Vec::new();
        let mut section_targets = Vec::new();

        // Group 1: 会话命令 (`/export`, `/tree`, `/compact`, `/fork`)
        let slash_specs: [(&str, &[&str], IconName, PaletteCommandTarget); 4] = [
            (
                "/export · 导出当前对话",
                &["/export", "export", "导出", "png", "html"],
                IconName::Download,
                PaletteCommandTarget::SlashExport,
            ),
            (
                "/tree · 查看会话分支树",
                &["/tree", "tree", "分支树", "分支", "导航"],
                IconName::GitBranch,
                PaletteCommandTarget::SlashTree,
            ),
            (
                "/compact · 压缩当前会话上下文",
                &["/compact", "compact", "压缩", "上下文"],
                IconName::Minimize2,
                PaletteCommandTarget::SlashCompact,
            ),
            (
                "/fork · 从当前对话派生新会话",
                &["/fork", "fork", "派生", "分支会话"],
                IconName::GitFork,
                PaletteCommandTarget::SlashFork,
            ),
        ];

        let mut g1 = CommandGroup::new().label("会话命令");
        let mut t1 = Vec::with_capacity(slash_specs.len());
        for (label, kw, icon, target) in slash_specs {
            g1 = g1.item(
                CommandItem::new()
                    .label(label)
                    .keywords(kw.iter().copied())
                    .icon(Icon::new(icon)),
            );
            t1.push(target);
        }
        groups.push(g1);
        section_targets.push(t1);

        // Group 2: 匹配的历史会话
        if !self.sessions.is_empty() {
            let mut g2 = CommandGroup::new().label("历史会话");
            let mut t2 = Vec::with_capacity(self.sessions.len());
            for sess in &self.sessions {
                let title = if sess.title.trim().is_empty() {
                    "新对话".to_owned()
                } else {
                    sess.title.clone()
                };
                g2 = g2.item(
                    CommandItem::new()
                        .label(SharedString::from(title))
                        .keywords([sess.id.as_str(), "会话", "session"])
                        .icon(Icon::new(IconName::MessageSquare)),
                );
                t2.push(PaletteCommandTarget::OpenSession(sess.id.clone()));
            }
            groups.push(g2);
            section_targets.push(t2);
        }

        // Group 3: 全局跳转 (`新建对话`, `打开设置`, `打开网页端`)
        let nav_specs: [(&str, &[&str], IconName, PaletteCommandTarget); 3] = [
            (
                "新建对话",
                &["new", "新建", "对话", "主页", "home"],
                IconName::Plus,
                PaletteCommandTarget::NewSession,
            ),
            (
                "打开设置",
                &["settings", "设置", "常规", "模型", "更新"],
                IconName::Settings,
                PaletteCommandTarget::OpenSettings,
            ),
            (
                "打开网页端",
                &["web", "网页端", "浏览器", "切换"],
                IconName::Globe,
                PaletteCommandTarget::OpenWeb,
            ),
        ];
        let mut g3 = CommandGroup::new().label("全局跳转");
        let mut t3 = Vec::with_capacity(nav_specs.len());
        for (label, kw, icon, target) in nav_specs {
            g3 = g3.item(
                CommandItem::new()
                    .label(label)
                    .keywords(kw.iter().copied())
                    .icon(Icon::new(icon)),
            );
            t3.push(target);
        }
        groups.push(g3);
        section_targets.push(t3);

        (groups, section_targets)
    }
}

impl Render for CommandPaletteModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let (groups, section_targets) = self.build_groups_and_targets();
        let ent_confirm = cx.entity();
        let ent_cancel = cx.entity();
        let ent_scrim = cx.entity();

        let mut cmd = Command::new(&self.command_state)
            .searchable(true)
            .filterable(true)
            .highlight_matches(true);

        for grp in groups {
            cmd = cmd.group(grp);
        }

        cmd = cmd
            .on_confirm(move |index_path, _, cx| {
                if let Some(target) = section_targets
                    .get(index_path.section)
                    .and_then(|sec| sec.get(index_path.row))
                    .cloned()
                {
                    ent_confirm.update(cx, |_, cx| {
                        cx.emit(CommandPaletteEvent::Execute(target));
                    });
                }
            })
            .on_cancel(move |_, cx| {
                ent_cancel.update(cx, |_, cx| {
                    cx.emit(CommandPaletteEvent::Dismiss);
                });
            });

        render_frosted_surface(
            "command-palette-scrim-x9p2zr",
            FrostedGlassSpec::scrim_dz0fp(&palette),
            palette.bg_scrim,
            transparency_enabled,
            scale_factor,
        )
        .test_support()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(96.0))
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
            ent_scrim.update(cx, |_, cx| {
                cx.emit(CommandPaletteEvent::Dismiss);
            });
        })
        .child(
            render_frosted_surface(
                "command-palette-modal-i70mz",
                FrostedGlassSpec::modal_k3x72(&palette, 8.0),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .w(layout::PALETTE_MODAL_WIDTH)
            .max_h(px(460.0))
            .rounded(radius::SPECIAL)
            .border_1()
            .border_color(palette.border_subtle)
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(div().flex_1().min_h_0().w_full().child(cmd))
            // Bottom `28px` keybinding navigation hint bar
            .child(
                apply_scale(div(), scale::CAPTION)
                    .id("command-palette-footer-hints")
                    .test_support()
                    .w_full()
                    .h(px(28.0))
                    .px(space::MD)
                    .border_t_1()
                    .border_color(palette.border_subtle)
                    .bg(palette.bg_surface)
                    .text_color(palette.text_muted)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(cjk_inline_label("↑↓ 选择条目 · Enter 确认执行"))
                    .child(cjk_inline_label("Esc 收起面板")),
            ),
        )
    }
}
