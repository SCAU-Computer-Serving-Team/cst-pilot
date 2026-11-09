use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cst_pilot::update::activity::{ActivityGate, ActivityLease};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{Notify, OwnedRwLockReadGuard, RwLock};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelMode {
    AppActive,
    WebActive,
}

impl ChannelMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppActive => "app_active",
            Self::WebActive => "web_active",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppPresenceState {
    WaitingFirstConnect,
    Connected,
    GracePeriod { epoch: u64 },
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerIdentity {
    App,
    Web { cookie_epoch: u64 },
}

#[derive(Debug, Clone)]
pub struct ModeState {
    pub mode: ChannelMode,
    pub web_cookie_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeRejection {
    Stopping,
    Updating(String),
    WebModeActive,
    WebModeInactive,
}

impl ModeRejection {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Stopping | Self::Updating(_) | Self::WebModeActive => 409,
            Self::WebModeInactive => 403,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Stopping => "stopping",
            Self::Updating(_) => "updating",
            Self::WebModeActive => "web_mode_active",
            Self::WebModeInactive => "web_mode_inactive",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Stopping => "程序正在退出，请勿继续提交操作",
            Self::Updating(msg) => msg.as_str(),
            Self::WebModeActive => "当前正在使用 Web 端操作，请先在桌面端退出 Web 模式",
            Self::WebModeInactive => "Web 模式尚未开启，请在桌面端打开 Web 界面",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModeControllerConfig {
    pub standalone_web: bool,
    pub first_connect_timeout: Duration,
    pub grace_period: Duration,
    pub ticket_ttl: Duration,
}

impl Default for ModeControllerConfig {
    fn default() -> Self {
        Self {
            standalone_web: false,
            first_connect_timeout: Duration::from_secs(15),
            grace_period: Duration::from_secs(6),
            ticket_ttl: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebOpenResponse {
    pub url: String,
    pub mode: ChannelMode,
}

struct PresenceInner {
    active_app_connections: usize,
    presence_epoch: u64,
    phase: AppPresenceState,
    timer_task: Option<JoinHandle<()>>,
}

/// Guard held during the persistent acceptance critical section of a business mutation.
/// Dropping this guard releases both the `ModeController` read lock and the `ActivityGate` lease.
pub struct WriteAdmissionGuard {
    _mode_guard: OwnedRwLockReadGuard<ModeState>,
    _activity_lease: Option<ActivityLease>,
}

impl WriteAdmissionGuard {
    pub(crate) fn into_parts(self) -> (OwnedRwLockReadGuard<ModeState>, Option<ActivityLease>) {
        (self._mode_guard, self._activity_lease)
    }
}

/// Guard tied to an active `/api/app/events` SSE connection.
pub struct AppConnectionGuard {
    controller: ModeController,
}

impl Drop for AppConnectionGuard {
    fn drop(&mut self) {
        self.controller.on_app_connection_closed();
    }
}

#[derive(Clone)]
pub struct ModeController {
    config: ModeControllerConfig,
    state: Arc<RwLock<ModeState>>,
    presence: Arc<Mutex<PresenceInner>>,
    tickets: Arc<Mutex<HashMap<String, Instant>>>,
    cookie_secret: Arc<[u8; 32]>,
    stopping: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
}

impl ModeController {
    pub fn new(config: ModeControllerConfig) -> Self {
        let initial_mode = if config.standalone_web {
            ChannelMode::WebActive
        } else {
            ChannelMode::AppActive
        };
        let initial_phase = if config.standalone_web {
            AppPresenceState::Connected
        } else {
            AppPresenceState::WaitingFirstConnect
        };
        let mut secret = [0u8; 32];
        rand::rng().fill_bytes(&mut secret);

        let controller = Self {
            config: config.clone(),
            state: Arc::new(RwLock::new(ModeState {
                mode: initial_mode,
                web_cookie_epoch: 1,
            })),
            presence: Arc::new(Mutex::new(PresenceInner {
                active_app_connections: 0,
                presence_epoch: 0,
                phase: initial_phase,
                timer_task: None,
            })),
            tickets: Arc::new(Mutex::new(HashMap::new())),
            cookie_secret: Arc::new(secret),
            stopping: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
        };

        if !config.standalone_web {
            controller.spawn_first_connect_timer(0, config.first_connect_timeout);
        }

        controller
    }

    pub fn is_standalone_web(&self) -> bool {
        self.config.standalone_web
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn shutdown_notified(&self) -> &Arc<Notify> {
        &self.shutdown_notify
    }

    pub fn presence_state(&self) -> AppPresenceState {
        self.presence.lock().expect("presence lock").phase
    }

    pub fn active_app_connections(&self) -> usize {
        self.presence
            .lock()
            .expect("presence lock")
            .active_app_connections
    }

    pub fn presence_epoch(&self) -> u64 {
        self.presence.lock().expect("presence lock").presence_epoch
    }

    pub async fn mode_snapshot(&self) -> ModeState {
        self.state.read().await.clone()
    }

    /// Registers a new `/api/app/events` SSE connection.
    /// Increments `active_app_connections` and `presence_epoch`, cancels any grace/first-connect timer,
    /// and strictly preserves the current `ChannelMode`.
    pub fn register_app_connection(&self) -> Result<AppConnectionGuard, ModeRejection> {
        let mut inner = self.presence.lock().expect("presence lock");
        if self.is_stopping() || inner.phase == AppPresenceState::ShuttingDown {
            return Err(ModeRejection::Stopping);
        }
        if let Some(task) = inner.timer_task.take() {
            task.abort();
        }
        inner.active_app_connections += 1;
        inner.presence_epoch = inner.presence_epoch.wrapping_add(1);
        inner.phase = AppPresenceState::Connected;
        drop(inner);
        Ok(AppConnectionGuard {
            controller: self.clone(),
        })
    }

    fn on_app_connection_closed(&self) {
        let mut inner = self.presence.lock().expect("presence lock");
        if inner.active_app_connections == 0 {
            return;
        }
        inner.active_app_connections -= 1;
        if inner.active_app_connections > 0
            || self.config.standalone_web
            || inner.phase == AppPresenceState::ShuttingDown
            || self.is_stopping()
        {
            return;
        }
        if let Some(task) = inner.timer_task.take() {
            task.abort();
        }
        inner.presence_epoch = inner.presence_epoch.wrapping_add(1);
        let epoch = inner.presence_epoch;
        inner.phase = AppPresenceState::GracePeriod { epoch };
        let grace = self.config.grace_period;
        let controller = self.clone();
        inner.timer_task = Some(tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            controller.expire_grace_period(epoch);
        }));
    }

    fn spawn_first_connect_timer(&self, expected_epoch: u64, timeout: Duration) {
        let controller = self.clone();
        let mut inner = self.presence.lock().expect("presence lock");
        inner.timer_task = Some(tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            controller.expire_first_connect(expected_epoch);
        }));
    }

    fn expire_first_connect(&self, expected_epoch: u64) {
        let mut inner = self.presence.lock().expect("presence lock");
        if inner.phase == AppPresenceState::WaitingFirstConnect
            && inner.presence_epoch == expected_epoch
            && inner.active_app_connections == 0
        {
            inner.phase = AppPresenceState::ShuttingDown;
            inner.timer_task = None;
            self.stopping.store(true, Ordering::SeqCst);
            self.shutdown_notify.notify_waiters();
        }
    }

    fn expire_grace_period(&self, expected_epoch: u64) {
        let mut inner = self.presence.lock().expect("presence lock");
        if inner.phase
            == (AppPresenceState::GracePeriod {
                epoch: expected_epoch,
            })
            && inner.presence_epoch == expected_epoch
            && inner.active_app_connections == 0
        {
            inner.phase = AppPresenceState::ShuttingDown;
            inner.timer_task = None;
            self.stopping.store(true, Ordering::SeqCst);
            self.shutdown_notify.notify_waiters();
        }
    }

    /// Explicitly initiates shutdown (e.g., `POST /api/app/exit` or `POST /api/lifecycle/exit`),
    /// bypassing any disconnect grace period.
    pub fn begin_shutdown(&self) -> bool {
        let mut inner = self.presence.lock().expect("presence lock");
        if let Some(task) = inner.timer_task.take() {
            task.abort();
        }
        inner.phase = AppPresenceState::ShuttingDown;
        let was_stopping = self.stopping.swap(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        !was_stopping
    }

    /// Switches `ChannelMode` to `WebActive` under the exclusive write lock and issues a 30s single-use ticket.
    pub async fn open_web(&self, port: u16) -> Result<(WebOpenResponse, String), ModeRejection> {
        let mut guard = self.state.write().await;
        if self.is_stopping() {
            return Err(ModeRejection::Stopping);
        }
        guard.mode = ChannelMode::WebActive;
        let ticket = crate::discovery::generate_random_hex_token();
        let now = Instant::now();
        {
            let mut tickets = self.tickets.lock().expect("tickets lock");
            tickets.retain(|_, exp| *exp > now);
            tickets.insert(ticket.clone(), now + self.config.ticket_ttl);
        }
        let url = format!("http://127.0.0.1:{port}/?ticket={ticket}");
        Ok((
            WebOpenResponse {
                url,
                mode: ChannelMode::WebActive,
            },
            ticket,
        ))
    }

    /// Switches `ChannelMode` back to `AppActive` under the exclusive write lock,
    /// increments `web_cookie_epoch` to invalidate browser cookies, and clears pending tickets.
    pub async fn close_web(&self) -> Result<u64, ModeRejection> {
        let mut guard = self.state.write().await;
        if self.is_stopping() {
            return Err(ModeRejection::Stopping);
        }
        guard.mode = ChannelMode::AppActive;
        guard.web_cookie_epoch = guard.web_cookie_epoch.wrapping_add(1);
        let new_epoch = guard.web_cookie_epoch;
        self.tickets.lock().expect("tickets lock").clear();
        Ok(new_epoch)
    }

    /// Redeems a single-use launch ticket into a signed session cookie value bound to the current `web_cookie_epoch`.
    pub async fn redeem_ticket(&self, ticket: &str) -> Option<String> {
        if self.is_stopping() {
            return None;
        }
        let state = self.state.read().await;
        if state.mode != ChannelMode::WebActive {
            return None;
        }
        let now = Instant::now();
        let valid = {
            let mut tickets = self.tickets.lock().expect("tickets lock");
            tickets.retain(|_, exp| *exp > now);
            tickets.remove(ticket).is_some_and(|exp| exp > now)
        };
        if !valid {
            return None;
        }
        Some(self.sign_cookie_for_epoch(state.web_cookie_epoch))
    }

    pub async fn issue_cookie_for_current_epoch(&self) -> String {
        let state = self.state.read().await;
        self.sign_cookie_for_epoch(state.web_cookie_epoch)
    }

    pub fn sign_cookie_for_epoch(&self, epoch: u64) -> String {
        let mac = self.compute_epoch_mac(epoch);
        format!("{epoch}:{mac}")
    }

    pub fn verify_cookie(&self, raw_cookie: &str) -> Option<u64> {
        let (epoch_str, mac_hex) = raw_cookie.split_once(':')?;
        let epoch: u64 = epoch_str.parse().ok()?;
        let expected = self.compute_epoch_mac(epoch);
        if bool::from(mac_hex.as_bytes().ct_eq(expected.as_bytes())) {
            Some(epoch)
        } else {
            None
        }
    }

    fn compute_epoch_mac(&self, epoch: u64) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.cookie_secret.as_slice());
        hasher.update(b":cst-web-cookie:");
        hasher.update(epoch.to_be_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Entry-level read check before acquiring business locks.
    pub async fn verify_caller_access(
        &self,
        caller: CallerIdentity,
        is_mutation: bool,
        allow_app_readonly_in_web: bool,
    ) -> Result<(), ModeRejection> {
        if self.is_stopping() && is_mutation {
            return Err(ModeRejection::Stopping);
        }
        let state = self.state.read().await;
        Self::check_caller_access(&state, caller, is_mutation, allow_app_readonly_in_web)
    }

    fn check_caller_access(
        state: &ModeState,
        caller: CallerIdentity,
        is_mutation: bool,
        allow_app_readonly_in_web: bool,
    ) -> Result<(), ModeRejection> {
        match caller {
            CallerIdentity::App => {
                if state.mode == ChannelMode::WebActive
                    && (is_mutation || !allow_app_readonly_in_web)
                {
                    return Err(ModeRejection::WebModeActive);
                }
                Ok(())
            }
            CallerIdentity::Web { cookie_epoch } => {
                if state.mode != ChannelMode::WebActive || cookie_epoch != state.web_cookie_epoch {
                    return Err(ModeRejection::WebModeInactive);
                }
                Ok(())
            }
        }
    }

    /// Holds channel admission only while the caller synchronously registers an event stream.
    pub async fn admit_events(
        &self,
        caller: CallerIdentity,
        allow_app_readonly_in_web: bool,
    ) -> Result<OwnedRwLockReadGuard<ModeState>, ModeRejection> {
        let guard = self.state.clone().read_owned().await;
        if self.is_stopping() {
            return Err(ModeRejection::Stopping);
        }
        Self::check_caller_access(&guard, caller, false, allow_app_readonly_in_web)?;
        Ok(guard)
    }

    /// Second check inside `KeyedLocks` for persistent acceptance critical sections:
    /// holds the shared `RwLock` read guard AND an optional `ActivityGate` lease.
    pub async fn admit_write(
        &self,
        caller: CallerIdentity,
        gate: Option<&Arc<ActivityGate>>,
    ) -> Result<WriteAdmissionGuard, ModeRejection> {
        let mode_guard = self.state.clone().read_owned().await;
        if self.is_stopping() {
            return Err(ModeRejection::Stopping);
        }
        match caller {
            CallerIdentity::App => {
                if mode_guard.mode != ChannelMode::AppActive {
                    return Err(ModeRejection::WebModeActive);
                }
            }
            CallerIdentity::Web { cookie_epoch } => {
                if mode_guard.mode != ChannelMode::WebActive
                    || cookie_epoch != mode_guard.web_cookie_epoch
                {
                    return Err(ModeRejection::WebModeInactive);
                }
            }
        }
        let lease = match gate {
            Some(g) => Some(g.enter().map_err(ModeRejection::Updating)?),
            None => None,
        };
        Ok(WriteAdmissionGuard {
            _mode_guard: mode_guard,
            _activity_lease: lease,
        })
    }
}
