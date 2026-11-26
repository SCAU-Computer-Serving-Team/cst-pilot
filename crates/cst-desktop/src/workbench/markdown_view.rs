use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui::{FontWeight, HighlightStyle, Overflow, StyleRefinement, rems};
use gpui_kit::base::Easing;
use gpui_kit::base::text::{
    CodeBlock, MarkdownExtensions, MarkdownNode, SelectionFormat, TextView, TextViewMotion,
    TextViewState, TextViewStyle, markdown_ast,
};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, CursorStyle, Entity, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::shell::open_url_in_trusted_browser;
use crate::theme::{
    DesktopPalette, ResolvedTheme, apply_scale, height, motion, radius, scale, space, typography,
};

pub type CodeCopiedCallback = Arc<dyn Fn(&mut App) + Send + Sync>;

#[derive(Clone)]
struct FencedCodeData {
    code: SharedString,
    lang: Option<SharedString>,
    span_start: usize,
}

struct CodeBodyCache {
    fenced_source: String,
    state: Entity<TextViewState>,
}

/// Wraps raw code in a CommonMark fence strictly longer than any backtick run in `code`.
fn wrap_fenced_code_markdown(code: &str) -> String {
    let mut max_run = 0usize;
    let mut cur_run = 0usize;
    for b in code.bytes() {
        if b == b'`' {
            cur_run += 1;
            max_run = max_run.max(cur_run);
        } else {
            cur_run = 0;
        }
    }
    let fence = "`".repeat(max_run.max(2) + 1);
    if code.is_empty() {
        format!("{fence}\n{fence}")
    } else {
        format!("{fence}\n{code}\n{fence}")
    }
}

fn build_code_body_style(palette: &DesktopPalette, mono_family: &SharedString) -> TextViewStyle {
    let code_block_style = StyleRefinement::default()
        .px(px(14.0))
        .py(px(12.0))
        .rounded(px(0.0))
        .border_0()
        .border_color(gpui::transparent_black())
        .font_family(mono_family.clone())
        .text_size(px(13.0))
        .line_height(px(21.0));

    TextViewStyle::default()
        .with_foreground(palette.text_primary)
        .with_muted_foreground(palette.text_secondary)
        .with_code_background(gpui::transparent_black())
        .with_border(gpui::transparent_black())
        .with_paragraph_gap(rems(0.0))
        .with_code_block(code_block_style)
        .with_dark(palette.mode.is_dark())
}

type SharedHighlighter =
    Arc<dyn Fn(&CodeBlock) -> Vec<(Range<usize>, HighlightStyle)> + Send + Sync>;

fn shared_markdown_code_highlighter(mode: ResolvedTheme) -> SharedHighlighter {
    static LIGHT: OnceLock<SharedHighlighter> = OnceLock::new();
    static DARK: OnceLock<SharedHighlighter> = OnceLock::new();
    let slot = match mode {
        ResolvedTheme::Light => &LIGHT,
        ResolvedTheme::Dark => &DARK,
    };
    Arc::clone(slot.get_or_init(|| {
        let palette = DesktopPalette::for_mode(mode);
        Arc::new(move |block: &CodeBlock| {
            highlight_code_block(block.code().as_ref(), block.lang().as_deref(), &palette)
        })
    }))
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_shell_lang(lang: Option<&str>) -> bool {
    matches!(
        lang.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        None | Some("")
            | Some("sh")
            | Some("bash")
            | Some("zsh")
            | Some("shell")
            | Some("ps")
            | Some("ps1")
            | Some("pwsh")
            | Some("powershell")
            | Some("cmd")
            | Some("bat")
            | Some("batch")
            | Some("dos")
            | Some("console")
            | Some("text")
    )
}

fn is_keyword_or_cmd(word: &str, shell_like: bool, at_line_cmd: bool) -> bool {
    let lower = word.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "fn" | "let"
            | "mut"
            | "const"
            | "static"
            | "pub"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "match"
            | "if"
            | "else"
            | "for"
            | "in"
            | "while"
            | "loop"
            | "break"
            | "continue"
            | "return"
            | "async"
            | "await"
            | "use"
            | "mod"
            | "where"
            | "type"
            | "as"
            | "ref"
            | "move"
            | "unsafe"
            | "function"
            | "var"
            | "class"
            | "export"
            | "import"
            | "from"
            | "default"
            | "new"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "switch"
            | "case"
            | "def"
            | "lambda"
            | "with"
            | "yield"
            | "pass"
            | "elif"
            | "and"
            | "or"
            | "not"
            | "select"
            | "insert"
            | "update"
            | "delete"
            | "create"
            | "drop"
            | "alter"
            | "table"
            | "join"
            | "order"
            | "group"
            | "by"
            | "limit"
            | "param"
            | "begin"
            | "process"
            | "end"
            | "foreach"
            | "echo"
            | "set"
    ) {
        return true;
    }

    if !shell_like {
        return false;
    }

    if let Some((verb, noun)) = word.split_once('-')
        && !noun.is_empty()
        && matches!(
            verb.to_ascii_lowercase().as_str(),
            "get"
                | "set"
                | "new"
                | "remove"
                | "clear"
                | "select"
                | "format"
                | "where"
                | "sort"
                | "group"
                | "measure"
                | "write"
                | "read"
                | "out"
                | "start"
                | "stop"
                | "restart"
                | "test"
                | "invoke"
                | "enable"
                | "disable"
                | "repair"
                | "update"
                | "install"
                | "uninstall"
                | "export"
                | "import"
                | "convertto"
                | "convertfrom"
                | "add"
                | "copy"
                | "move"
                | "rename"
                | "resolve"
                | "find"
        )
    {
        return true;
    }

    if at_line_cmd
        && matches!(
            lower.as_str(),
            "powercfg"
                | "diskpart"
                | "chkdsk"
                | "sfc"
                | "dism"
                | "netsh"
                | "ipconfig"
                | "ping"
                | "tracert"
                | "nslookup"
                | "route"
                | "arp"
                | "reg"
                | "sc"
                | "schtasks"
                | "tasklist"
                | "taskkill"
                | "bcdedit"
                | "manage-bde"
                | "wmic"
                | "wevtutil"
                | "pnputil"
                | "driverquery"
                | "systeminfo"
                | "whoami"
                | "hostname"
                | "pwsh"
                | "powershell"
                | "cmd"
                | "git"
                | "cargo"
                | "rustc"
                | "npm"
                | "node"
                | "python"
                | "uv"
                | "curl"
                | "ssh"
                | "scp"
                | "cd"
                | "dir"
                | "ls"
                | "mkdir"
                | "del"
                | "rm"
                | "cp"
                | "mv"
                | "cat"
                | "type"
        )
    {
        return true;
    }

    false
}

fn is_literal_value(word: &str, shell_like: bool) -> bool {
    let lower = word.to_ascii_lowercase();
    if matches!(lower.as_str(), "true" | "false" | "null" | "none" | "nil") {
        return true;
    }
    shell_like && matches!(lower.as_str(), "on" | "off" | "yes" | "no")
}

/// Three-tone restrained syntax highlighter matching `00H · Markdown 富文本` (`HmEJM`):
/// - Comments: `$text-muted`
/// - Keywords / commands / PowerShell cmdlets: `$accent-ink`
/// - Strings / CLI flags / literals / numbers: `$status-success`
/// - Pipes & logical operators: `$text-secondary`
pub fn highlight_code_block(
    code: &str,
    lang: Option<&str>,
    palette: &DesktopPalette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::new();
    let bytes = code.as_bytes();
    let len = bytes.len();
    let shell_like = is_shell_lang(lang);

    let comment_style = HighlightStyle {
        color: Some(palette.text_muted),
        ..Default::default()
    };
    let cmd_style = HighlightStyle {
        color: Some(palette.accent_ink),
        ..Default::default()
    };
    let value_style = HighlightStyle {
        color: Some(palette.status_success),
        ..Default::default()
    };
    let op_style = HighlightStyle {
        color: Some(palette.text_secondary),
        ..Default::default()
    };

    let mut i = 0usize;
    let mut at_cmd_pos = true;
    let mut prev_was_flag = false;

    while i < len {
        let b = bytes[i];

        if b == b'\n' || b == b'\r' {
            i += 1;
            at_cmd_pos = true;
            prev_was_flag = false;
            continue;
        }

        if b == b' ' || b == b'\t' {
            i += 1;
            continue;
        }

        // Line comments: `# ...`, `// ...`, `-- ...`
        let is_hash_comment = b == b'#'
            && (shell_like
                || matches!(
                    lang.map(|s| s.to_ascii_lowercase()).as_deref(),
                    Some("py" | "python" | "toml" | "yaml" | "yml" | "ini" | "conf")
                ));
        let is_slash_comment = b == b'/' && i + 1 < len && bytes[i + 1] == b'/';
        let is_sql_comment = b == b'-'
            && i + 1 < len
            && bytes[i + 1] == b'-'
            && matches!(lang.map(|s| s.to_ascii_lowercase()).as_deref(), Some("sql"));

        if is_hash_comment || is_slash_comment || is_sql_comment {
            let start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            out.push((start..i, comment_style));
            at_cmd_pos = false;
            prev_was_flag = false;
            continue;
        }

        // Block comments: `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(len);
            out.push((start..i, comment_style));
            at_cmd_pos = false;
            prev_was_flag = false;
            continue;
        }

        // Quoted strings: `"..."`, `'...'`, `` `...` ``
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b;
            let start = i;
            i += 1;
            while i < len {
                let c = bytes[i];
                if c == b'\\' && quote != b'\'' && i + 1 < len {
                    i += 2;
                    continue;
                }
                i += 1;
                if c == quote {
                    break;
                }
                if c == b'\n' && quote != b'`' {
                    break;
                }
            }
            out.push((start..i, value_style));
            at_cmd_pos = false;
            prev_was_flag = false;
            continue;
        }

        // Pipes & logical / arrow operators
        if b == b'|' {
            let start = i;
            i += if i + 1 < len && bytes[i + 1] == b'|' {
                2
            } else {
                1
            };
            out.push((start..i, op_style));
            at_cmd_pos = true;
            prev_was_flag = false;
            continue;
        }
        if b == b'&' && i + 1 < len && bytes[i + 1] == b'&' {
            out.push((i..(i + 2), op_style));
            i += 2;
            at_cmd_pos = true;
            prev_was_flag = false;
            continue;
        }
        if b == b';' {
            out.push((i..(i + 1), op_style));
            i += 1;
            at_cmd_pos = true;
            prev_was_flag = false;
            continue;
        }
        if (b == b'-' || b == b'=') && i + 1 < len && bytes[i + 1] == b'>' {
            out.push((i..(i + 2), op_style));
            i += 2;
            at_cmd_pos = false;
            prev_was_flag = false;
            continue;
        }

        // Shell flags: `/h`, `-Name`, `--release`
        if shell_like
            && !at_cmd_pos
            && ((b == b'/' && i + 1 < len && bytes[i + 1].is_ascii_alphanumeric())
                || (b == b'-'
                    && i + 1 < len
                    && (bytes[i + 1].is_ascii_alphanumeric() || bytes[i + 1] == b'-')))
        {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
            {
                i += 1;
            }
            out.push((start..i, value_style));
            prev_was_flag = true;
            continue;
        }

        // Numeric literals: `25.4`, `0x1F`, `100`
        if b.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
            {
                i += 1;
            }
            out.push((start..i, value_style));
            at_cmd_pos = false;
            prev_was_flag = false;
            continue;
        }

        // Identifiers, keywords, cmdlets, drive letters
        if is_ident_start(b) {
            let start = i;
            i += 1;
            while i < len && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let word = &code[start..i];

            if shell_like && at_cmd_pos && word.eq_ignore_ascii_case("rem") {
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push((start..i, comment_style));
                at_cmd_pos = false;
                prev_was_flag = false;
                continue;
            }

            if is_keyword_or_cmd(word, shell_like, at_cmd_pos) {
                out.push((start..i, cmd_style));
                at_cmd_pos = false;
                prev_was_flag = false;
            } else if is_literal_value(word, shell_like)
                || (shell_like
                    && !at_cmd_pos
                    && (prev_was_flag
                        || (word.len() == 1 && word.as_bytes()[0].is_ascii_uppercase())))
            {
                out.push((start..i, value_style));
                at_cmd_pos = false;
                prev_was_flag = false;
            } else {
                at_cmd_pos = false;
                prev_was_flag = false;
            }
            continue;
        }

        // Advance safely by one UTF-8 scalar
        let ch_len = code[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
        at_cmd_pos = false;
        prev_was_flag = false;
    }

    out
}

/// Maintains stable `Entity<TextViewState>` instances keyed by message/entry ID so streaming
/// incremental deltas (`push_str`) and snapshot replacements (`set_text`) never recreate the view
/// or drop active user text selections.
#[derive(Default)]
pub struct MarkdownStreamStore {
    states: BTreeMap<String, (String, Entity<TextViewState>)>,
}

impl MarkdownStreamStore {
    /// Synchronizes `key` with `markdown_source`, calling `push_str` when `markdown_source` extends
    /// the previously committed prefix or `set_text` on initial load / full replacement.
    pub fn sync_entry(
        &mut self,
        key: &str,
        markdown_source: &str,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        if let Some((prev_text, entity)) = self.states.get_mut(key) {
            if prev_text != markdown_source {
                if !prev_text.is_empty()
                    && let Some(delta) = markdown_source.strip_prefix(prev_text.as_str())
                {
                    let delta_owned = delta.to_owned();
                    entity.update(cx, |st, cx| {
                        st.push_str(&delta_owned, cx);
                    });
                } else {
                    let full = markdown_source.to_owned();
                    entity.update(cx, |st, cx| {
                        st.set_text(&full, cx);
                    });
                }
                prev_text.clear();
                prev_text.push_str(markdown_source);
            }
            return entity.clone();
        }

        let initial = markdown_source.to_owned();
        let entity = cx.new(|cx| {
            TextViewState::markdown(&initial, cx)
                .selectable(true)
                .selection_format(SelectionFormat::SourceAcrossStyles)
        });
        self.states
            .insert(key.to_owned(), (markdown_source.to_owned(), entity.clone()));
        entity
    }

    pub fn retain_keys(&mut self, live_keys: &BTreeSet<String>) {
        self.states.retain(|k, _| live_keys.contains(k));
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

pub fn build_markdown_style(palette: &DesktopPalette) -> TextViewStyle {
    let mut table_style = StyleRefinement::default().rounded(radius::ROW);
    table_style.overflow.x = Some(Overflow::Scroll);

    let table_head_style = StyleRefinement::default()
        .font_weight(FontWeight::MEDIUM)
        .text_size(px(13.0))
        .line_height(px(20.0))
        .text_color(palette.text_secondary);
    let table_cell_style = StyleRefinement::default()
        .text_size(scale::UI.font_size)
        .line_height(px(21.0))
        .px(px(14.0))
        .py(px(11.0))
        // Hide inner vertical column dividers while keeping the outer frame and horizontal row rules.
        .border_color(gpui::transparent_black());

    let code_block_style = StyleRefinement::default()
        .px(px(14.0))
        .py(px(12.0))
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .text_size(px(13.0))
        .line_height(px(21.0));

    let blockquote_style = StyleRefinement::default()
        .border_l(px(3.0))
        .border_color(palette.border_strong)
        .bg(palette.bg_surface)
        .rounded_tr(radius::CONTROL)
        .rounded_br(radius::CONTROL)
        .px(px(14.0))
        .py(px(10.0))
        .text_size(px(15.0))
        .line_height(px(24.0))
        .text_color(palette.text_secondary);

    let list_style = StyleRefinement::default().text_color(palette.text_muted);

    let border_subtle = palette.border_subtle;
    let text_secondary = palette.text_secondary;

    TextViewStyle::default()
        .with_foreground(palette.text_primary)
        .with_muted_foreground(palette.text_secondary)
        .with_link(palette.accent_ink)
        .with_code_background(palette.bg_surface)
        .with_inline_code(HighlightStyle {
            background_color: Some(palette.bg_surface),
            color: Some(palette.text_primary),
            ..Default::default()
        })
        .with_inline_code_style(
            StyleRefinement::default()
                .text_size(px(13.0))
                .px(px(5.0))
                .py(px(1.0))
                .rounded(radius::CONTROL)
                .bg(palette.bg_surface)
                .border_1()
                .border_color(palette.border_subtle),
        )
        .with_list_style(list_style)
        .with_blockquote_style(blockquote_style)
        .with_border(palette.border_subtle)
        .with_paragraph_gap(rems(1.0))
        .with_heading(move |level| match level {
            1 => StyleRefinement::default()
                .text_size(px(24.0))
                .line_height(px(32.0))
                .font_weight(FontWeight::SEMIBOLD)
                .pb(px(10.0))
                .border_b_1()
                .border_color(border_subtle),
            2 => StyleRefinement::default()
                .text_size(scale::TITLE.font_size)
                .line_height(scale::TITLE.line_height)
                .font_weight(FontWeight::SEMIBOLD)
                .pt(px(6.0)),
            3 => StyleRefinement::default()
                .text_size(px(17.0))
                .line_height(px(25.0))
                .font_weight(FontWeight::SEMIBOLD),
            4 | 5 => StyleRefinement::default()
                .text_size(px(15.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::SEMIBOLD),
            _ => StyleRefinement::default()
                .text_size(scale::UI.font_size)
                .line_height(px(22.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(text_secondary),
        })
        .with_table(table_style)
        .with_table_head(table_head_style)
        .with_table_cell(table_cell_style)
        .with_table_row_hover(palette.bg_item_hover)
        .with_code_block(code_block_style)
        .with_dark(palette.mode.is_dark())
}

/// Renders a streaming or finalized assistant Markdown block backed by a stable `Entity<TextViewState>`.
pub fn render_assistant_markdown(
    state: &Entity<TextViewState>,
    is_streaming: bool,
    reduce_motion: bool,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let style = build_markdown_style(palette);
    render_assistant_markdown_with_style(state, is_streaming, reduce_motion, palette, style)
}

/// Renders an assistant Markdown block reusing a pre-built `TextViewStyle` so multi-turn frames
/// avoid rebuilding identical `StyleRefinement` allocations on every block.
pub fn render_assistant_markdown_with_style(
    state: &Entity<TextViewState>,
    is_streaming: bool,
    reduce_motion: bool,
    palette: &DesktopPalette,
    style: TextViewStyle,
) -> impl IntoElement {
    render_assistant_markdown_with_copy_notice(
        state,
        is_streaming,
        reduce_motion,
        palette,
        style,
        None,
    )
}

/// Renders an assistant Markdown block with a standard header-bar fenced code block layout
/// (`language` on left, `28×28` copy button on right, `1px` bottom border, symmetric body padding)
/// and persistent `1.6s` icon-swap + optional toast callback on copy.
pub fn render_assistant_markdown_with_copy_notice(
    state: &Entity<TextViewState>,
    is_streaming: bool,
    reduce_motion: bool,
    palette: &DesktopPalette,
    style: TextViewStyle,
    on_code_copied: Option<CodeCopiedCallback>,
) -> impl IntoElement {
    let palette_copy = *palette;
    let state_entity_id = state.entity_id();
    let state_weak = state.downgrade();
    let text_motion = if is_streaming && !reduce_motion {
        TextViewMotion::default()
            .with_stream_fade(Duration::from_millis(280))
            .with_stream_fade_stagger(Duration::from_millis(10))
            .with_stream_fade_easing(Easing::EaseOut)
    } else {
        TextViewMotion::default()
    };

    let extensions = MarkdownExtensions::default()
        .parser_revision(1)
        .block_parser(|node, parse_cx| {
            let markdown_ast::Node::Code(code) = node else {
                return None;
            };
            let span_start = node
                .position()
                .map(|p| parse_cx.offset() + p.start.offset)
                .unwrap_or(0);
            let lang = code
                .lang
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| SharedString::from(s.to_owned()));
            let code_text = SharedString::from(code.value.clone());
            let raw_md = parse_cx
                .node_source(node)
                .map(str::to_owned)
                .unwrap_or_else(|| wrap_fenced_code_markdown(&code.value));
            Some(
                MarkdownNode::new(
                    "cst-code-block",
                    FencedCodeData {
                        code: code_text.clone(),
                        lang,
                        span_start,
                    },
                )
                .text(code_text)
                .markdown(raw_md),
            )
        })
        .block_renderer("cst-code-block", move |node, window, cx| {
            let Some(data) = node.data::<FencedCodeData>() else {
                return div().into_any_element();
            };
            let span_start = node
                .source_range()
                .map(|r| r.start)
                .unwrap_or(data.span_start);
            let code_str = data.code.clone();
            let lang_label = data.lang.clone();
            let mono_family = cx
                .try_global::<crate::theme::DesktopThemeGlobal>()
                .map(|g| g.mono_font_family.clone())
                .unwrap_or_else(|| SharedString::from(typography::MONO_FONT_FAMILY));
            let fenced_source = wrap_fenced_code_markdown(code_str.as_ref());

            let body_cache = window.use_keyed_state(
                SharedString::from(format!("md-code-body-{state_entity_id:?}-{span_start}")),
                cx,
                |_, cx| {
                    let initial = fenced_source.clone();
                    let body_state = cx.new(|cx| {
                        TextViewState::markdown(&initial, cx)
                            .selectable(true)
                            .selection_format(SelectionFormat::Plain)
                    });
                    CodeBodyCache {
                        fenced_source: initial,
                        state: body_state,
                    }
                },
            );
            let body_state = {
                let needs_update = body_cache.read(cx).fenced_source != fenced_source;
                if needs_update {
                    let next_src = fenced_source.clone();
                    body_cache.update(cx, |cache, cx| {
                        cache.fenced_source = next_src.clone();
                        cache.state.update(cx, |st, cx| {
                            st.set_text(&next_src, cx);
                        });
                    });
                }
                body_cache.read(cx).state.clone()
            };

            let copied_state = window.use_keyed_state(
                SharedString::from(format!("md-code-copied-{state_entity_id:?}-{span_start}")),
                cx,
                |_, _| Option::<gpui::Task<()>>::None,
            );
            let copied_recently = copied_state.read(cx).is_some();
            let copied_state_click = copied_state.clone();
            let weak_notify = state_weak.clone();
            let on_copied_cb = on_code_copied.clone();

            let code_copy_id = SharedString::from(format!("md-code-copy-{span_start}"));
            let motion_copy_id =
                SharedString::from(format!("md-code-copy-{state_entity_id:?}-{span_start}"));
            let copy_btn = div()
                .id(code_copy_id.clone())
                .test_support()
                .a11y_button(code_copy_id, "复制代码块")
                .w(height::COMPACT)
                .h(height::COMPACT)
                .rounded(radius::CONTROL)
                .flex()
                .items_center()
                .justify_center()
                .hover(move |s| s.bg(palette_copy.bg_item_hover))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code_str.to_string()));
                    let weak_exp = weak_notify.clone();
                    copied_state_click.update(cx, |slot, cx| {
                        let timer = cx.background_executor().timer(motion::ICON_SWAP_RESTORE);
                        // Replacing the task cancels the previous click's restore.
                        *slot = Some(cx.spawn(async move |weak, cx| {
                            timer.await;
                            if weak
                                .update(cx, |slot, cx| {
                                    *slot = None;
                                    cx.notify();
                                })
                                .is_ok()
                                && let Some(ent) = weak_exp.upgrade()
                            {
                                ent.update(cx, |_, cx| cx.notify());
                            }
                        }));
                        cx.notify();
                    });
                    if let Some(ent) = weak_notify.upgrade() {
                        ent.update(cx, |_, cx| cx.notify());
                    }
                    if let Some(ref cb) = on_copied_cb {
                        cb(cx);
                    }
                })
                .child(motion::animated_copy_icon(
                    motion_copy_id,
                    copied_recently,
                    px(14.0),
                    if copied_recently {
                        palette_copy.status_success
                    } else {
                        palette_copy.text_secondary
                    },
                    window,
                    cx,
                ));

            let lang_for_hl = lang_label.clone();
            let body_highlighter: SharedHighlighter = Arc::new(move |block: &CodeBlock| {
                highlight_code_block(block.code().as_ref(), lang_for_hl.as_deref(), &palette_copy)
            });

            let header = div()
                .id(SharedString::from(format!("md-code-header-{span_start}")))
                .test_support()
                .w_full()
                .h(px(36.0))
                .pl(px(14.0))
                .pr(space::SM)
                .border_b_1()
                .border_color(palette_copy.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    div()
                        .font_family(mono_family.clone())
                        .text_size(scale::CAPTION.font_size)
                        .line_height(scale::CAPTION.line_height)
                        .text_color(palette_copy.text_muted)
                        .child(lang_label.unwrap_or_default()),
                )
                .child(copy_btn);

            div()
                .id(SharedString::from(format!("md-code-block-{span_start}")))
                .test_support()
                .w_full()
                .min_w_0()
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette_copy.border_subtle)
                .bg(palette_copy.bg_surface)
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(header)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .font_family(mono_family.clone())
                        .child(
                            TextView::new(&body_state)
                                .style(build_code_body_style(&palette_copy, &mono_family))
                                .selectable(true)
                                .scrollable(false)
                                .shared_code_block_highlighter(body_highlighter),
                        ),
                )
                .into_any_element()
        });

    div()
        .text_size(scale::BODY.font_size)
        .line_height(px(26.0))
        .font_weight(scale::BODY.weight)
        .w_full()
        .min_w_0()
        .text_color(palette.text_primary)
        .child(
            TextView::new(state)
                .style(style)
                .selectable(true)
                .scrollable(false)
                .motion(text_motion)
                .markdown_extensions(extensions)
                .shared_code_block_highlighter(shared_markdown_code_highlighter(palette.mode))
                .on_link_click(|url, _event, _window, _cx| {
                    open_url_in_trusted_browser(url.as_ref());
                }),
        )
}

/// Visual-only inert Markdown snapshot used during `150ms` process-drawer collapse exit:
/// non-selectable, no link click handler, no copy buttons, and no accessibility/test hooks.
pub fn render_assistant_markdown_inert(
    state: &Entity<TextViewState>,
    palette: &DesktopPalette,
    style: TextViewStyle,
) -> impl IntoElement {
    let palette_copy = *palette;
    let extensions = MarkdownExtensions::default()
        .parser_revision(1)
        .block_parser(|node, parse_cx| {
            let markdown_ast::Node::Code(code) = node else {
                return None;
            };
            let span_start = node
                .position()
                .map(|p| parse_cx.offset() + p.start.offset)
                .unwrap_or(0);
            let lang = code
                .lang
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| SharedString::from(s.to_owned()));
            let code_text = SharedString::from(code.value.clone());
            let raw_md = parse_cx
                .node_source(node)
                .map(str::to_owned)
                .unwrap_or_else(|| wrap_fenced_code_markdown(&code.value));
            Some(
                MarkdownNode::new(
                    "cst-code-block",
                    FencedCodeData {
                        code: code_text.clone(),
                        lang,
                        span_start,
                    },
                )
                .text(code_text)
                .markdown(raw_md),
            )
        })
        .block_renderer("cst-code-block", move |node, _window, cx| {
            let Some(data) = node.data::<FencedCodeData>() else {
                return div().into_any_element();
            };
            let mono_family = cx
                .try_global::<crate::theme::DesktopThemeGlobal>()
                .map(|g| g.mono_font_family.clone())
                .unwrap_or_else(|| SharedString::from(typography::MONO_FONT_FAMILY));
            let header = div()
                .w_full()
                .h(px(36.0))
                .pl(px(14.0))
                .pr(space::SM)
                .border_b_1()
                .border_color(palette_copy.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    div()
                        .font_family(mono_family.clone())
                        .text_size(scale::CAPTION.font_size)
                        .line_height(scale::CAPTION.line_height)
                        .text_color(palette_copy.text_muted)
                        .child(data.lang.clone().unwrap_or_default()),
                );
            div()
                .w_full()
                .min_w_0()
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette_copy.border_subtle)
                .bg(palette_copy.bg_surface)
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(header)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .px(px(14.0))
                        .py(space::MD)
                        .font_family(mono_family)
                        .text_size(scale::CAPTION.font_size)
                        .line_height(scale::CAPTION.line_height)
                        .text_color(palette_copy.text_secondary)
                        .child(data.code.clone()),
                )
                .into_any_element()
        });

    div()
        .text_size(scale::BODY.font_size)
        .line_height(px(26.0))
        .font_weight(scale::BODY.weight)
        .w_full()
        .min_w_0()
        .text_color(palette.text_primary)
        .child(
            TextView::new(state)
                .style(style)
                .selectable(false)
                .scrollable(false)
                .motion(TextViewMotion::default())
                .markdown_extensions(extensions)
                .shared_code_block_highlighter(shared_markdown_code_highlighter(palette.mode)),
        )
}

/// Renders a `12px/16px` (`$font-caption`) monospace or plain block (`$bg-surface`) with a
/// persistent top-right `32×32px` copy button (`copy` -> 1.6s `check` swap) and right padding
/// so text is never occluded.
#[allow(clippy::too_many_arguments)]
pub fn render_copyable_text_block<V: 'static>(
    block_id: impl Into<SharedString>,
    text: impl Into<SharedString>,
    monospace: bool,
    mono_family: SharedString,
    copied_recently: bool,
    palette: &DesktopPalette,
    on_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let block_id = block_id.into();
    let text_shared = text.into();
    let copy_btn_id = SharedString::from(format!("{block_id}-copy-btn"));
    let text_for_copy = text_shared.clone();
    let id_for_cb = block_id.clone();
    let motion_id = copy_btn_id.clone();

    let mut content_div = apply_scale(div(), scale::CAPTION)
        .w_full()
        .min_w_0()
        .text_color(palette.text_secondary);
    if monospace {
        content_div = content_div.font_family(mono_family);
    }

    div()
        .id(block_id)
        .relative()
        .w_full()
        .min_w_0()
        .min_h(px(36.0))
        .pl(space::MD)
        .pr(px(40.0))
        .py(px(10.0))
        .rounded(radius::CONTROL)
        .bg(palette.bg_surface)
        .child(content_div.child(text_shared))
        .child(
            div()
                .id(copy_btn_id.clone())
                .test_support()
                .a11y_button(copy_btn_id, "复制文本内容")
                .absolute()
                .top(space::XS)
                .right(space::SM)
                .w(px(28.0))
                .h(px(28.0))
                .rounded(radius::CONTROL)
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(palette.bg_item_hover))
                .on_click(cx.listener(move |view, _, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text_for_copy.to_string()));
                    on_copied(id_for_cb.clone(), window, cx);
                    let _ = view;
                }))
                .child(motion::animated_copy_icon(
                    motion_id,
                    copied_recently,
                    px(16.0),
                    if copied_recently {
                        palette.status_success
                    } else {
                        palette.text_secondary
                    },
                    window,
                    cx,
                )),
        )
}

/// Visual-only non-interactive copyable text block used during `150ms` collapse exits.
pub fn render_copyable_text_block_inert(
    text: impl Into<SharedString>,
    monospace: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut content_div = apply_scale(div(), scale::CAPTION)
        .w_full()
        .min_w_0()
        .text_color(palette.text_secondary);
    if monospace {
        content_div = content_div.font_family(mono_family);
    }
    div()
        .relative()
        .w_full()
        .min_w_0()
        .min_h(px(36.0))
        .pl(space::MD)
        .pr(px(40.0))
        .py(px(10.0))
        .rounded(radius::CONTROL)
        .bg(palette.bg_surface)
        .child(content_div.child(text.into()))
}

/// Helper tracker that remembers recently-copied IDs for 1.6s (`iconSwap` contract).
#[derive(Default)]
pub struct CopyFeedbackTracker {
    restore_tasks: BTreeMap<SharedString, gpui::Task<()>>,
}

impl CopyFeedbackTracker {
    pub fn is_copied(&self, id: &str) -> bool {
        self.restore_tasks.contains_key(id)
    }

    pub fn mark_copied<V: 'static>(
        &mut self,
        id: SharedString,
        get_mut: fn(&mut V) -> &mut Self,
        cx: &mut Context<V>,
    ) {
        let key = id.clone();
        let timer = cx.background_executor().timer(motion::ICON_SWAP_RESTORE);
        let task = cx.spawn(async move |weak, cx| {
            timer.await;
            let _ = weak.update(cx, |view, cx| {
                get_mut(view).restore_tasks.remove(&key);
                cx.notify();
            });
        });
        // Dropping an existing task cancels its restore, including on session change.
        self.restore_tasks.insert(id, task);
        cx.notify();
    }
}

/// Renders an external link pill with explicit pointer hand cursor (`CursorStyle::PointingHand`).
pub fn external_link_cursor() -> CursorStyle {
    CursorStyle::PointingHand
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_markdown_style_matches_00h_spec_tokens() {
        let light = DesktopPalette::light();
        let style = build_markdown_style(&light);

        assert_eq!(style.foreground(), light.text_primary);
        assert_eq!(style.muted_foreground(), light.text_secondary);
        assert_eq!(style.link(), light.accent_ink);
        assert_eq!(style.code_background(), light.bg_surface);
        assert_eq!(style.border(), light.border_subtle);
        assert_eq!(style.table_row_hover(), Some(light.bg_item_hover));
        assert_eq!(
            style.table_cell().border_color,
            Some(gpui::transparent_black())
        );
        assert_eq!(style.table_head().text.color, Some(light.text_secondary));
        assert_eq!(style.code_block().border_color, Some(light.border_subtle));
        assert_eq!(
            style.blockquote_style().border_color,
            Some(light.border_strong)
        );
        assert_eq!(style.list_style().text.color, Some(light.text_muted));
        assert_eq!(style.heading(1).border_color, Some(light.border_subtle));
        assert_eq!(style.heading(6).text.color, Some(light.text_secondary));
    }

    #[test]
    fn highlight_code_block_applies_three_tone_semantics_and_valid_utf8_ranges() {
        let palette = DesktopPalette::dark();
        let code = "# 关闭系统休眠并立即释放 hiberfil.sys\npowercfg /h off\nGet-PSDrive C | Select-Object Used, Free";
        let spans = highlight_code_block(code, Some("powershell"), &palette);

        let mut prev_end = 0;
        for (range, _) in &spans {
            assert!(
                range.start >= prev_end,
                "ranges must be sorted and non-overlapping"
            );
            assert!(range.start < range.end && range.end <= code.len());
            assert!(code.is_char_boundary(range.start) && code.is_char_boundary(range.end));
            prev_end = range.end;
        }

        let find_color = |needle: &str| {
            let start = code.find(needle).expect("needle exists");
            let end = start + needle.len();
            spans
                .iter()
                .find(|(r, _)| r.start == start && r.end == end)
                .and_then(|(_, s)| s.color)
        };

        assert_eq!(
            find_color("# 关闭系统休眠并立即释放 hiberfil.sys"),
            Some(palette.text_muted)
        );
        assert_eq!(find_color("powercfg"), Some(palette.accent_ink));
        assert_eq!(find_color("/h"), Some(palette.status_success));
        assert_eq!(find_color("off"), Some(palette.status_success));
        assert_eq!(find_color("Get-PSDrive"), Some(palette.accent_ink));
        assert_eq!(find_color("C"), Some(palette.status_success));
        assert_eq!(find_color("|"), Some(palette.text_secondary));
        assert_eq!(find_color("Select-Object"), Some(palette.accent_ink));
    }

    #[test]
    fn wrap_fenced_code_markdown_avoids_inner_backtick_collision() {
        assert_eq!(
            wrap_fenced_code_markdown("powercfg /h off"),
            "```\npowercfg /h off\n```"
        );
        assert_eq!(wrap_fenced_code_markdown(""), "```\n```");
        // The parser removes the newline before the closing fence, so keep an extra separator.
        assert_eq!(wrap_fenced_code_markdown("a\n\n"), "```\na\n\n\n```");
        assert_eq!(
            wrap_fenced_code_markdown("echo ```hello```"),
            "````\necho ```hello```\n````"
        );
    }
}
