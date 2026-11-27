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

/// Returns `true` when an environment variable string represents an enabled boolean flag.
pub fn is_truthy_env_value(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Pure helper evaluating `(CST_OFFLINE, PI_OFFLINE)` string values for deterministic unit tests.
pub fn is_offline_env_values(cst_offline: Option<&str>, pi_offline: Option<&str>) -> bool {
    cst_offline.is_some_and(is_truthy_env_value) || pi_offline.is_some_and(is_truthy_env_value)
}

/// Returns `true` when caller requested offline execution via `CST_OFFLINE` or `PI_OFFLINE`.
pub fn is_offline_env_enabled() -> bool {
    let cst = std::env::var("CST_OFFLINE").ok();
    let pi = std::env::var("PI_OFFLINE").ok();
    is_offline_env_values(cst.as_deref(), pi.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_env_helper_parses_truthy_and_falsy_values() {
        assert!(!is_offline_env_values(None, None));
        assert!(!is_offline_env_values(Some(""), Some("0")));
        assert!(!is_offline_env_values(Some("false"), Some("off")));
        assert!(is_offline_env_values(Some("1"), None));
        assert!(is_offline_env_values(None, Some("1")));
        assert!(is_offline_env_values(Some(" TRUE "), None));
        assert!(is_offline_env_values(None, Some("yes")));
    }
}
