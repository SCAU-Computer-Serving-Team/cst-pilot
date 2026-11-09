use pi_agent::AgentMessage;
use pi_ai::ContentBlock;
use pi_session::{
    AgentSessionEvent, ExtensionRegistrar, MessageEndEventResult, SessionBeforeEventOutput,
    SessionDataPolicy,
};
use regex::Regex;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

pub const HIDDEN: &str = "[BitLocker 恢复密码已隐藏]";
const TTL: Duration = Duration::from_secs(180);

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:[0-9]{6}-){7}[0-9]{6}|[0-9]{48}").expect("fixed password regex")
    })
}
pub fn redact(text: &str) -> String {
    pattern().replace_all(text, HIDDEN).into_owned()
}
pub fn validate(password: Zeroizing<String>) -> Result<Zeroizing<String>, String> {
    let trimmed = Zeroizing::new(password.trim().to_owned());
    if trimmed.len() != 55
        || trimmed.split('-').count() != 8
        || trimmed
            .split('-')
            .any(|s| s.len() != 6 || !s.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err("恢复密码必须为八组六位数字，用连字符分隔。".into());
    }
    Ok(trimmed)
}

fn stream_redact(text: &str) -> String {
    let mut safe = redact(text);
    // An incomplete numeric suffix may be the beginning of a credential. Never
    // publish those bytes while a text/thinking snapshot is still growing.
    let suffix = safe
        .trim_end_matches(|c: char| c.is_ascii_digit() || c == '-')
        .len();
    if safe[suffix..].bytes().any(|c| c.is_ascii_digit()) {
        safe.truncate(suffix);
        safe.push_str(HIDDEN);
    }
    safe
}
fn scrub(value: &mut Value, streaming: bool) {
    match value {
        Value::String(text) => {
            let safe = if streaming {
                stream_redact(text)
            } else {
                redact(text)
            };
            text.zeroize();
            *text = safe;
        }
        Value::Array(items) => items.iter_mut().for_each(|v| scrub(v, streaming)),
        Value::Object(map) => {
            for (key, value) in map {
                if key == "recoveryPassword" {
                    wipe(value);
                    *value = Value::String(HIDDEN.into());
                } else if streaming && key == "delta" {
                    // Deltas can split the credential at arbitrary byte offsets.
                    // Consumers render the redacted complete partial snapshot.
                    wipe(value);
                    *value = Value::String(String::new());
                } else {
                    scrub(
                        value,
                        streaming
                            && matches!(
                                key.as_str(),
                                "text"
                                    | "thinking"
                                    | "content"
                                    | "message"
                                    | "partial"
                                    | "assistantMessageEvent"
                            ),
                    );
                }
            }
        }
        _ => {}
    }
}
fn wipe(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(wipe),
        Value::Object(o) => o.values_mut().for_each(wipe),
        _ => {}
    }
}
fn project<T: Serialize + DeserializeOwned>(value: &T, streaming: bool) -> Result<T, String> {
    let mut json = serde_json::to_value(value).map_err(|_| "敏感数据投影失败。")?;
    scrub(&mut json, streaming);
    serde_json::from_value(json).map_err(|_| "敏感数据投影失败。".into())
}

/// Stateless durable-input and subscriber projections. Transient secrets stay
/// in the session's Vault, never in this policy or an event listener.
pub struct PrivacyPolicy;
impl SessionDataPolicy for PrivacyPolicy {
    fn persisted_input(&self, text: &str) -> Result<String, String> {
        Ok(redact(text))
    }
    fn persisted_message(&self, message: AgentMessage) -> Result<AgentMessage, String> {
        project(&message, false)
    }
    fn project_event(&self, event: AgentSessionEvent) -> Result<AgentSessionEvent, String> {
        let streaming = matches!(
            event,
            AgentSessionEvent::MessageUpdate { .. } | AgentSessionEvent::MessageStart { .. }
        );
        project(&event, streaming)
    }
}

struct SavedCall {
    drive: Option<String>,
    scope: String,
    password: Zeroizing<String>,
    created: Instant,
}
struct SavedUser {
    message: Zeroizing<String>,
    created: Instant,
}
#[derive(Default)]
struct State {
    calls: BTreeMap<String, SavedCall>,
    users: BTreeMap<u64, SavedUser>,
}
#[derive(Clone, Default)]
pub struct Vault(Arc<Mutex<State>>);
impl Vault {
    pub fn clear(&self) {
        let mut state = self.0.lock().expect("credential vault");
        state.calls.clear();
        state.users.clear();
    }
    pub fn claim(
        &self,
        id: &str,
        scope: &str,
        drive: Option<&str>,
        provided: Option<Zeroizing<String>>,
    ) -> Result<Option<Zeroizing<String>>, String> {
        let saved = self.0.lock().expect("credential vault").calls.remove(id);
        if let Some(saved) = saved {
            if saved.created.elapsed() > TTL {
                return Err("BitLocker 凭据已过期，请重新输入。".into());
            }
            if saved.scope != "unlock" || scope != "unlock" || saved.drive.as_deref() != drive {
                return Err("BitLocker 凭据与目标调用不匹配。".into());
            }
            return Ok(Some(saved.password));
        }
        if provided.as_deref().is_some_and(|s| s.as_str() == HIDDEN) {
            return Err("BitLocker 凭据已过期，请重新输入。".into());
        }
        Ok(provided)
    }
    fn capture(&self, message: &AgentMessage) -> Result<(), String> {
        let mut state = self.0.lock().expect("credential vault");
        state.users.retain(|_, v| v.created.elapsed() <= TTL);
        state.calls.retain(|_, v| v.created.elapsed() <= TTL);
        if let AgentMessage::User(user) = message {
            let raw = Zeroizing::new(serde_json::to_string(user).map_err(|_| "凭据暂存失败。")?);
            if raw.len() <= 128 * 1024 && pattern().is_match(&raw) && state.users.len() < 32 {
                state.users.insert(
                    user.timestamp,
                    SavedUser {
                        message: raw,
                        created: Instant::now(),
                    },
                );
            }
        }
        if let AgentMessage::Assistant(assistant) = message {
            for part in &assistant.content {
                let ContentBlock::ToolCall(call) = part else {
                    continue;
                };
                if call.name != "bitlocker" {
                    continue;
                }
                let Some(Value::String(password)) = call.arguments.get("recoveryPassword") else {
                    continue;
                };
                if password == HIDDEN || password.len() > 4096 || state.calls.len() >= 1024 {
                    continue;
                }
                let scope = call
                    .arguments
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("status");
                let drive = call
                    .arguments
                    .get("drive")
                    .and_then(Value::as_str)
                    .and_then(normalize_drive);
                state.calls.insert(
                    call.id.clone(),
                    SavedCall {
                        drive,
                        scope: scope.into(),
                        password: Zeroizing::new(password.clone()),
                        created: Instant::now(),
                    },
                );
            }
        }
        Ok(())
    }
    pub fn register(&self, api: &mut ExtensionRegistrar) {
        let vault = self.clone();
        api.on_message_end(move |event, _| {
            let vault = vault.clone();
            async move {
                vault.capture(&event.message)?;
                Ok(Some(MessageEndEventResult {
                    message: Some(project(&event.message, false)?),
                }))
            }
        });
        let vault = self.clone();
        api.on_context(Arc::new(move |event, _| {
            let vault = vault.clone();
            Box::pin(async move {
                let state = vault.0.lock().expect("credential vault");
                for message in &mut event.messages {
                    let AgentMessage::User(user) = message else {
                        continue;
                    };
                    if let Some(saved) = state.users.get(&user.timestamp)
                        && saved.created.elapsed() <= TTL
                        && let Ok(original) = serde_json::from_str(&saved.message)
                    {
                        *user = original;
                    }
                }
                Ok(None)
            })
        }));
        let vault = self.clone();
        api.on_tool_result(move |event, _| {
            if event.tool_name == "bitlocker" {
                vault
                    .0
                    .lock()
                    .expect("credential vault")
                    .calls
                    .remove(&event.tool_call_id);
            }
            async { Ok(None) }
        });
        for name in ["agent_end", "session_start", "session_shutdown"] {
            let vault = self.clone();
            api.on_lifecycle(name, move |_, _| {
                vault.clear();
                async { Ok(SessionBeforeEventOutput::None) }
            });
        }
    }
}
fn normalize_drive(drive: &str) -> Option<String> {
    let bytes = drive.as_bytes();
    if (bytes.len() == 1 || bytes.len() == 2 && bytes[1] == b':') && bytes[0].is_ascii_alphabetic()
    {
        Some(format!("{}:", char::from(bytes[0].to_ascii_uppercase())))
    } else {
        None
    }
}
