use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::{BoxFuture, Credential, CredentialStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthRuntime, oa::OaClient};

const MAX_SAFE: u64 = 9_007_199_254_740_991;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub quota_points: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BillingSummary {
    pub requests: usize,
    pub settled_requests: usize,
    pub quota_points: Option<u64>,
}
/// Only aggregate totals leave this object; request IDs belong exclusively to local drafts.
#[derive(Clone, Default)]
pub struct Billing(Arc<Mutex<Vec<BillingRequest>>>);
impl Billing {
    pub fn restore(requests: Vec<BillingRequest>) -> Self {
        Self(Arc::new(Mutex::new(requests)))
    }
    pub fn start(&self) -> usize {
        let mut requests = self.0.lock().expect("billing");
        requests.push(BillingRequest::default());
        requests.len() - 1
    }
    pub fn finish(&self, index: usize, id: Option<&str>) {
        let id = id.filter(|id| {
            !id.is_empty() && id.len() <= 256 && id.bytes().all(|c| c.is_ascii_graphic())
        });
        if let Some(request) = self.0.lock().expect("billing").get_mut(index) {
            request.request_id = id.map(str::to_owned);
        }
    }
    pub fn snapshot(&self) -> Vec<BillingRequest> {
        let mut seen = BTreeSet::new();
        self.0
            .lock()
            .expect("billing")
            .iter()
            .filter(|item| {
                item.request_id
                    .as_ref()
                    .is_none_or(|id| seen.insert(id.clone()))
            })
            .cloned()
            .collect()
    }
    pub fn summary(&self) -> Option<BillingSummary> {
        let items = self.snapshot();
        if items.is_empty() {
            return None;
        }
        let settled: Vec<_> = items.iter().filter_map(|item| item.quota_points).collect();
        let sum = settled.iter().try_fold(0u64, |sum, item| {
            sum.checked_add(*item).filter(|v| *v <= MAX_SAFE)
        });
        Some(BillingSummary {
            requests: items.len(),
            settled_requests: settled.len(),
            quota_points: if settled.is_empty() { None } else { sum },
        })
    }
    /// One 4.5s deadline covers every batch, including credential reads. Failure remains unknown.
    pub async fn settle(
        &self,
        lookup: &dyn BillingLookup,
        cancel: CancellationToken,
        budget: Duration,
    ) {
        let ids: Vec<_> = self
            .snapshot()
            .into_iter()
            .filter(|item| item.quota_points.is_none())
            .filter_map(|item| item.request_id)
            .collect();
        let child = cancel.child_token();
        let _cancel_on_drop = child.clone().drop_guard();
        let work = async {
            for batch in ids.chunks(50) {
                if child.is_cancelled() {
                    break;
                }
                let result = tokio::time::timeout(
                    Duration::from_secs(2),
                    lookup.lookup(batch, child.clone()),
                )
                .await;
                let Ok(Ok(values)) = result else { continue };
                let mut requests = self.0.lock().expect("billing");
                for item in requests.iter_mut() {
                    if let Some(value) = item
                        .request_id
                        .as_ref()
                        .filter(|id| batch.contains(id))
                        .and_then(|id| values.get(id))
                        .copied()
                        .flatten()
                        .filter(|v| *v <= MAX_SAFE)
                    {
                        item.quota_points = Some(value);
                    }
                }
            }
        };
        tokio::select! { biased; _=cancel.cancelled()=>(), _=tokio::time::timeout(budget.min(Duration::from_millis(4500)),work)=>() }
    }
}
pub trait BillingLookup: Send + Sync {
    fn lookup<'a>(
        &'a self,
        ids: &'a [String],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>>;
}
/// Read-only credentials: no token refresh, no shell resolution, no credential writes.
pub struct OaBilling {
    credentials: Arc<dyn CredentialStore>,
    client: OaClient,
}
impl OaBilling {
    pub fn from_auth(auth: &AuthRuntime) -> Self {
        Self {
            credentials: Arc::new(auth.runtime().credentials().clone()),
            client: auth.catalog.client.clone(),
        }
    }
    fn token(&self) -> Option<String> {
        match self.credentials.inspect("cstoa")?.credential {
            Credential::ApiKey(c) => c.key.filter(|key| !key.is_empty()),
            Credential::OAuth(c) if c.expires > self.client.clock.now() => Some(c.access),
            _ => None,
        }
    }
}
impl BillingLookup for OaBilling {
    fn lookup<'a>(
        &'a self,
        ids: &'a [String],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
        Box::pin(async move {
            if ids.len() > 50 {
                return Err("账单查询每批最多 50 项。".into());
            }
            let Some(token) = self.token() else {
                return Ok(BTreeMap::new());
            };
            let (status, value) = self
                .client
                .request(
                    "/api/agent/llm/billing",
                    Some(json!({"requestIds":ids})),
                    Some(&token),
                    cancel,
                    2,
                )
                .await?;
            if self.token().as_deref() != Some(&token) {
                return Ok(BTreeMap::new());
            }
            if !(200..300).contains(&status)
                || value.get("result") != Some(&Value::Bool(true))
                || value.pointer("/data/unit").and_then(Value::as_str) != Some("quota_point")
            {
                return Ok(BTreeMap::new());
            }
            let mut out = BTreeMap::new();
            let mut seen = BTreeSet::new();
            for item in value
                .pointer("/data/requests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(id) = item
                    .get("requestId")
                    .and_then(Value::as_str)
                    .filter(|id| ids.iter().any(|allowed| allowed == id))
                else {
                    continue;
                };
                // Duplicate response rows are ambiguous; do not confirm either value.
                if !seen.insert(id.to_owned()) {
                    out.remove(id);
                    continue;
                }
                match item.get("quotaPoints") {
                    Some(Value::Null) => {
                        out.insert(id.to_owned(), None);
                    }
                    Some(value) if value.as_u64().is_some_and(|v| v <= MAX_SAFE) => {
                        out.insert(id.to_owned(), value.as_u64());
                    }
                    _ => (),
                }
            }
            Ok(out)
        })
    }
}
