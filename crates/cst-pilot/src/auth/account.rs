use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::future::{BoxFuture, FutureExt, WeakShared};
use pi_ai::{AuthResolutionOverrides, Credential, CredentialStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{AuthRuntime, oa::requires_login};

type WeakPending = WeakShared<BoxFuture<'static, (Value, bool)>>;
struct Entry {
    key: String,
    epoch: u64,
    pending: Option<WeakPending>,
    cached: Option<(u64, Value)>,
}
#[derive(Default)]
pub(crate) struct AccountService {
    generation: AtomicU64,
    entries: Mutex<BTreeMap<String, Entry>>,
}
impl AccountService {
    pub fn invalidate(&self) {
        let mut entries = self.entries.lock().expect("accounts");
        self.generation.fetch_add(1, Ordering::SeqCst);
        entries.clear();
    }
    fn invalidate_if(&self, epoch: u64) {
        let mut entries = self.entries.lock().expect("accounts");
        if self
            .generation
            .compare_exchange(
                epoch,
                epoch.wrapping_add(1),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            entries.clear();
        }
    }
}
struct Access {
    key: String,
    token: String,
    endpoint: String,
}
fn empty(signed_in: bool, requires_login: bool, reason: &str) -> Value {
    json!({"account":{"providerId":"cstoa","signedIn":signed_in,"requiresLogin":requires_login,
        "profile":{"supported":true,"studentId":null,"name":null,"reason":reason}},
        "quota":{"provider":"cstoa","supported":false,"balance":null,"unit":"credit","requiresLogin":!signed_in || requires_login,"reason":reason}})
}
fn unsupported(provider: &str, reason: &str) -> Value {
    if provider == "cstoa" {
        empty(true, false, reason)
    } else {
        json!({"provider":provider,"supported":false,"reason":reason})
    }
}
fn stamp(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
impl AuthRuntime {
    pub fn invalidate_account(&self) {
        self.accounts.invalidate();
    }
    pub async fn account(&self, cancel: CancellationToken) -> Result<Value, String> {
        Ok(self.read_account("cstoa", cancel).await?["account"].clone())
    }
    pub async fn quota(&self, provider: &str, cancel: CancellationToken) -> Result<Value, String> {
        let value = self.read_account(provider, cancel).await?;
        Ok(if provider == "cstoa" {
            value["quota"].clone()
        } else {
            value
        })
    }
    pub async fn account_snapshot(&self, cancel: CancellationToken) -> Result<Value, String> {
        self.read_account("cstoa", cancel).await
    }
    async fn account_access(
        &self,
        provider: &str,
        cancel: &CancellationToken,
    ) -> Result<Access, Value> {
        let credential = self
            .runtime
            .credentials()
            .read(provider)
            .await
            .map_err(|_| unsupported(provider, "登录凭据暂时无法读取，请检查存储后重试。"))?;
        if provider == "cstoa" && credential.is_none() {
            return Err(empty(false, false, "登录后查看账号与额度。"));
        }
        let api_key = self
            .runtime
            .resolve_request_api_key(provider)
            .await
            .map_err(|_| unsupported(provider, "登录凭据暂时无法读取，请检查存储后重试。"))?;
        let auth = self
            .runtime
            .models()
            .get_auth_cancellable(
                provider,
                Some(&AuthResolutionOverrides {
                    api_key,
                    ..Default::default()
                }),
                Some(cancel),
            )
            .await
            .map_err(|e| {
                if provider == "cstoa" && requires_login(&e.message) {
                    empty(true, true, "登录已失效，请重新登录。")
                } else {
                    unsupported(
                        provider,
                        "登录凭据暂时无法读取或刷新，请检查网络和存储后重试。",
                    )
                }
            })?
            .ok_or_else(|| unsupported(provider, "缺少凭据。"))?;
        let current = self
            .runtime
            .credentials()
            .read(provider)
            .await
            .map_err(|_| unsupported(provider, "凭据暂时无法读取。"))?;
        let token = auth
            .auth
            .api_key
            .filter(|s| !s.is_empty() && s.len() <= 16384 && !s.chars().any(char::is_control))
            .ok_or_else(|| unsupported(provider, "缺少有效凭据。"))?;
        let endpoint = if provider == "cstoa" {
            let Some(c) = &current else {
                return Err(empty(false, false, "登录后查看账号与额度。"));
            };
            if super::models::token(c).1 != token {
                return Err(empty(false, false, "登录身份已变更，请重试。"));
            }
            format!(
                "{}{}",
                self.catalog.client.host(),
                if matches!(c, Credential::OAuth(_)) {
                    "/api/agent/profile"
                } else {
                    "/api/agent/key-profile"
                }
            )
        } else {
            if credential != current {
                return Err(unsupported(provider, "登录身份已变更，请重试。"));
            }
            let endpoint = if let Some(base) = auth.auth.base_url.as_deref() {
                quota_endpoint(provider, base)
            } else {
                self.runtime
                    .get_all_models()
                    .iter()
                    .filter(|m| m.provider == provider)
                    .find_map(|m| quota_endpoint(provider, &m.base_url))
            };
            endpoint.ok_or_else(|| unsupported(provider, "该模型服务未提供额度查询接口。"))?
        };
        let kind = if matches!(current, Some(Credential::OAuth(_))) {
            "oauth"
        } else {
            "api_key"
        };
        let key = format!(
            "{:x}",
            Sha256::digest(format!("{endpoint}\0{kind}\0{token}"))
        );
        Ok(Access {
            key,
            token,
            endpoint,
        })
    }
    async fn read_account(
        &self,
        provider: &str,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        if !matches!(provider, "cstoa" | "deepseek" | "opencode-go") {
            return Ok(unsupported(provider, "该模型服务未提供额度查询接口。"));
        }
        for _ in 0..2 {
            if cancel.is_cancelled() {
                return Err("查询已取消。".into());
            }
            let epoch = self.accounts.generation.load(Ordering::SeqCst);
            let access = match self.account_access(provider, &cancel).await {
                Ok(v) => v,
                Err(v) => return Ok(v),
            };
            if epoch != self.accounts.generation.load(Ordering::SeqCst) {
                continue;
            }
            let now = self.catalog.client.clock.now();
            let task = {
                let mut entries = self.accounts.entries.lock().expect("accounts");
                if epoch != self.accounts.generation.load(Ordering::SeqCst) {
                    continue;
                }
                let entry = entries.entry(provider.into()).or_insert_with(|| Entry {
                    key: access.key.clone(),
                    epoch,
                    pending: None,
                    cached: None,
                });
                if entry.key != access.key || entry.epoch != epoch {
                    *entry = Entry {
                        key: access.key.clone(),
                        epoch,
                        pending: None,
                        cached: None,
                    };
                }
                if let Some((at, value)) = &entry.cached
                    && now.saturating_sub(*at) < 60000
                {
                    let value = value.clone();
                    futures_util::future::ready((value, false)).boxed().shared()
                } else if let Some(task) = entry.pending.as_ref().and_then(WeakShared::upgrade) {
                    task
                } else {
                    let client = self.catalog.client.clone();
                    let provider = provider.to_owned();
                    let token = access.token.clone();
                    let endpoint = access.endpoint.clone();
                    let task = async move {
                        let response = client
                            .request_url(endpoint, None, Some(&token), CancellationToken::new(), 8)
                            .await;
                        match response {
                            Ok((status, body)) if provider == "cstoa" => {
                                parse_account(status, &body, client.clock.now())
                            }
                            Ok((status, body)) if (200..300).contains(&status) => {
                                match parse_quota(&provider, &body) {
                                    Some(mut result) => {
                                        result["fetchedAt"] = json!(stamp(client.clock.now()));
                                        (result, true)
                                    }
                                    None => (
                                        unsupported(&provider, "额度响应格式无效，请稍后重试。"),
                                        false,
                                    ),
                                }
                            }
                            Ok((401 | 403, _)) => {
                                let mut v = unsupported(&provider, "登录已失效，请重新登录。");
                                v["requiresLogin"] = json!(true);
                                (v, false)
                            }
                            _ => (
                                unsupported(&provider, "额度查询失败，请检查网络后重试。"),
                                false,
                            ),
                        }
                    }
                    .boxed()
                    .shared();
                    entry.pending = task.downgrade();
                    task
                }
            };
            let (value, cacheable) = tokio::select! { biased; _ = cancel.cancelled()=>return Err("查询已取消。".into()), result=task.clone()=>result };
            let current = self.account_access(provider, &cancel).await;
            if epoch != self.accounts.generation.load(Ordering::SeqCst) {
                continue;
            }
            match current {
                Ok(current) if current.key == access.key => (),
                Ok(_) => {
                    self.accounts.invalidate_if(epoch);
                    continue;
                }
                Err(v) => {
                    self.accounts.invalidate_if(epoch);
                    return Ok(v);
                }
            }
            let mut entries = self.accounts.entries.lock().expect("accounts");
            if let Some(entry) = entries.get_mut(provider).filter(|e| {
                e.epoch == epoch
                    && e.key == access.key
                    && e.pending
                        .as_ref()
                        .and_then(WeakShared::upgrade)
                        .is_some_and(|p| p.ptr_eq(&task))
            }) {
                entry.pending = None;
                if cacheable {
                    entry.cached = Some((self.catalog.client.clock.now(), value.clone()));
                }
            }
            return Ok(value);
        }
        Ok(if provider == "cstoa" {
            empty(false, false, "登录身份已变更，请重试。")
        } else {
            unsupported(provider, "登录身份已变更，请重试。")
        })
    }
}
fn parse_account(status: u16, body: &Value, now: u64) -> (Value, bool) {
    if status == 401 || status == 403 || body["code"].as_i64() == Some(19008) {
        return (empty(true, true, "登录已失效，请重新登录。"), false);
    }
    if !(200..300).contains(&status) || body["result"] != true {
        return (empty(true, false, "账号资料查询失败，请重试。"), false);
    }
    let data = &body["data"];
    let member = data["memberId"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 64 && !s.chars().any(char::is_control));
    let name = data["name"]
        .as_str()
        .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control));
    let (Some(member), Some(name)) = (member, name) else {
        return (empty(true, false, "账号资料格式无效，请稍后重试。"), false);
    };
    let mut value = empty(true, false, "额度暂不可用，请重试。");
    value["account"]["profile"] = json!({"supported":true,"studentId":member.trim(),"name":if name.trim().is_empty() { Value::Null } else { json!(name.trim()) }});
    if name.trim().is_empty() {
        value["account"]["profile"]["reason"] = json!("姓名暂未提供。");
    }
    let balance = data["balance"]
        .as_f64()
        .filter(|n| {
            n.is_finite() && n.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(n)
        })
        .map(|n| n as u64);
    let valid = data["balanceAvailable"] == true && balance.is_some();
    if valid {
        value["quota"] = json!({"provider":"cstoa","supported":true,"balance":balance,"unit":"credit","fetchedAt":stamp(now)});
    }
    (value, valid && !name.trim().is_empty())
}
fn quota_endpoint(provider: &str, base: &str) -> Option<String> {
    let url = Url::parse(base).ok()?;
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    match (provider, url.host_str(), path) {
        ("deepseek", Some("api.deepseek.com"), "" | "/v1") => {
            Some("https://api.deepseek.com/user/balance".into())
        }
        (
            "opencode-go",
            Some("opencode.ai"),
            "/zen/go"
            | "/zen/go/v1"
            | "/inference/go/openai/v1"
            | "/inference/go/anthropic"
            | "/inference/go/google/v1beta",
        ) => Some("https://opencode.ai/zen/go/v1/usage".into()),
        _ => None,
    }
}
fn parse_quota(provider: &str, body: &Value) -> Option<Value> {
    if provider == "deepseek" {
        body["is_available"].as_bool()?;
        let mut balances = Vec::new();
        for item in body["balance_infos"].as_array()? {
            let currency = item["currency"]
                .as_str()
                .filter(|s| matches!(*s, "CNY" | "USD"))?;
            let raw = item["total_balance"].as_str()?;
            let (whole, fraction) = raw
                .split_once('.')
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((raw, None));
            if whole.is_empty()
                || !whole.bytes().all(|c| c.is_ascii_digit())
                || (whole.len() > 1 && whole.starts_with('0'))
                || fraction.is_some_and(|s| s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()))
            {
                return None;
            }
            let amount = raw
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)?;
            balances.push((currency, amount));
        }
        let selected = balances
            .iter()
            .find(|(c, _)| *c == "CNY")
            .or_else(|| balances.first())?;
        Some(
            json!({"provider":provider,"supported":true,"balance":selected.1,"currency":selected.0}),
        )
    } else {
        let mut windows = json!({});
        for name in ["rolling", "weekly", "monthly"] {
            let w = &body["usage"][name];
            let percent = w["percent"]
                .as_f64()
                .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))?;
            let status = w["status"]
                .as_str()
                .filter(|s| matches!(*s, "ok" | "rate-limited"))?;
            let time = w["resetsAt"].as_str()?;
            chrono::DateTime::parse_from_rfc3339(time).ok()?;
            windows[name] = json!({"status":status,"percent":percent,"resetsAt":time});
        }
        Some(
            json!({"provider":provider,"supported":true,"windows":windows,"balance":null,"currency":null}),
        )
    }
}
