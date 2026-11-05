use std::collections::BTreeMap;
use std::sync::Arc;

use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{
    AnyModel, ApiKeyAuth, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    AuthResult, CreateProviderOptions, JsonValue, Message, Model, ModelCost, ModelInputModality,
    ModelThinkingLevel, Provider, ProviderApiConfig, ProviderAuth, ProviderStreams,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevelMap, TranscriptContext,
    create_assistant_message_event_stream, create_provider,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::session_manager::{SessionEntry, SessionModelRef};

pub const VIRTUAL_MODEL_API: &str = "pi-virtual";
pub const VIRTUAL_MODEL_STATE_ENTRY: &str = "pi.virtual-model-state";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualModelStateData {
    pub provider: String,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<JsonValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRouteReason {
    User,
    Continuation,
    Retry,
    Direct,
}

#[derive(Debug, Clone)]
pub struct PreviousModelRoute {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Debug, Clone)]
pub struct FailedModelRoute {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub message: AssistantMessage,
}

#[derive(Debug, Clone)]
pub struct ModelRouteRequest {
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub reason: ModelRouteReason,
    pub previous: Option<PreviousModelRoute>,
    pub failed: Option<FailedModelRoute>,
    pub state: Option<JsonValue>,
    pub messages: Vec<Message>,
    pub cancel_token: Option<CancellationToken>,
}

#[derive(Debug, Clone)]
pub struct ModelRoute {
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub state: Option<JsonValue>,
}

pub type VirtualModelRouteFn =
    Arc<dyn Fn(ModelRouteRequest) -> BoxFuture<'static, Result<ModelRoute, String>> + Send + Sync>;

#[derive(Clone)]
pub struct VirtualModelDefinition {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub thinking_levels: Option<Vec<ModelThinkingLevel>>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub input: Option<Vec<ModelInputModality>>,
    pub route: VirtualModelRouteFn,
}

impl std::fmt::Debug for VirtualModelDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualModelDefinition")
            .field("provider", &self.provider)
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

pub fn is_virtual_model(model: &Model) -> bool {
    model.api == VIRTUAL_MODEL_API
}

pub fn is_virtual_assistant_message(msg: &AssistantMessage) -> bool {
    msg.api == VIRTUAL_MODEL_API
}

/// 查找最近一条成功的物理模型助手回复（对照 `findLatestResponse`）。
pub fn find_latest_response(messages: &[AgentMessage]) -> Option<&AssistantMessage> {
    for msg in messages.iter().rev() {
        if let AgentMessage::Assistant(a) = msg
            && !matches!(a.stop_reason, StopReason::Error | StopReason::Aborted)
            && !is_virtual_assistant_message(a)
        {
            return Some(a);
        }
    }
    None
}

/// 推导会话分支记录的生效模型选择：若最近一次 `model_change` 为仍已注册的虚拟模型，
/// 则即使后续助手回复记录的是路由后的物理模型，分支当前选择仍保持为该虚拟模型（对照 `getBranchSelection`）。
pub fn get_branch_selection(
    branch: &[SessionEntry],
    get_model: impl Fn(&str, &str) -> Option<Model>,
) -> Option<SessionModelRef> {
    for (i, entry) in branch.iter().enumerate().rev() {
        match entry {
            SessionEntry::ModelChange(mc) => {
                return Some(SessionModelRef {
                    provider: mc.provider.clone(),
                    model_id: mc.model_id.clone(),
                });
            }
            SessionEntry::Message(m) => {
                if let AgentMessage::Assistant(a) = &m.message
                    && !is_virtual_assistant_message(a)
                {
                    let response = SessionModelRef {
                        provider: a.provider.clone(),
                        model_id: a.model.clone(),
                    };
                    if let Some(change) = find_last_model_change(branch, i)
                        && let Some(model) = get_model(&change.provider, &change.model_id)
                        && is_virtual_model(&model)
                    {
                        return Some(change);
                    }
                    return Some(response);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_last_model_change(branch: &[SessionEntry], before: usize) -> Option<SessionModelRef> {
    for entry in branch[..before].iter().rev() {
        if let SessionEntry::ModelChange(mc) = entry {
            return Some(SessionModelRef {
                provider: mc.provider.clone(),
                model_id: mc.model_id.clone(),
            });
        }
    }
    None
}

/// 读取会话分支上保存的指定虚拟模型的最新路由状态（对照 `getVirtualModelState`）。
pub fn get_virtual_model_state(
    branch: &[SessionEntry],
    provider: &str,
    model_id: &str,
) -> Option<JsonValue> {
    for entry in branch.iter().rev() {
        if let SessionEntry::Custom(ce) = entry
            && ce.custom_type == VIRTUAL_MODEL_STATE_ENTRY
            && let Some(data) = &ce.data
            && let Ok(parsed) = serde_json::from_value::<VirtualModelStateData>(data.clone())
            && parsed.provider == provider
            && parsed.model_id == model_id
        {
            return parsed.state;
        }
    }
    None
}

const ALL_MODEL_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
    ModelThinkingLevel::Max,
];

/// 将 `VirtualModelDefinition` 转换为目录中的 `Model` 描述对象（对照 `createVirtualModel`）。
pub fn create_virtual_model(def: &VirtualModelDefinition) -> Model {
    let levels = def
        .thinking_levels
        .clone()
        .unwrap_or_else(|| vec![ModelThinkingLevel::Off]);
    let mut thinking_level_map: ThinkingLevelMap = BTreeMap::new();
    for lvl in ALL_MODEL_THINKING_LEVELS {
        let mapped = levels.contains(&lvl).then(|| lvl.as_str().to_owned());
        thinking_level_map.insert(lvl, mapped);
    }

    Model {
        id: def.id.clone(),
        name: def.name.clone(),
        api: VIRTUAL_MODEL_API.to_owned(),
        provider: def.provider.clone(),
        base_url: String::new(),
        input: def
            .input
            .clone()
            .unwrap_or_else(|| vec![ModelInputModality::Text, ModelInputModality::Image]),
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        model_type: None,
        reasoning: levels.iter().any(|&l| l != ModelThinkingLevel::Off),
        thinking_level_map: Some(thinking_level_map),
        prompt_cache: None,
        context_window: def.context_window.unwrap_or(0),
        max_tokens: def.max_tokens.unwrap_or(0),
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn unrouted_stream(model: &Model) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let mut err_msg = AssistantMessage::new_pending(model);
    err_msg.stop_reason = StopReason::Error;
    err_msg.error_message = Some(format!(
        "Virtual model {}/{} must be routed before streaming",
        model.provider, model.id
    ));
    stream.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: err_msg.clone(),
    });
    stream.end(Some(err_msg));
    stream
}

fn unrouted_provider_streams() -> ProviderStreams {
    ProviderStreams::new(
        Arc::new(
            |model: &Model, _ctx: &TranscriptContext, _opts: Option<&StreamOptions>| {
                unrouted_stream(model)
            },
        ),
        Arc::new(
            |model: &Model, _ctx: &TranscriptContext, _opts: Option<&SimpleStreamOptions>| {
                unrouted_stream(model)
            },
        ),
    )
}

/// 将虚拟模型列表合入服务商（若基础服务商不存在，则生成免凭据的纯虚拟模型服务商，对照 `withVirtualModels`）。
pub fn with_virtual_models(
    provider_id: &str,
    base_provider: Option<Provider>,
    virtual_models: Vec<Model>,
) -> Provider {
    let Some(base) = base_provider else {
        let mut api_map = BTreeMap::new();
        api_map.insert(VIRTUAL_MODEL_API.to_owned(), unrouted_provider_streams());
        let auth = ProviderAuth::api_key(ApiKeyAuth {
            name: "Virtual model".to_owned(),
            login_fn: None,
            resolve_async: None,
            configured_without_credentials: false,
            resolve: Arc::new(|_cred, _opts| {
                Ok(Some(AuthResult {
                    auth: Default::default(),
                    env: None,
                    source: Some("virtual".to_owned()),
                }))
            }),
        });
        return create_provider(CreateProviderOptions {
            id: provider_id.to_owned(),
            name: Some(provider_id.to_owned()),
            base_url: None,
            headers: None,
            auth,
            models: virtual_models.into_iter().map(AnyModel::Chat).collect(),
            fetch_models: None,
            filter_models: None,
            api: Some(ProviderApiConfig::ByApi(api_map)),
            classifiers: None,
        })
        .expect("valid virtual provider");
    };

    let base_for_stream = base.clone();
    let base_for_simple = base.clone();
    let delegated_streams = ProviderStreams::new(
        Arc::new(
            move |model: &Model, ctx: &TranscriptContext, opts: Option<&StreamOptions>| {
                if is_virtual_model(model) {
                    unrouted_stream(model)
                } else {
                    base_for_stream.stream(model, ctx, opts)
                }
            },
        ),
        Arc::new(
            move |model: &Model, ctx: &TranscriptContext, opts: Option<&SimpleStreamOptions>| {
                if is_virtual_model(model) {
                    unrouted_stream(model)
                } else {
                    base_for_simple.stream_simple(model, ctx, opts)
                }
            },
        ),
    );

    base.with_chat_model_overlay(virtual_models, ProviderApiConfig::Single(delegated_streams))
}
