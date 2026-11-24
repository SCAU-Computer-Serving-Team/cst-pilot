use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cst_kit::KitHost;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::discovery::{ServerInstanceLock, write_discovery_file};
use crate::routes::handle_http_request;
use crate::state::{DEFAULT_SERVER_PORT, ServerConfig, ServerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedPort {
    Explicit(u16),
    DefaultWithFallback(u16),
}

/// Resolves the requested loopback port from config and `CST_WEB_PORT`, falling back only when
/// neither explicitly pins a port.
fn resolve_requested_port_from(
    config_port: Option<u16>,
    env_web_port: Option<&str>,
) -> RequestedPort {
    if let Some(port) = config_port {
        return RequestedPort::Explicit(port);
    }
    if let Some(env_port) = env_web_port
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|&p| p >= 1024)
    {
        return RequestedPort::Explicit(env_port);
    }
    RequestedPort::DefaultWithFallback(DEFAULT_SERVER_PORT)
}

/// Port conflicts can retry on any platform. Windows may also report `WSAEACCES` for
/// excluded ports or a conflicting exclusive bind.
fn should_fallback_from_default_port(err: &std::io::Error) -> bool {
    const WSAEACCES: i32 = 10013;
    err.kind() == std::io::ErrorKind::AddrInUse
        || (cfg!(windows) && err.raw_os_error() == Some(WSAEACCES))
}

/// Binds `127.0.0.1`, automatically falling back to `127.0.0.1:0` when the implicit default port
/// is occupied or blocked by the OS.
async fn bind_loopback_listener(requested: RequestedPort) -> Result<TcpListener, String> {
    match requested {
        RequestedPort::Explicit(port) => TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("无法绑定本机端口 127.0.0.1:{port}：{e}")),
        RequestedPort::DefaultWithFallback(default_port) => {
            match TcpListener::bind(("127.0.0.1", default_port)).await {
                Ok(listener) => Ok(listener),
                Err(first_err) if should_fallback_from_default_port(&first_err) => {
                    TcpListener::bind(("127.0.0.1", 0)).await.map_err(|fallback_err| {
                        format!(
                            "无法绑定默认本机端口 127.0.0.1:{default_port}（{first_err}），且动态分配空闲端口失败：{fallback_err}"
                        )
                    })
                }
                Err(err) => Err(format!("无法绑定本机端口 127.0.0.1:{default_port}：{err}")),
            }
        }
    }
}

pub struct RunningServer {
    pub state: Arc<ServerState>,
    pub addr: SocketAddr,
    _instance_lock: Option<ServerInstanceLock>,
    listener_task: JoinHandle<()>,
}

impl RunningServer {
    pub async fn start(host: KitHost, mut config: ServerConfig) -> Result<Self, String> {
        let instance_lock = if config.acquire_instance_lock {
            Some(ServerInstanceLock::acquire(host.kit().root())?)
        } else {
            None
        };

        let env_web_port = std::env::var("CST_WEB_PORT").ok();
        let requested_port = resolve_requested_port_from(config.port, env_web_port.as_deref());
        let listener = bind_loopback_listener(requested_port).await?;
        let bound_addr = listener
            .local_addr()
            .map_err(|e| format!("无法读取监听地址：{e}"))?;

        config.port = Some(bound_addr.port());
        let state = ServerState::new(host, &config);
        state.set_bound_port(bound_addr.port());

        write_discovery_file(state.kit_root(), bound_addr.port(), state.app_token())
            .map_err(|e| e.to_string())?;

        // Watch for automatic presence shutdown (15s first-connect timeout or disconnect grace period).
        // Loops across busy-session shutdown cancellations and registers the Notify waiter before
        // checking `is_stopping()` so early signals are never lost.
        let state_for_auto = state.clone();
        tokio::spawn(async move {
            loop {
                let notified = state_for_auto.mode.shutdown_notified().notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if state_for_auto.mode.is_stopping() {
                    if state_for_auto.execute_shutdown().await.is_ok() {
                        break;
                    }
                    continue;
                }
                tokio::select! {
                    biased;
                    _ = state_for_auto.server_stop.cancelled() => break,
                    () = notified => {}
                }
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

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    use super::*;

    #[test]
    fn resolve_requested_port_respects_config_env_and_default_fallback() {
        assert_eq!(
            resolve_requested_port_from(Some(0), Some("52900")),
            RequestedPort::Explicit(0)
        );
        assert_eq!(
            resolve_requested_port_from(Some(53000), None),
            RequestedPort::Explicit(53000)
        );
        assert_eq!(
            resolve_requested_port_from(None, Some("52900")),
            RequestedPort::Explicit(52900)
        );
        let default_port = DEFAULT_SERVER_PORT.to_string();
        assert_eq!(
            resolve_requested_port_from(Some(DEFAULT_SERVER_PORT), None),
            RequestedPort::Explicit(DEFAULT_SERVER_PORT)
        );
        assert_eq!(
            resolve_requested_port_from(None, Some(&default_port)),
            RequestedPort::Explicit(DEFAULT_SERVER_PORT)
        );
        assert_eq!(
            resolve_requested_port_from(None, Some("1023")),
            RequestedPort::DefaultWithFallback(DEFAULT_SERVER_PORT)
        );
        assert_eq!(
            resolve_requested_port_from(None, Some("not-a-port")),
            RequestedPort::DefaultWithFallback(DEFAULT_SERVER_PORT)
        );
        assert_eq!(
            resolve_requested_port_from(None, None),
            RequestedPort::DefaultWithFallback(DEFAULT_SERVER_PORT)
        );
    }

    #[test]
    fn should_fallback_matches_addr_in_use_and_windows_wsaeacces() {
        assert!(should_fallback_from_default_port(&Error::new(
            ErrorKind::AddrInUse,
            "in use"
        )));
        assert!(!should_fallback_from_default_port(&Error::new(
            ErrorKind::PermissionDenied,
            "denied"
        )));
        assert_eq!(
            should_fallback_from_default_port(&Error::from_raw_os_error(10013)),
            cfg!(windows)
        );
        assert!(!should_fallback_from_default_port(&Error::new(
            ErrorKind::InvalidInput,
            "invalid"
        )));
    }

    #[tokio::test]
    async fn bind_loopback_listener_falls_back_when_default_port_occupied() {
        let occupied = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind ephemeral port");
        let occupied_port = occupied.local_addr().unwrap().port();

        let fallback = bind_loopback_listener(RequestedPort::DefaultWithFallback(occupied_port))
            .await
            .expect("default port contention should fall back to an ephemeral port");
        let bound_port = fallback.local_addr().unwrap().port();
        assert_ne!(bound_port, occupied_port);
        assert!(bound_port > 0);
    }

    #[tokio::test]
    async fn bind_loopback_listener_rejects_explicit_port_when_occupied() {
        let occupied = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind ephemeral port");
        let occupied_port = occupied.local_addr().unwrap().port();

        let err = bind_loopback_listener(RequestedPort::Explicit(occupied_port))
            .await
            .expect_err("explicit port contention should error without fallback");
        assert!(err.contains(&occupied_port.to_string()));
    }
}
