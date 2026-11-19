//! Offline signed release verification, transactional replacement, rollback, and `cst-updater` CLI.

pub mod applier;
pub mod handoff;
pub mod io;
pub mod layout;
pub mod manifest;
pub mod process;
pub mod verify;

pub mod json {
    pub use cst_os::json::{
        parse_local_json as parse_local, parse_strict_json as parse, strip_json_comments,
    };
}

pub mod lock {
    pub use cst_os::locks::UpdateLock;
}
