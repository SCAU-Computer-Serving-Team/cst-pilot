use cst_client::{RecentExportSummary, SessionSummary};
use gpui::{BoxShadow, KeyDownEvent, ScrollHandle, point};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription,
    TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::sessions::input_is_composing;
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius,
    scale, space,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// `Ctrl+P`: current-session commands + common navigation.
    Commands,
    /// `Ctrl+O`: target sessions + recent completed exports.
    QuickOpen,
}

impl PaletteMode {
    fn placeholder(self) -> &'static str {
        match self {
            Self::Commands => "搜索命令或导航操作…",
            Self::QuickOpen => "搜索历史会话或近期导出…",
        }
    }

    fn dialog_label(self) -> &'static str {
        match self {
            Self::Commands => "命令面板",
            Self::QuickOpen => "快速打开面板",
        }
    }

    fn empty_label(self) -> &'static str {
        match self {
            Self::Commands => "未找到匹配的命令或导航操作",
            Self::QuickOpen => "未找到匹配的会话或近期导出",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteCommandTarget {
    SlashExport,
    SlashTree,
    SlashCompact,
    SlashFork,
    OpenSession(String),
    OpenExport(String),
    NewSession,
    OpenAllSessions,
    ToggleSidebar,
    OpenSettings,
    OpenWeb,
}

#[derive(Debug, Clone)]
pub enum CommandPaletteEvent {
    Execute(PaletteCommandTarget),
    Dismiss,
}

#[derive(Debug, Clone)]
struct PaletteEntry {
    group: &'static str,
    title: SharedString,
    title_is_cjk: bool,
    command_col_width: Option<f32>,
    subtitle: Option<SharedString>,
    subtitle_is_cjk: bool,
    trailing_kbd: Option<&'static str>,
    trailing_meta: Option<SharedString>,
    icon: IconName,
    disabled: bool,
    target: PaletteCommandTarget,
    search_haystack: String,
}

/// Global `540px` dual quick palette (`cst-pilot-desktop.pen` 「01 · 工作台」 `OmK5U` ->
/// `X9p2ZR` / `I70mz` (`Ctrl+P`) & `MMTeT` / `Xyg0l` (`Ctrl+O`), `0ms` instant open).
/// Anchored top-center (`96px`), styled to match `RoKYE` 95% frosted glass with `Source Han Sans CN`.
pub struct CommandPaletteModal {
    mode: PaletteMode,
    query_input: Entity<InputState>,
    prior_focus: Option<FocusHandle>,
    trap_focus: FocusHandle,
    scroll_handle: ScrollHandle,
    sessions: Vec<SessionSummary>,
    active_session_id: Option<String>,
    has_active_session: bool,
    recent_exports: Vec<RecentExportSummary>,
    selected_idx: usize,
    _query_sub: Subscription,
}

impl EventEmitter<CommandPaletteEvent> for CommandPaletteModal {}

impl CommandPaletteModal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: PaletteMode,
        sessions: Vec<SessionSummary>,
        active_session_id: Option<String>,
        has_active_session: bool,
        recent_exports: Vec<RecentExportSummary>,
        prior_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder(mode.placeholder()));
        query_input.update(cx, |inp, cx| inp.focus(window, cx));
        let query_sub = cx.subscribe(&query_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected_idx = 0;
                cx.notify();
            }
        });
        Self {
            mode,
            query_input,
            prior_focus,
            trap_focus: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            sessions,
            active_session_id,
            has_active_session,
            recent_exports,
            selected_idx: 0,
            _query_sub: query_sub,
        }
    }

    pub fn mode(&self) -> PaletteMode {
        self.mode
    }

    pub fn switch_mode(&mut self, mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.selected_idx = 0;
        self.query_input.update(cx, |inp, cx| {
            inp.set_placeholder(mode.placeholder(), window, cx);
            inp.set_value("", window, cx);
            inp.focus(window, cx);
        });
        cx.notify();
    }

    pub fn query_input(&self) -> &Entity<InputState> {
        &self.query_input
    }

    pub fn sync_context(
        &mut self,
        sessions: Vec<SessionSummary>,
        active_session_id: Option<String>,
        has_active_session: bool,
    ) {
        self.sessions = sessions;
        self.active_session_id = active_session_id;
        self.has_active_session = has_active_session;
    }

    pub fn set_recent_exports(
        &mut self,
        exports: Vec<RecentExportSummary>,
        cx: &mut Context<Self>,
    ) {
        self.recent_exports = exports;
        cx.notify();
    }

    pub fn prior_focus(&self) -> Option<FocusHandle> {
        self.prior_focus.clone()
    }

    fn build_entries(&self, query: &str) -> Vec<PaletteEntry> {
        let q = query.trim().to_lowercase();
        let all = match self.mode {
            PaletteMode::Commands => self.build_command_entries(),
            PaletteMode::QuickOpen => self.build_quick_open_entries(),
        };

        if q.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|item| item.search_haystack.contains(&q))
                .collect()
        }
    }

    fn build_command_entries(&self) -> Vec<PaletteEntry> {
        let slash_disabled = !self.has_active_session;
        let mut all = Vec::with_capacity(9);

        struct CommandSpec<'a> {
            cmd: &'a str,
            desc: &'a str,
            kws: &'a [&'a str],
            icon: IconName,
            target: PaletteCommandTarget,
        }

        let cmd_specs = [
            CommandSpec {
                cmd: "export",
                desc: "导出当前诊断报告",
                kws: &["/export", "export", "导出", "png", "html"],
                icon: IconName::Download,
                target: PaletteCommandTarget::SlashExport,
            },
            CommandSpec {
                cmd: "tree",
                desc: "查看会话分支树与节点总结",
                kws: &["/tree", "tree", "分支树", "分支", "导航"],
                icon: IconName::GitBranch,
                target: PaletteCommandTarget::SlashTree,
            },
            CommandSpec {
                cmd: "compact",
                desc: "压缩早期上下文并保留关键结论",
                kws: &["/compact", "compact", "压缩", "上下文"],
                icon: IconName::Minimize2,
                target: PaletteCommandTarget::SlashCompact,
            },
            CommandSpec {
                cmd: "fork",
                desc: "从当前对话派生新会话",
                kws: &["/fork", "fork", "派生", "分支会话"],
                icon: IconName::GitFork,
                target: PaletteCommandTarget::SlashFork,
            },
        ];

        for spec in cmd_specs {
            let hay = format!("{} {} {}", spec.cmd, spec.desc, spec.kws.join(" ")).to_lowercase();
            all.push(PaletteEntry {
                group: "当前会话命令",
                title: SharedString::from(spec.cmd),
                title_is_cjk: false,
                command_col_width: Some(72.0),
                subtitle: Some(SharedString::from(spec.desc)),
                subtitle_is_cjk: true,
                trailing_kbd: None,
                trailing_meta: None,
                icon: spec.icon,
                disabled: slash_disabled,
                target: spec.target,
                search_haystack: hay,
            });
        }

        struct NavSpec<'a> {
            title: &'a str,
            kbd: Option<&'a str>,
            kws: &'a [&'a str],
            icon: IconName,
            target: PaletteCommandTarget,
        }

        let nav_specs = [
            NavSpec {
                title: "新建对话",
                kbd: Some("Ctrl+N"),
                kws: &["new", "新建", "对话", "主页", "home", "ctrl+n"],
                icon: IconName::SquarePen,
                target: PaletteCommandTarget::NewSession,
            },
            NavSpec {
                title: "打开全部会话",
                kbd: None,
                kws: &["sessions", "all", "全部会话", "会话列表", "历史"],
                icon: IconName::RotateCcwClock,
                target: PaletteCommandTarget::OpenAllSessions,
            },
            NavSpec {
                title: "切换侧边栏显示",
                kbd: Some("Ctrl+B"),
                kws: &["sidebar", "侧边栏", "导航栏", "折叠", "展开", "ctrl+b"],
                icon: IconName::PanelLeft,
                target: PaletteCommandTarget::ToggleSidebar,
            },
            NavSpec {
                title: "打开设置",
                kbd: Some("Ctrl+,"),
                kws: &[
                    "settings",
                    "设置",
                    "常规",
                    "模型",
                    "更新",
                    "提供商",
                    "密钥",
                    "ctrl+,",
                ],
                icon: IconName::Settings,
                target: PaletteCommandTarget::OpenSettings,
            },
            NavSpec {
                title: "打开网页端",
                kbd: None,
                kws: &["web", "网页端", "浏览器", "切换"],
                icon: IconName::Globe,
                target: PaletteCommandTarget::OpenWeb,
            },
        ];

        for spec in nav_specs {
            let kbd_str = spec.kbd.unwrap_or("");
            let hay = format!("{} {kbd_str} {}", spec.title, spec.kws.join(" ")).to_lowercase();
            all.push(PaletteEntry {
                group: "常见导航",
                title: SharedString::from(spec.title),
                title_is_cjk: true,
                command_col_width: None,
                subtitle: None,
                subtitle_is_cjk: false,
                trailing_kbd: spec.kbd,
                trailing_meta: None,
                icon: spec.icon,
                disabled: false,
                target: spec.target,
                search_haystack: hay,
            });
        }

        all
    }

    fn build_quick_open_entries(&self) -> Vec<PaletteEntry> {
        let mut sessions = self.sessions.clone();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let mut exports: Vec<&RecentExportSummary> = self
            .recent_exports
            .iter()
            .filter(|e| e.status == "completed")
            .collect();
        exports.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));

        let mut all = Vec::with_capacity(sessions.len() + exports.len());

        for sess in sessions {
            let title = if sess.title.trim().is_empty() {
                "新对话".to_owned()
            } else {
                sess.title.clone()
            };
            let is_current = self.active_session_id.as_deref() == Some(sess.id.as_str());
            let date_meta = format_palette_time(sess.updated_at.as_deref());
            let hay = format!("{title} {} 会话 session", sess.id).to_lowercase();
            all.push(PaletteEntry {
                group: "指定会话",
                title: SharedString::from(title),
                title_is_cjk: true,
                command_col_width: None,
                subtitle: is_current.then(|| SharedString::from("当前会话")),
                subtitle_is_cjk: true,
                trailing_kbd: None,
                trailing_meta: date_meta.map(SharedString::from),
                icon: IconName::MessageSquare,
                disabled: false,
                target: PaletteCommandTarget::OpenSession(sess.id),
                search_haystack: hay,
            });
        }

        for exp in exports {
            let is_png = exp.format().eq_ignore_ascii_case("png");
            let title = if !exp.title.trim().is_empty() {
                exp.title.clone()
            } else if let Some(first) = exp.files.first() {
                first.name.clone()
            } else {
                "会话导出".to_owned()
            };
            let (subtitle, subtitle_is_cjk, icon) = if is_png {
                let pages = exp.page_count().max(1);
                (format!("PNG · {pages} 页"), true, IconName::Image)
            } else {
                let size_str = format_export_bytes(exp.total_bytes());
                (format!("HTML · {size_str}"), false, IconName::FileCode)
            };
            let file_names = exp
                .files
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let fmt_kw = if is_png { "png 图片" } else { "html 网页" };
            let hay = format!("{title} {file_names} {fmt_kw} 导出 export").to_lowercase();
            let time_meta = format_palette_time(Some(exp.captured_at.as_str()));

            all.push(PaletteEntry {
                group: "近期导出",
                title: SharedString::from(title),
                title_is_cjk: true,
                command_col_width: None,
                subtitle: Some(SharedString::from(subtitle)),
                subtitle_is_cjk,
                trailing_kbd: None,
                trailing_meta: time_meta.map(SharedString::from),
                icon,
                disabled: false,
                target: PaletteCommandTarget::OpenExport(exp.id.clone()),
                search_haystack: hay,
            });
        }

        all
    }

    fn first_enabled_index(entries: &[PaletteEntry]) -> Option<usize> {
        entries.iter().position(|e| !e.disabled)
    }

    fn scroll_to_entry(&self, entries: &[PaletteEntry], index: usize) {
        // ScrollHandle addresses actual children, including each group heading and separator.
        let transitions = entries[..=index]
            .windows(2)
            .filter(|pair| pair[0].group != pair[1].group)
            .count();
        self.scroll_handle
            .scroll_to_item(index + 1 + 2 * transitions);
    }

    fn step_selection(&mut self, entries: &[PaletteEntry], forward: bool) {
        if entries.is_empty() || !entries.iter().any(|e| !e.disabled) {
            return;
        }
        let len = entries.len();
        let mut idx = self.selected_idx.min(len - 1);
        for _ in 0..len {
            idx = if forward {
                (idx + 1) % len
            } else {
                idx.checked_sub(1).unwrap_or(len - 1)
            };
            if !entries[idx].disabled {
                self.selected_idx = idx;
                self.scroll_to_entry(entries, idx);
                return;
            }
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Leave all modified shortcuts (Ctrl+P / Ctrl+O in-place switching, OS/screenshot hotkeys) untouched.
        if event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }
        if input_is_composing(&self.query_input, window, cx) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let query = self.query_input.read(cx).value().to_string();
        let entries = self.build_entries(&query);

        match key {
            "down" => {
                self.step_selection(&entries, true);
                cx.notify();
                cx.stop_propagation();
            }
            "up" => {
                self.step_selection(&entries, false);
                cx.notify();
                cx.stop_propagation();
            }
            "home" => {
                if let Some(first) = Self::first_enabled_index(&entries) {
                    self.selected_idx = first;
                    self.scroll_to_entry(&entries, first);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "end" => {
                if let Some(last) = entries.iter().rposition(|e| !e.disabled) {
                    self.selected_idx = last;
                    self.scroll_to_entry(&entries, last);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" if !event.is_held => {
                if let Some(entry) = entries.get(self.selected_idx).filter(|e| !e.disabled) {
                    cx.emit(CommandPaletteEvent::Execute(entry.target.clone()));
                }
                cx.stop_propagation();
            }
            "escape" if !event.is_held => {
                if !query.is_empty() {
                    self.query_input
                        .update(cx, |inp, cx| inp.set_value("", window, cx));
                    self.selected_idx = 0;
                    cx.notify();
                } else {
                    cx.emit(CommandPaletteEvent::Dismiss);
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn render_kbd_chip(label: &'static str, palette: &DesktopPalette) -> impl IntoElement {
        div()
            .h(px(20.0))
            .px(space::TIGHT)
            .rounded(radius::CONTROL)
            .bg(palette.bg_surface)
            .border_1()
            .border_color(palette.border_subtle)
            .text_size(px(11.0))
            .line_height(px(16.0))
            .text_color(palette.text_secondary)
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .child(label)
    }

    /// Three-layer modal shadow matching `RoKYE` (`LKIQI` -> `lRw5X`).
    fn rokye_shadows(palette: &DesktopPalette) -> Vec<BoxShadow> {
        vec![
            BoxShadow {
                color: palette.highlight_top,
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: true,
            },
            BoxShadow {
                color: palette.shadow_panel_sm,
                offset: point(px(0.0), px(4.0)),
                blur_radius: px(10.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: palette.shadow_panel_lg,
                offset: point(px(0.0), px(14.0)),
                blur_radius: px(28.0),
                spread_radius: px(0.0),
                inset: false,
            },
        ]
    }
}

fn format_palette_time(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    if s.len() >= 16 && s.as_bytes().get(10) == Some(&b'T') {
        let today = chrono_free_today_prefix();
        if today.as_deref() == Some(&s[..10]) {
            return Some(s[11..16].to_owned());
        }
    }
    if s.len() >= 10 {
        Some(s[..10].replace('-', "/"))
    } else {
        Some(s.to_owned())
    }
}

fn chrono_free_today_prefix() -> Option<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    // Civil date from Unix days (Howard Hinnant's algorithm, UTC fallback).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    Some(format!("{year:04}-{m:02}-{d:02}"))
}

fn format_export_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes == 0 {
        "0 B".to_owned()
    } else if (bytes as f64) >= MB {
        format!("{:.1} MB", (bytes as f64) / MB)
    } else if bytes >= 1024 {
        format!("{:.0} KB", (bytes as f64) / KB)
    } else {
        format!("{bytes} B")
    }
}

impl Render for CommandPaletteModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let query = self.query_input.read(cx).value().to_string();
        let entries = self.build_entries(&query);
        if entries.is_empty() {
            self.selected_idx = 0;
        } else if self.selected_idx >= entries.len() || entries[self.selected_idx].disabled {
            self.selected_idx = Self::first_enabled_index(&entries).unwrap_or(0);
        }

        let ent_scrim = cx.entity();

        let mut list = div()
            .id("command-palette-list")
            .track_scroll(&self.scroll_handle)
            .relative()
            .w_full()
            .max_h(px(380.0))
            .overflow_y_scroll()
            .p(space::SM)
            .flex()
            .flex_col()
            .gap(space::MICRO);

        if entries.is_empty() {
            list = list.child(
                apply_scale(div(), scale::UI)
                    .w_full()
                    .py(space::GROUP)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(palette.text_muted)
                    .child(cjk_inline_label(self.mode.empty_label())),
            );
        } else {
            let mut last_group: Option<&'static str> = None;
            for (idx, entry) in entries.iter().enumerate() {
                if last_group != Some(entry.group) {
                    if last_group.is_some() {
                        list = list.child(
                            div()
                                .w_full()
                                .py(space::XS)
                                .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                        );
                    }
                    last_group = Some(entry.group);
                    list = list.child(
                        apply_scale(div(), scale::CAPTION)
                            .w_full()
                            .h(px(24.0))
                            .px(space::MD)
                            .flex()
                            .items_center()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label(entry.group)),
                    );
                }

                let is_sel = idx == self.selected_idx && !entry.disabled;
                let disabled = entry.disabled;
                let target = entry.target.clone();
                let ent_click = cx.entity();
                let row_id = SharedString::from(format!("command-palette-item-{idx}"));

                let mut title_el = div().text_color(palette.text_primary).flex().items_center();
                if let Some(col_w) = entry.command_col_width {
                    title_el = title_el.w(px(col_w)).flex_shrink_0();
                } else {
                    title_el = title_el.truncate();
                }
                title_el = if entry.title_is_cjk {
                    title_el.child(cjk_inline_label(entry.title.clone()))
                } else {
                    title_el.child(entry.title.clone())
                };

                let mut left_group = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(Icon::new(entry.icon).size(px(14.0)).text_color(if is_sel {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    }))
                    .child(title_el);

                if let Some(sub) = entry.subtitle.clone() {
                    let sub_el = apply_scale(div(), scale::CAPTION)
                        .text_color(palette.text_secondary)
                        .truncate()
                        .flex()
                        .items_center();
                    let sub_el = if entry.subtitle_is_cjk {
                        sub_el.child(cjk_inline_label(sub))
                    } else {
                        sub_el.child(sub)
                    };
                    left_group = left_group.child(sub_el);
                }

                let mut row = apply_scale(div(), scale::UI)
                    .id(row_id.clone())
                    .test_support()
                    .a11y_listitem(row_id, entry.title.clone(), is_sel)
                    .w_full()
                    .h(px(36.0))
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .bg(if is_sel {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .opacity(if disabled { 0.45 } else { 1.0 })
                    .child(left_group);

                if let Some(kbd) = entry.trailing_kbd {
                    row = row.child(Self::render_kbd_chip(kbd, &palette));
                } else if let Some(ref meta) = entry.trailing_meta {
                    row = row.child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_muted)
                            .flex_shrink_0()
                            .child(meta.clone()),
                    );
                }

                if !disabled {
                    row = row.hover(move |s| s.bg(palette.bg_item_hover)).on_click(
                        move |_, _, cx| {
                            ent_click.update(cx, |_, cx| {
                                cx.emit(CommandPaletteEvent::Execute(target.clone()));
                            });
                        },
                    );
                }

                list = list.child(row);
            }
        }

        render_frosted_surface(
            "command-palette-scrim-x9p2zr",
            FrostedGlassSpec::scrim(&palette),
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
                FrostedGlassSpec::modal(&palette, 8.0),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .a11y_dialog("command-palette-modal", self.mode.dialog_label())
            .w(layout::PALETTE_MODAL_WIDTH)
            .max_h(px(460.0))
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_hairline)
            .shadow(Self::rokye_shadows(&palette))
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .flex_col()
                    .tab_stop(false)
                    .focus_trap("command-palette-focus-trap", &self.trap_focus)
                    .on_key_down(cx.listener(Self::handle_key_down))
                    .child(
                        div()
                            .w_full()
                            .h(px(48.0))
                            .px(space::LG)
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .child(
                                Icon::new(IconName::Search)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.query_input)
                                        .accessibility_id("command-palette-search-input")
                                        .aria_label(self.mode.placeholder())
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0()
                                        .text_size(scale::UI.font_size)
                                        .line_height(scale::UI.line_height)
                                        .h(height::CONTROL)
                                        .w_full(),
                                ),
                            )
                            .child(Self::render_kbd_chip("Esc", &palette)),
                    )
                    .child(list),
            ),
        )
    }
}
