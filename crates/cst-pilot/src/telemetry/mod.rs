pub mod billing;
pub mod collect;
pub mod config;
pub mod delivery;
pub mod diagnostics;
pub mod draft;
pub mod environment;
pub mod file_lock;
pub mod heartbeat;
pub mod identity;
pub mod outbox;
pub mod owner;
pub mod record;
mod sdk;
pub mod service;
mod storage;
pub mod submission;
pub mod transport;

use config::Config;
use environment::{Environment, EnvironmentSource};
use identity::Installation;
pub(crate) use sdk::TelemetryBinding;
pub use service::{TelemetryOptions, TelemetryService};
use std::path::Path;

/// Startup preparation is deliberately separate from the SDK event subscription.
/// Callers skip every probe, observer and write when this returns None.
pub struct PreparedTelemetry {
    pub config: Config,
    pub installation: Option<Installation>,
    pub environment: Environment,
}
impl PreparedTelemetry {
    pub fn load(root: &Path, source: &dyn EnvironmentSource) -> Result<Option<Self>, String> {
        let Some(config) = Config::load(root)? else {
            return Ok(None);
        };
        let installation = Installation::load(root);
        let environment = Environment::capture(installation.as_ref(), source);
        Ok(Some(Self {
            config,
            installation,
            environment,
        }))
    }
}
