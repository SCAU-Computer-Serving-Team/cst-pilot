use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::browser_protocol::emulation::MediaFeature;
use chromiumoxide::cdp::browser_protocol::target::{
    CreateBrowserContextParams, CreateTargetParams,
};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use regex::Regex;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{ServiceError, ServiceErrorKind};

#[derive(Debug, Clone)]
pub struct ExportBrowserOptions {
    pub work_dir: PathBuf,
    pub executable_path: Option<PathBuf>,
    pub idle_ms: u64,
}

impl ExportBrowserOptions {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
            executable_path: None,
            idle_ms: 30_000,
        }
    }
}

pub fn resolve_browser_candidates(explicit: Option<&Path>) -> Result<Vec<PathBuf>, ServiceError> {
    let configured = explicit
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var("CST_EXPORT_BROWSER")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var("CHROME_PATH")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        });

    let candidates: Vec<PathBuf> = if let Some(cfg) = configured {
        vec![cfg]
    } else {
        let mut list = vec![
            PathBuf::from("C:/Program Files/Google/Chrome/Application/chrome.exe"),
            PathBuf::from("C:/Program Files (x86)/Google/Chrome/Application/chrome.exe"),
        ];
        if let Ok(local_app) = std::env::var("LOCALAPPDATA")
            && !local_app.trim().is_empty()
        {
            list.push(PathBuf::from(local_app).join("Google/Chrome/Application/chrome.exe"));
        }
        list.push(PathBuf::from(
            "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
        ));
        list.push(PathBuf::from(
            "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
        ));
        list
    };

    let found: Vec<PathBuf> = candidates.into_iter().filter(|p| p.exists()).collect();
    if found.is_empty() {
        return Err(ServiceError::new(
            ServiceErrorKind::Unavailable,
            "export_browser_missing",
            "导出需要已安装的 Edge 或 Chrome，可用 CST_EXPORT_BROWSER 指定浏览器路径。",
        ));
    }
    Ok(found)
}

pub fn resolve_browser_executable(explicit: Option<&Path>) -> Result<PathBuf, ServiceError> {
    resolve_browser_candidates(explicit).map(|mut v| v.remove(0))
}

fn parse_chromium_major(version_product: &str) -> Option<u32> {
    static VER_RE: OnceLock<Regex> = OnceLock::new();
    let re = VER_RE.get_or_init(|| Regex::new(r"/(\d+)\.").unwrap());
    re.captures(version_product)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u32>().ok())
}

async fn kill_process_tree(pid: Option<u32>) {
    #[cfg(windows)]
    if let Some(pid) = pid {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = tokio::process::Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/t", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .await;
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

async fn remove_dir_retry(path: &Path) {
    for _ in 0..20 {
        if !path.exists() {
            return;
        }
        if tokio::fs::remove_dir_all(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = fs::remove_dir_all(path);
}

pub struct BrowserLease {
    id: u64,
    browser: AsyncMutex<Browser>,
    profile: PathBuf,
    pid: Option<u32>,
    alive: Arc<AtomicBool>,
    handler_task: Mutex<Option<JoinHandle<()>>>,
}

impl BrowserLease {
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub async fn is_connected(&self) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            return false;
        }
        let mut guard = self.browser.lock().await;
        if let Ok(Some(_)) = guard.try_wait() {
            self.alive.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    pub async fn kill_process(&self) {
        self.alive.store(false, Ordering::SeqCst);
        kill_process_tree(self.pid).await;
        let mut guard = self.browser.lock().await;
        let _ = guard.kill().await;
    }

    async fn create_isolated_page(&self) -> Result<(BrowserContextId, Page), ServiceError> {
        let guard = self.browser.lock().await;
        let ctx_id = guard
            .create_browser_context(CreateBrowserContextParams::default())
            .await
            .map_err(|_| {
                self.alive.store(false, Ordering::SeqCst);
                ServiceError::new(
                    ServiceErrorKind::Internal,
                    "export_failed",
                    "无法创建隔离的导出浏览器环境",
                )
            })?;
        let target_params = CreateTargetParams::builder()
            .url("about:blank")
            .browser_context_id(ctx_id.clone())
            .build()
            .map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Internal,
                    "export_failed",
                    "无法构造导出页面参数",
                )
            })?;
        match guard.new_page(target_params).await {
            Ok(page) => Ok((ctx_id, page)),
            Err(_) => {
                self.alive.store(false, Ordering::SeqCst);
                let _ = guard.dispose_browser_context(ctx_id).await;
                Err(ServiceError::new(
                    ServiceErrorKind::Internal,
                    "export_failed",
                    "无法打开导出浏览器页面",
                ))
            }
        }
    }

    async fn dispose_context(&self, ctx_id: BrowserContextId) {
        if !self.alive.load(Ordering::SeqCst) {
            return;
        }
        let guard = self.browser.lock().await;
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            guard.dispose_browser_context(ctx_id),
        )
        .await;
    }

    async fn dispose(&self) {
        self.alive.store(false, Ordering::SeqCst);
        {
            let mut guard = self.browser.lock().await;
            let close_res = tokio::time::timeout(Duration::from_secs(5), async {
                let close_ok = guard.close().await.is_ok();
                let _ = guard.wait().await;
                close_ok
            })
            .await;
            if !matches!(close_res, Ok(true)) {
                kill_process_tree(self.pid).await;
                let _ = guard.kill().await;
            }
        }
        let handle = self.handler_task.lock().expect("handler lock").take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
        remove_dir_retry(&self.profile).await;
    }
}

type LaunchResultSlot = Arc<Mutex<Option<Result<Arc<BrowserLease>, ServiceError>>>>;

enum LaunchSlotState {
    Empty,
    Starting {
        id: u64,
        notify: Arc<Notify>,
        result: LaunchResultSlot,
    },
    Ready(Arc<BrowserLease>),
}

#[derive(Clone)]
pub struct ExportBrowser {
    options: ExportBrowserOptions,
    working_executable: Arc<Mutex<Option<PathBuf>>>,
    state: Arc<Mutex<LaunchSlotState>>,
    next_lease_id: Arc<AtomicUsize>,
    operations: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    idle_timer: Arc<Mutex<Option<JoinHandle<()>>>>,
    retiring: Arc<AsyncMutex<Vec<JoinHandle<()>>>>,
}

impl ExportBrowser {
    pub fn new(options: ExportBrowserOptions) -> Self {
        Self {
            options,
            working_executable: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(LaunchSlotState::Empty)),
            next_lease_id: Arc::new(AtomicUsize::new(1)),
            operations: Arc::new(AtomicUsize::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
            idle_timer: Arc::new(Mutex::new(None)),
            retiring: Arc::new(AsyncMutex::new(Vec::new())),
        }
    }

    fn cancel_idle_timer(&self) {
        if let Some(timer) = self.idle_timer.lock().expect("idle timer lock").take() {
            timer.abort();
        }
    }

    async fn launch_once(&self, lease_id: u64) -> Result<Arc<BrowserLease>, ServiceError> {
        let cached = self
            .working_executable
            .lock()
            .expect("working executable lock")
            .clone();
        let mut candidates = resolve_browser_candidates(self.options.executable_path.as_deref())?;
        if let Some(ref preferred) = cached
            && let Some(pos) = candidates.iter().position(|p| p == preferred)
        {
            let p = candidates.remove(pos);
            candidates.insert(0, p);
        }
        let mut last_err: Option<ServiceError> = None;
        for executable in candidates {
            match self.launch_with_executable(lease_id, &executable).await {
                Ok(lease) => {
                    *self
                        .working_executable
                        .lock()
                        .expect("working executable lock") = Some(executable);
                    return Ok(lease);
                }
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_browser_launch_failed",
                "导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
            )
        }))
    }

    async fn launch_with_executable(
        &self,
        lease_id: u64,
        executable: &Path,
    ) -> Result<Arc<BrowserLease>, ServiceError> {
        tokio::fs::create_dir_all(&self.options.work_dir)
            .await
            .map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_browser_launch_failed",
                    "导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
                )
            })?;
        let profile = self
            .options
            .work_dir
            .join(format!("browser-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&profile).await.map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_browser_launch_failed",
                "导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
            )
        })?;

        let config = match BrowserConfig::builder()
            .chrome_executable(executable)
            .user_data_dir(&profile)
            .new_headless_mode()
            .launch_timeout(Duration::from_secs(10))
            .request_timeout(Duration::from_secs(30))
            .viewport(Viewport {
                width: 1240,
                height: 3200,
                device_scale_factor: Some(2.0),
                emulating_mobile: false,
                is_landscape: false,
                has_touch: false,
            })
            .args(["disable-component-update"])
            .build()
        {
            Ok(c) => c,
            Err(_) => {
                remove_dir_retry(&profile).await;
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_browser_launch_failed",
                    "导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
                ));
            }
        };

        let (mut browser, mut handler) = match Browser::launch(config).await {
            Ok(pair) => pair,
            Err(_) => {
                remove_dir_retry(&profile).await;
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_browser_launch_failed",
                    "导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
                ));
            }
        };

        let pid = browser.get_mut_child().and_then(|c| c.as_mut_inner().id());
        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_handler = alive.clone();
        let handler_task = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
            alive_for_handler.store(false, Ordering::SeqCst);
        });

        let lease = Arc::new(BrowserLease {
            id: lease_id,
            browser: AsyncMutex::new(browser),
            profile,
            pid,
            alive,
            handler_task: Mutex::new(Some(handler_task)),
        });

        let version_res = {
            let guard = lease.browser.lock().await;
            guard.version().await
        };
        match version_res {
            Ok(ver) => {
                let major = parse_chromium_major(&ver.product).unwrap_or(0);
                if major < 111 {
                    lease.dispose().await;
                    return Err(ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_browser_incompatible",
                        "导出需要 Chromium 111 或更新版本，请更新 Edge / Chrome 或设置 CST_EXPORT_BROWSER。",
                    ));
                }
                Ok(lease)
            }
            Err(_) => {
                lease.dispose().await;
                Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_browser_launch_failed",
                    "导出浏览器无法启动，请检查浏览器版本、运行权限或 CST_EXPORT_BROWSER 路径。",
                ))
            }
        }
    }

    async fn acquire(&self) -> Result<Arc<BrowserLease>, ServiceError> {
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_stopping",
                    "程序正在退出，请稍后导出",
                ));
            }

            enum Action {
                Launch {
                    id: u64,
                    notify: Arc<Notify>,
                    result_slot: LaunchResultSlot,
                },
                Wait {
                    notify: Arc<Notify>,
                    result_slot: LaunchResultSlot,
                },
                CheckReady(Arc<BrowserLease>),
            }

            let action = {
                let mut guard = self.state.lock().expect("browser state lock");
                match &*guard {
                    LaunchSlotState::Empty => {
                        let id = self.next_lease_id.fetch_add(1, Ordering::SeqCst) as u64;
                        let notify = Arc::new(Notify::new());
                        let result_slot = Arc::new(Mutex::new(None));
                        *guard = LaunchSlotState::Starting {
                            id,
                            notify: notify.clone(),
                            result: result_slot.clone(),
                        };
                        Action::Launch {
                            id,
                            notify,
                            result_slot,
                        }
                    }
                    LaunchSlotState::Starting { notify, result, .. } => Action::Wait {
                        notify: notify.clone(),
                        result_slot: result.clone(),
                    },
                    LaunchSlotState::Ready(lease) => Action::CheckReady(lease.clone()),
                }
            };

            let lease = match action {
                Action::Launch {
                    id,
                    notify,
                    result_slot,
                } => {
                    let res = self.launch_once(id).await;
                    let mut should_dispose: Option<Arc<BrowserLease>> = None;
                    let final_res = {
                        let mut guard = self.state.lock().expect("browser state lock");
                        match res {
                            Ok(ref l) if self.stopped.load(Ordering::SeqCst) => {
                                if matches!(&*guard, LaunchSlotState::Starting { id: cur, .. } if *cur == id)
                                {
                                    *guard = LaunchSlotState::Empty;
                                }
                                should_dispose = Some(l.clone());
                                Err(ServiceError::new(
                                    ServiceErrorKind::Unavailable,
                                    "export_stopping",
                                    "程序正在退出，请稍后导出",
                                ))
                            }
                            Ok(ref l) => {
                                if matches!(&*guard, LaunchSlotState::Starting { id: cur, .. } if *cur == id)
                                {
                                    *guard = LaunchSlotState::Ready(l.clone());
                                    Ok(l.clone())
                                } else {
                                    should_dispose = Some(l.clone());
                                    Err(ServiceError::new(
                                        ServiceErrorKind::Unavailable,
                                        "export_stopping",
                                        "程序正在退出，请稍后导出",
                                    ))
                                }
                            }
                            Err(e) => {
                                if matches!(&*guard, LaunchSlotState::Starting { id: cur, .. } if *cur == id)
                                {
                                    *guard = LaunchSlotState::Empty;
                                }
                                Err(e)
                            }
                        }
                    };
                    *result_slot.lock().expect("result slot lock") = Some(final_res.clone());
                    notify.notify_waiters();
                    if let Some(d) = should_dispose {
                        self.spawn_retire(d).await;
                    }
                    final_res?
                }
                Action::Wait {
                    notify,
                    result_slot,
                } => loop {
                    let notified = notify.notified();
                    if let Some(res) = result_slot.lock().expect("result slot lock").clone() {
                        break res?;
                    }
                    notified.await;
                },
                Action::CheckReady(lease) => lease,
            };

            if self.stopped.load(Ordering::SeqCst) {
                self.retire_if_current(lease.id).await;
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_stopping",
                    "程序正在退出，请稍后导出",
                ));
            }

            if !lease.is_connected().await {
                self.retire_if_current(lease.id).await;
                continue;
            }

            return Ok(lease);
        }
    }

    async fn spawn_retire(&self, lease: Arc<BrowserLease>) {
        let handle = tokio::spawn(async move {
            lease.dispose().await;
        });
        self.retiring.lock().await.push(handle);
    }

    async fn retire_if_current(&self, lease_id: u64) {
        let removed = {
            let mut guard = self.state.lock().expect("browser state lock");
            match &*guard {
                LaunchSlotState::Ready(l) if l.id == lease_id => {
                    let l = l.clone();
                    *guard = LaunchSlotState::Empty;
                    Some(l)
                }
                _ => None,
            }
        };
        if let Some(lease) = removed {
            self.cancel_idle_timer();
            self.spawn_retire(lease).await;
        }
    }

    async fn retire_current(&self) {
        let removed = {
            let mut guard = self.state.lock().expect("browser state lock");
            match std::mem::replace(&mut *guard, LaunchSlotState::Empty) {
                LaunchSlotState::Ready(l) => Some(l),
                LaunchSlotState::Starting { id, notify, result } => {
                    *guard = LaunchSlotState::Starting { id, notify, result };
                    None
                }
                LaunchSlotState::Empty => None,
            }
        };
        if let Some(lease) = removed {
            self.cancel_idle_timer();
            self.spawn_retire(lease).await;
        }
    }

    pub async fn run<T, F, Fut>(
        &self,
        cancel: CancellationToken,
        action: F,
    ) -> Result<T, ServiceError>
    where
        F: FnOnce(Page, Arc<BrowserLease>) -> Fut,
        Fut: Future<Output = Result<T, ServiceError>>,
    {
        if cancel.is_cancelled() {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_interrupted",
                "导出已取消",
            ));
        }
        if self.stopped.load(Ordering::SeqCst) {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_stopping",
                "程序正在退出，请稍后导出",
            ));
        }

        self.cancel_idle_timer();
        self.operations.fetch_add(1, Ordering::SeqCst);

        let res = async {
            let lease = self.acquire().await?;
            if cancel.is_cancelled() {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_interrupted",
                    "导出已取消",
                ));
            }
            let (ctx_id, page) = match lease.create_isolated_page().await {
                Ok(pair) => pair,
                Err(_) => {
                    self.retire_if_current(lease.id).await;
                    let retry_lease = self.acquire().await?;
                    let pair = retry_lease.create_isolated_page().await?;
                    return self
                        .run_with_context(retry_lease, pair.0, pair.1, cancel, action)
                        .await;
                }
            };
            self.run_with_context(lease, ctx_id, page, cancel, action)
                .await
        }
        .await;

        let remaining = self.operations.fetch_sub(1, Ordering::SeqCst) - 1;
        if remaining == 0 && !self.stopped.load(Ordering::SeqCst) {
            let idle_ms = self.options.idle_ms;
            let browser = self.clone();
            let handle = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(idle_ms)).await;
                if browser.operations.load(Ordering::SeqCst) == 0
                    && !browser.stopped.load(Ordering::SeqCst)
                {
                    browser.retire_current().await;
                }
            });
            *self.idle_timer.lock().expect("idle timer lock") = Some(handle);
        }

        res
    }

    async fn run_with_context<T, F, Fut>(
        &self,
        lease: Arc<BrowserLease>,
        ctx_id: BrowserContextId,
        page: Page,
        cancel: CancellationToken,
        action: F,
    ) -> Result<T, ServiceError>
    where
        F: FnOnce(Page, Arc<BrowserLease>) -> Fut,
        Fut: Future<Output = Result<T, ServiceError>>,
    {
        let _ = page
            .emulate_media_features(vec![MediaFeature {
                name: "prefers-reduced-motion".to_owned(),
                value: "reduce".to_owned(),
            }])
            .await;

        let action_res = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_interrupted",
                "导出已取消",
            )),
            out = action(page, lease.clone()) => out,
        };

        lease.dispose_context(ctx_id).await;
        if !lease.is_connected().await {
            self.retire_if_current(lease.id).await;
        }
        action_res
    }

    pub async fn close(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.cancel_idle_timer();
        loop {
            let wait_notify = {
                let guard = self.state.lock().expect("browser state lock");
                match &*guard {
                    LaunchSlotState::Starting { notify, .. } => Some(notify.clone()),
                    _ => None,
                }
            };
            if let Some(notify) = wait_notify {
                notify.notified().await;
            } else {
                break;
            }
        }
        self.retire_current().await;
        let handles: Vec<JoinHandle<()>> = {
            let mut guard = self.retiring.lock().await;
            std::mem::take(&mut *guard)
        };
        for h in handles {
            let _ = h.await;
        }
    }
}
