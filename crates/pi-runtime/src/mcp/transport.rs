use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use pi_agent::BoxFuture;
use pi_ai::JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::mpsc;

pub type SessionReinitializedHandler = Arc<dyn Fn(super::protocol::InitializeResult) + Send + Sync>;
const MAX_MCP_MESSAGE_BYTES: usize = super::sse::MAX_MCP_MESSAGE_BYTES;

pub trait McpTransport: Send + Sync {
    fn start(&self) -> BoxFuture<'_, Result<mpsc::UnboundedReceiver<JsonValue>, String>>;
    fn send(&self, message: JsonValue) -> BoxFuture<'_, Result<(), String>>;
    fn set_protocol_version(&self, _version: &str) {}
    fn on_session_reinitialized(&self, _handler: SessionReinitializedHandler) {}
    fn close(&self) -> BoxFuture<'_, Result<(), String>>;
}

pub struct InMemoryTransport {
    tx: mpsc::UnboundedSender<JsonValue>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<JsonValue>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl InMemoryTransport {
    pub fn pair() -> (Arc<Self>, Arc<Self>) {
        let (tx_a, rx_b) = mpsc::unbounded_channel();
        let (tx_b, rx_a) = mpsc::unbounded_channel();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        (
            Arc::new(Self {
                tx: tx_a,
                rx: Mutex::new(Some(rx_a)),
                closed: closed.clone(),
            }),
            Arc::new(Self {
                tx: tx_b,
                rx: Mutex::new(Some(rx_b)),
                closed,
            }),
        )
    }
}

impl McpTransport for InMemoryTransport {
    fn start(&self) -> BoxFuture<'_, Result<mpsc::UnboundedReceiver<JsonValue>, String>> {
        let rx = self.rx.lock().expect("rx lock").take();
        Box::pin(async move { rx.ok_or_else(|| "InMemoryTransport already started".to_owned()) })
    }

    fn send(&self, message: JsonValue) -> BoxFuture<'_, Result<(), String>> {
        let tx = self.tx.clone();
        let is_closed = self.closed.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if is_closed {
                return Err("MCP connection closed".to_owned());
            }
            tx.send(message)
                .map_err(|_| "MCP connection closed".to_owned())
        })
    }

    fn close(&self) -> BoxFuture<'_, Result<(), String>> {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

pub struct StdioTransportOptions {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub stderr_log_path: Option<PathBuf>,
}

struct StdioState {
    child: Option<std::process::Child>,
    stdin: Option<Arc<tokio::sync::Mutex<ChildStdin>>>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    closed: bool,
    #[cfg(windows)]
    job: Option<crate::config_value::win_job::WinJobGuard>,
}

impl StdioState {
    fn empty() -> Self {
        Self {
            child: None,
            stdin: None,
            readers: Vec::new(),
            closed: false,
            #[cfg(windows)]
            job: None,
        }
    }
}

impl Drop for StdioState {
    fn drop(&mut self) {
        self.stdin.take();
        for reader in self.readers.drain(..) {
            reader.abort();
        }
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            crate::config_value::kill_process_tree_sync(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct StdioTransport {
    options: StdioTransportOptions,
    state: Arc<Mutex<StdioState>>,
}

impl StdioTransport {
    pub fn new(options: StdioTransportOptions) -> Self {
        Self {
            options,
            state: Arc::new(Mutex::new(StdioState::empty())),
        }
    }
}

async fn read_bounded_line<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1);
        let consumed = end.unwrap_or(available.len());
        if line.len().saturating_add(consumed) > MAX_MCP_MESSAGE_BYTES {
            return Err(std::io::Error::other(
                "MCP stdio message exceeds size limit",
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if end.is_some() {
            return Ok(Some(line));
        }
    }
}

impl McpTransport for StdioTransport {
    fn start(&self) -> BoxFuture<'_, Result<mpsc::UnboundedReceiver<JsonValue>, String>> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("stdio state lock");
            if state.closed || state.child.is_some() {
                return Err("MCP stdio transport already started or closed".to_owned());
            }
            let mut command = std::process::Command::new(&self.options.command);
            command
                .args(&self.options.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(cwd) = &self.options.cwd {
                command.current_dir(cwd);
            }
            command.envs(&self.options.env);
            #[cfg(windows)]
            let job = {
                use crate::config_value::win_job;
                use std::os::windows::process::CommandExt;
                command.creation_flags(win_job::CREATE_NO_WINDOW | win_job::CREATE_SUSPENDED);
                win_job::WinJobGuard::create().ok_or("Failed to create MCP process containment")?
            };
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let mut child = command
                .spawn()
                .map_err(|e| format!("Failed to spawn MCP server: {e}"))?;
            #[cfg(windows)]
            if !job.assign_and_resume(&mut child) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Failed to contain or resume MCP process".to_owned());
            }
            // Store the child before converting pipes so every error path reaps it.
            state.child = Some(child);
            #[cfg(windows)]
            {
                state.job = Some(job);
            }
            let child = state.child.as_mut().expect("child stored");
            let pipes = (|| {
                let stdin = ChildStdin::from_std(child.stdin.take().ok_or("Missing MCP stdin")?)
                    .map_err(|e| e.to_string())?;
                let stdout = tokio::process::ChildStdout::from_std(
                    child.stdout.take().ok_or("Missing MCP stdout")?,
                )
                .map_err(|e| e.to_string())?;
                let stderr = tokio::process::ChildStderr::from_std(
                    child.stderr.take().ok_or("Missing MCP stderr")?,
                )
                .map_err(|e| e.to_string())?;
                Ok::<_, String>((stdin, stdout, stderr))
            })();
            let (stdin, stdout, stderr) = match pipes {
                Ok(pipes) => pipes,
                Err(error) => {
                    let mut failed = StdioState::empty();
                    std::mem::swap(&mut *state, &mut failed);
                    return Err(error);
                }
            };
            state.stdin = Some(Arc::new(tokio::sync::Mutex::new(stdin)));
            let (tx, rx) = mpsc::unbounded_channel();
            state.readers.push(tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                while let Ok(Some(line)) = read_bounded_line(&mut reader).await {
                    if let Ok(message) = serde_json::from_slice::<JsonValue>(&line)
                        && tx.send(message).is_err()
                    {
                        break;
                    }
                }
            }));
            let path = self.options.stderr_log_path.clone();
            state.readers.push(tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                while let Ok(Some(line)) = read_bounded_line(&mut reader).await {
                    if let Some(path) = &path {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Ok(mut file) =
                            OpenOptions::new().create(true).append(true).open(path)
                        {
                            let _ = file.write_all(&line);
                        }
                    }
                }
            }));
            let weak_state = Arc::downgrade(&self.state);
            state.readers.push(tokio::spawn(async move {
                let mut timer = tokio::time::interval(std::time::Duration::from_millis(50));
                loop {
                    timer.tick().await;
                    let Some(state) = weak_state.upgrade() else {
                        break;
                    };
                    let mut state = state.lock().expect("stdio state lock");
                    if state.closed {
                        break;
                    }
                    let exited = state
                        .child
                        .as_mut()
                        .is_some_and(|child| !matches!(child.try_wait(), Ok(None)));
                    if !exited {
                        continue;
                    }
                    state.closed = true;
                    state.stdin.take();
                    #[cfg(windows)]
                    if let Some(job) = state.job.take() {
                        job.terminate();
                    }
                    #[cfg(unix)]
                    if let Some(child) = &state.child {
                        crate::config_value::kill_process_tree_sync(child.id());
                    }
                    // Keep stdout/stderr readers alive to drain the root's final messages.
                    break;
                }
            }));
            Ok(rx)
        })
    }

    fn send(&self, message: JsonValue) -> BoxFuture<'_, Result<(), String>> {
        let stdin = self.state.lock().expect("stdio state lock").stdin.clone();
        Box::pin(async move {
            let stdin = stdin.ok_or("MCP stdio transport is not started")?;
            let mut line = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
            if line.len() > MAX_MCP_MESSAGE_BYTES {
                return Err("MCP stdio message exceeds size limit".to_owned());
            }
            line.push(b'\n');
            let mut writer = stdin.lock().await;
            writer.write_all(&line).await.map_err(|e| e.to_string())?;
            writer.flush().await.map_err(|e| e.to_string())
        })
    }

    fn close(&self) -> BoxFuture<'_, Result<(), String>> {
        let previous = {
            let mut state = self.state.lock().expect("stdio state lock");
            let mut closed = StdioState::empty();
            closed.closed = true;
            std::mem::replace(&mut *state, closed)
        };
        Box::pin(async move {
            tokio::task::spawn_blocking(move || drop(previous))
                .await
                .map_err(|e| e.to_string())
        })
    }
}

pub use super::http::{StreamableHttpTransport, StreamableHttpTransportOptions};
