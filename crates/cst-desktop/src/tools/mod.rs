pub mod bitlocker_card;
pub mod normal_card;
pub mod runbook_card;
mod secret_input;
pub mod sys_overview;
pub mod web_search_bar;

use std::rc::Rc;

use cst_client::ClientHandle;
use cst_client::tool_presenter::SpecialToolView;
use gpui_kit::{App, Context, Entity, IntoElement, SharedString, Window, div};
use serde_json::Value;

use crate::theme::DesktopPalette;
use crate::tools::bitlocker_card::{
    BitLockerCardProps, SecretInput, find_pending_bitlocker_request, render_bitlocker_card,
};
use crate::tools::runbook_card::{OpenRunbookFn, RunbookCardProps, render_runbook_card};
use crate::tools::sys_overview::{SysOverviewProps, render_sys_overview_card};
use crate::tools::web_search_bar::{WebSearchBarProps, render_web_search_bar};

pub type ToggleKeyFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type HoverWarningFn = Rc<dyn Fn(String, bool, &mut Window, &mut App)>;

pub struct SpecialToolDispatchProps<'a> {
    pub session_id: &'a str,
    pub entry_id: &'a str,
    pub call_id: &'a str,
    pub has_result: bool,
    pub view: &'a SpecialToolView,
    pub bitlocker_snapshot: Option<&'a Value>,
    pub secret_input: Option<Entity<SecretInput>>,
    pub details_expanded: bool,
    pub warning_hovered: bool,
    pub transparency_enabled: bool,
    pub scale_factor: f32,
}

/// Unified dispatcher for the 4 standalone special persistent components
/// (`sys(overview)`, `bitlocker(unlock/decrypt)`, `command_plan / runbook_deliver / runbook`, `web_search`).
#[allow(clippy::too_many_arguments)]
pub fn render_special_tool_component<V: 'static>(
    props: SpecialToolDispatchProps<'_>,
    client: ClientHandle,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_key: ToggleKeyFn,
    on_hover_warning: HoverWarningFn,
    on_open_runbook: OpenRunbookFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    on_error_notice: impl Fn(String, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    match props.view {
        SpecialToolView::SysOverview {
            status,
            title,
            summary,
            fields,
            errors,
            machine,
            cpu,
            cores,
            memory,
            capacity,
            uptime,
            ..
        } => render_sys_overview_card(
            SysOverviewProps {
                call_id: props.call_id,
                status: *status,
                title,
                summary,
                machine,
                uptime,
                cpu_pct: *cpu,
                cores: *cores,
                memory_pct: *memory,
                capacity,
                fields,
                errors,
                details_expanded: props.details_expanded,
            },
            mono_family,
            palette,
            on_toggle_key,
        ),
        SpecialToolView::Runbook {
            status,
            title,
            summary,
            commands,
            level,
            version,
            planned,
            delivery,
            errors,
            ..
        } => render_runbook_card(
            RunbookCardProps {
                call_id: props.call_id,
                entry_id: props.entry_id,
                status: *status,
                title,
                summary,
                commands,
                level,
                version: *version,
                planned: *planned,
                delivery: delivery.as_ref(),
                errors,
            },
            mono_family,
            palette,
            is_copied_fn,
            on_open_runbook,
            on_mark_copied,
            cx,
        ),
        SpecialToolView::WebSearch {
            status,
            title,
            summary,
            queries,
            successful,
            fields,
            errors,
            ..
        } => render_web_search_bar(
            WebSearchBarProps {
                call_id: props.call_id,
                status: *status,
                title,
                summary,
                queries,
                successful: *successful,
                fields,
                errors,
                expanded: props.details_expanded,
                warning_hovered: props.warning_hovered,
            },
            palette,
            props.transparency_enabled,
            props.scale_factor,
            on_toggle_key,
            on_hover_warning,
        ),
        SpecialToolView::BitLocker {
            status,
            title,
            summary,
            fields,
            errors,
            scope,
            volumes,
            ..
        } => {
            let pending = find_pending_bitlocker_request(
                props.bitlocker_snapshot,
                props.call_id,
                props.has_result,
            );
            render_bitlocker_card(
                BitLockerCardProps {
                    session_id: props.session_id,
                    call_id: props.call_id,
                    status: *status,
                    title,
                    summary,
                    scope,
                    volumes,
                    fields,
                    errors,
                    pending,
                    secret_input: props.secret_input,
                    details_expanded: props.details_expanded,
                },
                client,
                mono_family,
                palette,
                on_toggle_key,
                on_error_notice,
                window,
                cx,
            )
        }
    }
}

pub fn render_empty_tool_placeholder() -> impl IntoElement {
    div()
}
