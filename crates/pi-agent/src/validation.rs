use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use pi_ai::{JsonObject, JsonValue, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::formats::check_builtin_format;
use crate::types::AgentTool;

/// 默认最大 Schema 递归校验/转换深度。
pub const DEFAULT_MAX_VALIDATION_DEPTH: usize = 256;

/// 默认正则执行步数上限，覆盖指令、回溯、字符循环及反向引用比较。
pub const DEFAULT_REGEX_BACKTRACK_LIMIT: usize = 100_000;

static NEXT_VALIDATOR_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub path: String,
    pub keyword: String,
    pub message: String,
}

/// JSON Schema 编译阶段错误（如非法或不支持的正则表达式语法）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaCompileError {
    pub path: String,
    pub keyword: String,
    pub message: String,
}

impl std::fmt::Display for SchemaCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let loc = if self.path.is_empty() {
            "root".to_owned()
        } else {
            self.path.clone()
        };
        write!(
            f,
            "Invalid JSON Schema at {} ({}): {}",
            loc, self.keyword, self.message
        )
    }
}

impl std::error::Error for SchemaCompileError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnresolvedRefs {
    Error,
    #[default]
    Reject,
}

/// 单个字符串格式的校验函数，可在编译选项中覆盖内置格式或注册新格式。
#[derive(Clone)]
pub struct FormatValidator(Arc<dyn Fn(&str) -> bool + Send + Sync>);

impl FormatValidator {
    pub fn new(check: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }

    fn check(&self, value: &str) -> bool {
        (self.0)(value)
    }
}

impl std::fmt::Debug for FormatValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormatValidator").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub coerce: bool,
    pub unresolved_refs: UnresolvedRefs,
    pub max_depth: usize,
    /// 每次正则匹配的执行预算。耗尽后校验失败；不包含编译和 UTF-16 编码。
    pub regex_backtrack_limit: usize,
    pub formats: BTreeMap<String, FormatValidator>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            coerce: true,
            unresolved_refs: UnresolvedRefs::Reject,
            max_depth: DEFAULT_MAX_VALIDATION_DEPTH,
            regex_backtrack_limit: DEFAULT_REGEX_BACKTRACK_LIMIT,
            formats: BTreeMap::new(),
        }
    }
}

impl CompileOptions {
    pub fn with_format(
        mut self,
        name: impl Into<String>,
        check: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.formats
            .insert(name.into(), FormatValidator::new(check));
        self
    }

    fn check_format(&self, name: &str, value: &str) -> bool {
        self.formats.get(name).map_or_else(
            || check_builtin_format(name, value),
            |check| check.check(value),
        )
    }
}

// =============================================================================
// Runtime Cycle & Depth Guard (RAII, unwind-safe for thread reuse)
// =============================================================================

struct RuntimeCheckState {
    call_depth: usize,
    ref_stack: Vec<(u64, usize, usize)>,
}

thread_local! {
    static CHECK_STATE: RefCell<RuntimeCheckState> = const {
        RefCell::new(RuntimeCheckState {
            call_depth: 0,
            ref_stack: Vec::new(),
        })
    };
}

struct CallDepthGuard {
    active: bool,
}

impl CallDepthGuard {
    fn enter(max_depth: usize) -> Option<Self> {
        let limit = max_depth.saturating_mul(4).max(64);
        let entered = CHECK_STATE.with(|cell| {
            let mut st = cell.borrow_mut();
            if st.call_depth >= limit {
                return false;
            }
            st.call_depth += 1;
            true
        });
        if entered {
            Some(Self { active: true })
        } else {
            None
        }
    }
}

impl Drop for CallDepthGuard {
    fn drop(&mut self) {
        if self.active {
            CHECK_STATE.with(|cell| {
                let mut st = cell.borrow_mut();
                st.call_depth = st.call_depth.saturating_sub(1);
            });
        }
    }
}

struct RefStackGuard {
    active: bool,
}

impl RefStackGuard {
    fn enter(validator_id: u64, slot_id: usize, value: &Value, max_depth: usize) -> Option<Self> {
        let val_ptr = value as *const Value as usize;
        let entered = CHECK_STATE.with(|cell| {
            let mut st = cell.borrow_mut();
            if st.ref_stack.len() >= max_depth {
                return false;
            }
            if st.ref_stack.contains(&(validator_id, slot_id, val_ptr)) {
                return false;
            }
            st.ref_stack.push((validator_id, slot_id, val_ptr));
            true
        });
        if entered {
            Some(Self { active: true })
        } else {
            None
        }
    }
}

impl Drop for RefStackGuard {
    fn drop(&mut self) {
        if self.active {
            CHECK_STATE.with(|cell| {
                cell.borrow_mut().ref_stack.pop();
            });
        }
    }
}

// =============================================================================
// ECMA-262 Regular Expression Engine with Bounded Backtracking
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternMatchOutcome {
    Matched,
    NotMatched,
    InvalidPattern(String),
    LimitExceeded(String),
}

#[derive(Clone)]
enum CompiledPattern {
    Valid {
        pattern: String,
        regex: Arc<regress::Regex>,
        limit: usize,
    },
    Invalid {
        pattern: String,
        error: String,
    },
}

impl CompiledPattern {
    fn compile(
        pattern: &str,
        path: &str,
        keyword: &str,
        backtrack_limit: usize,
        errors: &mut Vec<SchemaCompileError>,
    ) -> Self {
        match compile_ecma_regex(pattern) {
            Ok(re) => Self::Valid {
                pattern: pattern.to_owned(),
                regex: Arc::new(re),
                limit: backtrack_limit,
            },
            Err(err) => {
                errors.push(SchemaCompileError {
                    path: path.to_owned(),
                    keyword: keyword.to_owned(),
                    message: format!("invalid regex pattern {pattern:?}: {err}"),
                });
                Self::Invalid {
                    pattern: pattern.to_owned(),
                    error: err,
                }
            }
        }
    }

    fn evaluate(&self, text: &str) -> PatternMatchOutcome {
        match self {
            Self::Valid { regex, limit, .. } => {
                match regex.find_utf16_bounded(&text.encode_utf16().collect::<Vec<_>>(), *limit) {
                    Ok(Some(_)) => PatternMatchOutcome::Matched,
                    Ok(None) => PatternMatchOutcome::NotMatched,
                    Err(err) => PatternMatchOutcome::LimitExceeded(err.to_string()),
                }
            }
            Self::Invalid { error, .. } => PatternMatchOutcome::InvalidPattern(error.clone()),
        }
    }

    fn is_match(&self, text: &str) -> bool {
        matches!(self.evaluate(text), PatternMatchOutcome::Matched)
    }

    fn pattern_str(&self) -> &str {
        match self {
            Self::Valid { pattern, .. } | Self::Invalid { pattern, .. } => pattern,
        }
    }
}

/// 对齐上游 sharedRegex：优先 Unicode 模式，失败后按 UTF-16 码元解析兼容模式。
/// 执行预算由 regress 原生匹配器同步执行，覆盖候选起点、回溯和嵌套断言。
fn compile_ecma_regex(pattern: &str) -> Result<regress::Regex, String> {
    regress::Regex::with_flags(pattern, "u")
        .or_else(|_| {
            regress::Regex::from_unicode(
                pattern.encode_utf16().map(u32::from),
                regress::Flags::default(),
            )
        })
        .map_err(|e| e.to_string())
}

// =============================================================================
// Precompiled Validator & Compiler
// =============================================================================

type CheckFn = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

struct RecursiveSlot {
    slot_id: usize,
    check: OnceLock<CheckFn>,
}

/// 预编译 JSON Schema 校验器（对照 `@earendil-works/pi-schema` 的 `CoercingValidator`）。
///
/// 快路径 `check(&Value) -> bool` 直接运行预编译闭包树；
/// 仅在失败时运行慢路径 `errors(&Value) -> Vec<ValidationError>`。
#[derive(Clone)]
pub struct CompiledValidator {
    validator_id: u64,
    schema: Arc<Value>,
    anchors: Arc<BTreeMap<String, Value>>,
    root_check: CheckFn,
    options: CompileOptions,
    schema_errors: Arc<Vec<SchemaCompileError>>,
}

impl std::fmt::Debug for CompiledValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledValidator")
            .field("validator_id", &self.validator_id)
            .field("options", &self.options)
            .field("schema_errors", &self.schema_errors)
            .finish()
    }
}

struct CompileState {
    validator_id: u64,
    next_slot_id: usize,
    compiling_refs: BTreeMap<String, Arc<RecursiveSlot>>,
    schema_errors: Vec<SchemaCompileError>,
}

impl CompileState {
    fn new(validator_id: u64) -> Self {
        Self {
            validator_id,
            next_slot_id: 1,
            compiling_refs: BTreeMap::new(),
            schema_errors: Vec::new(),
        }
    }

    fn alloc_slot_id(&mut self) -> usize {
        let id = self.next_slot_id;
        self.next_slot_id += 1;
        id
    }
}

/// 尝试编译 JSON Schema；若包含非法正则表达式或当 `unresolved_refs == UnresolvedRefs::Error` 且存在悬空引用时，
/// 显式返回 `Err(SchemaCompileError)`。
pub fn try_compile_schema(
    schema: &Value,
    options: CompileOptions,
) -> Result<CompiledValidator, SchemaCompileError> {
    let validator = compile_schema(schema, options);
    if let Some(first_err) = validator.schema_errors.first() {
        return Err(first_err.clone());
    }
    Ok(validator)
}

pub fn try_compile_tool_schema(schema: &Value) -> Result<CompiledValidator, SchemaCompileError> {
    try_compile_schema(
        schema,
        CompileOptions {
            coerce: true,
            unresolved_refs: UnresolvedRefs::Reject,
            ..CompileOptions::default()
        },
    )
}

pub fn compile_schema(schema: &Value, options: CompileOptions) -> CompiledValidator {
    let validator_id = NEXT_VALIDATOR_ID.fetch_add(1, Ordering::Relaxed);
    let root = Arc::new(schema.clone());
    let mut anchors = BTreeMap::new();
    collect_anchors(&root, &mut anchors);
    let anchors = Arc::new(anchors);
    let mut state = CompileState::new(validator_id);
    let root_check = compile_node(&root, &root, &anchors, &options, "", &mut state);
    CompiledValidator {
        validator_id,
        schema: root,
        anchors,
        root_check,
        options,
        schema_errors: Arc::new(state.schema_errors),
    }
}

pub fn compile_tool_schema(schema: &Value) -> CompiledValidator {
    compile_schema(
        schema,
        CompileOptions {
            coerce: true,
            unresolved_refs: UnresolvedRefs::Reject,
            ..CompileOptions::default()
        },
    )
}

impl CompiledValidator {
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    pub fn schema_errors(&self) -> &[SchemaCompileError] {
        &self.schema_errors
    }

    pub fn check(&self, value: &Value) -> bool {
        if !self.schema_errors.is_empty() {
            return false;
        }
        (self.root_check)(value)
    }

    pub fn coerce(&self, value: &Value) -> Value {
        let mut coercer = Coercer::new(&self.schema, &self.anchors, &self.options);
        coercer.coerce(value)
    }

    pub fn convert_only(&self, value: &Value) -> Value {
        let mut coercer = Coercer::new(&self.schema, &self.anchors, &self.options);
        coercer.convert_only(value)
    }

    pub fn errors(&self, value: &Value) -> Vec<ValidationError> {
        if !self.schema_errors.is_empty() {
            return self
                .schema_errors
                .iter()
                .map(|e| ValidationError {
                    path: e.path.clone(),
                    keyword: e.keyword.clone(),
                    message: e.message.clone(),
                })
                .collect();
        }
        if self.check(value) {
            return Vec::new();
        }
        let explainer = Explainer::new(&self.schema, &self.anchors, &self.options);
        explainer.errors(value)
    }

    pub fn parse(&self, value: &Value) -> Result<Value, Vec<ValidationError>> {
        if !self.schema_errors.is_empty() {
            return Err(self.errors(value));
        }
        let target = if self.options.coerce {
            self.coerce(value)
        } else {
            value.clone()
        };
        if self.check(&target) {
            Ok(target)
        } else {
            Err(self.errors(&target))
        }
    }
}

// =============================================================================
// JSON Pointer & Anchor Resolution
// =============================================================================

fn collect_anchors(node: &Value, anchors: &mut BTreeMap<String, Value>) {
    match node {
        Value::Object(obj) => {
            if let Some(Value::String(anchor)) = obj.get("$anchor")
                && !anchor.is_empty()
            {
                anchors.insert(format!("#{anchor}"), node.clone());
            }
            if let Some(Value::String(id)) = obj.get("$id").or_else(|| obj.get("id"))
                && id.starts_with('#')
            {
                anchors.insert(id.clone(), node.clone());
            }
            for v in obj.values() {
                collect_anchors(v, anchors);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_anchors(item, anchors);
            }
        }
        _ => {}
    }
}

enum ResolvedRef<'a> {
    Found(&'a Value),
    Anchor(Value),
    Unresolved,
}

fn resolve_ref<'a>(
    root: &'a Value,
    anchors: &'a BTreeMap<String, Value>,
    ref_str: &str,
) -> ResolvedRef<'a> {
    if ref_str == "#" {
        return ResolvedRef::Found(root);
    }
    if let Some(pointer) = ref_str.strip_prefix("#/") {
        let mut current = root;
        for raw_seg in pointer.split('/') {
            let seg = raw_seg.replace("~1", "/").replace("~0", "~");
            match current {
                Value::Object(map) => {
                    let Some(next) = map.get(&seg) else {
                        return ResolvedRef::Unresolved;
                    };
                    current = next;
                }
                Value::Array(items) => {
                    let Ok(idx) = seg.parse::<usize>() else {
                        return ResolvedRef::Unresolved;
                    };
                    let Some(next) = items.get(idx) else {
                        return ResolvedRef::Unresolved;
                    };
                    current = next;
                }
                _ => return ResolvedRef::Unresolved,
            }
        }
        return ResolvedRef::Found(current);
    }
    if let Some(found) = anchors.get(ref_str) {
        return ResolvedRef::Anchor(found.clone());
    }
    ResolvedRef::Unresolved
}

fn check_subschema(
    node: &Value,
    root: &Value,
    anchors: &BTreeMap<String, Value>,
    options: &CompileOptions,
    value: &Value,
) -> bool {
    let validator_id = NEXT_VALIDATOR_ID.fetch_add(1, Ordering::Relaxed);
    let mut state = CompileState::new(validator_id);
    let check = compile_node(node, root, anchors, options, "", &mut state);
    if !state.schema_errors.is_empty() {
        return false;
    }
    check(value)
}

// =============================================================================
// Precompiled Fast-Path Compiler
// =============================================================================

fn compile_node(
    node: &Value,
    root: &Value,
    anchors: &BTreeMap<String, Value>,
    options: &CompileOptions,
    schema_path: &str,
    state: &mut CompileState,
) -> CheckFn {
    let max_depth = options.max_depth;

    let inner: CheckFn = match node {
        Value::Bool(true) => Arc::new(|_| true),
        Value::Bool(false) => Arc::new(|_| false),
        Value::Object(obj) => {
            compile_object_schema(obj, root, anchors, options, schema_path, state)
        }
        _ => {
            state.schema_errors.push(SchemaCompileError {
                path: schema_path.to_owned(),
                keyword: "schema".to_owned(),
                message: format!("A schema must be an object or a boolean, got {node}"),
            });
            Arc::new(|_| false)
        }
    };

    Arc::new(move |v| {
        let Some(_guard) = CallDepthGuard::enter(max_depth) else {
            return false;
        };
        inner(v)
    })
}

fn wrap_recursive_slot(slot: Arc<RecursiveSlot>, validator_id: u64, max_depth: usize) -> CheckFn {
    Arc::new(move |v| {
        let Some(_guard) = RefStackGuard::enter(validator_id, slot.slot_id, v, max_depth) else {
            return false;
        };
        slot.check.get().is_some_and(|f| f(v))
    })
}

fn compile_object_schema(
    obj: &JsonObject,
    root: &Value,
    anchors: &BTreeMap<String, Value>,
    options: &CompileOptions,
    schema_path: &str,
    state: &mut CompileState,
) -> CheckFn {
    let mut checks: Vec<CheckFn> = Vec::new();
    let validator_id = state.validator_id;
    let max_depth = options.max_depth;

    // 1. $ref
    if let Some(ref_val) = obj.get("$ref") {
        if let Some(ref_str) = ref_val.as_str() {
            if let Some(slot) = state.compiling_refs.get(ref_str) {
                checks.push(wrap_recursive_slot(slot.clone(), validator_id, max_depth));
            } else {
                match resolve_ref(root, anchors, ref_str) {
                    ResolvedRef::Found(target) => {
                        let slot = Arc::new(RecursiveSlot {
                            slot_id: state.alloc_slot_id(),
                            check: OnceLock::new(),
                        });
                        state
                            .compiling_refs
                            .insert(ref_str.to_owned(), slot.clone());
                        let compiled_target =
                            compile_node(target, root, anchors, options, ref_str, state);
                        let _ = slot.check.set(compiled_target);
                        state.compiling_refs.remove(ref_str);
                        checks.push(wrap_recursive_slot(slot, validator_id, max_depth));
                    }
                    ResolvedRef::Anchor(target) => {
                        let slot = Arc::new(RecursiveSlot {
                            slot_id: state.alloc_slot_id(),
                            check: OnceLock::new(),
                        });
                        state
                            .compiling_refs
                            .insert(ref_str.to_owned(), slot.clone());
                        let compiled_target =
                            compile_node(&target, root, anchors, options, ref_str, state);
                        let _ = slot.check.set(compiled_target);
                        state.compiling_refs.remove(ref_str);
                        checks.push(wrap_recursive_slot(slot, validator_id, max_depth));
                    }
                    ResolvedRef::Unresolved => {
                        if options.unresolved_refs == UnresolvedRefs::Error {
                            state.schema_errors.push(SchemaCompileError {
                                path: schema_path.to_owned(),
                                keyword: "$ref".to_owned(),
                                message: format!("unresolved reference {ref_str}"),
                            });
                        }
                        checks.push(Arc::new(|_| false));
                    }
                }
            }
        } else {
            state.schema_errors.push(SchemaCompileError {
                path: schema_path.to_owned(),
                keyword: "$ref".to_owned(),
                message: "$ref must be a string".to_owned(),
            });
            checks.push(Arc::new(|_| false));
        }
    }

    // 2. type
    if let Some(type_val) = obj.get("type") {
        let allowed_types: Vec<String> = match type_val {
            Value::String(s) => vec![s.clone()],
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                .collect(),
            _ => Vec::new(),
        };
        if !allowed_types.is_empty() {
            checks.push(Arc::new(move |v| {
                allowed_types.iter().any(|t| matches_json_type(v, t))
            }));
        }
    }

    // 3. const & enum
    if let Some(const_val) = obj.get("const").cloned() {
        checks.push(Arc::new(move |v| json_values_equal(v, &const_val)));
    }
    if let Some(Value::Array(enum_vals)) = obj.get("enum") {
        let enum_vals = enum_vals.clone();
        checks.push(Arc::new(move |v| {
            enum_vals.iter().any(|item| json_values_equal(v, item))
        }));
    }

    // 4. String constraints
    let min_len = obj
        .get("minLength")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let max_len = obj
        .get("maxLength")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let pattern_re: Option<CompiledPattern> = match obj.get("pattern") {
        Some(Value::String(p)) => Some(CompiledPattern::compile(
            p,
            schema_path,
            "pattern",
            options.regex_backtrack_limit,
            &mut state.schema_errors,
        )),
        Some(other) => {
            state.schema_errors.push(SchemaCompileError {
                path: schema_path.to_owned(),
                keyword: "pattern".to_owned(),
                message: format!("pattern must be a string, got {other}"),
            });
            Some(CompiledPattern::Invalid {
                pattern: other.to_string(),
                error: "pattern must be a string".to_owned(),
            })
        }
        None => None,
    };
    let format_name = obj
        .get("format")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let custom_format = format_name
        .as_ref()
        .and_then(|name| options.formats.get(name))
        .cloned();
    if min_len.is_some() || max_len.is_some() || pattern_re.is_some() || format_name.is_some() {
        checks.push(Arc::new(move |v| {
            let Value::String(s) = v else {
                return true;
            };
            let count = s.chars().count();
            if let Some(min) = min_len
                && count < min
            {
                return false;
            }
            if let Some(max) = max_len
                && count > max
            {
                return false;
            }
            if let Some(re) = &pattern_re
                && !re.is_match(s)
            {
                return false;
            }
            if let Some(fmt) = &format_name
                && !custom_format
                    .as_ref()
                    .map_or_else(|| check_builtin_format(fmt, s), |check| check.check(s))
            {
                return false;
            }
            true
        }));
    }

    // 5. Number constraints
    let min_num = obj.get("minimum").and_then(|v| v.as_f64());
    let max_num = obj.get("maximum").and_then(|v| v.as_f64());
    let ex_min_num = obj
        .get("exclusiveMinimum")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            if obj.get("exclusiveMinimum") == Some(&Value::Bool(true)) {
                min_num
            } else {
                None
            }
        });
    let ex_max_num = obj
        .get("exclusiveMaximum")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            if obj.get("exclusiveMaximum") == Some(&Value::Bool(true)) {
                max_num
            } else {
                None
            }
        });
    let multiple_of = obj
        .get("multipleOf")
        .and_then(|v| v.as_f64())
        .filter(|&d| d > 0.0);

    if min_num.is_some()
        || max_num.is_some()
        || ex_min_num.is_some()
        || ex_max_num.is_some()
        || multiple_of.is_some()
    {
        checks.push(Arc::new(move |v| {
            let Some(n) = v.as_f64() else {
                return true;
            };
            if let Some(min) = min_num
                && n < min
            {
                return false;
            }
            if let Some(max) = max_num
                && n > max
            {
                return false;
            }
            if let Some(xmin) = ex_min_num
                && n <= xmin
            {
                return false;
            }
            if let Some(xmax) = ex_max_num
                && n >= xmax
            {
                return false;
            }
            if let Some(mult) = multiple_of
                && !is_multiple_of(n, mult)
            {
                return false;
            }
            true
        }));
    }

    // 6. Object constraints
    let required: Vec<String> = obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default();

    let properties: BTreeMap<String, CheckFn> = obj
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|props| {
            props
                .iter()
                .map(|(k, sub)| {
                    let sub_path = append_pointer(schema_path, k);
                    (
                        k.clone(),
                        compile_node(sub, root, anchors, options, &sub_path, state),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let pattern_properties: Vec<(CompiledPattern, CheckFn)> = obj
        .get("patternProperties")
        .and_then(|v| v.as_object())
        .map(|pats| {
            pats.iter()
                .map(|(pat, sub)| {
                    let sub_path = append_pointer(schema_path, pat);
                    let compiled_pat = CompiledPattern::compile(
                        pat,
                        &sub_path,
                        "patternProperties",
                        options.regex_backtrack_limit,
                        &mut state.schema_errors,
                    );
                    let sub_check = compile_node(sub, root, anchors, options, &sub_path, state);
                    (compiled_pat, sub_check)
                })
                .collect()
        })
        .unwrap_or_default();

    let additional_properties: Option<CheckFn> = obj.get("additionalProperties").map(|sub| {
        let sub_path = append_pointer(schema_path, "additionalProperties");
        compile_node(sub, root, anchors, options, &sub_path, state)
    });

    let property_names_check: Option<CheckFn> = obj.get("propertyNames").map(|sub| {
        let sub_path = append_pointer(schema_path, "propertyNames");
        compile_node(sub, root, anchors, options, &sub_path, state)
    });

    let min_props = obj
        .get("minProperties")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let max_props = obj
        .get("maxProperties")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let dep_required: Vec<(String, Vec<String>)> = {
        let mut out = Vec::new();
        if let Some(Value::Object(dr)) = obj.get("dependentRequired") {
            for (k, v) in dr {
                if let Some(arr) = v.as_array() {
                    let reqs: Vec<String> = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                        .collect();
                    out.push((k.clone(), reqs));
                }
            }
        }
        if let Some(Value::Object(deps)) = obj.get("dependencies") {
            for (k, v) in deps {
                if let Some(arr) = v.as_array() {
                    let reqs: Vec<String> = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                        .collect();
                    out.push((k.clone(), reqs));
                }
            }
        }
        out
    };

    let dep_schemas: Vec<(String, CheckFn)> = {
        let mut out = Vec::new();
        if let Some(Value::Object(ds)) = obj.get("dependentSchemas") {
            for (k, sub) in ds {
                let sub_path = append_pointer(schema_path, k);
                out.push((
                    k.clone(),
                    compile_node(sub, root, anchors, options, &sub_path, state),
                ));
            }
        }
        if let Some(Value::Object(deps)) = obj.get("dependencies") {
            for (k, sub) in deps {
                if !sub.is_array() {
                    let sub_path = append_pointer(schema_path, k);
                    out.push((
                        k.clone(),
                        compile_node(sub, root, anchors, options, &sub_path, state),
                    ));
                }
            }
        }
        out
    };

    if !required.is_empty()
        || !properties.is_empty()
        || !pattern_properties.is_empty()
        || additional_properties.is_some()
        || property_names_check.is_some()
        || min_props.is_some()
        || max_props.is_some()
        || !dep_required.is_empty()
        || !dep_schemas.is_empty()
    {
        checks.push(Arc::new(move |v| {
            let Value::Object(map) = v else {
                return true;
            };
            if let Some(min) = min_props
                && map.len() < min
            {
                return false;
            }
            if let Some(max) = max_props
                && map.len() > max
            {
                return false;
            }
            for req in &required {
                if !map.contains_key(req) {
                    return false;
                }
            }
            for (k, prop_check) in &properties {
                if let Some(val) = map.get(k)
                    && !prop_check(val)
                {
                    return false;
                }
            }
            if !pattern_properties.is_empty()
                || additional_properties.is_some()
                || property_names_check.is_some()
            {
                for (k, val) in map {
                    if let Some(pn_check) = &property_names_check
                        && !pn_check(&Value::String(k.clone()))
                    {
                        return false;
                    }
                    let mut matched = properties.contains_key(k);
                    for (re, pat_check) in &pattern_properties {
                        match re.evaluate(k) {
                            PatternMatchOutcome::Matched => {
                                matched = true;
                                if !pat_check(val) {
                                    return false;
                                }
                            }
                            PatternMatchOutcome::NotMatched => {}
                            PatternMatchOutcome::InvalidPattern(_)
                            | PatternMatchOutcome::LimitExceeded(_) => {
                                return false;
                            }
                        }
                    }
                    if !matched
                        && let Some(add_check) = &additional_properties
                        && !add_check(val)
                    {
                        return false;
                    }
                }
            }
            for (k, reqs) in &dep_required {
                if map.contains_key(k) {
                    for r in reqs {
                        if !map.contains_key(r) {
                            return false;
                        }
                    }
                }
            }
            for (k, schema_check) in &dep_schemas {
                if map.contains_key(k) && !schema_check(v) {
                    return false;
                }
            }
            true
        }));
    }

    // 7. Array constraints
    let min_items = obj
        .get("minItems")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let max_items = obj
        .get("maxItems")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let unique_items = obj.get("uniqueItems") == Some(&Value::Bool(true));

    let (prefix_checks, rest_check): (Vec<CheckFn>, Option<CheckFn>) =
        if let Some(Value::Array(items_arr)) = obj.get("items") {
            let prefix = items_arr
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let p = format!("{schema_path}/items/{i}");
                    compile_node(s, root, anchors, options, &p, state)
                })
                .collect();
            let rest = obj.get("additionalItems").map(|s| {
                let p = format!("{schema_path}/additionalItems");
                compile_node(s, root, anchors, options, &p, state)
            });
            (prefix, rest)
        } else {
            let prefix = obj
                .get("prefixItems")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let p = format!("{schema_path}/prefixItems/{i}");
                            compile_node(s, root, anchors, options, &p, state)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let rest = obj.get("items").map(|s| {
                let p = format!("{schema_path}/items");
                compile_node(s, root, anchors, options, &p, state)
            });
            (prefix, rest)
        };

    let contains_spec: Option<(CheckFn, usize, usize)> = obj.get("contains").map(|sub| {
        let p = format!("{schema_path}/contains");
        let c = compile_node(sub, root, anchors, options, &p, state);
        let min_c = obj
            .get("minContains")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(1);
        let max_c = obj
            .get("maxContains")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(usize::MAX);
        (c, min_c, max_c)
    });

    if min_items.is_some()
        || max_items.is_some()
        || unique_items
        || !prefix_checks.is_empty()
        || rest_check.is_some()
        || contains_spec.is_some()
    {
        checks.push(Arc::new(move |v| {
            let Value::Array(items) = v else {
                return true;
            };
            if let Some(min) = min_items
                && items.len() < min
            {
                return false;
            }
            if let Some(max) = max_items
                && items.len() > max
            {
                return false;
            }
            for (idx, item) in items.iter().enumerate() {
                if idx < prefix_checks.len() {
                    if !prefix_checks[idx](item) {
                        return false;
                    }
                } else if let Some(rest) = &rest_check
                    && !rest(item)
                {
                    return false;
                }
            }
            if unique_items && !has_unique_items(items) {
                return false;
            }
            if let Some((c_check, min_c, max_c)) = &contains_spec {
                let mut count = 0usize;
                for item in items {
                    if c_check(item) {
                        count += 1;
                        if count > *max_c {
                            return false;
                        }
                    }
                }
                if count < *min_c {
                    return false;
                }
            }
            true
        }));
    }

    // 8. Combinators: allOf, anyOf, oneOf, not, if/then/else
    if let Some(Value::Array(all_of)) = obj.get("allOf") {
        for (i, sub) in all_of.iter().enumerate() {
            let p = format!("{schema_path}/allOf/{i}");
            checks.push(compile_node(sub, root, anchors, options, &p, state));
        }
    }
    if let Some(Value::Array(any_of)) = obj.get("anyOf") {
        let sub_checks: Vec<CheckFn> = any_of
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let p = format!("{schema_path}/anyOf/{i}");
                compile_node(s, root, anchors, options, &p, state)
            })
            .collect();
        checks.push(Arc::new(move |v| sub_checks.iter().any(|c| c(v))));
    }
    if let Some(Value::Array(one_of)) = obj.get("oneOf") {
        let sub_checks: Vec<CheckFn> = one_of
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let p = format!("{schema_path}/oneOf/{i}");
                compile_node(s, root, anchors, options, &p, state)
            })
            .collect();
        checks.push(Arc::new(move |v| {
            let mut matched = 0usize;
            for c in &sub_checks {
                if c(v) {
                    matched += 1;
                    if matched > 1 {
                        return false;
                    }
                }
            }
            matched == 1
        }));
    }
    if let Some(not_sub) = obj.get("not") {
        let p = format!("{schema_path}/not");
        let not_check = compile_node(not_sub, root, anchors, options, &p, state);
        checks.push(Arc::new(move |v| !not_check(v)));
    }
    if let Some(if_sub) = obj.get("if") {
        let p_if = format!("{schema_path}/if");
        let cond_check = compile_node(if_sub, root, anchors, options, &p_if, state);
        let then_check = obj.get("then").map(|s| {
            let p_then = format!("{schema_path}/then");
            compile_node(s, root, anchors, options, &p_then, state)
        });
        let else_check = obj.get("else").map(|s| {
            let p_else = format!("{schema_path}/else");
            compile_node(s, root, anchors, options, &p_else, state)
        });
        if then_check.is_some() || else_check.is_some() {
            checks.push(Arc::new(move |v| {
                if cond_check(v) {
                    then_check.as_ref().is_none_or(|c| c(v))
                } else {
                    else_check.as_ref().is_none_or(|c| c(v))
                }
            }));
        }
    }

    match checks.len() {
        0 => Arc::new(|_| true),
        1 => checks.pop().expect("single check"),
        _ => Arc::new(move |v| checks.iter().all(|c| c(v))),
    }
}

// =============================================================================
// Three-Stage Coercer (对照 packages/schema/src/coerce.ts)
// =============================================================================

struct Coercer<'a> {
    root: &'a Value,
    anchors: &'a BTreeMap<String, Value>,
    options: &'a CompileOptions,
    entered: Vec<(String, Value)>,
    depth: usize,
}

impl<'a> Coercer<'a> {
    fn new(
        root: &'a Value,
        anchors: &'a BTreeMap<String, Value>,
        options: &'a CompileOptions,
    ) -> Self {
        Self {
            root,
            anchors,
            options,
            entered: Vec::new(),
            depth: 0,
        }
    }

    fn coerce(&mut self, value: &Value) -> Value {
        self.entered.clear();
        self.depth = 0;
        let pass1 = self.drop_optional_nulls(value, self.root);
        let pass2 = self.convert_only(&pass1);
        self.entered.clear();
        self.depth = 0;
        self.coerce_types(&pass2, self.root)
    }

    fn convert_only(&mut self, value: &Value) -> Value {
        self.entered.clear();
        self.depth = 0;
        let res = self.convert(self.root, value);
        self.entered.clear();
        self.depth = 0;
        res
    }

    fn passes(&self, node: &Value, value: &Value) -> Option<bool> {
        Some(check_subschema(
            node,
            self.root,
            self.anchors,
            self.options,
            value,
        ))
    }

    fn schema_identity_key(node: &Value, ref_hint: Option<&str>) -> String {
        if let Some(r) = ref_hint {
            return format!("ref:{r}");
        }
        if let Some(r) = node
            .as_object()
            .and_then(|o| o.get("$ref"))
            .and_then(|v| v.as_str())
        {
            return format!("ref:{r}");
        }
        format!(
            "ptr:{:p}:{}",
            node,
            serde_json::to_string(node).unwrap_or_default()
        )
    }

    fn reenter(&mut self, node: &Value, value: &Value, ref_hint: Option<&str>) -> Value {
        if self.depth >= self.options.max_depth {
            return value.clone();
        }
        let key = Self::schema_identity_key(node, ref_hint);
        if self
            .entered
            .iter()
            .any(|(k, v)| *k == key && json_values_equal(v, value))
        {
            return value.clone();
        }
        self.entered.push((key, value.clone()));
        self.depth += 1;
        let result = self.convert(node, value);
        self.depth = self.depth.saturating_sub(1);
        self.entered.pop();
        result
    }

    // -------------------------------------------------------------------------
    // Pass 1: drop_optional_nulls
    // -------------------------------------------------------------------------

    fn drop_optional_nulls(&mut self, value: &Value, schema: &Value) -> Value {
        if self.depth >= self.options.max_depth {
            return value.clone();
        }
        let Value::Object(schema_obj) = schema else {
            return value.clone();
        };

        self.depth += 1;
        let result = self.drop_optional_nulls_inner(value, schema_obj);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    fn drop_optional_nulls_inner(&mut self, value: &Value, schema_obj: &JsonObject) -> Value {
        if let Value::Array(items) = value {
            return items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    if let Some(item_schema) = item_schema_at(schema_obj, i) {
                        self.drop_optional_nulls(item, item_schema)
                    } else {
                        item.clone()
                    }
                })
                .collect::<Vec<_>>()
                .into();
        }

        let Value::Object(val_obj) = value else {
            return value.clone();
        };

        let properties = schema_obj.get("properties").and_then(|v| v.as_object());
        let pattern_properties: Vec<(CompiledPattern, &Value)> = schema_obj
            .get("patternProperties")
            .and_then(|v| v.as_object())
            .map(|pats| {
                let mut dummy_errs = Vec::new();
                pats.iter()
                    .map(|(p, s)| {
                        (
                            CompiledPattern::compile(
                                p,
                                "",
                                "patternProperties",
                                self.options.regex_backtrack_limit,
                                &mut dummy_errs,
                            ),
                            s,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let additional = schema_obj
            .get("additionalProperties")
            .filter(|v| v.is_object());

        if properties.is_none() && pattern_properties.is_empty() && additional.is_none() {
            return value.clone();
        }

        let required: BTreeSet<&str> = schema_obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();

        let mut out = val_obj.clone();
        if let Some(props) = properties {
            for (key, prop_schema) in props {
                let Some(prop_val) = out.get(key).cloned() else {
                    continue;
                };
                let is_ref = prop_schema
                    .as_object()
                    .is_some_and(|o| o.get("$ref").is_some_and(|r| r.is_string()));
                if prop_val.is_null()
                    && !required.contains(key.as_str())
                    && !is_ref
                    && self.passes(prop_schema, &Value::Null) == Some(false)
                {
                    out.remove(key);
                    continue;
                }
                let cleaned = self.drop_optional_nulls(&prop_val, prop_schema);
                out.insert(key.clone(), cleaned);
            }
        }

        if !pattern_properties.is_empty() || additional.is_some() {
            let keys: Vec<String> = out.keys().cloned().collect();
            for key in keys {
                let mut matched = properties.is_some_and(|p| p.contains_key(&key));
                for (re, pat_schema) in &pattern_properties {
                    if re.is_match(&key) {
                        matched = true;
                        if let Some(cur) = out.get(&key).cloned() {
                            out.insert(key.clone(), self.drop_optional_nulls(&cur, pat_schema));
                        }
                    }
                }
                if !matched
                    && let Some(add_schema) = additional
                    && let Some(cur) = out.get(&key).cloned()
                {
                    out.insert(key.clone(), self.drop_optional_nulls(&cur, add_schema));
                }
            }
        }

        Value::Object(out)
    }

    // -------------------------------------------------------------------------
    // Pass 2: convert (TypeBox Value.Convert rules)
    // -------------------------------------------------------------------------

    fn convert(&mut self, node: &Value, value: &Value) -> Value {
        if self.depth >= self.options.max_depth {
            return value.clone();
        }
        let Value::Object(obj) = node else {
            return value.clone();
        };
        if let Some(Value::String(ref_str)) = obj.get("$ref") {
            return match resolve_ref(self.root, self.anchors, ref_str) {
                ResolvedRef::Found(target) if target.is_object() => {
                    self.reenter(target, value, Some(ref_str))
                }
                ResolvedRef::Anchor(target) if target.is_object() => {
                    self.reenter(&target, value, Some(ref_str))
                }
                _ => value.clone(),
            };
        }
        self.depth += 1;
        let result = self.convert_node(node, obj, value);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    fn convert_node(&mut self, node: &Value, obj: &JsonObject, value: &Value) -> Value {
        if let Some(const_val) = obj.get("const") {
            return convert_literal(const_val, value);
        }
        if let Some(Value::Array(enum_vals)) = obj.get("enum") {
            return self.convert_enum(node, enum_vals, value);
        }
        if let Some(Value::Array(any_of)) = obj.get("anyOf") {
            return self.convert_union(node, obj, any_of, value);
        }
        if let Some(Value::Array(one_of)) = obj.get("oneOf") {
            return self.convert_union(node, obj, one_of, value);
        }
        if let Some(Value::Array(all_of)) = obj.get("allOf") {
            let mut next = value.clone();
            for member in all_of {
                next = self.convert(member, &next);
            }
            return self.convert_members(obj, &next);
        }

        match obj.get("type").and_then(|v| v.as_str()) {
            Some("string") => try_string(value).unwrap_or_else(|| value.clone()),
            Some("number") => try_number(value)
                .and_then(Number::from_f64)
                .map(Value::Number)
                .unwrap_or_else(|| value.clone()),
            Some("integer") => try_number(value)
                .map(|n| Value::Number(Number::from(n.trunc() as i64)))
                .unwrap_or_else(|| value.clone()),
            Some("boolean") => try_boolean(value)
                .map(Value::Bool)
                .unwrap_or_else(|| value.clone()),
            Some("null") => try_null(value).unwrap_or_else(|| value.clone()),
            Some("array") => self.convert_array(obj, value),
            Some("object") => self.convert_object(obj, value),
            None => {
                if obj.contains_key("properties") || obj.contains_key("patternProperties") {
                    return self.convert_object(obj, value);
                }
                if obj.contains_key("items") || obj.contains_key("prefixItems") {
                    return self.convert_array(obj, value);
                }
                value.clone()
            }
            Some(_) => value.clone(),
        }
    }

    fn convert_enum(&self, node: &Value, enum_vals: &[Value], value: &Value) -> Value {
        if enum_vals.iter().any(|m| json_values_equal(m, value)) {
            return value.clone();
        }
        for member in enum_vals {
            let candidate = convert_literal(member, value);
            if !json_values_equal(&candidate, value) && self.passes(node, &candidate) == Some(true)
            {
                return candidate;
            }
        }
        value.clone()
    }

    fn convert_union(
        &mut self,
        node: &Value,
        obj: &JsonObject,
        branches: &[Value],
        value: &Value,
    ) -> Value {
        if branches.iter().any(|b| self.passes(b, value) == Some(true)) {
            return self.convert_members(obj, value);
        }
        for branch in branches {
            let converted_branch = self.convert(branch, value);
            let candidate = self.convert_members(obj, &converted_branch);
            if self.passes(node, &candidate) == Some(true) {
                return candidate;
            }
        }
        self.convert_members(obj, value)
    }

    fn convert_members(&mut self, obj: &JsonObject, value: &Value) -> Value {
        if obj.contains_key("properties")
            || obj.contains_key("patternProperties")
            || obj.contains_key("additionalProperties")
        {
            self.convert_object(obj, value)
        } else {
            value.clone()
        }
    }

    fn convert_array(&mut self, obj: &JsonObject, value: &Value) -> Value {
        let prefix = obj
            .get("items")
            .and_then(|v| v.as_array())
            .or_else(|| obj.get("prefixItems").and_then(|v| v.as_array()));

        if let Some(prefix_schemas) = prefix {
            let Value::Array(items) = value else {
                return value.clone();
            };
            let rest = if obj.get("items").is_some_and(|v| v.is_array()) {
                None
            } else {
                obj.get("items")
            };
            let out: Vec<Value> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let schema = if i < prefix_schemas.len() {
                        Some(&prefix_schemas[i])
                    } else {
                        rest
                    };
                    match schema {
                        Some(s) => self.convert(s, item),
                        None => item.clone(),
                    }
                })
                .collect();
            return Value::Array(out);
        }

        if let Value::Array(items) = value {
            let Some(item_schema) = obj.get("items") else {
                return value.clone();
            };
            let out: Vec<Value> = items
                .iter()
                .map(|item| self.convert(item_schema, item))
                .collect();
            return Value::Array(out);
        }

        // 非元组数组遇到单个标量/对象时自动包装为单元素数组 [value]
        let single = match obj.get("items") {
            Some(item_schema) if item_schema.is_object() => self.reenter(item_schema, value, None),
            _ => value.clone(),
        };
        Value::Array(vec![single])
    }

    fn convert_object(&mut self, obj: &JsonObject, value: &Value) -> Value {
        let Value::Object(val_map) = value else {
            return value.clone();
        };
        let properties = obj.get("properties").and_then(|v| v.as_object());
        let patterns: Vec<(CompiledPattern, &Value)> = obj
            .get("patternProperties")
            .and_then(|v| v.as_object())
            .map(|pats| {
                let mut dummy_errs = Vec::new();
                pats.iter()
                    .map(|(p, s)| {
                        (
                            CompiledPattern::compile(
                                p,
                                "",
                                "patternProperties",
                                self.options.regex_backtrack_limit,
                                &mut dummy_errs,
                            ),
                            s,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let additional = obj.get("additionalProperties").filter(|v| v.is_object());

        let mut out = val_map.clone();
        let keys: Vec<String> = val_map.keys().cloned().collect();
        for key in keys {
            let mut next = val_map[&key].clone();
            let mut matched = false;
            if let Some(props) = properties
                && let Some(prop_schema) = props.get(&key)
            {
                matched = true;
                next = self.convert(prop_schema, &next);
            }
            for (re, pat_schema) in &patterns {
                if re.is_match(&key) {
                    matched = true;
                    next = self.convert(pat_schema, &next);
                }
            }
            if !matched && let Some(add_schema) = additional {
                next = self.convert(add_schema, &next);
            }
            out.insert(key, next);
        }
        Value::Object(out)
    }

    // -------------------------------------------------------------------------
    // Pass 3: coerce_types (Ajv-style primitive coercion for type arrays)
    // -------------------------------------------------------------------------

    fn coerce_types(&mut self, value: &Value, schema: &Value) -> Value {
        if self.depth >= self.options.max_depth {
            return value.clone();
        }
        let Value::Object(obj) = schema else {
            return value.clone();
        };
        if let Some(Value::String(ref_str)) = obj.get("$ref") {
            let key = format!("coerce_ref:{ref_str}");
            if self
                .entered
                .iter()
                .any(|(k, v)| *k == key && json_values_equal(v, value))
            {
                return value.clone();
            }
            return match resolve_ref(self.root, self.anchors, ref_str) {
                ResolvedRef::Found(target) if target.is_object() => {
                    self.entered.push((key, value.clone()));
                    self.depth += 1;
                    let res = self.coerce_types(value, target);
                    self.depth = self.depth.saturating_sub(1);
                    self.entered.pop();
                    res
                }
                ResolvedRef::Anchor(target) if target.is_object() => {
                    self.entered.push((key, value.clone()));
                    self.depth += 1;
                    let res = self.coerce_types(value, &target);
                    self.depth = self.depth.saturating_sub(1);
                    self.entered.pop();
                    res
                }
                _ => value.clone(),
            };
        }

        self.depth += 1;
        let result = self.coerce_types_inner(value, obj);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    fn coerce_types_inner(&mut self, value: &Value, obj: &JsonObject) -> Value {
        let mut next = value.clone();
        if let Some(Value::Array(all_of)) = obj.get("allOf") {
            for member in all_of {
                next = self.coerce_types(&next, member);
            }
        }
        if let Some(Value::Array(any_of)) = obj.get("anyOf") {
            next = self.coerce_union(&next, any_of);
        }
        if let Some(Value::Array(one_of)) = obj.get("oneOf") {
            next = self.coerce_union(&next, one_of);
        }

        let types = types_of(obj);
        let matches_union_member =
            types.len() > 1 && types.iter().any(|t| matches_json_type(&next, t));
        if !types.is_empty() && !matches_union_member {
            for t in &types {
                let candidate = coerce_primitive(&next, t);
                if !json_values_equal(&candidate, &next) {
                    next = candidate;
                    break;
                }
            }
        }

        if types.iter().any(|t| t == "object")
            && let Value::Object(map) = &next
        {
            next = Value::Object(self.coerce_properties(map, obj));
        }
        if types.iter().any(|t| t == "array")
            && let Value::Array(items) = &next
        {
            next = Value::Array(self.coerce_items(items, obj));
        }
        next
    }

    fn coerce_union(&mut self, value: &Value, branches: &[Value]) -> Value {
        for branch in branches {
            if self.passes(branch, value) == Some(true) {
                return value.clone();
            }
        }
        for branch in branches {
            let coerced = self.coerce_types(value, branch);
            if self.passes(branch, &coerced) == Some(true) {
                return coerced;
            }
        }
        value.clone()
    }

    fn coerce_properties(&mut self, val_map: &JsonObject, obj: &JsonObject) -> JsonObject {
        let properties = obj.get("properties").and_then(|v| v.as_object());
        let patterns: Vec<(CompiledPattern, &Value)> = obj
            .get("patternProperties")
            .and_then(|v| v.as_object())
            .map(|pats| {
                let mut dummy_errs = Vec::new();
                pats.iter()
                    .map(|(p, s)| {
                        (
                            CompiledPattern::compile(
                                p,
                                "",
                                "patternProperties",
                                self.options.regex_backtrack_limit,
                                &mut dummy_errs,
                            ),
                            s,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let additional = obj.get("additionalProperties").filter(|v| v.is_object());

        let mut out = val_map.clone();
        if let Some(props) = properties {
            for (k, prop_schema) in props {
                if let Some(cur) = out.get(k).cloned() {
                    out.insert(k.clone(), self.coerce_types(&cur, prop_schema));
                }
            }
        }
        let keys: Vec<String> = out.keys().cloned().collect();
        for key in keys {
            let mut matched = properties.is_some_and(|p| p.contains_key(&key));
            for (re, pat_schema) in &patterns {
                if re.is_match(&key) {
                    matched = true;
                    if let Some(cur) = out.get(&key).cloned() {
                        out.insert(key.clone(), self.coerce_types(&cur, pat_schema));
                    }
                }
            }
            if !matched
                && let Some(add_schema) = additional
                && let Some(cur) = out.get(&key).cloned()
            {
                out.insert(key.clone(), self.coerce_types(&cur, add_schema));
            }
        }
        out
    }

    fn coerce_items(&mut self, items: &[Value], obj: &JsonObject) -> Vec<Value> {
        items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                if let Some(s) = item_schema_at(obj, i)
                    && s.is_object()
                {
                    self.coerce_types(item, s)
                } else {
                    item.clone()
                }
            })
            .collect()
    }
}

fn item_schema_at(obj: &JsonObject, index: usize) -> Option<&Value> {
    if let Some(Value::Array(items)) = obj.get("items") {
        return items.get(index);
    }
    if let Some(Value::Array(prefix)) = obj.get("prefixItems")
        && index < prefix.len()
    {
        return prefix.get(index);
    }
    obj.get("items")
}

fn convert_literal(literal: &Value, value: &Value) -> Value {
    if json_values_equal(literal, value) {
        return value.clone();
    }
    let converted = match literal {
        Value::String(_) => try_string(value),
        Value::Number(_) => try_number(value).and_then(|n| {
            if literal.is_i64() || literal.is_u64() {
                if n.fract() == 0.0 {
                    Some(Value::Number(Number::from(n as i64)))
                } else {
                    Number::from_f64(n).map(Value::Number)
                }
            } else {
                Number::from_f64(n).map(Value::Number)
            }
        }),
        Value::Bool(_) => try_boolean(value).map(Value::Bool),
        _ => None,
    };
    if let Some(cand) = converted
        && json_values_equal(&cand, literal)
    {
        return literal.clone();
    }
    value.clone()
}

fn try_string(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Bool(b) => Some(Value::String(b.to_string())),
        Value::Number(n) => Some(Value::String(n.to_string())),
        Value::Null => Some(Value::String("null".to_owned())),
        _ => None,
    }
}

fn try_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Some(0.0);
            }
            if let Ok(n) = trimmed.parse::<f64>()
                && n.is_finite()
            {
                return Some(n);
            }
            let lower = s.to_ascii_lowercase();
            if lower == "false" {
                return Some(0.0);
            }
            if lower == "true" {
                return Some(1.0);
            }
            if let Some(stripped) = s.strip_suffix('n')
                && let Ok(i) = stripped.parse::<i64>()
            {
                return Some(i as f64);
            }
            None
        }
        _ => None,
    }
}

fn try_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => match n.as_f64() {
            Some(0.0) => Some(false),
            Some(1.0) => Some(true),
            _ => None,
        },
        Value::Null => Some(false),
        Value::String(s) => {
            let lower = s.to_ascii_lowercase();
            if lower == "false" || lower == "0" {
                Some(false)
            } else if lower == "true" || lower == "1" {
                Some(true)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn try_null(value: &Value) -> Option<Value> {
    match value {
        Value::Null => Some(Value::Null),
        Value::Bool(false) => Some(Value::Null),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(Value::Null),
        Value::String(s) => {
            let lower = s.to_ascii_lowercase();
            if lower.is_empty() || lower == "null" || lower == "undefined" || lower == "0" {
                Some(Value::Null)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn types_of(obj: &JsonObject) -> Vec<String> {
    match obj.get("type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

fn coerce_primitive(value: &Value, target_type: &str) -> Value {
    match target_type {
        "number" | "integer" => {
            if value.is_null() {
                return Value::Number(Number::from(0));
            }
            if let Value::String(s) = value
                && !s.trim().is_empty()
                && let Ok(parsed) = s.trim().parse::<f64>()
                && parsed.is_finite()
            {
                if target_type == "integer" {
                    if parsed.fract() == 0.0 {
                        return Value::Number(Number::from(parsed as i64));
                    }
                } else if let Some(num) = Number::from_f64(parsed) {
                    return Value::Number(num);
                }
            }
            if let Value::Bool(b) = value {
                return Value::Number(Number::from(if *b { 1 } else { 0 }));
            }
            value.clone()
        }
        "boolean" => {
            if value.is_null() {
                return Value::Bool(false);
            }
            if value == &Value::String("true".to_owned()) || value.as_f64() == Some(1.0) {
                return Value::Bool(true);
            }
            if value == &Value::String("false".to_owned()) || value.as_f64() == Some(0.0) {
                return Value::Bool(false);
            }
            value.clone()
        }
        "string" => {
            if value.is_null() {
                return Value::String(String::new());
            }
            match value {
                Value::Number(n) => Value::String(n.to_string()),
                Value::Bool(b) => Value::String(b.to_string()),
                _ => value.clone(),
            }
        }
        "null" => {
            if value == &Value::String(String::new())
                || value.as_f64() == Some(0.0)
                || value == &Value::Bool(false)
            {
                Value::Null
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

// =============================================================================
// Slow-Path Explainer (对照 packages/schema/src/explain.ts)
// =============================================================================

struct Explainer<'a> {
    root: &'a Value,
    anchors: &'a BTreeMap<String, Value>,
    options: &'a CompileOptions,
    ref_stack: RefCell<Vec<(String, usize)>>,
    depth: std::cell::Cell<usize>,
}

impl<'a> Explainer<'a> {
    fn new(
        root: &'a Value,
        anchors: &'a BTreeMap<String, Value>,
        options: &'a CompileOptions,
    ) -> Self {
        Self {
            root,
            anchors,
            options,
            ref_stack: RefCell::new(Vec::new()),
            depth: std::cell::Cell::new(0),
        }
    }

    fn errors(&self, value: &Value) -> Vec<ValidationError> {
        let mut out = Vec::new();
        self.ref_stack.borrow_mut().clear();
        self.depth.set(0);
        self.walk(self.root, value, "", &mut out);
        if out.is_empty() {
            out.push(ValidationError {
                path: String::new(),
                keyword: "schema".to_owned(),
                message: "invalid".to_owned(),
            });
        }
        out
    }

    fn ok(&self, node: &Value, v: &Value) -> bool {
        let mut sub_opts = self.options.clone();
        sub_opts.max_depth = self
            .options
            .max_depth
            .saturating_sub(self.ref_stack.borrow().len());
        check_subschema(node, self.root, self.anchors, &sub_opts, v)
    }

    fn walk(&self, node: &Value, v: &Value, path: &str, out: &mut Vec<ValidationError>) {
        let current_depth = self.depth.get();
        if current_depth >= self.options.max_depth {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "maxDepth".to_owned(),
                message: format!(
                    "maximum validation depth ({}) exceeded",
                    self.options.max_depth
                ),
            });
            return;
        }

        match node {
            Value::Bool(true) => return,
            Value::Bool(false) => {
                out.push(ValidationError {
                    path: path.to_owned(),
                    keyword: "false".to_owned(),
                    message: "no value is allowed here".to_owned(),
                });
                return;
            }
            Value::Object(_) => {}
            _ => return,
        }

        if self.ok(node, v) {
            return;
        }
        let Value::Object(obj) = node else {
            return;
        };

        self.depth.set(current_depth + 1);
        self.walk_object_node(obj, v, path, out);
        self.depth.set(current_depth);
    }

    fn walk_object_node(
        &self,
        obj: &JsonObject,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        // 1. Check type first; if type mismatches, stop walking inner keywords
        if let Some(type_val) = obj.get("type") {
            let types: Vec<String> = match type_val {
                Value::String(s) => vec![s.clone()],
                Value::Array(arr) => arr
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                    .collect(),
                _ => Vec::new(),
            };
            if !types.is_empty() && !types.iter().any(|t| matches_json_type(v, t)) {
                out.push(ValidationError {
                    path: path.to_owned(),
                    keyword: "type".to_owned(),
                    message: format!("expected {}, got {}", types.join(" or "), json_type_name(v)),
                });
                return;
            }
        }

        if let Some(Value::Array(enum_vals)) = obj.get("enum")
            && !enum_vals.iter().any(|item| json_values_equal(v, item))
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "enum".to_owned(),
                message: format!("expected one of {}", list_values(enum_vals)),
            });
        }

        if let Some(const_val) = obj.get("const")
            && !json_values_equal(v, const_val)
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "const".to_owned(),
                message: format!(
                    "expected {}",
                    serde_json::to_string(const_val).unwrap_or_else(|_| "value".to_owned())
                ),
            });
        }

        if let Value::String(s) = v {
            self.walk_string(obj, s, path, out);
        }
        if let Some(n) = v.as_f64() {
            self.walk_number(obj, n, path, out);
        }
        if let Value::Object(map) = v {
            self.walk_object(obj, map, path, out);
        }
        if let Value::Array(items) = v {
            self.walk_array(obj, items, path, out);
        }

        if let Some(Value::Array(all_of)) = obj.get("allOf") {
            for s in all_of {
                self.walk(s, v, path, out);
            }
        }
        if let Some(Value::Array(any_of)) = obj.get("anyOf") {
            self.walk_union(any_of, v, path, out, false);
        }
        if let Some(Value::Array(one_of)) = obj.get("oneOf") {
            self.walk_union(one_of, v, path, out, true);
        }
        if let Some(not_sub) = obj.get("not")
            && self.ok(not_sub, v)
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "not".to_owned(),
                message: "must not match the schema in not".to_owned(),
            });
        }
        if let Some(if_sub) = obj.get("if") {
            let branch = if self.ok(if_sub, v) {
                obj.get("then")
            } else {
                obj.get("else")
            };
            if let Some(b) = branch {
                self.walk(b, v, path, out);
            }
        }
        if let Some(Value::String(ref_str)) = obj.get("$ref") {
            self.walk_ref(ref_str, v, path, out);
        }
    }

    fn walk_ref(&self, ref_str: &str, v: &Value, path: &str, out: &mut Vec<ValidationError>) {
        let val_ptr = v as *const Value as usize;
        {
            let mut stack = self.ref_stack.borrow_mut();
            if stack.len() >= self.options.max_depth {
                out.push(ValidationError {
                    path: path.to_owned(),
                    keyword: "maxDepth".to_owned(),
                    message: format!(
                        "maximum validation depth ({}) exceeded",
                        self.options.max_depth
                    ),
                });
                return;
            }
            if stack.iter().any(|(r, p)| r == ref_str && *p == val_ptr) {
                out.push(ValidationError {
                    path: path.to_owned(),
                    keyword: "$ref".to_owned(),
                    message: format!("circular reference {ref_str}"),
                });
                return;
            }
            stack.push((ref_str.to_owned(), val_ptr));
        }

        match resolve_ref(self.root, self.anchors, ref_str) {
            ResolvedRef::Found(target) => self.walk(target, v, path, out),
            ResolvedRef::Anchor(target) => self.walk(&target, v, path, out),
            ResolvedRef::Unresolved => {
                out.push(ValidationError {
                    path: path.to_owned(),
                    keyword: "$ref".to_owned(),
                    message: format!("unresolved reference {ref_str}"),
                });
            }
        }

        self.ref_stack.borrow_mut().pop();
    }

    fn walk_string(&self, obj: &JsonObject, s: &str, path: &str, out: &mut Vec<ValidationError>) {
        let len = s.chars().count();
        if let Some(min) = obj.get("minLength").and_then(|v| v.as_u64())
            && len < min as usize
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "minLength".to_owned(),
                message: format!("expected at least {min} characters"),
            });
        }
        if let Some(max) = obj.get("maxLength").and_then(|v| v.as_u64())
            && len > max as usize
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "maxLength".to_owned(),
                message: format!("expected at most {max} characters"),
            });
        }
        if let Some(pat) = obj.get("pattern").and_then(|v| v.as_str()) {
            let mut dummy_errs = Vec::new();
            let compiled = CompiledPattern::compile(
                pat,
                path,
                "pattern",
                self.options.regex_backtrack_limit,
                &mut dummy_errs,
            );
            match compiled.evaluate(s) {
                PatternMatchOutcome::Matched => {}
                PatternMatchOutcome::NotMatched => {
                    out.push(ValidationError {
                        path: path.to_owned(),
                        keyword: "pattern".to_owned(),
                        message: format!("expected to match {pat}"),
                    });
                }
                PatternMatchOutcome::InvalidPattern(err) => {
                    out.push(ValidationError {
                        path: path.to_owned(),
                        keyword: "pattern".to_owned(),
                        message: format!("invalid regex pattern {pat:?}: {err}"),
                    });
                }
                PatternMatchOutcome::LimitExceeded(err) => {
                    out.push(ValidationError {
                        path: path.to_owned(),
                        keyword: "pattern".to_owned(),
                        message: format!("regex match limit exceeded for {pat:?}: {err}"),
                    });
                }
            }
        }
        if let Some(fmt) = obj.get("format").and_then(|v| v.as_str())
            && !self.options.check_format(fmt, s)
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "format".to_owned(),
                message: format!("expected format {fmt}"),
            });
        }
    }

    fn walk_number(&self, obj: &JsonObject, n: f64, path: &str, out: &mut Vec<ValidationError>) {
        if let Some(min) = obj.get("minimum")
            && let Some(min_f) = min.as_f64()
            && n < min_f
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "minimum".to_owned(),
                message: format!("expected >= {min}"),
            });
        }
        if let Some(max) = obj.get("maximum")
            && let Some(max_f) = max.as_f64()
            && n > max_f
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "maximum".to_owned(),
                message: format!("expected <= {max}"),
            });
        }
        if let Some(xmin) = obj.get("exclusiveMinimum")
            && let Some(xmin_f) = xmin.as_f64()
            && n <= xmin_f
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "exclusiveMinimum".to_owned(),
                message: format!("expected > {xmin}"),
            });
        }
        if let Some(xmax) = obj.get("exclusiveMaximum")
            && let Some(xmax_f) = xmax.as_f64()
            && n >= xmax_f
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "exclusiveMaximum".to_owned(),
                message: format!("expected < {xmax}"),
            });
        }
        if let Some(mult) = obj.get("multipleOf")
            && let Some(mult_f) = mult.as_f64()
            && mult_f > 0.0
            && !is_multiple_of(n, mult_f)
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "multipleOf".to_owned(),
                message: format!("expected a multiple of {mult}"),
            });
        }
    }

    fn walk_object(
        &self,
        obj: &JsonObject,
        map: &JsonObject,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        let properties = obj.get("properties").and_then(|v| v.as_object());
        if let Some(Value::Array(required)) = obj.get("required") {
            for req in required {
                if let Some(k) = req.as_str()
                    && !map.contains_key(k)
                {
                    out.push(ValidationError {
                        path: append_pointer(path, k),
                        keyword: "required".to_owned(),
                        message: "missing".to_owned(),
                    });
                }
            }
        }

        if let Some(props) = properties {
            for (k, prop_schema) in props {
                if let Some(val) = map.get(k) {
                    self.walk(prop_schema, val, &append_pointer(path, k), out);
                }
            }
        }

        let patterns: Vec<(CompiledPattern, &Value)> = obj
            .get("patternProperties")
            .and_then(|v| v.as_object())
            .map(|pats| {
                let mut dummy_errs = Vec::new();
                pats.iter()
                    .map(|(p, s)| {
                        (
                            CompiledPattern::compile(
                                p,
                                path,
                                "patternProperties",
                                self.options.regex_backtrack_limit,
                                &mut dummy_errs,
                            ),
                            s,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        for (k, val) in map {
            let mut matched = properties.is_some_and(|p| p.contains_key(k));
            for (re, pat_schema) in &patterns {
                match re.evaluate(k) {
                    PatternMatchOutcome::Matched => {
                        matched = true;
                        self.walk(pat_schema, val, &append_pointer(path, k), out);
                    }
                    PatternMatchOutcome::NotMatched => {}
                    PatternMatchOutcome::InvalidPattern(err) => {
                        out.push(ValidationError {
                            path: append_pointer(path, k),
                            keyword: "patternProperties".to_owned(),
                            message: format!(
                                "invalid patternProperties regex {:?}: {err}",
                                re.pattern_str()
                            ),
                        });
                    }
                    PatternMatchOutcome::LimitExceeded(err) => {
                        out.push(ValidationError {
                            path: append_pointer(path, k),
                            keyword: "patternProperties".to_owned(),
                            message: format!(
                                "patternProperties regex match limit exceeded {:?}: {err}",
                                re.pattern_str()
                            ),
                        });
                    }
                }
            }
            if !matched && let Some(add) = obj.get("additionalProperties") {
                if add == &Value::Bool(false) {
                    out.push(ValidationError {
                        path: append_pointer(path, k),
                        keyword: "additionalProperties".to_owned(),
                        message: "unknown field".to_owned(),
                    });
                } else {
                    self.walk(add, val, &append_pointer(path, k), out);
                }
            }
            if let Some(pn) = obj.get("propertyNames")
                && !self.ok(pn, &Value::String(k.clone()))
            {
                out.push(ValidationError {
                    path: append_pointer(path, k),
                    keyword: "propertyNames".to_owned(),
                    message: "invalid property name".to_owned(),
                });
            }
        }

        if let Some(min) = obj.get("minProperties").and_then(|v| v.as_u64())
            && map.len() < min as usize
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "minProperties".to_owned(),
                message: format!("expected at least {min} properties"),
            });
        }
        if let Some(max) = obj.get("maxProperties").and_then(|v| v.as_u64())
            && map.len() > max as usize
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "maxProperties".to_owned(),
                message: format!("expected at most {max} properties"),
            });
        }

        if let Some(Value::Object(dr)) = obj.get("dependentRequired") {
            for (k, reqs) in dr {
                if map.contains_key(k)
                    && let Some(arr) = reqs.as_array()
                {
                    for r in arr.iter().filter_map(|x| x.as_str()) {
                        if !map.contains_key(r) {
                            out.push(ValidationError {
                                path: append_pointer(path, r),
                                keyword: "dependentRequired".to_owned(),
                                message: format!("required when {k} is present"),
                            });
                        }
                    }
                }
            }
        }
        if let Some(Value::Object(ds)) = obj.get("dependentSchemas") {
            for (k, s) in ds {
                if map.contains_key(k) {
                    self.walk(s, &Value::Object(map.clone()), path, out);
                }
            }
        }
    }

    fn walk_array(
        &self,
        obj: &JsonObject,
        items: &[Value],
        path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        let (prefix, rest): (Option<&[Value]>, Option<&Value>) =
            if let Some(Value::Array(arr)) = obj.get("items") {
                (Some(arr.as_slice()), obj.get("additionalItems"))
            } else {
                (
                    obj.get("prefixItems")
                        .and_then(|v| v.as_array())
                        .map(|v| v.as_slice()),
                    obj.get("items"),
                )
            };

        for (i, item) in items.iter().enumerate() {
            let sub = if let Some(p) = prefix
                && i < p.len()
            {
                Some(&p[i])
            } else {
                rest
            };
            if let Some(s) = sub {
                self.walk(s, item, &format!("{path}/{i}"), out);
            }
        }

        if let Some(min) = obj.get("minItems").and_then(|v| v.as_u64())
            && items.len() < min as usize
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "minItems".to_owned(),
                message: format!("expected at least {min} items"),
            });
        }
        if let Some(max) = obj.get("maxItems").and_then(|v| v.as_u64())
            && items.len() > max as usize
        {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "maxItems".to_owned(),
                message: format!("expected at most {max} items"),
            });
        }
        if obj.get("uniqueItems") == Some(&Value::Bool(true)) && !has_unique_items(items) {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: "uniqueItems".to_owned(),
                message: "expected unique items".to_owned(),
            });
        }
        if let Some(contains) = obj.get("contains") {
            let min_c = obj
                .get("minContains")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(1);
            let max_c = obj
                .get("maxContains")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let matched = items.iter().filter(|item| self.ok(contains, item)).count();
            if matched < min_c || max_c.is_some_and(|m| matched > m) {
                let msg = match (min_c, max_c) {
                    (min, None) => format!(
                        "expected at least {min} matching {}",
                        if min == 1 { "item" } else { "items" }
                    ),
                    (0, Some(max)) => format!(
                        "expected at most {max} matching {}",
                        if max == 1 { "item" } else { "items" }
                    ),
                    (min, Some(max)) => format!("expected between {min} and {max} matching items"),
                };
                out.push(ValidationError {
                    path: path.to_owned(),
                    keyword: "contains".to_owned(),
                    message: msg,
                });
            }
        }
    }

    fn walk_union(
        &self,
        branches: &[Value],
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
        exactly_one: bool,
    ) {
        let matches = branches.iter().filter(|s| self.ok(s, v)).count();
        if matches == 1 || (!exactly_one && matches > 0) {
            return;
        }
        let keyword = if exactly_one { "oneOf" } else { "anyOf" };
        if matches > 1 {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: keyword.to_owned(),
                message: "matches more than one alternative".to_owned(),
            });
            return;
        }

        // Tagged union detection
        if let Value::Object(map) = v
            && let Some((tag_key, tag_values)) = self.tag_of(branches)
        {
            if let Some(actual_tag) = map.get(&tag_key)
                && let Some(idx) = tag_values
                    .iter()
                    .position(|tv| json_values_equal(tv, actual_tag))
            {
                self.walk(&branches[idx], v, path, out);
                return;
            }
            out.push(ValidationError {
                path: append_pointer(path, &tag_key),
                keyword: keyword.to_owned(),
                message: format!("expected one of {}", list_values(&tag_values)),
            });
            return;
        }

        // Heuristic: pick branch with most declared input keys, then deepest first error path
        let mut best: Option<(Vec<ValidationError>, usize)> = None;
        for branch in branches {
            let mut branch_errors = Vec::new();
            self.walk(branch, v, path, &mut branch_errors);
            if branch_errors.is_empty() {
                continue;
            }
            let declared = match v {
                Value::Object(map) => self.declared_keys(branch, map),
                _ => 0,
            };
            let is_better = match &best {
                None => true,
                Some((best_errs, best_decl)) => {
                    declared > *best_decl
                        || (declared == *best_decl
                            && branch_errors[0].path.len() > best_errs[0].path.len())
                }
            };
            if is_better {
                best = Some((branch_errors, declared));
            }
        }

        if let Some((errs, _)) = best {
            out.extend(errs);
        } else {
            out.push(ValidationError {
                path: path.to_owned(),
                keyword: keyword.to_owned(),
                message: "matches no alternative".to_owned(),
            });
        }
    }

    fn resolve_branch_object(&self, branch: &Value) -> Option<JsonObject> {
        let mut current = branch.as_object()?.clone();
        let mut visited_refs = BTreeSet::new();
        while let Some(Value::String(ref_str)) = current.get("$ref") {
            if !visited_refs.insert(ref_str.clone()) {
                return None;
            }
            match resolve_ref(self.root, self.anchors, ref_str) {
                ResolvedRef::Found(Value::Object(o)) => current = o.clone(),
                ResolvedRef::Anchor(Value::Object(o)) => current = o,
                _ => return None,
            }
        }
        Some(current)
    }

    fn declared_keys(&self, branch: &Value, map: &JsonObject) -> usize {
        let Some(obj) = self.resolve_branch_object(branch) else {
            return 0;
        };
        let Some(Value::Object(props)) = obj.get("properties") else {
            return 0;
        };
        map.keys().filter(|k| props.contains_key(*k)).count()
    }

    fn tag_of(&self, branches: &[Value]) -> Option<(String, Vec<Value>)> {
        let resolved: Vec<JsonObject> = branches
            .iter()
            .filter_map(|b| self.resolve_branch_object(b))
            .collect();
        if resolved.len() != branches.len() || resolved.is_empty() {
            return None;
        }
        let first_props = resolved[0].get("properties")?.as_object()?;
        for key in first_props.keys() {
            let mut values = Vec::with_capacity(resolved.len());
            let mut valid = true;
            for branch_obj in &resolved {
                let const_val = branch_obj
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .and_then(|p| p.get(key))
                    .and_then(|s| s.as_object())
                    .and_then(|s| s.get("const"));
                match const_val {
                    Some(v) if !v.is_object() && !v.is_array() => {
                        if values.iter().any(|existing| json_values_equal(existing, v)) {
                            valid = false;
                            break;
                        }
                        values.push(v.clone());
                    }
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && values.len() == branches.len() {
                return Some((key.clone(), values));
            }
        }
        None
    }
}

// =============================================================================
// Public Tool Argument Validation Entrypoints (对照 pi-ai/utils/validation.ts)
// =============================================================================

/// 将 JSON Pointer（如 `/edits/0/oldText`）转换为面向模型的点分路径（`edits.0.oldText`，空路径为 `root`）。
pub fn format_validation_path(pointer: &str) -> String {
    if pointer.is_empty() {
        return "root".to_owned();
    }
    pointer
        .strip_prefix('/')
        .unwrap_or(pointer)
        .split('/')
        .map(|seg| seg.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".")
}

/// 按名称查找工具并对 `tool_call` 参数执行深拷贝、三阶段宽松类型转换与 JSON Schema 校验。
pub fn validate_tool_call(tools: &[AgentTool], tool_call: &ToolCall) -> Result<JsonValue, String> {
    let tool = tools
        .iter()
        .find(|t| t.name == tool_call.name)
        .ok_or_else(|| format!("Tool \"{}\" not found", tool_call.name))?;
    validate_tool_arguments(tool, tool_call)
}

/// 对 `tool_call` 参数执行深拷贝、三阶段宽松类型转换与 JSON Schema 校验。
pub fn validate_tool_arguments(
    tool: &AgentTool,
    tool_call: &ToolCall,
) -> Result<JsonValue, String> {
    if let Some(err) = &tool_call.arguments_error {
        return Err(format!(
            "Invalid arguments for tool \"{}\": {err}",
            tool.name
        ));
    }

    let cloned_args = Value::Object(tool_call.arguments.clone());
    let validator = tool.compiled_validator();
    match validator.parse(&cloned_args) {
        Ok(coerced) => Ok(coerced),
        Err(errors) => {
            let formatted_errors = errors
                .iter()
                .map(|e| format!("  - {}: {}", format_validation_path(&e.path), e.message))
                .collect::<Vec<_>>()
                .join("\n");
            let pretty_received = serde_json::to_string_pretty(&tool_call.arguments)
                .unwrap_or_else(|_| "{}".to_owned());
            Err(format!(
                "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
                tool.name, formatted_errors, pretty_received
            ))
        }
    }
}

// =============================================================================
// Shared Helper Functions
// =============================================================================

fn matches_json_type(v: &Value, expected: &str) -> bool {
    match expected {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => match v {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        },
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => false,
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0) {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn json_values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => match (na.as_f64(), nb.as_f64()) {
            (Some(fa), Some(fb)) => fa == fb,
            _ => na == nb,
        },
        (Value::Array(xa), Value::Array(xb)) => {
            xa.len() == xb.len()
                && xa
                    .iter()
                    .zip(xb.iter())
                    .all(|(ia, ib)| json_values_equal(ia, ib))
        }
        (Value::Object(ma), Value::Object(mb)) => {
            ma.len() == mb.len()
                && ma
                    .iter()
                    .all(|(k, va)| mb.get(k).is_some_and(|vb| json_values_equal(va, vb)))
        }
        _ => a == b,
    }
}

fn has_unique_items(items: &[Value]) -> bool {
    for i in 1..items.len() {
        for j in 0..i {
            if json_values_equal(&items[i], &items[j]) {
                return false;
            }
        }
    }
    true
}

fn is_multiple_of(v: f64, d: f64) -> bool {
    if d == 0.0 {
        return false;
    }
    let q = v / d;
    if q.is_finite() && (q - q.round()).abs() < 1e-9 {
        return true;
    }
    (v % d).abs() < 1e-9
}

fn append_pointer(path: &str, key: &str) -> String {
    let escaped = key.replace('~', "~0").replace('/', "~1");
    format!("{path}/{escaped}")
}

fn list_values(values: &[Value]) -> String {
    const LISTED_VALUES: usize = 20;
    if values.len() <= LISTED_VALUES {
        serde_json::to_string(values).unwrap_or_else(|_| "[]".to_owned())
    } else {
        let prefix =
            serde_json::to_string(&values[..LISTED_VALUES]).unwrap_or_else(|_| "[]".to_owned());
        let trimmed = prefix.strip_suffix(']').unwrap_or(&prefix);
        format!("{trimmed}, ... and {} more]", values.len() - LISTED_VALUES)
    }
}
