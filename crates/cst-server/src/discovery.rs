use std::path::Path;

use rand::Rng;

pub use cst_os::{
    ServerDiscoveryRecord, ServerInstanceLock, SessionOwnerLock, StorageError,
    cleanup_discovery_file, discovery_file_path, normalize_canonical_path_for_hash,
    read_discovery_file, strip_windows_verbatim_prefix,
};

pub fn generate_random_hex_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

pub fn write_discovery_file(
    kit_root: &Path,
    port: u16,
    token: &str,
) -> Result<ServerDiscoveryRecord, StorageError> {
    cst_os::write_discovery_file_with_timestamp(
        kit_root,
        port,
        token,
        pi_session::now_iso_timestamp(),
    )
}
