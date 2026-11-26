//! Cross-process OS primitives shared by `cst-kit`, `cst-updater`, `cst-server`, and `cst-client`.

pub mod discovery;
pub mod fs_safe;
pub mod json;
pub mod layout;
pub mod locks;
pub mod process;
pub mod update_types;

pub use discovery::{
    ServerDiscoveryRecord, cleanup_discovery_file, discovery_file_path, read_discovery_file,
    write_discovery_file_with_timestamp,
};
pub use fs_safe::{
    DirectoryLease, Storage, StorageError, StorageErrorKind, atomic_write_bytes_sync,
    atomic_write_json_sync, is_reparse_or_symlink, is_storage_full_io, is_transient_replace_error,
    link_count,
};
pub use json::{parse_local_json, parse_strict_json, strip_json_comments};
pub use layout::{
    KitLayout, REQUIRED_SKILL_NAMES, normalize_canonical_path_for_hash, resolve_kit_root,
    strip_windows_verbatim_prefix,
};
pub use locks::{RunbookDirectoryLock, ServerInstanceLock, SessionOwnerLock, UpdateLock};
pub use process::{
    ProcessIdentity, ProcessTokenInfo, attach_parent_console_if_stdio_missing,
    can_elevate_same_user, current_process_token_info, is_elevated, is_process_alive,
    launch_detached, query_machine_app_path, query_process_image_path, query_process_token_info,
    system_directory, windows_directory,
};
pub use update_types::{HandoffRequest, LaunchReceipt, RestartUi, RollbackReason, UpdateReport};
