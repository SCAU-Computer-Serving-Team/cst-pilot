//! Local multi-session HTTP/SSE server for CST Pilot (`cst-desktop`, built-in TUI, and Web).
pub mod auth_guard;
pub mod discovery;
pub mod error;
pub mod exports;
pub mod locks;
pub mod mode;
pub mod oauth;
pub mod project_files;
pub mod routes;
pub mod runbook;
pub mod server;
pub mod session;
pub mod settings;
pub mod state;

pub use server::RunningServer;
pub use state::{DEFAULT_SERVER_PORT, ServerConfig, ServerState};
