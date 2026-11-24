//! UI-agnostic local client library for `cst-desktop` (Phase 9) and `cst-tui` (Phase 10).
//!
//! Coordinates per-kit desktop single-instance wakeup, UAC / `server.json` bidirectional identity
//! verification, isolated `/api/app/events` keepalive + `/api/events/stream` multiplex state
//! reduction, shared `WorkspaceState` (`ComposerDraft` + `48 MiB` image budget), optimistic
//! `SessionOutbox`, and 17-tool `tool_presenter` data projections.

pub mod bootstrap;
pub mod error;
pub mod http;
pub mod outbox;
pub mod runtime;
pub mod session_store;
pub mod sse;
pub mod state;
pub mod tool_presenter;
pub mod workspace;

pub use bootstrap::{
    BootstrapOptions, DesktopInstanceGuard, DesktopInstanceOutcome, VerifiedServerEndpoint,
    acquire_or_activate_desktop_instance, ensure_server_running,
    recover_pending_update_transaction,
};
pub use error::ClientError;
pub use http::AppHttpClient;
pub use outbox::{ExitCheckReport, OutboxItem, OutboxItemState, SessionOutbox, WebSwitchCheck};
pub use runtime::{
    ClientBootstrapResult, ClientHandle, ClientRuntimeOptions, CstClient, OpReceipt,
    ReconcileReport, SendSubmissionOutcome,
};
pub use session_store::{FinishLoadingOutcome, SessionStore, SessionViewSnapshot};
pub use state::{
    AccountStatusDto, AppHealthDto, ChannelModeDto, ClientSnapshot, DefaultViewDto,
    DesktopSettingsDto, DesktopSettingsPatchDto, KeyboardFocusStyleDto, LifecycleReportDto,
    MissingKeyClaimDto, ModelChoiceDto, ModelScopeMatchDto, ModelScopeRuleDto, ModelsSnapshot,
    ProviderStatusDto, RecentExportFileDto, RecentExportSummary, SessionSummary, ThemeModeDto,
    UiHintsDto, UpdatePhaseDto, UpdateStateDto, UpdateStatusDto, WebOpenDto,
};
pub use tool_presenter::{
    CardStatus, CommandItem, FieldRow, ListingItem, NoticeStatus, SpecialStatus, SpecialToolKind,
    SpecialToolView, ToolBlock, ToolCallInput, ToolCardView, ToolPresenterCache, ToolResultInput,
    ToolRunState, is_standalone_tool, map_special_tool, map_tool, special_tool_kind,
    split_tool_calls,
};
pub use workspace::{
    ComposerDraft, DEFAULT_GLOBAL_IMAGE_BUDGET_BYTES, DraftKey, ImageBudgetPool, InlineToken,
    MAX_IMAGE_DIMENSION_PX, MAX_SUBMISSION_IMAGE_BYTES, SendMode, SharedImageHandle,
    WorkspaceState, WorkspaceView,
};
