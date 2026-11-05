use futures_util::FutureExt;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static RUNTIME_HOOK: usize;
    static TRANSITION_CANCEL: CancellationToken;
}
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{UserContent, UserContentBlock};
use pi_session::{
    AgentSession, NewSessionOptions, ReplacedSessionContext, SessionBeforeEventOutput,
    SessionEntry, SessionLifecycleEvent, SessionManager, read_session_header,
};
use thiserror::Error;

use crate::resource_loader::LoadExtensionsResult;
use crate::sdk::{AgentSessionRuntimeDiagnostic, AgentSessionServices, resolve_path};
use crate::skills::resource_path_key;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("File not found: {}", .0.display())]
pub struct SessionImportFileNotFoundError(pub PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Session working directory does not exist: {}", .cwd.display())]
pub struct MissingSessionCwdError {
    pub cwd: PathBuf,
    pub session_file: Option<PathBuf>,
}

pub fn assert_session_cwd_exists(
    session_manager: &SessionManager,
    fallback_cwd: &Path,
) -> Result<(), MissingSessionCwdError> {
    let cwd_str = session_manager.get_cwd();
    let target = if cwd_str.trim().is_empty() {
        fallback_cwd.to_path_buf()
    } else {
        PathBuf::from(cwd_str)
    };
    if !target.is_dir() {
        return Err(MissingSessionCwdError {
            cwd: target,
            session_file: session_manager.get_session_file().map(Path::to_path_buf),
        });
    }
    Ok(())
}

#[derive(Clone)]
pub struct CreateAgentSessionRuntimeResult {
    pub session: AgentSession,
    pub services: AgentSessionServices,
    pub extensions_result: LoadExtensionsResult,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

pub struct CreateRuntimeOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_manager: SessionManager,
    pub session_start_event: Option<(String, Option<PathBuf>)>,
}

pub type CreateAgentSessionRuntimeFactory = Arc<
    dyn Fn(
            CreateRuntimeOptions,
        ) -> BoxFuture<'static, Result<CreateAgentSessionRuntimeResult, String>>
        + Send
        + Sync,
>;

pub type RebindSessionFn =
    Arc<dyn Fn(AgentSession) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
pub type WithReplacedSessionFn =
    Arc<dyn Fn(ReplacedSessionContext) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
pub type SetupSessionManagerFn =
    Arc<dyn Fn(&AgentSession) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchSessionOutcome {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkSessionOutcome {
    pub cancelled: bool,
    pub selected_text: Option<String>,
}

#[derive(Clone, Default)]
pub struct SwitchSessionRuntimeOptions {
    pub cwd_override: Option<String>,
    pub with_session: Option<WithReplacedSessionFn>,
}

#[derive(Clone, Default)]
pub struct NewSessionRuntimeOptions {
    pub parent_session: Option<String>,
    pub setup: Option<SetupSessionManagerFn>,
    pub with_session: Option<WithReplacedSessionFn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ForkPosition {
    #[default]
    Before,
    At,
}

impl ForkPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            ForkPosition::Before => "before",
            ForkPosition::At => "at",
        }
    }
}

#[derive(Clone, Default)]
pub struct ForkSessionRuntimeOptions {
    pub position: Option<ForkPosition>,
    pub with_session: Option<WithReplacedSessionFn>,
}

#[derive(Clone)]
struct ActiveRuntimeSlot {
    session: AgentSession,
    services: AgentSessionServices,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecoverableRuntimeFailure {
    pub last_error: String,
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_dir: PathBuf,
    pub previous_session_file: Option<PathBuf>,
    pub was_persisted: bool,
}

#[derive(Clone)]
enum RuntimeSlotState {
    Active(Box<ActiveRuntimeSlot>),
    Failed(RecoverableRuntimeFailure),
    Disposed,
}

struct AgentSessionRuntimeInner {
    slot: RwLock<RuntimeSlotState>,
    create_runtime: CreateAgentSessionRuntimeFactory,
    transition_lock: tokio::sync::Mutex<()>,
    disposing: AtomicBool,
    next_transition: AtomicU64,
    transitions: Mutex<BTreeMap<u64, CancellationToken>>,
    rebind_session: Mutex<Option<RebindSessionFn>>,
    before_session_invalidate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

/// 跨会话生命周期宿主控制器（对照 `AgentSessionRuntime`）。
#[derive(Clone)]
pub struct AgentSessionRuntime {
    inner: Arc<AgentSessionRuntimeInner>,
}

impl AgentSessionRuntime {
    pub fn new(
        initial: CreateAgentSessionRuntimeResult,
        create_runtime: CreateAgentSessionRuntimeFactory,
    ) -> Self {
        Self {
            inner: Arc::new(AgentSessionRuntimeInner {
                slot: RwLock::new(RuntimeSlotState::Active(Box::new(ActiveRuntimeSlot {
                    session: initial.session,
                    services: initial.services,
                    diagnostics: initial.diagnostics,
                    model_fallback_message: initial.model_fallback_message,
                }))),
                create_runtime,
                transition_lock: tokio::sync::Mutex::new(()),
                disposing: AtomicBool::new(false),
                next_transition: AtomicU64::new(1),
                transitions: Mutex::new(BTreeMap::new()),
                rebind_session: Mutex::new(None),
                before_session_invalidate: Mutex::new(None),
            }),
        }
    }

    pub fn try_session(&self) -> Result<AgentSession, String> {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => Ok(slot.session.clone()),
            RuntimeSlotState::Failed(fail) => Err(format!(
                "Runtime is in a failed state after session replacement error: {}",
                fail.last_error
            )),
            RuntimeSlotState::Disposed => Err("AgentSessionRuntime has been disposed".to_owned()),
        }
    }

    pub fn session(&self) -> AgentSession {
        self.try_session()
            .expect("AgentSessionRuntime has an active session")
    }

    pub fn try_services(&self) -> Result<AgentSessionServices, String> {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => Ok(slot.services.clone()),
            RuntimeSlotState::Failed(fail) => {
                Err(format!("Runtime is in a failed state: {}", fail.last_error))
            }
            RuntimeSlotState::Disposed => Err("AgentSessionRuntime has been disposed".to_owned()),
        }
    }

    pub fn services(&self) -> AgentSessionServices {
        self.try_services()
            .expect("AgentSessionRuntime has active services")
    }

    pub fn is_failed(&self) -> bool {
        matches!(
            *self.inner.slot.read().expect("slot lock"),
            RuntimeSlotState::Failed(_)
        )
    }

    pub fn is_disposed(&self) -> bool {
        matches!(
            *self.inner.slot.read().expect("slot lock"),
            RuntimeSlotState::Disposed
        )
    }

    pub fn recovery_info(&self) -> Option<RecoverableRuntimeFailure> {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Failed(f) => Some(f.clone()),
            _ => None,
        }
    }

    pub fn cwd(&self) -> PathBuf {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => slot.services.cwd.clone(),
            RuntimeSlotState::Failed(fail) => fail.cwd.clone(),
            RuntimeSlotState::Disposed => PathBuf::from("."),
        }
    }

    pub fn agent_dir(&self) -> PathBuf {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => slot.services.agent_dir.clone(),
            RuntimeSlotState::Failed(fail) => fail.agent_dir.clone(),
            RuntimeSlotState::Disposed => PathBuf::from("."),
        }
    }

    pub fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic> {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => slot.diagnostics.clone(),
            _ => Vec::new(),
        }
    }

    pub fn model_fallback_message(&self) -> Option<String> {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => slot.model_fallback_message.clone(),
            _ => None,
        }
    }

    pub fn set_rebind_session(&self, cb: Option<RebindSessionFn>) {
        *self.inner.rebind_session.lock().expect("rebind lock") = cb;
    }

    pub fn set_before_session_invalidate(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock") = cb.clone();
        if let RuntimeSlotState::Active(slot) = &*self.inner.slot.read().expect("slot lock") {
            slot.session.set_before_session_invalidate(cb);
        }
    }

    fn ensure_not_reentrant(&self) -> Result<(), String> {
        let id = Arc::as_ptr(&self.inner) as usize;
        if RUNTIME_HOOK
            .try_with(|current| *current == id)
            .unwrap_or(false)
            || self.try_session().is_ok_and(|s| s.is_in_callback_context())
        {
            return Err(
                "Recursive session replacement inside a session transition hook is not allowed"
                    .to_owned(),
            );
        }
        Ok(())
    }

    fn ensure_not_reentrant_or_disposed(&self) -> Result<(), String> {
        self.ensure_not_reentrant()?;
        if self.is_disposed() || self.inner.disposing.load(Ordering::SeqCst) {
            return Err("AgentSessionRuntime has been disposed".to_owned());
        }
        Ok(())
    }

    /// Cancel pending before/factory stages. Store teardown and started finalizers still settle.
    pub fn request_cancel_transition(&self) {
        for token in self
            .inner
            .transitions
            .lock()
            .expect("transitions lock")
            .values()
        {
            token.cancel();
        }
    }

    pub(crate) fn prepare_dispose(&self) {
        let transitions = self.inner.transitions.lock().expect("transitions lock");
        self.inner.disposing.store(true, Ordering::SeqCst);
        for token in transitions.values() {
            token.cancel();
        }
    }

    async fn spawn_transition<T, F, Fut>(&self, dispose: bool, work: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        self.ensure_not_reentrant()?;
        let token = CancellationToken::new();
        let id = self.inner.next_transition.fetch_add(1, Ordering::SeqCst);
        {
            let mut transitions = self.inner.transitions.lock().expect("transitions lock");
            if dispose {
                self.inner.disposing.store(true, Ordering::SeqCst);
                for token in transitions.values() {
                    token.cancel();
                }
            } else {
                self.ensure_not_reentrant_or_disposed()?;
            }
            transitions.insert(id, token.clone());
        }
        let host = self.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _registration = TransitionRegistration {
                host: host.clone(),
                id,
            };
            let result = AssertUnwindSafe(TRANSITION_CANCEL.scope(token, work(host)))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("Session transition task panicked".to_owned()));
            let _ = sender.send(result);
        });
        receiver
            .await
            .map_err(|_| "Session transition task stopped unexpectedly".to_owned())?
    }

    async fn lock_transition(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
        let cancel = TRANSITION_CANCEL.with(Clone::clone);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err("Session transition cancelled".to_owned()),
            guard = self.inner.transition_lock.lock() => Ok(guard),
        }
    }

    async fn cancellable_hook<T>(&self, future: impl Future<Output = T>) -> Result<T, String> {
        let cancel = TRANSITION_CANCEL.with(Clone::clone);
        let id = Arc::as_ptr(&self.inner) as usize;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err("Session transition cancelled".to_owned()),
            result = RUNTIME_HOOK.scope(id, future) => Ok(result),
        }
    }

    async fn create_target(
        &self,
        options: CreateRuntimeOptions,
    ) -> Result<CreateAgentSessionRuntimeResult, String> {
        self.cancellable_hook((self.inner.create_runtime)(options))
            .await?
    }

    fn preflight_execution_log(&self, target: &mut SessionManager) -> Result<(), String> {
        let _log = match self.try_session() {
            Ok(session) => session.prepare_replacement_execution_log(target),
            Err(_) => AgentSession::default_execution_log(target).and_then(|mut log| {
                pi_session::reconcile_on_startup(target, &mut log, &BTreeMap::new())?;
                Ok(log)
            }),
        }
        .map_err(|err| format!("Failed to preflight target execution log: {err}"))?;
        Ok(())
    }

    async fn emit_before_switch(
        &self,
        reason: &str,
        target_session_file: Option<PathBuf>,
    ) -> Result<bool, String> {
        let active_session = match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => Some(slot.session.clone()),
            RuntimeSlotState::Failed(_) => None,
            RuntimeSlotState::Disposed => {
                return Err("AgentSessionRuntime has been disposed".to_owned());
            }
        };
        let Some(session) = active_session else {
            return Ok(false);
        };

        let out = self
            .cancellable_hook(session.extension_runner().emit_lifecycle(
                SessionLifecycleEvent::SessionBeforeSwitch {
                    reason: reason.to_owned(),
                    target_session_file,
                },
            ))
            .await?;

        Ok(matches!(out, SessionBeforeEventOutput::Switch(r) if r.cancel))
    }

    async fn emit_before_fork(
        &self,
        entry_id: &str,
        position: ForkPosition,
    ) -> Result<bool, String> {
        let session = self.try_session()?;
        let out = self
            .cancellable_hook(session.extension_runner().emit_lifecycle(
                SessionLifecycleEvent::SessionBeforeFork {
                    entry_id: entry_id.to_owned(),
                    position: position.as_str().to_owned(),
                },
            ))
            .await?;

        Ok(matches!(out, SessionBeforeEventOutput::Fork(r) if r.cancel))
    }

    fn snapshot_metadata(&self) -> (PathBuf, PathBuf, PathBuf, Option<PathBuf>, bool) {
        match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => {
                let (sdir, sfile, persisted) = slot.session.with_session_manager(|sm| {
                    (
                        sm.get_session_dir().to_path_buf(),
                        sm.get_session_file().map(Path::to_path_buf),
                        sm.is_persisted(),
                    )
                });
                (
                    slot.services.cwd.clone(),
                    slot.services.agent_dir.clone(),
                    sdir,
                    sfile,
                    persisted,
                )
            }
            RuntimeSlotState::Failed(fail) => (
                fail.cwd.clone(),
                fail.agent_dir.clone(),
                fail.session_dir.clone(),
                fail.previous_session_file.clone(),
                fail.was_persisted,
            ),
            RuntimeSlotState::Disposed => (
                PathBuf::from("."),
                PathBuf::from("."),
                PathBuf::from("."),
                None,
                false,
            ),
        }
    }

    async fn teardown_current(
        &self,
        reason: &str,
        target_session_file: Option<PathBuf>,
    ) -> Result<(), String> {
        let active_session = match &*self.inner.slot.read().expect("slot lock") {
            RuntimeSlotState::Active(slot) => Some(slot.session.clone()),
            _ => None,
        };
        let Some(session) = active_session else {
            return Ok(());
        };
        let inv_cb = self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock")
            .clone();
        session.set_before_session_invalidate(inv_cb);

        let (cwd, agent_dir, session_dir, previous_file, persisted) = self.snapshot_metadata();
        let res = session
            .shutdown_with_reason(reason, target_session_file)
            .await;
        self.commit_failed(
            res.clone()
                .err()
                .unwrap_or_else(|| "Session replacement in progress".to_owned()),
            cwd,
            agent_dir,
            session_dir,
            previous_file,
            persisted,
        );
        res
    }

    fn commit_active(&self, result: CreateAgentSessionRuntimeResult) -> AgentSession {
        let inv_cb = self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock")
            .clone();
        result.session.set_before_session_invalidate(inv_cb);
        let new_session = result.session.clone();
        *self.inner.slot.write().expect("slot lock") =
            RuntimeSlotState::Active(Box::new(ActiveRuntimeSlot {
                session: result.session,
                services: result.services,
                diagnostics: result.diagnostics,
                model_fallback_message: result.model_fallback_message,
            }));
        new_session
    }

    fn commit_failed(
        &self,
        err: String,
        cwd: PathBuf,
        agent_dir: PathBuf,
        session_dir: PathBuf,
        previous_session_file: Option<PathBuf>,
        was_persisted: bool,
    ) {
        *self.inner.slot.write().expect("slot lock") =
            RuntimeSlotState::Failed(RecoverableRuntimeFailure {
                last_error: err,
                cwd,
                agent_dir,
                session_dir,
                previous_session_file,
                was_persisted,
            });
    }

    async fn run_post_commit_callbacks(
        &self,
        session: AgentSession,
        with_session: Option<WithReplacedSessionFn>,
    ) -> Result<(), String> {
        let rebind = self
            .inner
            .rebind_session
            .lock()
            .expect("rebind lock")
            .clone();
        let cancel = TRANSITION_CANCEL.with(Clone::clone);
        if let Some(rebind_fn) = rebind {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err("Session transition callback cancelled".to_owned()),
                result = rebind_fn(session.clone()) => result?,
            }
        }
        if let Some(with_fn) = with_session {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err("Session transition callback cancelled".to_owned()),
                result = with_fn(session.create_replaced_session_context()) => result?,
            }
        }
        Ok(())
    }

    pub async fn switch_session(
        &self,
        session_path: impl AsRef<Path>,
        options: Option<SwitchSessionRuntimeOptions>,
    ) -> Result<SwitchSessionOutcome, String> {
        let path = session_path.as_ref().to_path_buf();
        self.spawn_transition(false, move |host| async move {
            host.switch_session_inner(path, options).await
        })
        .await
    }

    async fn switch_session_inner(
        &self,
        session_path: impl AsRef<Path>,
        options: Option<SwitchSessionRuntimeOptions>,
    ) -> Result<SwitchSessionOutcome, String> {
        self.ensure_not_reentrant_or_disposed()?;
        let _lock = self.lock_transition().await?;
        self.ensure_not_reentrant_or_disposed()?;

        let opts = options.unwrap_or_default();
        let (current_cwd, agent_dir, session_dir, previous_session_file, was_persisted) =
            self.snapshot_metadata();
        let target_path = resolve_path(session_path.as_ref(), Some(&current_cwd));

        if self
            .emit_before_switch("resume", Some(target_path.clone()))
            .await?
        {
            return Ok(SwitchSessionOutcome { cancelled: true });
        }

        let is_same_file = previous_session_file
            .as_ref()
            .is_some_and(|prev| resource_path_key(prev) == resource_path_key(&target_path));

        let new_session = if !is_same_file {
            // 跨文件切换：在 teardown_current 之前完成目标文件打开、加锁、正文解析、cwd 校验与执行日志恢复核对预检
            if !target_path.is_file() {
                return Err(format!("Session file not found: {}", target_path.display()));
            }
            let mut target_sm =
                SessionManager::open_existing(&target_path, None, opts.cwd_override.clone())
                    .map_err(|e| e.to_string())?;
            if let Err(err) = assert_session_cwd_exists(&target_sm, &current_cwd) {
                target_sm.release_persistent_store();
                return Err(err.to_string());
            }
            self.preflight_execution_log(&mut target_sm)?;
            let target_cwd = PathBuf::from(target_sm.get_cwd());

            // 拆解旧会话并释放旧锁
            if let Err(err) = self
                .teardown_current("resume", Some(target_path.clone()))
                .await
            {
                target_sm.release_persistent_store();
                self.commit_failed(
                    err.clone(),
                    current_cwd,
                    agent_dir,
                    session_dir,
                    previous_session_file,
                    was_persisted,
                );
                return Err(err);
            }

            let created = self
                .create_target(CreateRuntimeOptions {
                    cwd: target_cwd.clone(),
                    agent_dir: agent_dir.clone(),
                    session_manager: target_sm,
                    session_start_event: Some(("resume".to_owned(), previous_session_file.clone())),
                })
                .await;

            match created {
                Ok(res) => self.commit_active(res),
                Err(err) => {
                    self.commit_failed(
                        err.clone(),
                        target_cwd,
                        agent_dir,
                        session_dir,
                        previous_session_file,
                        was_persisted,
                    );
                    return Err(err);
                }
            }
        } else {
            // 同文件重开分支：先做无锁头部与 cwd 预检，再结算释放旧锁后重开
            if !target_path.is_file() {
                return Err(format!("Session file not found: {}", target_path.display()));
            }
            let header = read_session_header(&target_path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    format!(
                        "Session file is not a valid pi session: {}",
                        target_path.display()
                    )
                })?;
            let effective_cwd = opts
                .cwd_override
                .clone()
                .unwrap_or_else(|| header.cwd.clone());
            let target_cwd = if effective_cwd.trim().is_empty() {
                current_cwd.clone()
            } else {
                PathBuf::from(&effective_cwd)
            };
            if !target_cwd.is_dir() {
                return Err(MissingSessionCwdError {
                    cwd: target_cwd,
                    session_file: Some(target_path),
                }
                .to_string());
            }

            if let Err(err) = self
                .teardown_current("resume", Some(target_path.clone()))
                .await
            {
                self.commit_failed(
                    err.clone(),
                    current_cwd,
                    agent_dir,
                    session_dir,
                    previous_session_file,
                    was_persisted,
                );
                return Err(err);
            }

            let target_sm =
                match SessionManager::open_existing(&target_path, None, opts.cwd_override) {
                    Ok(sm) => sm,
                    Err(e) => {
                        let err = e.to_string();
                        self.commit_failed(
                            err.clone(),
                            target_cwd,
                            agent_dir,
                            session_dir,
                            previous_session_file,
                            was_persisted,
                        );
                        return Err(err);
                    }
                };
            let created = self
                .create_target(CreateRuntimeOptions {
                    cwd: target_cwd.clone(),
                    agent_dir: agent_dir.clone(),
                    session_manager: target_sm,
                    session_start_event: Some(("resume".to_owned(), previous_session_file.clone())),
                })
                .await;

            match created {
                Ok(res) => self.commit_active(res),
                Err(err) => {
                    self.commit_failed(
                        err.clone(),
                        target_cwd,
                        agent_dir,
                        session_dir,
                        previous_session_file,
                        was_persisted,
                    );
                    return Err(err);
                }
            }
        };

        // 释放转换锁后再执行可重入的宿主回调
        drop(_lock);
        self.run_post_commit_callbacks(new_session, opts.with_session)
            .await?;
        Ok(SwitchSessionOutcome { cancelled: false })
    }

    pub async fn new_session(
        &self,
        options: Option<NewSessionRuntimeOptions>,
    ) -> Result<SwitchSessionOutcome, String> {
        self.spawn_transition(false, move |host| async move {
            host.new_session_inner(options).await
        })
        .await
    }

    async fn new_session_inner(
        &self,
        options: Option<NewSessionRuntimeOptions>,
    ) -> Result<SwitchSessionOutcome, String> {
        self.ensure_not_reentrant_or_disposed()?;
        let _lock = self.lock_transition().await?;
        self.ensure_not_reentrant_or_disposed()?;

        let opts = options.unwrap_or_default();
        if self.emit_before_switch("new", None).await? {
            return Ok(SwitchSessionOutcome { cancelled: true });
        }

        let (cwd, agent_dir, session_dir, previous_session_file, was_persisted) =
            self.snapshot_metadata();
        let cwd_str = cwd.to_string_lossy().into_owned();
        let new_sm_opts = opts.parent_session.map(|p| NewSessionOptions {
            id: None,
            parent_session: Some(p),
        });

        let mut new_sm = if was_persisted {
            SessionManager::create(&cwd_str, &session_dir, new_sm_opts)
                .map_err(|e| e.to_string())?
        } else {
            SessionManager::in_memory(&cwd_str, new_sm_opts, None).map_err(|e| e.to_string())?
        };
        let target_file = new_sm.get_session_file().map(Path::to_path_buf);

        if let Err(err) = self.teardown_current("new", target_file).await {
            new_sm.release_persistent_store();
            self.commit_failed(
                err.clone(),
                cwd,
                agent_dir,
                session_dir,
                previous_session_file,
                was_persisted,
            );
            return Err(err);
        }

        let created = self
            .create_target(CreateRuntimeOptions {
                cwd: cwd.clone(),
                agent_dir: agent_dir.clone(),
                session_manager: new_sm,
                session_start_event: Some(("new".to_owned(), previous_session_file.clone())),
            })
            .await;

        let new_session = match created {
            Ok(res) => self.commit_active(res),
            Err(err) => {
                self.commit_failed(
                    err.clone(),
                    cwd,
                    agent_dir,
                    session_dir,
                    previous_session_file,
                    was_persisted,
                );
                return Err(err);
            }
        };

        drop(_lock);
        if let Some(setup_fn) = opts.setup {
            let cancel = TRANSITION_CANCEL.with(Clone::clone);
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err("Session setup cancelled".to_owned()),
                result = setup_fn(&new_session) => result?,
            }
            new_session.refresh_finalized_context();
        }

        self.run_post_commit_callbacks(new_session, opts.with_session)
            .await?;
        Ok(SwitchSessionOutcome { cancelled: false })
    }

    pub async fn fork(
        &self,
        entry_id: &str,
        options: Option<ForkSessionRuntimeOptions>,
    ) -> Result<ForkSessionOutcome, String> {
        let entry_id = entry_id.to_owned();
        self.spawn_transition(false, move |host| async move {
            host.fork_inner(&entry_id, options).await
        })
        .await
    }

    async fn fork_inner(
        &self,
        entry_id: &str,
        options: Option<ForkSessionRuntimeOptions>,
    ) -> Result<ForkSessionOutcome, String> {
        self.ensure_not_reentrant_or_disposed()?;
        let _lock = self.lock_transition().await?;
        self.ensure_not_reentrant_or_disposed()?;

        let opts = options.unwrap_or_default();
        let position = opts.position.unwrap_or(ForkPosition::Before);
        let active_session = self.try_session()?;

        if self.emit_before_fork(entry_id, position).await? {
            return Ok(ForkSessionOutcome {
                cancelled: true,
                selected_text: None,
            });
        }

        // 先校验 entry_id 与角色，且若为持久化会话非根分叉则校验文件已落盘
        let pre_entry = active_session
            .with_session_manager(|sm| sm.get_entry(entry_id).cloned())
            .ok_or_else(|| "Invalid entry ID for forking".to_owned())?;

        if position == ForkPosition::Before {
            match &pre_entry {
                SessionEntry::Message(m) if m.message.role() == "user" => {}
                _ => return Err("Invalid entry ID for forking".to_owned()),
            }
        }

        let (cwd, agent_dir, session_dir, previous_session_file, was_persisted) =
            self.snapshot_metadata();

        if was_persisted {
            let current_file = previous_session_file
                .as_ref()
                .ok_or_else(|| "Persisted session is missing a session file".to_owned())?;
            let preliminary_leaf = match position {
                ForkPosition::At => Some(pre_entry.id().to_owned()),
                ForkPosition::Before => pre_entry.parent_id().map(ToOwned::to_owned),
            };
            if preliminary_leaf.is_some() && !current_file.exists() {
                return Err(
                    "This session has not been saved yet. Send a message before cloning or forking it."
                        .to_owned(),
                );
            }
        }

        // Close first. The retained manager snapshot includes settled tool results but owns no old lock.
        self.teardown_current("fork", None).await?;

        let settled_entry = active_session
            .with_session_manager(|sm| sm.get_entry(entry_id).cloned())
            .ok_or_else(|| "Invalid entry ID for forking".to_owned())?;

        let (target_leaf_id, selected_text) = match position {
            ForkPosition::At => (Some(settled_entry.id().to_owned()), None),
            ForkPosition::Before => {
                let SessionEntry::Message(m) = &settled_entry else {
                    return Err("Invalid entry ID for forking".to_owned());
                };
                if m.message.role() != "user" {
                    return Err("Invalid entry ID for forking".to_owned());
                }
                let text = extract_user_message_text(&m.message);
                (settled_entry.parent_id().map(ToOwned::to_owned), Some(text))
            }
        };

        let forked_sm = match &target_leaf_id {
            Some(leaf_id) => active_session
                .with_session_manager(|sm| sm.fork_branch_to_new_manager(leaf_id))
                .map_err(|e| e.to_string())?,
            None => {
                let cwd_str = cwd.to_string_lossy().into_owned();
                let parent_opt = previous_session_file
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned());
                if was_persisted {
                    SessionManager::create(
                        &cwd_str,
                        &session_dir,
                        Some(NewSessionOptions {
                            id: None,
                            parent_session: parent_opt,
                        }),
                    )
                    .map_err(|e| e.to_string())?
                } else {
                    SessionManager::in_memory(
                        &cwd_str,
                        Some(NewSessionOptions {
                            id: None,
                            parent_session: parent_opt,
                        }),
                        None,
                    )
                    .map_err(|e| e.to_string())?
                }
            }
        };

        let target_cwd = PathBuf::from(forked_sm.get_cwd());
        let created = self
            .create_target(CreateRuntimeOptions {
                cwd: target_cwd.clone(),
                agent_dir: agent_dir.clone(),
                session_manager: forked_sm,
                session_start_event: Some(("fork".to_owned(), previous_session_file.clone())),
            })
            .await;

        let new_session = match created {
            Ok(res) => self.commit_active(res),
            Err(err) => {
                self.commit_failed(
                    err.clone(),
                    target_cwd,
                    agent_dir,
                    session_dir,
                    previous_session_file,
                    was_persisted,
                );
                return Err(err);
            }
        };

        drop(_lock);
        self.run_post_commit_callbacks(new_session, opts.with_session)
            .await?;
        Ok(ForkSessionOutcome {
            cancelled: false,
            selected_text,
        })
    }

    pub async fn import_from_jsonl(
        &self,
        input_path: impl AsRef<Path>,
        cwd_override: Option<String>,
    ) -> Result<SwitchSessionOutcome, String> {
        let path = input_path.as_ref().to_path_buf();
        self.spawn_transition(false, move |host| async move {
            host.import_from_jsonl_inner(path, cwd_override).await
        })
        .await
    }

    async fn import_from_jsonl_inner(
        &self,
        input_path: impl AsRef<Path>,
        cwd_override: Option<String>,
    ) -> Result<SwitchSessionOutcome, String> {
        self.ensure_not_reentrant_or_disposed()?;
        let _lock = self.lock_transition().await?;
        self.ensure_not_reentrant_or_disposed()?;

        let (current_cwd, agent_dir, session_dir, previous_session_file, was_persisted) =
            self.snapshot_metadata();
        let resolved_input = resolve_path(input_path.as_ref(), Some(&current_cwd));
        if !resolved_input.is_file() {
            return Err(SessionImportFileNotFoundError(resolved_input).to_string());
        }

        if !session_dir.as_os_str().is_empty() && !session_dir.exists() {
            fs::create_dir_all(&session_dir).map_err(|e| e.to_string())?;
        }

        let basename = resolved_input
            .file_name()
            .ok_or_else(|| "Invalid import file path".to_owned())?;
        let mut destination_path = session_dir.join(basename);
        let source_already_stored =
            resource_path_key(&destination_path) == resource_path_key(&resolved_input);
        if source_already_stored
            && previous_session_file
                .as_ref()
                .is_some_and(|p| resource_path_key(p) == resource_path_key(&resolved_input))
        {
            drop(_lock);
            return self
                .switch_session_inner(
                    resolved_input,
                    Some(SwitchSessionRuntimeOptions {
                        cwd_override,
                        with_session: None,
                    }),
                )
                .await;
        }

        if !source_already_stored {
            let stem = destination_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "session".to_owned());
            let ext = destination_path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let mut suffix = 1usize;
            while destination_path.exists() {
                destination_path = session_dir.join(format!("{stem}-{suffix}{ext}"));
                suffix += 1;
            }
        }

        if self
            .emit_before_switch("resume", Some(destination_path.clone()))
            .await?
        {
            return Ok(SwitchSessionOutcome { cancelled: true });
        }

        if !source_already_stored {
            // 排他创建目标副本，绝不覆盖已有文件
            let mut src_file = File::open(&resolved_input).map_err(|e| e.to_string())?;
            let stem = resolved_input
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let ext = resolved_input
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let mut suffix = 1usize;
            let mut dst_file = loop {
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination_path)
                {
                    Ok(file) => break file,
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        destination_path = session_dir.join(format!("{stem}-{suffix}{ext}"));
                        suffix += 1;
                    }
                    Err(err) => return Err(err.to_string()),
                }
            };
            if let Err(err) =
                std::io::copy(&mut src_file, &mut dst_file).and_then(|_| dst_file.sync_all())
            {
                drop(dst_file);
                let _ = fs::remove_file(&destination_path);
                return Err(err.to_string());
            }
        }

        let cleanup_copied_file = || {
            if !source_already_stored {
                let _ = fs::remove_file(&destination_path);
            }
        };

        let mut target_sm = match SessionManager::open_existing(
            &destination_path,
            Some(session_dir.clone()),
            cwd_override,
        ) {
            Ok(sm) => sm,
            Err(err) => {
                cleanup_copied_file();
                return Err(err.to_string());
            }
        };

        if let Err(err) = assert_session_cwd_exists(&target_sm, &current_cwd) {
            target_sm.release_persistent_store();
            cleanup_copied_file();
            return Err(err.to_string());
        }

        if let Err(err) = self.preflight_execution_log(&mut target_sm) {
            target_sm.release_persistent_store();
            cleanup_copied_file();
            return Err(err);
        }
        let target_cwd = PathBuf::from(target_sm.get_cwd());
        if let Err(err) = self
            .teardown_current("resume", Some(destination_path.clone()))
            .await
        {
            target_sm.release_persistent_store();
            cleanup_copied_file();
            self.commit_failed(
                err.clone(),
                current_cwd,
                agent_dir,
                session_dir,
                previous_session_file,
                was_persisted,
            );
            return Err(err);
        }

        let created = self
            .create_target(CreateRuntimeOptions {
                cwd: target_cwd.clone(),
                agent_dir: agent_dir.clone(),
                session_manager: target_sm,
                session_start_event: Some(("resume".to_owned(), previous_session_file.clone())),
            })
            .await;

        let new_session = match created {
            Ok(res) => self.commit_active(res),
            Err(err) => {
                self.commit_failed(
                    err.clone(),
                    target_cwd,
                    agent_dir,
                    session_dir,
                    previous_session_file,
                    was_persisted,
                );
                return Err(err);
            }
        };

        drop(_lock);
        self.run_post_commit_callbacks(new_session, None).await?;
        Ok(SwitchSessionOutcome { cancelled: false })
    }

    pub async fn dispose(&self) -> Result<(), String> {
        self.spawn_transition(true, move |host| async move { host.dispose_inner().await })
            .await
    }

    async fn dispose_inner(&self) -> Result<(), String> {
        let _lock = self.inner.transition_lock.lock().await;
        if self.is_disposed() {
            return Ok(());
        }
        let res = self.teardown_current("quit", None).await;
        *self.inner.slot.write().expect("slot lock") = RuntimeSlotState::Disposed;
        self.inner
            .rebind_session
            .lock()
            .expect("rebind lock")
            .take();
        self.inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock")
            .take();
        res
    }
}

struct TransitionRegistration {
    host: AgentSessionRuntime,
    id: u64,
}
impl Drop for TransitionRegistration {
    fn drop(&mut self) {
        self.host
            .inner
            .transitions
            .lock()
            .expect("transitions lock")
            .remove(&self.id);
    }
}

fn extract_user_message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User(u) => match &u.content {
            UserContent::Text(s) => s.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        },
        _ => String::new(),
    }
}

pub async fn create_agent_session_runtime(
    create_runtime: CreateAgentSessionRuntimeFactory,
    options: CreateRuntimeOptions,
) -> Result<AgentSessionRuntime, String> {
    assert_session_cwd_exists(&options.session_manager, &options.cwd).map_err(|e| e.to_string())?;
    let initial = create_runtime(options).await?;
    Ok(AgentSessionRuntime::new(initial, create_runtime))
}
