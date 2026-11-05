use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

pub struct McpOAuthCallbackServer {
    listener: TcpListener,
    pub redirect_uri: String,
}

#[derive(Debug, Clone)]
pub struct McpOAuthCallback {
    pub code: String,
    pub issuer: Option<String>,
}

impl McpOAuthCallbackServer {
    pub async fn listen(callback_url: Option<&str>, port: Option<u16>) -> Result<Self, String> {
        let mut redirect = Url::parse(callback_url.unwrap_or("http://127.0.0.1/callback"))
            .map_err(|_| "Invalid OAuth callback URL")?;
        if redirect.scheme() != "http"
            || !matches!(
                redirect.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
        {
            return Err("OAuth callback must be an HTTP loopback URL without credentials, query or fragment".to_owned());
        }
        if let (Some(in_url), Some(in_config)) = (redirect.port(), port)
            && in_url != in_config
        {
            return Err("OAuth callback URL and port disagree".to_owned());
        }
        let bind_port = redirect.port().or(port).unwrap_or(0);
        let host = if redirect.host_str().is_some_and(|host| host.contains(':')) {
            "::1"
        } else {
            "127.0.0.1"
        };
        let listener = TcpListener::bind((host, bind_port))
            .await
            .map_err(|_| "Failed to bind OAuth loopback callback port".to_owned())?;
        redirect
            .set_port(Some(
                listener
                    .local_addr()
                    .map_err(|_| "Missing OAuth callback address")?
                    .port(),
            ))
            .map_err(|_| "Invalid OAuth callback port")?;
        Ok(Self {
            listener,
            redirect_uri: redirect.to_string(),
        })
    }

    pub async fn wait(
        self,
        state: &str,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<McpOAuthCallback, String> {
        let expected = Url::parse(&self.redirect_uri).map_err(|_| "Invalid callback URL")?;
        let listen = async {
            loop {
                let (mut socket, peer) = self
                    .listener
                    .accept()
                    .await
                    .map_err(|_| "OAuth callback accept failed")?;
                if !peer.ip().is_loopback() {
                    continue;
                }
                let request = tokio::time::timeout(Duration::from_secs(2), async {
                    let mut header = Vec::new();
                    loop {
                        let byte = socket
                            .read_u8()
                            .await
                            .map_err(|_| "Incomplete OAuth callback")?;
                        header.push(byte);
                        if header.ends_with(b"\r\n\r\n") {
                            break;
                        }
                        if header.len() > 8192 {
                            return Err("OAuth callback header exceeds size limit");
                        }
                    }
                    String::from_utf8(header).map_err(|_| "Invalid OAuth callback header")
                })
                .await;
                let Ok(Ok(header)) = request else {
                    continue;
                };
                let mut first = header.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("");
                let target = first.next().unwrap_or("");
                let url = Url::parse(&format!("http://callback{target}"));
                let mut fields = BTreeMap::new();
                let mut duplicate = false;
                if let Ok(url) = &url {
                    for (name, value) in url.query_pairs() {
                        duplicate |= fields.insert(name.to_string(), value.to_string()).is_some();
                    }
                }
                let correct = method == "GET"
                    && url.as_ref().is_ok_and(|url| url.path() == expected.path())
                    && fields.get("state").is_some_and(|given| given == state)
                    && !duplicate;
                let (status, text) = if correct {
                    (
                        200,
                        "Authorization response received. Return to the application.",
                    )
                } else {
                    (400, "Invalid or expired OAuth callback.")
                };
                let response = format!(
                    "HTTP/1.1 {status} Result\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                if !correct {
                    continue;
                }
                if fields.contains_key("error") {
                    return Err("OAuth authorization was rejected".to_owned());
                }
                let code = fields
                    .remove("code")
                    .filter(|code| !code.is_empty())
                    .ok_or("OAuth callback omitted authorization code")?;
                return Ok(McpOAuthCallback {
                    code,
                    issuer: fields.remove("iss"),
                });
            }
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err("OAuth authorization cancelled".to_owned()),
            result = tokio::time::timeout(timeout, listen) => result.map_err(|_| "OAuth callback timed out".to_owned())?,
        }
    }
}
