use std::path::Path;

use tokio_util::sync::CancellationToken;

pub(crate) use cst_os::fs_safe::{DirectoryLease, Storage, io_error};
pub use cst_os::locks::RunbookDirectoryLock as DirectoryLock;

pub(crate) trait StorageAtomicExt {
    fn atomic(
        &self,
        file: &Path,
        bytes: &[u8],
        replace: bool,
        cancel: &CancellationToken,
    ) -> Result<(), String>;
}

impl StorageAtomicExt for Storage {
    fn atomic(
        &self,
        file: &Path,
        bytes: &[u8],
        replace: bool,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        self.atomic_checked(file, bytes, replace, &|| cancel.is_cancelled())
    }
}

pub(super) fn cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("命令交付已取消。".into())
    } else {
        Ok(())
    }
}
