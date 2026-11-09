use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Compatible with the existing TypeScript owner's local-only proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proof {
    pub token: String,
    pub address: Address,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Address {
    Pipe(String),
    Port(u16),
}
impl Proof {
    pub fn valid(&self) -> bool {
        if Uuid::parse_str(&self.token)
            .ok()
            .map(|id| id.to_string())
            .as_deref()
            != Some(self.token.as_str())
        {
            return false;
        }
        match &self.address {
            Address::Pipe(address) => {
                cfg!(windows)
                    && [pipe_name(&self.token, false), pipe_name(&self.token, true)]
                        .contains(address)
            }
            Address::Port(port) => *port > 0,
        }
    }
    /// Only a definite missing endpoint or a mismatched token is considered dead.
    pub fn alive(&self) -> bool {
        if !self.valid() {
            return false;
        }
        let deadline = Instant::now() + Duration::from_millis(250);
        match &self.address {
            Address::Port(port) => {
                use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
                let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, *port));
                match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
                    Ok(mut stream) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
                        read_proof(&mut stream, &self.token, deadline, false)
                    }
                    Err(error) => !definitely_gone(&error),
                }
            }
            Address::Pipe(address) => pipe_alive(address, &self.token, deadline),
        }
    }
}
fn read_proof(
    reader: &mut dyn std::io::Read,
    token: &str,
    deadline: Instant,
    zero_is_pending: bool,
) -> bool {
    let mut bytes = Vec::new();
    let mut buffer = [0; 64];
    loop {
        match reader.read(&mut buffer) {
            // PIPE_NOWAIT can report a successful zero-byte read while the server is writing.
            Ok(0) if zero_is_pending => (),
            Ok(0) => return bytes == token.as_bytes(),
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() >= token.len() {
                    return bytes == token.as_bytes();
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) || e.raw_os_error() == Some(232) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                ) =>
            {
                return bytes == token.as_bytes();
            }
            Err(_) => return true,
        }
        if Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn definitely_gone(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}
#[cfg(windows)]
fn pipe_alive(address: &str, token: &str, deadline: Instant) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        System::Pipes::{PIPE_NOWAIT, SetNamedPipeHandleState},
    };
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(address)
    {
        Ok(mut file) => {
            let mode = PIPE_NOWAIT;
            if unsafe {
                SetNamedPipeHandleState(HANDLE(file.as_raw_handle()), Some(&mode), None, None)
            }
            .is_err()
            {
                return true;
            }
            read_proof(&mut file, token, deadline, true)
        }
        Err(e) => !definitely_gone(&e),
    }
}
#[cfg(not(windows))]
fn pipe_alive(_: &str, _: &str, _: Instant) -> bool {
    false
}
fn pipe_name(token: &str, draft: bool) -> String {
    format!(
        r"\\.\pipe\cst-pilot-telemetry-{}{token}",
        if draft { "draft-" } else { "" }
    )
}

pub struct Owner {
    proof: Proof,
    stop: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Owner {
    /// Listener has its own reactor so synchronous file transactions can probe it safely.
    pub fn start() -> Result<Self, String> {
        let stop = CancellationToken::new();
        let child = stop.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("cst-telemetry-owner".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(serve(sender, child)),
                    Err(_) => {
                        let _ = sender.send(Err("遥测存活证明无法启动。".into()));
                    }
                }
            })
            .map_err(|_| "遥测存活证明线程无法启动。")?;
        let proof = receiver.recv().map_err(|_| "遥测存活证明初始化中断。")??;
        Ok(Self {
            proof,
            stop,
            thread: Some(thread),
        })
    }
    pub fn global() -> Result<Arc<Self>, String> {
        static OWNER: OnceLock<Result<Arc<Owner>, String>> = OnceLock::new();
        OWNER.get_or_init(|| Self::start().map(Arc::new)).clone()
    }
    pub fn proof(&self) -> Proof {
        self.proof.clone()
    }
    pub fn draft_address(&self) -> String {
        pipe_name(&self.proof.token, true)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(windows)]
async fn serve(
    sender: std::sync::mpsc::SyncSender<Result<Proof, String>>,
    stop: CancellationToken,
) {
    use tokio::net::windows::named_pipe::ServerOptions;
    let token = Uuid::new_v4().to_string();
    let legacy = pipe_name(&token, false);
    let draft = pipe_name(&token, true);
    let make = |name: &str| {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(name)
    };
    let (Ok(a), Ok(b)) = (make(&legacy), make(&draft)) else {
        let _ = sender.send(Err("遥测命名管道无法创建。".into()));
        return;
    };
    let _ = sender.send(Ok(Proof {
        token: token.clone(),
        address: Address::Pipe(legacy.clone()),
    }));
    async fn listen(
        mut server: tokio::net::windows::named_pipe::NamedPipeServer,
        name: String,
        token: String,
        stop: CancellationToken,
    ) {
        loop {
            tokio::select! { biased; _=stop.cancelled()=>break, result=server.connect()=>if result.is_err(){break} }
            // Keep another instance listening before dropping the accepted handle.
            let Ok(next) = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&name)
            else {
                break;
            };
            tokio::select! { biased; _=stop.cancelled()=>break, _=tokio::time::timeout(Duration::from_millis(250), server.write_all(token.as_bytes()))=>() }
            server = next;
        }
    }
    tokio::join!(
        listen(a, legacy, token.clone(), stop.clone()),
        listen(b, draft, token, stop)
    );
}
#[cfg(not(windows))]
async fn serve(
    sender: std::sync::mpsc::SyncSender<Result<Proof, String>>,
    stop: CancellationToken,
) {
    let Ok(listener) = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await
    else {
        let _ = sender.send(Err("遥测本机监听无法创建。".into()));
        return;
    };
    let Ok(address) = listener.local_addr() else {
        return;
    };
    let token = Uuid::new_v4().to_string();
    let _ = sender.send(Ok(Proof {
        token: token.clone(),
        address: Address::Port(address.port()),
    }));
    loop {
        let accepted =
            tokio::select! { biased; _=stop.cancelled()=>break, value=listener.accept()=>value };
        if let Ok((mut stream, _)) = accepted {
            tokio::select! { biased; _=stop.cancelled()=>break, _=tokio::time::timeout(Duration::from_millis(250),stream.write_all(token.as_bytes()))=>() }
        }
    }
}
