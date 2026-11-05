use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use pi_agent::QueueMode;
use pi_ai::{ModelThinkingLevel, RetryPolicy};
use pi_session::{CompactionSettings, DEFAULT_COMPACTION_SETTINGS, TreeFilterMode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::auth_storage::{
    AtomicWriteOutcome, DedicatedFileLockGuard, atomic_write_file_sync, strip_utf8_bom,
};

fn require_synced(outcome: AtomicWriteOutcome) -> Result<(), String> {
    match outcome {
        AtomicWriteOutcome::CommittedAndSynced => Ok(()),
        AtomicWriteOutcome::CommittedWithParentSyncWarning(_) => Err(
            "Settings committed but directory sync failed; reload before writing again".to_owned(),
        ),
    }
}

pub const CONFIG_DIR_NAME: &str = ".pi";
pub const DEFAULT_TOOL_NAMES: &[&str] = &["read", "bash", "edit", "write"];
pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsScope {
    #[default]
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingMode {
    Off,
    #[default]
    Streaming,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummarySettings {
    pub reserve_tokens: u64,
    pub skip_prompt: bool,
}

impl Default for BranchSummarySettings {
    fn default() -> Self {
        Self {
            reserve_tokens: 16_384,
            skip_prompt: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSettings {
    pub auto_resize: bool,
    pub block_images: bool,
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            auto_resize: true,
            block_images: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetrySettings {
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    pub scope: SettingsScope,
    pub path: Option<PathBuf>,
    pub message: String,
}

/// 剥离 UTF-8 BOM 与 JSON 单行/多行注释（保持字符串字面量内的 `//` 与 `/*` 原样不变）。
pub fn strip_json_comments(input: &str) -> String {
    let text = strip_utf8_bom(input);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut in_string = false;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                let ch = text[i..].chars().next().unwrap_or('\\');
                out.push(ch);
                i += ch.len_utf8();
                if i < bytes.len() {
                    let next_ch = text[i..].chars().next().unwrap_or(' ');
                    out.push(next_ch);
                    i += next_ch.len_utf8();
                }
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            let ch = text[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        } else {
            if bytes[i] == b'"' {
                in_string = true;
                out.push('"');
                i += 1;
                continue;
            }
            if bytes[i] == b'/' && i + 1 < bytes.len() {
                if bytes[i + 1] == b'/' {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                if bytes[i + 1] == b'*' {
                    let comment_start = i;
                    out.push(' ');
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        if bytes[i] == b'\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                    if i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        // Keep invalid syntax invalid instead of accepting a truncated configuration.
                        out.push_str(&text[comment_start..]);
                        i = bytes.len();
                    }
                    continue;
                }
            }
            let ch = text[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }

    out
}

pub fn is_tool_modifier(entry: &str) -> bool {
    entry.starts_with('+') || entry.starts_with('-')
}

/// 校验 `--tools` 或 SDK `tools` 列表：
/// 禁止将普通工具名与 `+name`/`-name` 修饰项混写，且 `+name`/`-name` 不允许包含 `*` 通配符。
pub fn get_tool_list_error(entries: &[String]) -> Option<String> {
    let modifiers: Vec<&String> = entries.iter().filter(|e| is_tool_modifier(e)).collect();
    if modifiers.is_empty() {
        return None;
    }
    if modifiers.len() < entries.len() {
        return Some("tool names cannot be mixed with +name or -name entries".to_owned());
    }
    if let Some(pattern) = modifiers.into_iter().find(|e| e.contains('*')) {
        return Some(format!(
            "+name and -name entries take exact tool names, not patterns: {pattern}"
        ));
    }
    None
}

pub fn apply_tool_modifiers(base: &[String], entries: &[String]) -> Vec<String> {
    let mut tools = base.to_vec();
    for entry in entries {
        if !is_tool_modifier(entry) {
            continue;
        }
        let name = &entry[1..];
        if entry.starts_with('+') {
            if !name.is_empty() && !tools.iter().any(|t| t == name) {
                tools.push(name.to_owned());
            }
        } else if entry.starts_with('-')
            && let Some(pos) = tools.iter().position(|t| t == name)
        {
            tools.remove(pos);
        }
    }
    tools
}

fn merge_default_tools_values(base: Option<&Value>, overrides: Option<&Value>) -> Option<Value> {
    let Some(ov) = overrides else {
        return base.cloned();
    };
    let Some(base_arr) = base.and_then(Value::as_array) else {
        return Some(ov.clone());
    };
    let Some(ov_arr) = ov.as_array() else {
        return Some(ov.clone());
    };
    let all_modifiers = ov_arr
        .iter()
        .all(|item| item.as_str().is_some_and(is_tool_modifier));
    if !all_modifiers {
        return Some(ov.clone());
    }
    let mut combined = base_arr.clone();
    combined.extend(ov_arr.iter().cloned());
    Some(Value::Array(combined))
}

pub fn resolve_default_tools(entries: &[String]) -> Vec<String> {
    let plain: Vec<String> = entries
        .iter()
        .filter(|e| !is_tool_modifier(e))
        .cloned()
        .collect();
    let base: Vec<String> = if !plain.is_empty() || entries.is_empty() {
        plain
    } else {
        DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect()
    };
    apply_tool_modifiers(&base, entries)
}

fn deep_merge_objects(
    base: &Map<String, Value>,
    overrides: &Map<String, Value>,
) -> Map<String, Value> {
    let mut result = base.clone();
    for (key, override_val) in overrides {
        if override_val.is_null() {
            result.remove(key);
            continue;
        }
        match (result.get(key), override_val) {
            (Some(Value::Object(b_obj)), Value::Object(o_obj)) => {
                result.insert(key.clone(), Value::Object(deep_merge_objects(b_obj, o_obj)));
            }
            _ => {
                result.insert(key.clone(), override_val.clone());
            }
        }
    }
    result
}

fn deep_merge_settings(
    base: &Map<String, Value>,
    overrides: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = deep_merge_objects(base, overrides);
    if let Some(dt) =
        merge_default_tools_values(base.get("defaultTools"), overrides.get("defaultTools"))
    {
        merged.insert("defaultTools".to_owned(), dt);
    }
    merged
}

/// 当 `project_trusted == false` 时，剥离项目级设置中的可执行或高风险路径项。
fn filter_untrusted_project_settings(project: &Map<String, Value>) -> Map<String, Value> {
    const TRUST_REQUIRED_KEYS: &[&str] = &[
        "shellPath",
        "shellCommandPrefix",
        "packages",
        "extensions",
        "skills",
        "prompts",
        "themes",
    ];
    let mut filtered = project.clone();
    for k in TRUST_REQUIRED_KEYS {
        filtered.remove(*k);
    }
    filtered
}

pub type SettingsUpdate<'a> = dyn FnMut(Option<&str>) -> Result<Option<String>, String> + 'a;

pub trait SettingsStorage: Send + Sync {
    fn path_for(&self, scope: SettingsScope) -> Option<PathBuf>;
    fn with_lock(&self, scope: SettingsScope, f: &mut SettingsUpdate<'_>) -> Result<(), String>;
}

pub struct FileSettingsStorage {
    global_path: PathBuf,
    project_path: PathBuf,
}

impl FileSettingsStorage {
    pub fn new(cwd: impl AsRef<Path>, agent_dir: impl AsRef<Path>) -> Self {
        Self {
            global_path: agent_dir.as_ref().join("settings.json"),
            project_path: cwd.as_ref().join(CONFIG_DIR_NAME).join("settings.json"),
        }
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn path_for(&self, scope: SettingsScope) -> Option<PathBuf> {
        Some(match scope {
            SettingsScope::Global => self.global_path.clone(),
            SettingsScope::Project => self.project_path.clone(),
        })
    }

    fn with_lock(&self, scope: SettingsScope, f: &mut SettingsUpdate<'_>) -> Result<(), String> {
        let path = match scope {
            SettingsScope::Global => &self.global_path,
            SettingsScope::Project => &self.project_path,
        };

        // 仅当文件已存在时才先加锁读取；避免只读加载不存在的 settings.json 时在磁盘创建空目录或锁文件
        let mut _lock_guard = if path.exists() {
            Some(DedicatedFileLockGuard::acquire(path)?)
        } else {
            None
        };

        let current_content = if path.exists() {
            let mut raw = String::new();
            File::open(path)
                .and_then(|mut file| file.read_to_string(&mut raw))
                .map_err(|_| "Failed to read settings.json".to_owned())?;
            Some(raw)
        } else {
            None
        };

        let next = f(current_content.as_deref())?;
        if let Some(next_text) = next {
            if _lock_guard.is_none() {
                _lock_guard = Some(DedicatedFileLockGuard::acquire(path)?);
                // 获取锁后若发现其他进程刚刚创建了该文件，重读一次再执行回调以防丢写
                if path.exists() && current_content.is_none() {
                    let mut raw = String::new();
                    File::open(path)
                        .and_then(|mut file| file.read_to_string(&mut raw))
                        .map_err(|_| "Failed to read settings.json".to_owned())?;
                    if let Some(recomputed) = f(Some(&raw))? {
                        require_synced(atomic_write_file_sync(
                            path,
                            recomputed.as_bytes(),
                            0o600,
                        )?)?;
                    }
                    return Ok(());
                }
            }
            require_synced(atomic_write_file_sync(path, next_text.as_bytes(), 0o600)?)?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemorySettingsStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
}

impl InMemorySettingsStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_initial(global: Option<Value>, project: Option<Value>) -> Self {
        Self {
            global: Mutex::new(
                global.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()),
            ),
            project: Mutex::new(
                project.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()),
            ),
        }
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn path_for(&self, _scope: SettingsScope) -> Option<PathBuf> {
        None
    }

    fn with_lock(&self, scope: SettingsScope, f: &mut SettingsUpdate<'_>) -> Result<(), String> {
        let mut slot = match scope {
            SettingsScope::Global => self.global.lock().expect("global lock"),
            SettingsScope::Project => self.project.lock().expect("project lock"),
        };
        if let Some(next) = f(slot.as_deref())? {
            *slot = Some(next);
        }
        Ok(())
    }
}

struct SettingsManagerState {
    global_settings: Map<String, Value>,
    project_settings: Map<String, Value>,
    runtime_overrides: Map<String, Value>,
    effective: Map<String, Value>,
    global_load_failed: bool,
    project_load_failed: bool,
    project_trusted: bool,
    errors: Vec<SettingsError>,
}

/// 全局与项目双层设置管理器（对照 `SettingsManager`）。
#[derive(Clone)]
pub struct SettingsManager {
    storage: Arc<dyn SettingsStorage>,
    state: Arc<RwLock<SettingsManagerState>>,
    update_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for SettingsManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.state.read().expect("state lock");
        f.debug_struct("SettingsManager")
            .field("project_trusted", &st.project_trusted)
            .field("global_load_failed", &st.global_load_failed)
            .field("project_load_failed", &st.project_load_failed)
            .field("error_count", &st.errors.len())
            .finish()
    }
}

impl SettingsManager {
    pub fn create(cwd: impl AsRef<Path>, agent_dir: impl AsRef<Path>) -> Self {
        Self::create_with_trust(cwd, agent_dir, true)
    }

    pub fn create_with_trust(
        cwd: impl AsRef<Path>,
        agent_dir: impl AsRef<Path>,
        project_trusted: bool,
    ) -> Self {
        let storage = Arc::new(FileSettingsStorage::new(cwd, agent_dir));
        Self::from_storage(storage, project_trusted)
    }

    pub fn in_memory() -> Self {
        Self::from_storage(Arc::new(InMemorySettingsStorage::new()), true)
    }

    pub fn from_storage(storage: Arc<dyn SettingsStorage>, project_trusted: bool) -> Self {
        let mgr = Self {
            storage,
            update_lock: Arc::new(Mutex::new(())),
            state: Arc::new(RwLock::new(SettingsManagerState {
                global_settings: Map::new(),
                project_settings: Map::new(),
                runtime_overrides: Map::new(),
                effective: Map::new(),
                global_load_failed: false,
                project_load_failed: false,
                project_trusted,
                errors: Vec::new(),
            })),
        };
        mgr.reload();
        mgr
    }

    fn parse_settings_text(content: Option<&str>) -> Result<Map<String, Value>, String> {
        let Some(raw) = content else {
            return Ok(Map::new());
        };
        let stripped = strip_json_comments(raw);
        if stripped.trim().is_empty() {
            return Err("Invalid settings.json: file is empty".to_owned());
        }
        let val: Value = serde_json::from_str(&stripped)
            .map_err(|_| "Invalid JSON syntax in settings.json".to_owned())?;
        let Some(obj) = val.as_object() else {
            return Err("Invalid settings.json: expected a top-level JSON object".to_owned());
        };
        Ok(obj.clone())
    }

    fn recompute_effective(st: &mut SettingsManagerState) {
        let proj = if st.project_trusted {
            st.project_settings.clone()
        } else {
            filter_untrusted_project_settings(&st.project_settings)
        };
        let merged = deep_merge_settings(&st.global_settings, &proj);
        st.effective = deep_merge_settings(&merged, &st.runtime_overrides);
    }

    pub fn reload(&self) {
        let _update_guard = self.update_lock.lock().expect("settings update lock");
        let mut errors = Vec::new();
        let mut global_map = Map::new();
        let mut global_failed = false;

        let global_res = self
            .storage
            .with_lock(SettingsScope::Global, &mut |current| {
                global_map = Self::parse_settings_text(current)?;
                Ok(None)
            });
        if let Err(msg) = global_res {
            global_failed = true;
            errors.push(SettingsError {
                scope: SettingsScope::Global,
                path: self.storage.path_for(SettingsScope::Global),
                message: msg,
            });
        }

        let mut project_map = Map::new();
        let mut project_failed = false;
        let project_res = self
            .storage
            .with_lock(SettingsScope::Project, &mut |current| {
                project_map = Self::parse_settings_text(current)?;
                Ok(None)
            });
        if let Err(msg) = project_res {
            project_failed = true;
            errors.push(SettingsError {
                scope: SettingsScope::Project,
                path: self.storage.path_for(SettingsScope::Project),
                message: msg,
            });
        }

        let mut st = self.state.write().expect("state lock");
        st.global_settings = global_map;
        st.global_load_failed = global_failed;
        st.project_settings = project_map;
        st.project_load_failed = project_failed;
        st.errors = errors;
        Self::recompute_effective(&mut st);
    }

    pub fn get_errors(&self) -> Vec<SettingsError> {
        self.state.read().expect("state lock").errors.clone()
    }

    pub fn drain_errors(&self) -> Vec<SettingsError> {
        std::mem::take(&mut self.state.write().expect("state lock").errors)
    }

    pub fn is_project_trusted(&self) -> bool {
        self.state.read().expect("state lock").project_trusted
    }

    pub fn set_project_trusted(&self, trusted: bool) {
        let mut st = self.state.write().expect("state lock");
        st.project_trusted = trusted;
        Self::recompute_effective(&mut st);
    }

    pub fn apply_overrides(&self, overrides: Map<String, Value>) {
        let mut st = self.state.write().expect("state lock");
        st.runtime_overrides = deep_merge_settings(&st.runtime_overrides, &overrides);
        Self::recompute_effective(&mut st);
    }

    /// 在指定作用域应用补丁更新：
    /// 1. 若该作用域原本加载损坏（`load_failed == true`），仅修改内存状态并记录保护提示，绝不写盘覆盖用户的损坏文件。
    /// 2. 否则在 `.lock` 文件锁内重读磁盘最新 JSON 对象，合并补丁后原子写回，保留磁盘未知字段与并发修改。
    pub fn patch_scope(
        &self,
        scope: SettingsScope,
        patch: Map<String, Value>,
    ) -> Result<(), String> {
        let _update_guard = self.update_lock.lock().expect("settings update lock");
        {
            let mut st = self.state.write().expect("state lock");
            let load_failed = match scope {
                SettingsScope::Global => st.global_load_failed,
                SettingsScope::Project => st.project_load_failed,
            };
            if load_failed {
                let target = match scope {
                    SettingsScope::Global => &mut st.global_settings,
                    SettingsScope::Project => &mut st.project_settings,
                };
                *target = deep_merge_objects(target, &patch);
                Self::recompute_effective(&mut st);
                return Ok(());
            }
        }

        let mut latest_merged = Map::new();
        let write_res = self.storage.with_lock(scope, &mut |current| {
            let disk_map = Self::parse_settings_text(current)?;
            latest_merged = deep_merge_objects(&disk_map, &patch);
            let serialized = serde_json::to_string_pretty(&Value::Object(latest_merged.clone()))
                .map_err(|_| "Failed to serialize settings.json".to_owned())?;
            Ok(Some(format!("{serialized}\n")))
        });

        let mut st = self.state.write().expect("state lock");
        match write_res {
            Ok(()) => {
                match scope {
                    SettingsScope::Global => st.global_settings = latest_merged,
                    SettingsScope::Project => st.project_settings = latest_merged,
                }
                Self::recompute_effective(&mut st);
                Ok(())
            }
            Err(err) => {
                match scope {
                    SettingsScope::Global => st.global_load_failed = true,
                    SettingsScope::Project => st.project_load_failed = true,
                }
                st.errors.push(SettingsError {
                    scope,
                    path: self.storage.path_for(scope),
                    message: err.clone(),
                });
                Err(err)
            }
        }
    }

    fn get_value(&self, key: &str) -> Option<Value> {
        self.state
            .read()
            .expect("state lock")
            .effective
            .get(key)
            .cloned()
    }

    pub fn get_default_provider(&self) -> Option<String> {
        self.get_value("defaultProvider")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
    }

    pub fn set_default_provider(
        &self,
        provider: Option<&str>,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let mut patch = Map::new();
        patch.insert(
            "defaultProvider".to_owned(),
            provider.map_or(Value::Null, |p| Value::String(p.to_owned())),
        );
        self.patch_scope(scope, patch)
    }

    pub fn get_default_model(&self) -> Option<String> {
        self.get_value("defaultModel")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
    }

    pub fn set_default_model(
        &self,
        model_id: Option<&str>,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let mut patch = Map::new();
        patch.insert(
            "defaultModel".to_owned(),
            model_id.map_or(Value::Null, |m| Value::String(m.to_owned())),
        );
        self.patch_scope(scope, patch)
    }

    pub fn set_default_model_and_provider(
        &self,
        provider: &str,
        model_id: &str,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let mut patch = Map::new();
        patch.insert(
            "defaultProvider".to_owned(),
            Value::String(provider.to_owned()),
        );
        patch.insert(
            "defaultModel".to_owned(),
            Value::String(model_id.to_owned()),
        );
        self.patch_scope(scope, patch)
    }

    pub fn get_default_thinking_level(&self) -> Option<ModelThinkingLevel> {
        self.get_value("defaultThinkingLevel")
            .and_then(|v| v.as_str().and_then(pi_session::parse_model_thinking_level))
    }

    pub fn set_default_thinking_level(
        &self,
        level: ModelThinkingLevel,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let mut patch = Map::new();
        patch.insert(
            "defaultThinkingLevel".to_owned(),
            Value::String(level.as_str().to_owned()),
        );
        self.patch_scope(scope, patch)
    }

    pub fn get_model_thinking_level(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Option<ModelThinkingLevel> {
        let key = format!("{provider}/{model_id}");
        self.get_value("modelThinkingLevels")
            .and_then(|v| v.get(&key).cloned())
            .and_then(|v| v.as_str().and_then(pi_session::parse_model_thinking_level))
    }

    pub fn get_all_model_thinking_levels(&self) -> BTreeMap<String, ModelThinkingLevel> {
        let mut out = BTreeMap::new();
        if let Some(Value::Object(map)) = self.get_value("modelThinkingLevels") {
            for (k, v) in map {
                if let Some(lvl) = v.as_str().and_then(pi_session::parse_model_thinking_level) {
                    out.insert(k, lvl);
                }
            }
        }
        out
    }

    pub fn set_model_thinking_level(
        &self,
        provider: &str,
        model_id: &str,
        level: Option<ModelThinkingLevel>,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let key = format!("{provider}/{model_id}");
        let mut inner = Map::new();
        inner.insert(
            key,
            level.map_or(Value::Null, |l| Value::String(l.as_str().to_owned())),
        );
        let mut patch = Map::new();
        patch.insert("modelThinkingLevels".to_owned(), Value::Object(inner));
        self.patch_scope(scope, patch)
    }

    pub fn get_enabled_models(&self) -> Option<Vec<String>> {
        self.get_value("enabledModels").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(ToOwned::to_owned))
                    .collect()
            })
        })
    }

    pub fn set_enabled_models(
        &self,
        patterns: Option<Vec<String>>,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let mut patch = Map::new();
        patch.insert(
            "enabledModels".to_owned(),
            patterns.map_or(Value::Null, |list| {
                Value::Array(list.into_iter().map(Value::String).collect())
            }),
        );
        self.patch_scope(scope, patch)
    }

    pub fn get_default_tools(&self) -> Option<Vec<String>> {
        let raw = self.get_value("defaultTools")?;
        let arr = raw.as_array()?;
        let entries: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(ToOwned::to_owned))
            .collect();
        Some(resolve_default_tools(&entries))
    }

    pub fn get_steering_mode(&self) -> QueueMode {
        match self
            .get_value("steeringMode")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .as_deref()
        {
            Some("all") => QueueMode::All,
            _ => QueueMode::OneAtATime,
        }
    }

    pub fn set_steering_mode(&self, mode: QueueMode, scope: SettingsScope) -> Result<(), String> {
        let s = match mode {
            QueueMode::All => "all",
            QueueMode::OneAtATime => "one-at-a-time",
        };
        let mut patch = Map::new();
        patch.insert("steeringMode".to_owned(), Value::String(s.to_owned()));
        self.patch_scope(scope, patch)
    }

    pub fn get_follow_up_mode(&self) -> QueueMode {
        match self
            .get_value("followUpMode")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .as_deref()
        {
            Some("all") => QueueMode::All,
            _ => QueueMode::OneAtATime,
        }
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode, scope: SettingsScope) -> Result<(), String> {
        let s = match mode {
            QueueMode::All => "all",
            QueueMode::OneAtATime => "one-at-a-time",
        };
        let mut patch = Map::new();
        patch.insert("followUpMode".to_owned(), Value::String(s.to_owned()));
        self.patch_scope(scope, patch)
    }

    pub fn get_compaction_settings(&self) -> CompactionSettings {
        let Some(Value::Object(obj)) = self.get_value("compaction") else {
            return DEFAULT_COMPACTION_SETTINGS;
        };
        CompactionSettings {
            enabled: obj
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(DEFAULT_COMPACTION_SETTINGS.enabled),
            reserve_tokens: obj
                .get("reserveTokens")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_COMPACTION_SETTINGS.reserve_tokens),
            keep_recent_tokens: obj
                .get("keepRecentTokens")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens),
        }
    }

    pub fn get_branch_summary_settings(&self) -> BranchSummarySettings {
        let def = BranchSummarySettings::default();
        let Some(Value::Object(obj)) = self.get_value("branchSummary") else {
            return def;
        };
        BranchSummarySettings {
            reserve_tokens: obj
                .get("reserveTokens")
                .and_then(Value::as_u64)
                .unwrap_or(def.reserve_tokens),
            skip_prompt: obj
                .get("skipPrompt")
                .and_then(Value::as_bool)
                .unwrap_or(def.skip_prompt),
        }
    }

    pub fn get_retry_policy(&self) -> RetryPolicy {
        let def = RetryPolicy::default();
        let Some(Value::Object(obj)) = self.get_value("retry") else {
            return def;
        };
        RetryPolicy {
            enabled: obj
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(def.enabled),
            max_retries: obj
                .get("maxRetries")
                .and_then(Value::as_u64)
                .map_or(def.max_retries, |n| n as u32),
            base_delay_ms: obj
                .get("baseDelayMs")
                .and_then(Value::as_u64)
                .unwrap_or(def.base_delay_ms),
            max_agent_delay_ms: obj
                .get("maxAgentDelayMs")
                .and_then(Value::as_u64)
                .or(def.max_agent_delay_ms),
        }
    }

    pub fn get_provider_retry_settings(&self) -> ProviderRetrySettings {
        let Some(Value::Object(retry_obj)) = self.get_value("retry") else {
            return ProviderRetrySettings {
                timeout_ms: None,
                max_retries: None,
                max_retry_delay_ms: Some(60_000),
            };
        };
        let Some(Value::Object(prov)) = retry_obj.get("provider") else {
            return ProviderRetrySettings {
                timeout_ms: None,
                max_retries: None,
                max_retry_delay_ms: Some(60_000),
            };
        };
        ProviderRetrySettings {
            timeout_ms: prov.get("timeoutMs").and_then(Value::as_u64),
            max_retries: prov
                .get("maxRetries")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
            max_retry_delay_ms: Some(
                prov.get("maxRetryDelayMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(60_000),
            ),
        }
    }

    pub fn get_image_settings(&self) -> ImageSettings {
        let def = ImageSettings::default();
        let Some(Value::Object(obj)) = self.get_value("images") else {
            return def;
        };
        ImageSettings {
            auto_resize: obj
                .get("autoResize")
                .and_then(Value::as_bool)
                .unwrap_or(def.auto_resize),
            block_images: obj
                .get("blockImages")
                .and_then(Value::as_bool)
                .unwrap_or(def.block_images),
        }
    }

    pub fn get_image_auto_resize(&self) -> bool {
        self.get_image_settings().auto_resize
    }

    pub fn get_codemode_mode(&self) -> Option<String> {
        let Some(Value::Object(obj)) = self.get_value("codemode") else {
            return None;
        };
        obj.get("mode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    pub fn get_block_images(&self) -> bool {
        self.get_image_settings().block_images
    }

    pub fn set_block_images(&self, block: bool, scope: SettingsScope) -> Result<(), String> {
        let mut patch = Map::new();
        patch.insert("images".to_owned(), json!({ "blockImages": block }));
        self.patch_scope(scope, patch)
    }

    pub fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        match self
            .get_value("cacheWarming")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .as_deref()
        {
            Some("off") => CacheWarmingMode::Off,
            Some("idle") => CacheWarmingMode::Idle,
            _ => CacheWarmingMode::Streaming,
        }
    }

    pub fn set_cache_warming_mode(
        &self,
        mode: CacheWarmingMode,
        scope: SettingsScope,
    ) -> Result<(), String> {
        let s = match mode {
            CacheWarmingMode::Off => "off",
            CacheWarmingMode::Streaming => "streaming",
            CacheWarmingMode::Idle => "idle",
        };
        let mut patch = Map::new();
        patch.insert("cacheWarming".to_owned(), Value::String(s.to_owned()));
        self.patch_scope(scope, patch)
    }

    pub fn get_tree_filter_mode(&self) -> TreeFilterMode {
        match self
            .get_value("treeFilterMode")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .as_deref()
        {
            Some("no-tools") => TreeFilterMode::NoTools,
            Some("user-only") => TreeFilterMode::UserOnly,
            Some("labeled-only") => TreeFilterMode::LabeledOnly,
            Some("all") => TreeFilterMode::All,
            _ => TreeFilterMode::Default,
        }
    }

    pub fn get_enable_skill_commands(&self) -> bool {
        self.get_value("enableSkillCommands")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    pub fn get_shell_path(&self) -> Option<String> {
        self.get_value("shellPath")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
    }

    pub fn get_shell_command_prefix(&self) -> Option<String> {
        self.get_value("shellCommandPrefix")
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
    }

    pub fn get_expose_session_environment(&self) -> bool {
        self.get_value("exposeSessionEnvironment")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    pub fn get_http_idle_timeout_ms(&self) -> u64 {
        self.get_value("httpIdleTimeoutMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS)
    }

    pub fn get_websocket_connect_timeout_ms(&self) -> u64 {
        self.get_value("websocketConnectTimeoutMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS)
    }

    pub fn get_skill_paths(&self) -> Vec<String> {
        self.get_value("skills")
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(ToOwned::to_owned))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    pub fn get_prompt_template_paths(&self) -> Vec<String> {
        self.get_value("prompts")
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(ToOwned::to_owned))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    pub fn get_raw_effective(&self) -> Map<String, Value> {
        self.state.read().expect("state lock").effective.clone()
    }
}
