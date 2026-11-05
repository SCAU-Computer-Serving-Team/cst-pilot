use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pi_agent::BoxFuture;
use pi_ai::JsonValue;
use rquickjs::context::EvalOptions;
use rquickjs::function::Rest;
use rquickjs::{Context, Function, Object, Runtime, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::codemode::declarations::CodemodeDeclarationItem;
use crate::codemode::prelude::{DETERMINISTIC_SETUP_SOURCE, PRELUDE_SOURCE};
use crate::codemode::source::to_codemode_identifier;

pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_STACK_SIZE_BYTES: usize = 512 * 1024;
pub const DEFAULT_INTERRUPT_BUDGET: u64 = 100_000;
const JOBS_PER_POLL: usize = 50;

const RESERVED_GLOBALS: &[&str] = &[
    "tools",
    "ALL_TOOLS",
    "console",
    "text",
    "image",
    "exit",
    "globalThis",
    "store",
    "load",
];

fn is_identifier_part(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CodemodeOutputItem {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        console: bool,
    },
    #[serde(rename_all = "camelCase")]
    Image { data: String, mime_type: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeCallStatus {
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeCall {
    pub name: String,
    pub status: CodemodeCallStatus,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeErrorKind {
    Script,
    Timeout,
    Aborted,
    Sandbox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeError {
    pub kind: CodemodeErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeStoreWrites {
    pub set: BTreeMap<String, JsonValue>,
    pub delete: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDuring {
    pub seq: u64,
    pub issued: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CodemodeError>,
    pub output: Vec<CodemodeOutputItem>,
    pub calls: Vec<CodemodeCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_writes: Option<CodemodeStoreWrites>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub during: Option<CodemodeDuring>,
}

pub type CodemodeToolExecuteFn = Arc<
    dyn Fn(
            Option<JsonValue>,
            CancellationToken,
        ) -> BoxFuture<'static, Result<Option<JsonValue>, String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct CodemodeTool {
    pub name: String,
    pub js_name: Option<String>,
    pub description: Option<String>,
    pub input_schema: Option<JsonValue>,
    pub output_schema: Option<JsonValue>,
    pub signature: Option<String>,
    pub spread: bool,
    pub execute: CodemodeToolExecuteFn,
}

impl CodemodeTool {
    pub fn new<F, Fut>(name: impl Into<String>, execute: F) -> Self
    where
        F: Fn(Option<JsonValue>, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<JsonValue>, String>> + Send + 'static,
    {
        let exec = Arc::new(execute);
        Self {
            name: name.into(),
            js_name: None,
            description: None,
            input_schema: None,
            output_schema: None,
            signature: None,
            spread: false,
            execute: Arc::new(move |args, cancel| {
                let exec = exec.clone();
                Box::pin(async move { exec(args, cancel).await })
            }),
        }
    }

    pub fn to_declaration_item(&self) -> CodemodeDeclarationItem {
        CodemodeDeclarationItem {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            output_schema: self.output_schema.clone(),
            signature: self.signature.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDeclaredTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDeclaredGlobal {
    pub name: String,
    #[serde(default)]
    pub spread: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeHostCall {
    pub id: u64,
    pub target: String,
    pub name: String,
    pub args: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<JsonValue>,
}

impl CodemodeOutcome {
    pub fn ok(value: Option<JsonValue>) -> Self {
        Self {
            ok: true,
            value,
            message: None,
            properties: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            value: None,
            message: Some(message.into()),
            properties: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct CodemodeSandboxOptions {
    pub tools: Vec<CodemodeTool>,
    pub globals: Vec<CodemodeTool>,
    pub timeout_ms: Option<u64>,
    pub memory_limit_bytes: Option<usize>,
    pub max_stack_size: Option<usize>,
    pub interrupt_budget: Option<u64>,
}

#[derive(Clone, Default)]
pub struct CodemodeExecuteOptions {
    pub timeout_ms: Option<u64>,
    pub cancel_token: Option<CancellationToken>,
    pub store: BTreeMap<String, JsonValue>,
}

#[derive(Clone)]
pub struct CodemodeRunOptions {
    pub started_at: f64,
    pub seed: u32,
    pub tools: Vec<CodemodeDeclaredTool>,
    pub globals: Vec<CodemodeDeclaredGlobal>,
    pub timeout_ms: Option<u64>,
    pub cancel_token: Option<CancellationToken>,
    pub store: BTreeMap<String, JsonValue>,
    pub on_started: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub on_call: Arc<dyn Fn(CodemodeHostCall) + Send + Sync>,
    pub on_idle: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
}

#[derive(Debug, Clone)]
enum WorkerToHostMessage {
    Started {
        fingerprint: String,
    },
    Idle {
        seq: u64,
        issued: u64,
    },
    Output {
        item: CodemodeOutputItem,
    },
    Call {
        id: u64,
        target: String,
        name: String,
        args: Option<String>,
    },
    Done {
        ok: bool,
        value: Option<String>,
        writes: Option<String>,
        error: Option<String>,
        during: Option<CodemodeDuring>,
    },
    Crash {
        message: String,
    },
}

#[derive(Debug, Clone)]
struct HostToWorkerMessage {
    id: u64,
    ok: bool,
    payload: Option<String>,
    properties: Option<String>,
    seq: Option<u64>,
    now: Option<f64>,
}

fn check_globals(globals: &[CodemodeDeclaredGlobal]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    for global in globals {
        let parts: Vec<&str> = global.name.split('.').collect();
        if parts.is_empty()
            || parts.len() > 2
            || !parts.iter().all(|p| is_identifier_part(p))
            || RESERVED_GLOBALS.contains(&parts[0])
        {
            return Err(format!("Invalid global name \"{}\"", global.name));
        }
        if !names.insert(global.name.clone()) {
            return Err(format!("Global \"{}\" is already registered", global.name));
        }
        if parts.len() == 2 {
            namespaces.insert(parts[0].to_owned());
        }
    }
    for ns in namespaces {
        if names.contains(&ns) {
            return Err(format!(
                "Global \"{ns}\" conflicts with the namespace \"{ns}\""
            ));
        }
    }
    Ok(())
}

fn fnv1a_hex(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

fn parse_bridge_json(json_str: &str, what: &str) -> Result<JsonValue, String> {
    serde_json::from_str(json_str).map_err(|_| {
        format!(
            "Sandbox bridge broken: {what} is not valid JSON. The script may have modified built-ins such as a prototype's toJSON."
        )
    })
}

fn parse_store_writes(json_str: &str) -> Result<CodemodeStoreWrites, String> {
    let parsed = parse_bridge_json(json_str, "store writes")?;
    let Some(entries) = parsed.as_array() else {
        return Err("Sandbox bridge broken: store writes are not an array. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
    };
    let mut writes = CodemodeStoreWrites::default();
    for entry in entries {
        let Some(arr) = entry.as_array() else {
            return Err("Sandbox bridge broken: store writes contain a malformed entry. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
        };
        let Some(key) = arr.first().and_then(|v| v.as_str()) else {
            return Err("Sandbox bridge broken: store writes contain a malformed entry. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
        };
        match arr.len() {
            1 => {
                writes.delete.push(key.to_owned());
            }
            2 => {
                let Some(val_str) = arr[1].as_str() else {
                    return Err("Sandbox bridge broken: store writes contain a malformed entry. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
                };
                let val = parse_bridge_json(val_str, &format!("store value for {key:?}"))?;
                writes.set.insert(key.to_owned(), val);
            }
            _ => {
                return Err("Sandbox bridge broken: store writes contain a malformed entry. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
            }
        }
    }
    Ok(writes)
}

fn parse_script_error(json_str: &str) -> Result<CodemodeError, String> {
    let parsed = parse_bridge_json(json_str, "script error")?;
    let Some(obj) = parsed.as_object() else {
        return Err("Sandbox bridge broken: script error is not an object. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
    };
    let Some(message) = obj.get("message").and_then(|v| v.as_str()) else {
        return Err("Sandbox bridge broken: script error is malformed. The script may have modified built-ins such as a prototype's toJSON.".to_owned());
    };
    let name = match obj.get("name") {
        None => None,
        Some(v) => Some(v.as_str().ok_or_else(|| {
            "Sandbox bridge broken: script error is malformed. The script may have modified built-ins such as a prototype's toJSON.".to_owned()
        })?.to_owned()),
    };
    let stack = match obj.get("stack") {
        None => None,
        Some(v) => Some(v.as_str().ok_or_else(|| {
            "Sandbox bridge broken: script error is malformed. The script may have modified built-ins such as a prototype's toJSON.".to_owned()
        })?.to_owned()),
    };
    Ok(CodemodeError {
        kind: CodemodeErrorKind::Script,
        name,
        message: message.to_owned(),
        stack,
    })
}

fn serialize_store(store: &BTreeMap<String, JsonValue>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in store {
        if let Ok(s) = serde_json::to_string(v) {
            out.insert(k.clone(), s);
        }
    }
    out
}

#[derive(Clone)]
struct VmStartConfig {
    code: String,
    tools_json: String,
    globals_json: String,
    store_json: String,
    memory_limit_bytes: usize,
    max_stack_size: usize,
    interrupt_budget: Option<u64>,
    deterministic: Option<(f64, u32)>,
}

fn describe_caught_exception(ctx: &rquickjs::Ctx<'_>) -> String {
    let exc = ctx.catch();
    if let Some(obj) = exc.as_object() {
        let name = obj
            .get::<_, String>("name")
            .unwrap_or_else(|_| "Error".to_owned());
        let message = obj.get::<_, String>("message").unwrap_or_default();
        let stack = obj.get::<_, String>("stack").unwrap_or_default();
        let head = if message.is_empty() {
            name.clone()
        } else {
            format!("{name}: {message}")
        };
        let stack_trimmed = stack.trim_end();
        let full_stack = if stack_trimmed.is_empty() {
            head
        } else {
            format!("{head}\n{stack_trimmed}")
        };
        return serde_json::to_string(&json!({
            "name": name,
            "message": message,
            "stack": full_stack,
        }))
        .unwrap_or_else(|_| r#"{"message":"JavaScript exception"}"#.to_owned());
    }
    let msg = exc
        .as_string()
        .and_then(|s| s.to_string().ok())
        .unwrap_or_else(|| "JavaScript exception".to_owned());
    serde_json::to_string(&json!({
        "name": "Error",
        "message": msg,
        "stack": msg,
    }))
    .unwrap_or_else(|_| r#"{"message":"JavaScript exception"}"#.to_owned())
}

// The local QuickJS patch applies only to this dedicated VM thread.
unsafe extern "C" {
    fn JS_SetThreadTimezoneOffset(enabled: std::ffi::c_int, minutes: std::ffi::c_int);
}

struct SandboxLifetime(CancellationToken);

impl Drop for SandboxLifetime {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct StopVmOnDrop(Arc<AtomicBool>);

impl Drop for StopVmOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn spawn_vm_worker(
    config: VmStartConfig,
    stop_flag: Arc<AtomicBool>,
    mut host_rx: mpsc::UnboundedReceiver<HostToWorkerMessage>,
    worker_tx: mpsc::UnboundedSender<WorkerToHostMessage>,
    reservation: Option<tokio::sync::OwnedSemaphorePermit>,
    cancel: CancellationToken,
) {
    let handle = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let _reservation = reservation;
        // SAFETY: the hook stores integers in thread-local C storage. No pointers cross FFI.
        unsafe { JS_SetThreadTimezoneOffset(i32::from(config.deterministic.is_some()), 0) };
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = worker_tx.send(WorkerToHostMessage::Crash {
                    message: format!("Failed to create QuickJS runtime: {e}"),
                });
                return;
            }
        };
        if config.memory_limit_bytes > 0 {
            rt.set_memory_limit(config.memory_limit_bytes);
        }
        rt.set_max_stack_size(config.max_stack_size);

        let step_counter = Arc::new(AtomicU64::new(0));
        let budget = config.interrupt_budget;
        let stop_for_interrupt = stop_flag.clone();
        let steps_for_interrupt = step_counter.clone();
        let interrupted_flag = Arc::new(AtomicBool::new(false));
        let interrupted_for_handler = interrupted_flag.clone();

        let cancel_for_interrupt = cancel.clone();
        rt.set_interrupt_handler(Some(Box::new(move || {
            if stop_for_interrupt.load(Ordering::Relaxed) || cancel_for_interrupt.is_cancelled() {
                interrupted_for_handler.store(true, Ordering::SeqCst);
                return true;
            }
            if let Some(max_steps) = budget {
                let current = steps_for_interrupt.fetch_add(1, Ordering::Relaxed) + 1;
                if current > max_steps {
                    interrupted_for_handler.store(true, Ordering::SeqCst);
                    return true;
                }
            }
            false
        })));

        let context = match Context::full(&rt) {
            Ok(ctx) => ctx,
            Err(e) => {
                let _ = worker_tx.send(WorkerToHostMessage::Crash {
                    message: format!("Failed to create QuickJS context: {e}"),
                });
                return;
            }
        };

        context.with(|ctx| {
            let mut set_virtual_now: Option<Function<'_>> = None;
            if let Some((started_at, seed)) = config.deterministic {
                let setup_fn: Function<'_> = match ctx.eval(DETERMINISTIC_SETUP_SOURCE) {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = worker_tx.send(WorkerToHostMessage::Crash {
                            message: format!("Deterministic setup failed: {e}"),
                        });
                        return;
                    }
                };
                match setup_fn.call::<_, Function<'_>>((started_at, seed)) {
                    Ok(f) => set_virtual_now = Some(f),
                    Err(e) => {
                        let _ = worker_tx.send(WorkerToHostMessage::Crash {
                            message: format!("Deterministic clock init failed: {e}"),
                        });
                        return;
                    }
                }
            }

            let issued_cell = Arc::new(AtomicU64::new(0));
            let current_seq_cell = Arc::new(AtomicU64::new(0));
            let done_sent = Arc::new(AtomicBool::new(false));
            let is_deterministic = config.deterministic.is_some();

            let tx_bridge = worker_tx.clone();
            let issued_bridge = issued_cell.clone();
            let current_seq_bridge = current_seq_cell.clone();
            let done_bridge = done_sent.clone();

            let bridge_fn = match Function::new(
                ctx.clone(),
                move |kind: String, rest: Rest<Value<'_>>| {
                    let a = rest.0.first();
                    let b = rest.0.get(1);
                    let c = rest.0.get(2);
                    match kind.as_str() {
                        "call" | "global" => {
                            issued_bridge.fetch_add(1, Ordering::SeqCst);
                            let id = a
                                .and_then(|v| v.as_number())
                                .map(|n| n as u64)
                                .unwrap_or(0);
                            let name = b
                                .and_then(|v| v.as_string())
                                .and_then(|s| s.to_string().ok())
                                .unwrap_or_default();
                            let args = c
                                .and_then(|v| {
                                    if v.is_undefined() {
                                        None
                                    } else {
                                        v.as_string().and_then(|s| s.to_string().ok())
                                    }
                                });
                            let _ = tx_bridge.send(WorkerToHostMessage::Call {
                                id,
                                target: if kind == "call" {
                                    "tool".to_owned()
                                } else {
                                    "global".to_owned()
                                },
                                name,
                                args,
                            });
                        }
                        "output" => {
                            let out_kind = a
                                .and_then(|v| v.as_string())
                                .and_then(|s| s.to_string().ok())
                                .unwrap_or_default();
                            let data = b
                                .and_then(|v| v.as_string())
                                .and_then(|s| s.to_string().ok())
                                .unwrap_or_default();
                            let mime = c
                                .and_then(|v| v.as_string())
                                .and_then(|s| s.to_string().ok())
                                .unwrap_or_default();
                            let item = match out_kind.as_str() {
                                "image" => CodemodeOutputItem::Image {
                                    data,
                                    mime_type: mime,
                                },
                                "console" => CodemodeOutputItem::Text {
                                    text: data,
                                    console: true,
                                },
                                _ => CodemodeOutputItem::Text {
                                    text: data,
                                    console: false,
                                },
                            };
                            let _ = tx_bridge.send(WorkerToHostMessage::Output { item });
                        }
                        "done" => {
                            done_bridge.store(true, Ordering::SeqCst);
                            let during = if is_deterministic {
                                Some(CodemodeDuring {
                                    seq: current_seq_bridge.load(Ordering::SeqCst),
                                    issued: issued_bridge.load(Ordering::SeqCst),
                                })
                            } else {
                                None
                            };
                            let ok = a.and_then(|v| v.as_bool()).unwrap_or(false);
                            if ok {
                                let value = b.and_then(|v| {
                                    if v.is_undefined() {
                                        None
                                    } else {
                                        v.as_string().and_then(|s| s.to_string().ok())
                                    }
                                });
                                let writes = c
                                    .and_then(|v| v.as_string())
                                    .and_then(|s| s.to_string().ok());
                                let _ = tx_bridge.send(WorkerToHostMessage::Done {
                                    ok: true,
                                    value,
                                    writes,
                                    error: None,
                                    during,
                                });
                            } else {
                                let error = b
                                    .and_then(|v| v.as_string())
                                    .and_then(|s| s.to_string().ok());
                                let _ = tx_bridge.send(WorkerToHostMessage::Done {
                                    ok: false,
                                    value: None,
                                    writes: None,
                                    error,
                                    during,
                                });
                            }
                        }
                        _ => {}
                    }
                },
            ) {
                Ok(f) => f,
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Failed to create bridge function: {e}"),
                    });
                    return;
                }
            };

            let mut prelude_opts = EvalOptions::default();
            prelude_opts.filename = Some("codemode-prelude.js".to_owned());
            let prelude_fn: Function<'_> = match ctx.eval_with_options(PRELUDE_SOURCE, prelude_opts) {
                Ok(f) => f,
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Failed to evaluate prelude: {e}"),
                    });
                    return;
                }
            };

            let api: Object<'_> = match prelude_fn.call((
                bridge_fn,
                config.tools_json.as_str(),
                config.globals_json.as_str(),
                config.store_json.as_str(),
            )) {
                Ok(obj) => obj,
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Failed to initialize prelude: {e}"),
                    });
                    return;
                }
            };

            let settle_fn: Function<'_> = match api.get("settle") {
                Ok(f) => f,
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Missing settle callback: {e}"),
                    });
                    return;
                }
            };
            let run_fn: Function<'_> = match api.get("run") {
                Ok(f) => f,
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Missing run callback: {e}"),
                    });
                    return;
                }
            };
            let stalled_fn: Function<'_> = match api.get("stalled") {
                Ok(f) => f,
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Missing stalled callback: {e}"),
                    });
                    return;
                }
            };

            if is_deterministic {
                let prelude_hash = fnv1a_hex(PRELUDE_SOURCE);
                let deterministic_hash = fnv1a_hex(DETERMINISTIC_SETUP_SOURCE);
                let budget_str = config
                    .interrupt_budget
                    .map_or_else(|| "none".to_owned(), |b| b.to_string());
                let fingerprint = format!(
                    "rquickjs 0.14.0-pi-utc1, prelude {prelude_hash}, deterministic 2:{deterministic_hash}, stack {}, heap {}, budget {budget_str}",
                    config.max_stack_size, config.memory_limit_bytes
                );
                let _ = worker_tx.send(WorkerToHostMessage::Started { fingerprint });
            }

            let wrapped_code = format!("(async (tools, console) => {{{}\n}})", config.code);
            let mut script_opts = EvalOptions::default();
            script_opts.filename = Some("codemode.js".to_owned());
            let script_fn: Function<'_> = match ctx.eval_with_options(wrapped_code, script_opts) {
                Ok(f) => f,
                Err(rquickjs::Error::Exception) => {
                    let err_json = describe_caught_exception(&ctx);
                    let _ = worker_tx.send(WorkerToHostMessage::Done {
                        ok: false,
                        value: None,
                        writes: None,
                        error: Some(err_json),
                        during: if is_deterministic {
                            Some(CodemodeDuring {
                                seq: 0,
                                issued: issued_cell.load(Ordering::SeqCst),
                            })
                        } else {
                            None
                        },
                    });
                    return;
                }
                Err(e) => {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!("Script compilation failed: {e}"),
                    });
                    return;
                }
            };

            let drain = |ctx: &rquickjs::Ctx<'_>| -> Result<(), String> {
                let mut jobs = 0usize;
                while ctx.execute_pending_job() {
                    if interrupted_flag.load(Ordering::SeqCst) || stop_flag.load(Ordering::Relaxed)
                    {
                        return Err("InternalError: interrupted".to_owned());
                    }
                    jobs += 1;
                    if jobs.is_multiple_of(JOBS_PER_POLL)
                        && let Some(max_steps) = config.interrupt_budget
                    {
                        let current = step_counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if current > max_steps {
                            interrupted_flag.store(true, Ordering::SeqCst);
                            return Err("InternalError: interrupted".to_owned());
                        }
                    }
                }
                if interrupted_flag.load(Ordering::SeqCst) {
                    return Err("InternalError: interrupted".to_owned());
                }
                stalled_fn
                    .call::<_, Value<'_>>(())
                    .map(|_| ())
                    .map_err(|_| {
                        if interrupted_flag.load(Ordering::SeqCst) {
                            "InternalError: interrupted".to_owned()
                        } else {
                            describe_caught_exception(ctx)
                        }
                    })
            };

            if let Err(e) = run_fn.call::<_, Value<'_>>((script_fn,)) {
                let msg = if interrupted_flag.load(Ordering::SeqCst) {
                    "InternalError: interrupted".to_owned()
                } else {
                    format!("{e}: {}", describe_caught_exception(&ctx))
                };
                let _ = worker_tx.send(WorkerToHostMessage::Crash { message: msg });
                return;
            }
            if let Err(msg) = drain(&ctx) {
                let _ = worker_tx.send(WorkerToHostMessage::Crash { message: msg });
                return;
            }
            if is_deterministic {
                let _ = worker_tx.send(WorkerToHostMessage::Idle {
                    seq: 0,
                    issued: issued_cell.load(Ordering::SeqCst),
                });
            }

            let mut next_seq: u64 = 1;
            let mut early: BTreeMap<u64, HostToWorkerMessage> = BTreeMap::new();

            let apply_message = |msg: HostToWorkerMessage| -> Result<(), String> {
                step_counter.store(0, Ordering::Relaxed);
                let call_res = match (&msg.payload, &msg.properties) {
                    (None, None) => settle_fn.call::<_, Value<'_>>((
                        msg.id,
                        msg.ok,
                        rquickjs::Undefined,
                        rquickjs::Undefined,
                    )),
                    (Some(p), None) => settle_fn.call::<_, Value<'_>>((
                        msg.id,
                        msg.ok,
                        p.as_str(),
                        rquickjs::Undefined,
                    )),
                    (None, Some(props)) => settle_fn.call::<_, Value<'_>>((
                        msg.id,
                        msg.ok,
                        rquickjs::Undefined,
                        props.as_str(),
                    )),
                    (Some(p), Some(props)) => settle_fn.call::<_, Value<'_>>((
                        msg.id,
                        msg.ok,
                        p.as_str(),
                        props.as_str(),
                    )),
                };
                if call_res.is_err() {
                    if interrupted_flag.load(Ordering::SeqCst) {
                        return Err("InternalError: interrupted".to_owned());
                    }
                    return Err(describe_caught_exception(&ctx));
                }
                drain(&ctx)
            };

            while !done_sent.load(Ordering::SeqCst) && !stop_flag.load(Ordering::Relaxed) {
                let Some(msg) = handle.block_on(async {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => None,
                        msg = host_rx.recv() => msg,
                    }
                }) else {
                    break;
                };
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                if !is_deterministic {
                    if let Err(err_msg) = apply_message(msg) {
                        let _ = worker_tx.send(WorkerToHostMessage::Crash { message: err_msg });
                        break;
                    }
                    continue;
                }

                let Some(seq) = msg.seq else {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!(
                            "Error: Result {} has sequence number undefined, expected {next_seq} or later, once",
                            msg.id
                        ),
                    });
                    break;
                };
                if seq < next_seq || early.contains_key(&seq) {
                    let _ = worker_tx.send(WorkerToHostMessage::Crash {
                        message: format!(
                            "Error: Result {} has sequence number {seq}, expected {next_seq} or later, once",
                            msg.id
                        ),
                    });
                    break;
                }
                early.insert(seq, msg);
                while let Some(next_msg) = early.remove(&next_seq) {
                    if let (Some(set_now), Some(now_val)) = (&set_virtual_now, next_msg.now)
                        && now_val.is_finite()
                    {
                        let _ = set_now.call::<_, Value<'_>>((now_val,));
                    }
                    current_seq_cell.store(next_seq, Ordering::SeqCst);
                    if let Err(err_msg) = apply_message(next_msg) {
                        let _ = worker_tx.send(WorkerToHostMessage::Crash { message: err_msg });
                        return;
                    }
                    let _ = worker_tx.send(WorkerToHostMessage::Idle {
                        seq: next_seq,
                        issued: issued_cell.load(Ordering::SeqCst),
                    });
                    next_seq += 1;
                }
            }
        });
    });
}

struct PendingCallEntry {
    call_index: Option<usize>,
    started_at: Instant,
    cancel_token: CancellationToken,
}

type DeliverFn = Arc<dyn Fn(u64, f64, u64, CodemodeOutcome) -> Result<(), String> + Send + Sync>;
type PendingRunMap = Arc<Mutex<BTreeMap<u64, (Option<usize>, Instant)>>>;

pub struct CodemodeRun {
    result_rx: Mutex<Option<oneshot::Receiver<CodemodeResult>>>,
    deliver_fn: DeliverFn,
    finished: Arc<AtomicBool>,
    cancel: CancellationToken,
}

impl Drop for CodemodeRun {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl CodemodeRun {
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    pub fn deliver(
        &self,
        seq: u64,
        now: f64,
        id: u64,
        outcome: CodemodeOutcome,
    ) -> Result<(), String> {
        (self.deliver_fn)(seq, now, id, outcome)
    }

    pub async fn wait(&self) -> CodemodeResult {
        let rx = self
            .result_rx
            .lock()
            .expect("result_rx lock")
            .take()
            .expect("CodemodeRun::wait called more than once");
        rx.await.unwrap_or_else(|_| CodemodeResult {
            ok: false,
            value: None,
            error: Some(CodemodeError {
                kind: CodemodeErrorKind::Sandbox,
                name: None,
                message: "Sandbox execution channel closed".to_owned(),
                stack: None,
            }),
            output: Vec::new(),
            calls: Vec::new(),
            store_writes: None,
            during: None,
        })
    }
}

#[derive(Clone)]
pub struct CodemodeSandbox {
    tools: Arc<Mutex<BTreeMap<String, CodemodeTool>>>,
    tool_order: Arc<Mutex<Vec<String>>>,
    globals: Arc<Mutex<BTreeMap<String, CodemodeTool>>>,
    global_order: Arc<Mutex<Vec<String>>>,
    timeout_ms: u64,
    memory_limit_bytes: usize,
    max_stack_size: usize,
    interrupt_budget: Option<u64>,
    lifetime: Arc<SandboxLifetime>,
}

impl CodemodeSandbox {
    pub fn new(options: CodemodeSandboxOptions) -> Result<Self, String> {
        let declared_globals: Vec<CodemodeDeclaredGlobal> = options
            .globals
            .iter()
            .map(|g| CodemodeDeclaredGlobal {
                name: g.name.clone(),
                spread: g.spread,
            })
            .collect();
        check_globals(&declared_globals)?;

        let mut tools_map = BTreeMap::new();
        let mut tool_order = Vec::new();
        for tool in options.tools {
            if tools_map.contains_key(&tool.name) {
                return Err(format!("Tool \"{}\" is already registered", tool.name));
            }
            tool_order.push(tool.name.clone());
            tools_map.insert(tool.name.clone(), tool);
        }

        let mut globals_map = BTreeMap::new();
        let mut global_order = Vec::new();
        for global in options.globals {
            global_order.push(global.name.clone());
            globals_map.insert(global.name.clone(), global);
        }

        Ok(Self {
            tools: Arc::new(Mutex::new(tools_map)),
            tool_order: Arc::new(Mutex::new(tool_order)),
            globals: Arc::new(Mutex::new(globals_map)),
            global_order: Arc::new(Mutex::new(global_order)),
            timeout_ms: options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            memory_limit_bytes: options
                .memory_limit_bytes
                .unwrap_or(DEFAULT_MEMORY_LIMIT_BYTES),
            max_stack_size: options
                .max_stack_size
                .unwrap_or(DEFAULT_MAX_STACK_SIZE_BYTES),
            interrupt_budget: options.interrupt_budget,
            lifetime: Arc::new(SandboxLifetime(CancellationToken::new())),
        })
    }

    pub fn register_tool(&self, tool: CodemodeTool) -> Result<(), String> {
        let mut tools = self.tools.lock().expect("tools lock");
        if tools.contains_key(&tool.name) {
            return Err(format!("Tool \"{}\" is already registered", tool.name));
        }
        self.tool_order
            .lock()
            .expect("order lock")
            .push(tool.name.clone());
        tools.insert(tool.name.clone(), tool);
        Ok(())
    }

    pub fn unregister_tool(&self, name: &str) -> bool {
        let removed = self
            .tools
            .lock()
            .expect("tools lock")
            .remove(name)
            .is_some();
        if removed {
            self.tool_order
                .lock()
                .expect("order lock")
                .retain(|n| n != name);
        }
        removed
    }

    pub fn tools(&self) -> Vec<CodemodeTool> {
        let tools = self.tools.lock().expect("tools lock");
        let order = self.tool_order.lock().expect("order lock");
        order
            .iter()
            .filter_map(|name| tools.get(name).cloned())
            .collect()
    }

    pub fn globals(&self) -> Vec<CodemodeTool> {
        let globals = self.globals.lock().expect("globals lock");
        let order = self.global_order.lock().expect("order lock");
        order
            .iter()
            .filter_map(|name| globals.get(name).cloned())
            .collect()
    }

    pub async fn close(&self) {
        self.lifetime.0.cancel();
    }

    pub async fn execute(&self, code: &str, options: CodemodeExecuteOptions) -> CodemodeResult {
        self.execute_reserved(code, options, None).await
    }

    pub(crate) async fn execute_reserved(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
        reservation: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> CodemodeResult {
        if self.lifetime.0.is_cancelled() {
            return CodemodeResult {
                ok: false,
                value: None,
                error: Some(CodemodeError {
                    kind: CodemodeErrorKind::Sandbox,
                    name: None,
                    message: "Sandbox is closed".to_owned(),
                    stack: None,
                }),
                output: Vec::new(),
                calls: Vec::new(),
                store_writes: None,
                during: None,
            };
        }

        let tools_snapshot = self.tools();
        let globals_snapshot = self.globals();

        let declared_tools: Vec<JsonValue> = tools_snapshot
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "jsName": t.js_name.clone().unwrap_or_else(|| to_codemode_identifier(&t.name)),
                    "description": t.description.clone().unwrap_or_default(),
                })
            })
            .collect();
        let declared_globals: Vec<JsonValue> = globals_snapshot
            .iter()
            .map(|g| {
                json!({
                    "name": g.name,
                    "spread": g.spread,
                })
            })
            .collect();

        let tools_by_name: BTreeMap<String, CodemodeTool> = tools_snapshot
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();
        let globals_by_name: BTreeMap<String, CodemodeTool> = globals_snapshot
            .into_iter()
            .map(|g| (g.name.clone(), g))
            .collect();

        let timeout_ms = options.timeout_ms.unwrap_or(self.timeout_ms);
        let run_cancel = self.lifetime.0.child_token();

        if let Some(caller_cancel) = &options.cancel_token
            && caller_cancel.is_cancelled()
        {
            return CodemodeResult {
                ok: false,
                value: None,
                error: Some(CodemodeError {
                    kind: CodemodeErrorKind::Aborted,
                    name: None,
                    message: "Execution aborted".to_owned(),
                    stack: None,
                }),
                output: Vec::new(),
                calls: Vec::new(),
                store_writes: None,
                during: None,
            };
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (host_tx, host_rx) = mpsc::unbounded_channel::<HostToWorkerMessage>();
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<WorkerToHostMessage>();

        let config = VmStartConfig {
            code: code.to_owned(),
            tools_json: serde_json::to_string(&declared_tools).unwrap_or_else(|_| "[]".to_owned()),
            globals_json: serde_json::to_string(&declared_globals)
                .unwrap_or_else(|_| "[]".to_owned()),
            store_json: serde_json::to_string(&serialize_store(&options.store))
                .unwrap_or_else(|_| "{}".to_owned()),
            memory_limit_bytes: self.memory_limit_bytes,
            max_stack_size: self.max_stack_size,
            interrupt_budget: self.interrupt_budget,
            deterministic: None,
        };

        let _cancel_on_drop = run_cancel.clone().drop_guard();
        let _stop_on_drop = StopVmOnDrop(stop_flag.clone());
        spawn_vm_worker(
            config,
            stop_flag.clone(),
            host_rx,
            worker_tx.clone(),
            reservation,
            run_cancel.clone(),
        );

        let mut output: Vec<CodemodeOutputItem> = Vec::new();
        let mut calls: Vec<CodemodeCall> = Vec::new();
        let mut pending: BTreeMap<u64, PendingCallEntry> = BTreeMap::new();
        let (internal_reply_tx, mut internal_reply_rx) =
            mpsc::unbounded_channel::<(u64, CodemodeCallStatus, HostToWorkerMessage)>();

        let sleep_fut = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(sleep_fut);

        let caller_cancel = options.cancel_token.unwrap_or_default();

        let final_result = loop {
            tokio::select! {
                biased;
                () = caller_cancel.cancelled() => {
                    stop_flag.store(true, Ordering::SeqCst);
                    break Err(CodemodeError {
                        kind: CodemodeErrorKind::Aborted,
                        name: None,
                        message: "Execution aborted".to_owned(),
                        stack: None,
                    });
                }
                () = run_cancel.cancelled() => {
                    stop_flag.store(true, Ordering::SeqCst);
                    break Err(CodemodeError {
                        kind: CodemodeErrorKind::Aborted,
                        name: None,
                        message: "Sandbox closed".to_owned(),
                        stack: None,
                    });
                }
                () = &mut sleep_fut => {
                    stop_flag.store(true, Ordering::SeqCst);
                    break Err(CodemodeError {
                        kind: CodemodeErrorKind::Timeout,
                        name: None,
                        message: format!("Execution timed out after {timeout_ms} ms"),
                        stack: None,
                    });
                }
                Some((id, status, reply)) = internal_reply_rx.recv() => {
                    if let Some(entry) = pending.remove(&id) {
                        if let Some(idx) = entry.call_index {
                            calls[idx].status = status;
                            calls[idx].duration_ms = entry.started_at.elapsed().as_secs_f64() * 1000.0;
                        }
                        let _ = host_tx.send(reply);
                    }
                }
                msg_opt = worker_rx.recv() => {
                    let Some(msg) = msg_opt else {
                        break Err(CodemodeError {
                            kind: CodemodeErrorKind::Sandbox,
                            name: None,
                            message: "Worker exited unexpectedly".to_owned(),
                            stack: None,
                        });
                    };
                    match msg {
                        WorkerToHostMessage::Started { .. } | WorkerToHostMessage::Idle { .. } => {}
                        WorkerToHostMessage::Output { item } => {
                            output.push(item);
                        }
                        WorkerToHostMessage::Call { id, target, name, args } => {
                            if pending.contains_key(&id) {
                                stop_flag.store(true, Ordering::SeqCst);
                                break Err(CodemodeError {
                                    kind: CodemodeErrorKind::Sandbox,
                                    name: None,
                                    message: format!("Sandbox bridge broken: duplicate call id {id}. The script may have modified built-ins such as a prototype's toJSON."),
                                    stack: None,
                                });
                            }
                            let is_tool = target == "tool";
                            let call_index = if is_tool {
                                let idx = calls.len();
                                calls.push(CodemodeCall {
                                    name: name.clone(),
                                    status: CodemodeCallStatus::Cancelled,
                                    duration_ms: 0.0,
                                });
                                Some(idx)
                            } else {
                                None
                            };
                            let call_cancel = run_cancel.child_token();
                            pending.insert(id, PendingCallEntry {
                                call_index,
                                started_at: Instant::now(),
                                cancel_token: call_cancel.clone(),
                            });

                            let tool_opt = if is_tool {
                                tools_by_name.get(&name).cloned()
                            } else {
                                globals_by_name.get(&name).cloned()
                            };
                            let reply_tx = internal_reply_tx.clone();
                            tokio::spawn(async move {
                                let outcome = match tool_opt {
                                    None => Err(format!(
                                        "Unknown {} \"{name}\"",
                                        if is_tool { "tool" } else { "global" }
                                    )),
                                    Some(tool) => {
                                        let parsed_args = match args {
                                            None => Ok(None),
                                            Some(raw) => serde_json::from_str::<JsonValue>(&raw)
                                                .map(Some)
                                                .map_err(|e| e.to_string()),
                                        };
                                        match parsed_args {
                                            Err(e) => Err(e),
                                            Ok(val) => (tool.execute)(val, call_cancel).await,
                                        }
                                    }
                                };
                                match outcome {
                                    Ok(val_opt) => {
                                        let payload = val_opt.and_then(|v| serde_json::to_string(&v).ok());
                                        let _ = reply_tx.send((
                                            id,
                                            CodemodeCallStatus::Ok,
                                            HostToWorkerMessage {
                                                id,
                                                ok: true,
                                                payload,
                                                properties: None,
                                                seq: None,
                                                now: None,
                                            },
                                        ));
                                    }
                                    Err(err_msg) => {
                                        let _ = reply_tx.send((
                                            id,
                                            CodemodeCallStatus::Error,
                                            HostToWorkerMessage {
                                                id,
                                                ok: false,
                                                payload: Some(err_msg),
                                                properties: None,
                                                seq: None,
                                                now: None,
                                            },
                                        ));
                                    }
                                }
                            });
                        }
                        WorkerToHostMessage::Done { ok, value, writes, error, during } => {
                            stop_flag.store(true, Ordering::SeqCst);
                            if !ok {
                                let err_str = error.unwrap_or_default();
                                match parse_script_error(&err_str) {
                                    Ok(script_err) => break Err(script_err),
                                    Err(bridge_err) => break Err(CodemodeError {
                                        kind: CodemodeErrorKind::Sandbox,
                                        name: None,
                                        message: bridge_err,
                                        stack: None,
                                    }),
                                }
                            }
                            let parsed_val = match value {
                                None => None,
                                Some(raw) => match parse_bridge_json(&raw, "return value") {
                                    Ok(v) => Some(v),
                                    Err(bridge_err) => break Err(CodemodeError {
                                        kind: CodemodeErrorKind::Sandbox,
                                        name: None,
                                        message: bridge_err,
                                        stack: None,
                                    }),
                                },
                            };
                            let parsed_writes = match writes {
                                None => CodemodeStoreWrites::default(),
                                Some(raw) => match parse_store_writes(&raw) {
                                    Ok(w) => w,
                                    Err(bridge_err) => break Err(CodemodeError {
                                        kind: CodemodeErrorKind::Sandbox,
                                        name: None,
                                        message: bridge_err,
                                        stack: None,
                                    }),
                                },
                            };
                            break Ok((parsed_val, parsed_writes, during));
                        }
                        WorkerToHostMessage::Crash { message } => {
                            stop_flag.store(true, Ordering::SeqCst);
                            break Err(CodemodeError {
                                kind: CodemodeErrorKind::Sandbox,
                                name: None,
                                message,
                                stack: None,
                            });
                        }
                    }
                }
            }
        };

        stop_flag.store(true, Ordering::SeqCst);
        for (_, entry) in pending {
            if let Some(idx) = entry.call_index {
                calls[idx].duration_ms = entry.started_at.elapsed().as_secs_f64() * 1000.0;
            }
            entry.cancel_token.cancel();
        }

        match final_result {
            Ok((value, store_writes, during)) => CodemodeResult {
                ok: true,
                value,
                error: None,
                output,
                calls,
                store_writes: Some(store_writes),
                during,
            },
            Err(error) => CodemodeResult {
                ok: false,
                value: None,
                error: Some(error),
                output,
                calls,
                store_writes: None,
                during: None,
            },
        }
    }

    pub fn start(&self, code: &str, options: CodemodeRunOptions) -> Result<CodemodeRun, String> {
        if self.lifetime.0.is_cancelled() {
            return Err("Sandbox is closed".to_owned());
        }
        if !options.started_at.is_finite() || options.started_at < 0.0 {
            return Err(
                "startedAt must be a non-negative number of milliseconds since the epoch"
                    .to_owned(),
            );
        }
        let mut seen_names = BTreeSet::new();
        let mut seen_js_names = BTreeSet::new();
        for tool in &options.tools {
            if !seen_names.insert(tool.name.clone()) {
                return Err("Tool names must be unique".to_owned());
            }
            let js_name = tool
                .js_name
                .clone()
                .unwrap_or_else(|| to_codemode_identifier(&tool.name));
            if !seen_js_names.insert(js_name) {
                return Err("Tool script names must be unique".to_owned());
            }
        }
        check_globals(&options.globals)?;

        let declared_tools: Vec<JsonValue> = options
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "jsName": t.js_name.clone().unwrap_or_else(|| to_codemode_identifier(&t.name)),
                    "description": t.description.clone().unwrap_or_default(),
                })
            })
            .collect();
        let declared_globals: Vec<JsonValue> = options
            .globals
            .iter()
            .map(|g| {
                json!({
                    "name": g.name,
                    "spread": g.spread,
                })
            })
            .collect();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let (host_tx, host_rx) = mpsc::unbounded_channel::<HostToWorkerMessage>();
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<WorkerToHostMessage>();
        let (result_tx, result_rx) = oneshot::channel::<CodemodeResult>();

        let config = VmStartConfig {
            code: code.to_owned(),
            tools_json: serde_json::to_string(&declared_tools).unwrap_or_else(|_| "[]".to_owned()),
            globals_json: serde_json::to_string(&declared_globals)
                .unwrap_or_else(|_| "[]".to_owned()),
            store_json: serde_json::to_string(&serialize_store(&options.store))
                .unwrap_or_else(|_| "{}".to_owned()),
            memory_limit_bytes: self.memory_limit_bytes,
            max_stack_size: self.max_stack_size,
            interrupt_budget: self.interrupt_budget,
            deterministic: Some((options.started_at, options.seed)),
        };

        let pending_map: PendingRunMap = Arc::new(Mutex::new(BTreeMap::new()));
        let calls_list: Arc<Mutex<Vec<CodemodeCall>>> = Arc::new(Mutex::new(Vec::new()));
        let delivered_seqs: Arc<Mutex<BTreeSet<u64>>> = Arc::new(Mutex::new(BTreeSet::new()));

        let finished_deliver = finished.clone();
        let pending_deliver = pending_map.clone();
        let calls_deliver = calls_list.clone();
        let seqs_deliver = delivered_seqs.clone();
        let host_tx_deliver = host_tx.clone();

        let deliver_fn: DeliverFn = Arc::new(move |seq, now, id, outcome| {
            if finished_deliver.load(Ordering::SeqCst) {
                return Ok(());
            }
            if !now.is_finite() {
                return Err(format!(
                    "The clock value of result {seq} is not a finite number"
                ));
            }
            if seq < 1 || seqs_deliver.lock().expect("seqs lock").contains(&seq) {
                return Err(format!(
                    "Result number {seq} is not a positive integer used once"
                ));
            }
            if !pending_deliver
                .lock()
                .expect("pending lock")
                .contains_key(&id)
            {
                return Err(format!("Call {id} is not waiting for a result"));
            }
            if !outcome.ok
                && let Some(props) = &outcome.properties
                && !props.is_object()
            {
                return Err(format!("The properties of result {seq} are not an object"));
            }

            let payload = if outcome.ok {
                outcome
                    .value
                    .as_ref()
                    .and_then(|v| serde_json::to_string(v).ok())
            } else {
                outcome.message.clone()
            };
            let properties = if !outcome.ok {
                outcome
                    .properties
                    .as_ref()
                    .and_then(|p| serde_json::to_string(p).ok())
            } else {
                None
            };

            seqs_deliver.lock().expect("seqs lock").insert(seq);
            if let Some((call_idx, started_at)) =
                pending_deliver.lock().expect("pending lock").remove(&id)
                && let Some(idx) = call_idx
            {
                let mut calls = calls_deliver.lock().expect("calls lock");
                calls[idx].status = if outcome.ok {
                    CodemodeCallStatus::Ok
                } else {
                    CodemodeCallStatus::Error
                };
                calls[idx].duration_ms = started_at.elapsed().as_secs_f64() * 1000.0;
            }

            let _ = host_tx_deliver.send(HostToWorkerMessage {
                id,
                ok: outcome.ok,
                payload,
                properties,
                seq: Some(seq),
                now: Some(now),
            });
            Ok(())
        });

        let run_cancel = self.lifetime.0.child_token();
        spawn_vm_worker(
            config,
            stop_flag.clone(),
            host_rx,
            worker_tx,
            None,
            run_cancel.clone(),
        );

        let finished_task = finished.clone();
        let timeout_ms = options.timeout_ms.unwrap_or(u64::MAX);
        let caller_cancel = options.cancel_token.unwrap_or_default();
        let on_started = options.on_started;
        let on_call = options.on_call;
        let on_idle = options.on_idle;

        let cancel_task = run_cancel.clone();
        let stop_on_drop = StopVmOnDrop(stop_flag.clone());
        tokio::spawn(async move {
            let _stop_on_drop = stop_on_drop;
            let _cancel_on_drop = cancel_task.clone().drop_guard();
            let mut output = Vec::new();
            let sleep_fut = tokio::time::sleep(Duration::from_millis(timeout_ms.min(86_400_000)));
            tokio::pin!(sleep_fut);

            let outcome = loop {
                tokio::select! {
                    biased;
                    () = caller_cancel.cancelled() => {
                        stop_flag.store(true, Ordering::SeqCst);
                        break Err((CodemodeError {
                            kind: CodemodeErrorKind::Aborted,
                            name: None,
                            message: "Execution aborted".to_owned(),
                            stack: None,
                        }, None));
                    }
                    () = cancel_task.cancelled() => {
                        break Err((CodemodeError {
                            kind: CodemodeErrorKind::Aborted,
                            name: None,
                            message: "Sandbox run closed".to_owned(),
                            stack: None,
                        }, None));
                    }
                    () = &mut sleep_fut, if timeout_ms < u64::MAX => {
                        stop_flag.store(true, Ordering::SeqCst);
                        break Err((CodemodeError {
                            kind: CodemodeErrorKind::Timeout,
                            name: None,
                            message: format!("Execution timed out after {timeout_ms} ms"),
                            stack: None,
                        }, None));
                    }
                    msg_opt = worker_rx.recv() => {
                        let Some(msg) = msg_opt else {
                            break Err((CodemodeError {
                                kind: CodemodeErrorKind::Sandbox,
                                name: None,
                                message: "Worker exited unexpectedly".to_owned(),
                                stack: None,
                            }, None));
                        };
                        match msg {
                            WorkerToHostMessage::Started { fingerprint } => {
                                if let Some(cb) = &on_started {
                                    cb(fingerprint);
                                }
                            }
                            WorkerToHostMessage::Idle { seq, issued } => {
                                if let Some(cb) = &on_idle {
                                    cb(seq, issued);
                                }
                            }
                            WorkerToHostMessage::Output { item } => {
                                output.push(item);
                            }
                            WorkerToHostMessage::Call { id, target, name, args } => {
                                let parsed_args = match args {
                                    None => None,
                                    Some(raw) => match parse_bridge_json(&raw, "call arguments") {
                                        Ok(v) => Some(v),
                                        Err(bridge_err) => {
                                            stop_flag.store(true, Ordering::SeqCst);
                                            break Err((CodemodeError {
                                                kind: CodemodeErrorKind::Sandbox,
                                                name: None,
                                                message: bridge_err,
                                                stack: None,
                                            }, None));
                                        }
                                    }
                                };
                                let call_idx = if target == "tool" {
                                    let mut calls = calls_list.lock().expect("calls lock");
                                    let idx = calls.len();
                                    calls.push(CodemodeCall {
                                        name: name.clone(),
                                        status: CodemodeCallStatus::Cancelled,
                                        duration_ms: 0.0,
                                    });
                                    Some(idx)
                                } else {
                                    None
                                };
                                pending_map.lock().expect("pending lock").insert(id, (call_idx, Instant::now()));
                                on_call(CodemodeHostCall {
                                    id,
                                    target,
                                    name,
                                    args: parsed_args,
                                });
                            }
                            WorkerToHostMessage::Done { ok, value, writes, error, during } => {
                                stop_flag.store(true, Ordering::SeqCst);
                                if !ok {
                                    let err_str = error.unwrap_or_default();
                                    match parse_script_error(&err_str) {
                                        Ok(script_err) => break Err((script_err, during)),
                                        Err(bridge_err) => break Err((CodemodeError {
                                            kind: CodemodeErrorKind::Sandbox,
                                            name: None,
                                            message: bridge_err,
                                            stack: None,
                                        }, during)),
                                    }
                                }
                                let parsed_val = match value {
                                    None => None,
                                    Some(raw) => match parse_bridge_json(&raw, "return value") {
                                        Ok(v) => Some(v),
                                        Err(bridge_err) => break Err((CodemodeError {
                                            kind: CodemodeErrorKind::Sandbox,
                                            name: None,
                                            message: bridge_err,
                                            stack: None,
                                        }, during)),
                                    },
                                };
                                let parsed_writes = match writes {
                                    None => CodemodeStoreWrites::default(),
                                    Some(raw) => match parse_store_writes(&raw) {
                                        Ok(w) => w,
                                        Err(bridge_err) => break Err((CodemodeError {
                                            kind: CodemodeErrorKind::Sandbox,
                                            name: None,
                                            message: bridge_err,
                                            stack: None,
                                        }, during)),
                                    },
                                };
                                break Ok((parsed_val, parsed_writes, during));
                            }
                            WorkerToHostMessage::Crash { message } => {
                                stop_flag.store(true, Ordering::SeqCst);
                                break Err((CodemodeError {
                                    kind: CodemodeErrorKind::Sandbox,
                                    name: None,
                                    message,
                                    stack: None,
                                }, None));
                            }
                        }
                    }
                }
            };

            finished_task.store(true, Ordering::SeqCst);
            stop_flag.store(true, Ordering::SeqCst);
            let calls = calls_list.lock().expect("calls lock").clone();
            let res = match outcome {
                Ok((value, store_writes, during)) => CodemodeResult {
                    ok: true,
                    value,
                    error: None,
                    output,
                    calls,
                    store_writes: Some(store_writes),
                    during,
                },
                Err((error, during)) => CodemodeResult {
                    ok: false,
                    value: None,
                    error: Some(error),
                    output,
                    calls,
                    store_writes: None,
                    during,
                },
            };
            let _ = result_tx.send(res);
        });

        Ok(CodemodeRun {
            result_rx: Mutex::new(Some(result_rx)),
            deliver_fn,
            finished,
            cancel: run_cancel,
        })
    }
}

pub use crate::codemode::prelude::{
    MAX_OUTPUT_CHARS as CODEMODE_MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS as CODEMODE_MAX_OUTPUT_ITEMS,
    MAX_STORE_TOTAL_CHARS as CODEMODE_MAX_STORE_TOTAL_CHARS,
    MAX_STORE_VALUE_CHARS as CODEMODE_MAX_STORE_VALUE_CHARS,
};
