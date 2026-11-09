use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use futures_util::{
    FutureExt,
    future::{Shared, join_all},
};
use pi_ai::{BoxFuture, CredentialStore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    billing::BillingLookup,
    collect::Collector,
    config::Config,
    delivery::Delivery,
    diagnostics::{Diagnostics, Event},
    draft::Drafts,
    environment::{Environment, EnvironmentSource, NativeEnvironment, SystemProbe},
    heartbeat::HeartbeatRun,
    identity::Installation,
    outbox::Outbox,
    owner::Owner,
    submission::SessionSubmission,
    transport::{FailureKind, NativeTransport, Sender, Transport, UploadAuth},
};
use crate::auth::oa::{Clock, SystemClock};

pub struct TelemetryOptions {
    pub environment: Arc<dyn EnvironmentSource>,
    pub transport: Arc<dyn Transport>,
    pub clock: Arc<dyn Clock>,
    pub kit_version: String,
}
impl Default for TelemetryOptions {
    fn default() -> Self {
        Self {
            environment: Arc::new(NativeEnvironment),
            transport: Arc::new(NativeTransport),
            clock: Arc::new(SystemClock),
            kit_version: String::new(),
        }
    }
}
pub(crate) struct Backend {
    pub environment: Environment,
    pub kit_version: String,
    pub installation: Option<Installation>,
    pub drafts: Drafts,
    pub delivery: Delivery,
    pub diagnostics: Diagnostics,
    pub heartbeat: HeartbeatRun,
}
/// Shared by canonical home. The registry and timer only hold weak references.
pub struct TelemetryService {
    pub(crate) config: Config,
    pub(crate) lookup: Arc<dyn BillingLookup>,
    pub(crate) stop: CancellationToken,
    credentials: Arc<dyn CredentialStore>,
    clock: Arc<dyn Clock>,
    kit_version: String,
    ready: Shared<BoxFuture<'static, Result<Arc<Backend>, String>>>,
    instances: Mutex<BTreeMap<String, Arc<SessionSubmission>>>,
    close_deadline: Mutex<Option<Instant>>,
    wake: Arc<tokio::sync::Notify>,
    recovery: tokio::sync::Mutex<()>,
    last_hint: Mutex<Option<Instant>>,
}
impl TelemetryService {
    pub async fn load(
        root: &Path,
        credentials: Arc<dyn CredentialStore>,
        lookup: Arc<dyn BillingLookup>,
        options: TelemetryOptions,
    ) -> Result<Option<Arc<Self>>, String> {
        let root = root
            .canonicalize()
            .map_err(|_| "遥测工具包目录无法读取。")?;
        let key = if cfg!(windows) {
            PathBuf::from(root.to_string_lossy().to_lowercase())
        } else {
            root.clone()
        };
        static SERVICES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<TelemetryService>>>> =
            OnceLock::new();
        let services = SERVICES.get_or_init(Default::default);
        if let Some(service) = services
            .lock()
            .expect("telemetry homes")
            .get(&key)
            .and_then(Weak::upgrade)
        {
            return Ok(Some(service));
        }
        let path = root.clone();
        let config = tokio::task::spawn_blocking(move || Config::load(&path))
            .await
            .map_err(|_| "遥测配置读取中断。")??;
        let Some(config) = config else {
            return Ok(None);
        };
        let mut registry = services.lock().expect("telemetry homes");
        if let Some(service) = registry.get(&key).and_then(Weak::upgrade) {
            return Ok(Some(service));
        }
        registry.retain(|_, value| value.strong_count() > 0);
        let sender = Sender::new(options.transport, options.clock.clone());
        let version = options.kit_version.clone();
        let ready = async move {
            tokio::task::spawn_blocking(move || {
                let owner = Owner::global()?;
                let installation = Installation::load(&root);
                let environment =
                    Environment::capture(installation.as_ref(), options.environment.as_ref());
                let diagnostics = Diagnostics::new(&root, owner.clone())?;
                let delivery = Delivery::new(
                    Outbox::new(&root, owner.clone())?,
                    sender.clone(),
                    diagnostics.clone(),
                );
                let heartbeat =
                    HeartbeatRun::new(environment.identity.clone(), sender, diagnostics.clone());
                let kit_version = if version.is_empty() {
                    crate::update::layout::read_version(&root).unwrap_or_default()
                } else {
                    version
                };
                Ok(Arc::new(Backend {
                    environment,
                    kit_version,
                    installation,
                    drafts: Drafts::new(&root, owner)?,
                    delivery,
                    diagnostics,
                    heartbeat,
                }))
            })
            .await
            .map_err(|_| "遥测初始化中断。".to_owned())?
        }
        .boxed()
        .shared();
        let service = Arc::new(Self {
            config,
            credentials,
            lookup,
            clock: options.clock,
            kit_version: options.kit_version,
            stop: CancellationToken::new(),
            ready,
            instances: Mutex::default(),
            close_deadline: Mutex::default(),
            wake: Arc::default(),
            recovery: tokio::sync::Mutex::new(()),
            last_hint: Mutex::default(),
        });
        registry.insert(key, Arc::downgrade(&service));
        let weak = Arc::downgrade(&service);
        let stop = service.stop.clone();
        let wake = service.wake.clone();
        let initial = service.clone();
        tokio::spawn(async move {
            let _ = initial.recover().await;
            initial.kick();
        });
        service.kick();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(
                Instant::now() + Duration::from_secs(30),
                Duration::from_secs(30),
            );
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let periodic = tokio::select! { biased; _=stop.cancelled()=>break, _=timer.tick()=>true, _=wake.notified()=>false };
                let Some(service) = weak.upgrade() else {
                    break;
                };
                if stop.is_cancelled() {
                    break;
                }
                if periodic {
                    service.tick();
                } else {
                    tokio::spawn(async move {
                        service.flush(false).await;
                    });
                }
            }
        });
        Ok(Some(service))
    }
    pub(crate) async fn backend(&self) -> Result<Arc<Backend>, String> {
        self.ready.clone().await
    }
    pub(crate) fn now(&self) -> u64 {
        self.clock.now()
    }
    pub(crate) fn kick(&self) {
        self.wake.notify_one();
    }
    pub fn instance(
        self: &Arc<Self>,
        session_id: &str,
        reason: &str,
        channel: &str,
    ) -> Arc<SessionSubmission> {
        struct Empty;
        impl EnvironmentSource for Empty {
            fn probe(&self) -> SystemProbe {
                SystemProbe::default()
            }
        }
        let collector = Collector::new(
            session_id.into(),
            reason.into(),
            channel.into(),
            self.now(),
            Environment::capture(None, &Empty),
            self.kit_version.clone(),
            &self.config,
        );
        let instance = SessionSubmission::new(self, collector);
        self.instances
            .lock()
            .expect("telemetry instances")
            .insert(instance.record_id.clone(), instance.clone());
        instance
    }
    pub(crate) fn finished(&self, record_id: &str, backend: &Backend) {
        self.instances
            .lock()
            .expect("telemetry instances")
            .remove(record_id);
        backend.heartbeat.remove(record_id);
    }
    pub fn active_instances(&self) -> usize {
        self.instances.lock().expect("telemetry instances").len()
    }
    pub async fn run_id(&self) -> Result<String, String> {
        Ok(self.backend().await?.heartbeat.run_id.clone())
    }
    pub(crate) fn activity(self: &Arc<Self>, record_id: &str, busy: Option<bool>) {
        let id = record_id.to_owned();
        let ready = self.ready.clone();
        let service = Arc::downgrade(self);
        tokio::spawn(async move {
            if let Ok(backend) = ready.await
                && let Some(service) = service.upgrade()
            {
                let instances = service.instances.lock().expect("telemetry instances");
                if instances.contains_key(&id) {
                    backend.heartbeat.register(&id);
                    backend.heartbeat.activity(&id, busy);
                }
            }
        });
    }
    pub(crate) async fn storage_failure(&self, backend: &Backend) {
        backend
            .diagnostics
            .record_async(
                Event::Failure {
                    endpoint: None,
                    kind: FailureKind::Storage,
                },
                self.now(),
                CancellationToken::new(),
            )
            .await;
    }
    async fn upload_auth(&self, backend: &Backend) -> Option<UploadAuth> {
        UploadAuth::from_store(
            backend.installation.clone(),
            self.credentials.as_ref(),
            &self.config.auth_provider,
            self.now(),
        )
    }
    pub async fn flush(&self, force_heartbeat: bool) {
        let Ok(backend) = self.backend().await else {
            return;
        };
        let Some(auth) = self.upload_auth(&backend).await else {
            return;
        };
        let heartbeat = backend.heartbeat.clone();
        let endpoints = self.config.endpoints.clone();
        let token = self.stop.child_token();
        let beat_auth = auth.clone();
        tokio::spawn(async move {
            heartbeat
                .pulse(beat_auth, &endpoints, false, force_heartbeat, token)
                .await;
        });
        backend
            .delivery
            .flush(auth, &self.config.endpoints, self.stop.child_token())
            .await;
    }
    async fn recover(&self) -> Result<(), String> {
        let Ok(_guard) = self.recovery.try_lock() else {
            return Ok(());
        };
        let backend = self.backend().await?;
        backend
            .delivery
            .migrate(&self.config.endpoints, self.stop.child_token())
            .await?;
        backend
            .drafts
            .recover(
                |record| {
                    backend
                        .delivery
                        .queue(record, &self.config.endpoints, self.stop.child_token())
                },
                Some(self.lookup.as_ref()),
                self.stop.child_token(),
            )
            .await?;
        Ok(())
    }
    pub fn tick(self: &Arc<Self>) {
        let tasks = self
            .instances
            .lock()
            .expect("telemetry instances")
            .values()
            .map(|s| s.periodic())
            .collect::<Vec<_>>();
        let service = self.clone();
        tokio::spawn(async move {
            let _ = join_all(tasks).await;
            service.flush(false).await;
        });
        let service = self.clone();
        tokio::spawn(async move {
            let _ = service.recover().await;
        });
    }
    /// Page visibility/online hints only retry; they never close a session or disable telemetry.
    pub fn hint(self: &Arc<Self>) {
        let mut last = self.last_hint.lock().expect("telemetry hint");
        if last.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
            return;
        }
        *last = Some(Instant::now());
        drop(last);
        self.tick();
        let service = self.clone();
        tokio::spawn(async move {
            service.flush(true).await;
        });
    }
    /// Real process exit only. Every session shares this first deadline for this home.
    pub fn begin_close(&self) -> Instant {
        *self
            .close_deadline
            .lock()
            .expect("telemetry close")
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(5))
    }
    pub(crate) fn close_or_session_deadline(&self) -> Instant {
        self.close_deadline
            .lock()
            .expect("telemetry close")
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(5))
    }
    pub fn cancel_close(&self) {
        self.close_deadline.lock().expect("telemetry close").take();
    }
    pub async fn close_process(self: &Arc<Self>, reason: &str) {
        let deadline = self.begin_close();
        let tasks = self
            .instances
            .lock()
            .expect("telemetry instances")
            .values()
            .map(|s| s.finalize(reason, None, deadline))
            .collect::<Vec<_>>();
        let _ = tokio::time::timeout_at(deadline, join_all(tasks)).await;
        // Never wait for queue draining at process exit. Closed heartbeat has its own bounded attempt.
        let service = self.clone();
        tokio::spawn(async move {
            if let Ok(backend) = service.backend().await
                && let Some(auth) = service.upload_auth(&backend).await
            {
                backend
                    .heartbeat
                    .pulse(
                        auth,
                        &service.config.endpoints,
                        true,
                        true,
                        CancellationToken::new(),
                    )
                    .await;
            }
        });
        self.stop.cancel();
    }
    pub async fn drain(&self) {
        let instances = self
            .instances
            .lock()
            .expect("telemetry instances")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        join_all(instances.iter().map(|s| s.drain())).await;
    }
}
impl Drop for TelemetryService {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
