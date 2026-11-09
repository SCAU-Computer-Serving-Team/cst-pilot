use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cst_pilot::KitHost;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::discovery::{ServerInstanceLock, write_discovery_file};
use crate::routes::handle_http_request;
use crate::state::{DEFAULT_SERVER_PORT, ServerConfig, ServerState};

pub struct RunningServer {
    pub state: Arc<ServerState>,
    pub addr: SocketAddr,
    _instance_lock: Option<ServerInstanceLock>,
    listener_task: JoinHandle<()>,
}

impl RunningServer {
    pub async fn start(host: KitHost, config: ServerConfig) -> Result<Self, String> {
        let instance_lock = if config.acquire_instance_lock {
            Some(ServerInstanceLock::acquire(host.kit().root())?)
        } else {
            None
        };

        let requested_port = config.port.unwrap_or_else(|| {
            std::env::var("CST_WEB_PORT")
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|&p| p >= 1024)
                .unwrap_or(DEFAULT_SERVER_PORT)
        });

        let listener = TcpListener::bind(("127.0.0.1", requested_port))
            .await
            .map_err(|e| format!("无法绑定本机端口 127.0.0.1:{requested_port}：{e}"))?;
        let bound_addr = listener
            .local_addr()
            .map_err(|e| format!("无法读取监听地址：{e}"))?;

        let state = ServerState::new(host, &config);
        state.set_bound_port(bound_addr.port());

        write_discovery_file(state.kit_root(), bound_addr.port(), state.app_token())
            .map_err(|e| e.to_string())?;

        // Watch for automatic presence shutdown (15s first-connect timeout or disconnect grace period)
        let state_for_auto = state.clone();
        tokio::spawn(async move {
            state_for_auto.mode.shutdown_notified().notified().await;
            if state_for_auto.mode.is_stopping() {
                let _ = state_for_auto.execute_shutdown().await;
            }
        });

        let state_for_loop = state.clone();
        let stop_token = state.server_stop.clone();

        let listener_task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = stop_token.cancelled() => break,
                    res = listener.accept() => res,
                };
                let Ok((stream, _peer)) = accepted else {
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let io = TokioIo::new(stream);
                let conn_cancel = CancellationToken::new();
                let conn_cancel_for_svc = conn_cancel.clone();
                let state_for_conn = state_for_loop.clone();
                let stop_for_conn = stop_token.clone();

                tokio::spawn(async move {
                    let svc = service_fn(move |req| {
                        handle_http_request(
                            state_for_conn.clone(),
                            req,
                            conn_cancel_for_svc.clone(),
                        )
                    });
                    let serve_fut = http1::Builder::new().serve_connection(io, svc);
                    tokio::select! {
                        biased;
                        _ = conn_cancel.cancelled() => (),
                        _ = stop_for_conn.cancelled() => (),
                        _ = serve_fut => (),
                    }
                });
            }
        });

        Ok(Self {
            state,
            addr: bound_addr,
            _instance_lock: instance_lock,
            listener_task,
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    pub fn app_token(&self) -> &str {
        self.state.app_token()
    }

    pub async fn shutdown(self) {
        let _ = self.state.execute_shutdown().await;
        self.state.server_stop.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.listener_task).await;
    }
}
