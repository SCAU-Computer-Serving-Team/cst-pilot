use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::api::{calculate_cost, invoke_on_payload};
use crate::auth::{ApiKeyAuth, ProviderAuth};
use crate::models::{
    CreateProviderOptions, Models, Provider, ProviderApiConfig, ProviderStreams, create_provider,
};
use crate::types::{
    AnyModel, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, ContentBlock,
    JsonObject, Message, Model, ModelCost, ModelInputModality, ProviderResponse,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, ToolCall,
    TranscriptContext, Usage, UsageCost, create_assistant_message_event_stream, now_ms,
};

pub const DEFAULT_FAUX_API: &str = "faux";
pub const DEFAULT_FAUX_PROVIDER: &str = "faux";
pub const DEFAULT_FAUX_MODEL_ID: &str = "faux-1";

pub fn faux_text(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text))
}

pub fn faux_thinking(thinking: impl Into<String>) -> ContentBlock {
    ContentBlock::Thinking(ThinkingContent::new(thinking))
}

pub fn faux_tool_call(
    name: impl Into<String>,
    arguments: JsonObject,
    id: Option<String>,
) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.unwrap_or_else(|| format!("call_faux_{}", now_ms())),
        name: name.into(),
        arguments,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })
}

pub fn faux_assistant_message(
    content: Vec<ContentBlock>,
    stop_reason: Option<StopReason>,
) -> AssistantMessage {
    AssistantMessage {
        content,
        api: DEFAULT_FAUX_API.to_owned(),
        provider: DEFAULT_FAUX_PROVIDER.to_owned(),
        model: DEFAULT_FAUX_MODEL_ID.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: stop_reason.unwrap_or(StopReason::Stop),
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
        duration_ms: None,
    }
}

pub fn faux_text_message(text: impl Into<String>) -> AssistantMessage {
    faux_assistant_message(vec![faux_text(text)], Some(StopReason::Stop))
}

pub type FauxResponseFactory =
    Arc<dyn Fn(&TranscriptContext, &Model, usize) -> AssistantMessage + Send + Sync>;

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum FauxResponseStep {
    Static(AssistantMessage),
    Dynamic(FauxResponseFactory),
}

impl From<AssistantMessage> for FauxResponseStep {
    fn from(msg: AssistantMessage) -> Self {
        FauxResponseStep::Static(msg)
    }
}

struct FauxSharedState {
    call_count: usize,
    queue: VecDeque<FauxResponseStep>,
}

#[derive(Clone)]
pub struct FauxProviderHandle {
    pub provider: Provider,
    pub model: Model,
    state: Arc<Mutex<FauxSharedState>>,
}

impl FauxProviderHandle {
    pub fn set_responses(&self, steps: impl IntoIterator<Item = FauxResponseStep>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.queue.clear();
            guard.queue.extend(steps);
        }
    }

    pub fn append_responses(&self, steps: impl IntoIterator<Item = FauxResponseStep>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.queue.extend(steps);
        }
    }

    pub fn pending_response_count(&self) -> usize {
        self.state.lock().map(|g| g.queue.len()).unwrap_or(0)
    }

    pub fn call_count(&self) -> usize {
        self.state.lock().map(|g| g.call_count).unwrap_or(0)
    }
}

fn estimate_tokens(chars_len: usize) -> u64 {
    (chars_len as u64).div_ceil(4)
}

fn estimate_context_chars(context: &TranscriptContext) -> usize {
    context
        .messages()
        .iter()
        .map(|m| match m {
            Message::System(s) => s.content.as_plain_text().len(),
            Message::User(u) => serde_json::to_string(&u.content)
                .map(|s| s.len())
                .unwrap_or(0),
            Message::Assistant(a) => serde_json::to_string(&a.content)
                .map(|s| s.len())
                .unwrap_or(0),
            Message::ToolResult(tr) => serde_json::to_string(&tr.content)
                .map(|s| s.len())
                .unwrap_or(0),
        })
        .sum()
}

pub fn create_faux_provider() -> FauxProviderHandle {
    let model = Model {
        id: DEFAULT_FAUX_MODEL_ID.to_owned(),
        name: "Faux Model".to_owned(),
        api: DEFAULT_FAUX_API.to_owned(),
        provider: DEFAULT_FAUX_PROVIDER.to_owned(),
        base_url: "http://localhost:0".to_owned(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        input_limits: None,
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
        model_type: Some("chat".to_owned()),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 128_000,
        max_tokens: 16_384,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    };

    let state = Arc::new(Mutex::new(FauxSharedState {
        call_count: 0,
        queue: VecDeque::new(),
    }));

    let state_for_stream = state.clone();
    let run_stream = move |m: &Model,
                           ctx: &TranscriptContext,
                           opts: Option<&StreamOptions>|
          -> AssistantMessageEventStream {
        let stream = create_assistant_message_event_stream();
        let writer = stream.clone();
        let m = m.clone();
        let ctx = ctx.clone();
        let opts = opts.cloned();
        let st = state_for_stream.clone();

        tokio::spawn(async move {
            let cancel_token = opts.as_ref().and_then(|o| o.cancel_token.clone());
            if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                let mut err_msg = AssistantMessage::new_pending(&m);
                err_msg.stop_reason = StopReason::Aborted;
                err_msg.error_message = Some("Request was aborted".to_owned());
                writer.push(AssistantMessageEvent::Error {
                    reason: StopReason::Aborted,
                    error: err_msg,
                });
                writer.end(None);
                return;
            }

            let hooks: Result<(), String> = async {
                invoke_on_payload(
                    opts.as_ref(),
                    json!({
                        "model": m.id,
                        "messages": ctx.messages(),
                    }),
                    &m,
                )
                .await?;

                if let Some(on_response) = opts.as_ref().and_then(|o| o.on_response.as_ref()) {
                    crate::api::run_provider_callback("on_response", async {
                        on_response(
                            ProviderResponse {
                                status: 200,
                                headers: BTreeMap::new(),
                            },
                            m.clone(),
                        )
                        .await
                    })
                    .await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = hooks {
                let mut message = AssistantMessage::new_pending(&m);
                message.stop_reason = StopReason::Error;
                message.error_message = Some(error);
                writer.push(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: message,
                });
                writer.end(None);
                return;
            }

            let step_opt = {
                let mut guard = st.lock().expect("faux lock");
                guard.call_count += 1;
                let call_idx = guard.call_count;
                guard.queue.pop_front().map(|s| (s, call_idx))
            };

            let Some((step, call_idx)) = step_opt else {
                let mut err_msg = AssistantMessage::new_pending(&m);
                err_msg.stop_reason = StopReason::Error;
                err_msg.error_message = Some("No more faux responses queued".to_owned());
                writer.push(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: err_msg,
                });
                writer.end(None);
                return;
            };

            let scripted = match step {
                FauxResponseStep::Static(msg) => msg,
                FauxResponseStep::Dynamic(f) => f(&ctx, &m, call_idx),
            };

            let mut partial = AssistantMessage::new_pending(&m);
            partial.stop_reason = scripted.stop_reason;
            partial.error_message = scripted.error_message.clone();
            partial.response_id = scripted.response_id.clone();

            if matches!(
                scripted.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) {
                partial.content = scripted.content;
                writer.push(AssistantMessageEvent::Error {
                    reason: scripted.stop_reason,
                    error: partial,
                });
                writer.end(None);
                return;
            }

            writer.push(AssistantMessageEvent::Start {
                partial: partial.clone(),
            });

            let mut output_chars = 0usize;
            for block in scripted.content {
                match block {
                    ContentBlock::Thinking(t) => {
                        output_chars += t.thinking.len();
                        partial
                            .content
                            .push(ContentBlock::Thinking(ThinkingContent {
                                thinking: String::new(),
                                thinking_signature: t.thinking_signature.clone(),
                                redacted: t.redacted,
                            }));
                        let idx = partial.content.len() - 1;
                        writer.push(AssistantMessageEvent::ThinkingStart {
                            content_index: idx,
                            partial: partial.clone(),
                        });
                        if !t.thinking.is_empty() {
                            if let Some(ContentBlock::Thinking(slot)) = partial.content.get_mut(idx)
                            {
                                slot.thinking = t.thinking.clone();
                            }
                            writer.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: idx,
                                delta: t.thinking.clone(),
                                partial: partial.clone(),
                            });
                        }
                        writer.push(AssistantMessageEvent::ThinkingEnd {
                            content_index: idx,
                            content: t.thinking,
                            partial: partial.clone(),
                        });
                    }
                    ContentBlock::Text(t) => {
                        output_chars += t.text.len();
                        partial.content.push(ContentBlock::Text(TextContent {
                            text: String::new(),
                            text_signature: t.text_signature.clone(),
                        }));
                        let idx = partial.content.len() - 1;
                        writer.push(AssistantMessageEvent::TextStart {
                            content_index: idx,
                            partial: partial.clone(),
                        });
                        if !t.text.is_empty() {
                            if let Some(ContentBlock::Text(slot)) = partial.content.get_mut(idx) {
                                slot.text = t.text.clone();
                            }
                            writer.push(AssistantMessageEvent::TextDelta {
                                content_index: idx,
                                delta: t.text.clone(),
                                partial: partial.clone(),
                            });
                        }
                        writer.push(AssistantMessageEvent::TextEnd {
                            content_index: idx,
                            content: t.text,
                            partial: partial.clone(),
                        });
                    }
                    ContentBlock::ToolCall(tc) => {
                        let args_str = serde_json::to_string(&tc.arguments)
                            .unwrap_or_else(|_| "{}".to_owned());
                        output_chars += tc.name.len() + args_str.len();
                        partial.content.push(ContentBlock::ToolCall(tc.clone()));
                        let idx = partial.content.len() - 1;
                        writer.push(AssistantMessageEvent::ToolCallStart {
                            content_index: idx,
                            partial: partial.clone(),
                        });
                        writer.push(AssistantMessageEvent::ToolCallDelta {
                            content_index: idx,
                            delta: args_str,
                            partial: partial.clone(),
                        });
                        writer.push(AssistantMessageEvent::ToolCallEnd {
                            content_index: idx,
                            tool_call: tc,
                            partial: partial.clone(),
                        });
                    }
                }
            }

            if partial
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolCall(_)))
                && partial.stop_reason == StopReason::Stop
            {
                partial.stop_reason = StopReason::ToolUse;
            }

            let has_explicit_usage = scripted.usage.total_tokens > 0
                || scripted.usage.input > 0
                || scripted.usage.output > 0
                || scripted.usage.cache_read > 0
                || scripted.usage.cache_write > 0;
            if has_explicit_usage {
                partial.usage = scripted.usage;
            } else {
                let in_tok = estimate_tokens(estimate_context_chars(&ctx));
                let out_tok = estimate_tokens(output_chars);
                partial.usage = Usage {
                    input: in_tok,
                    output: out_tok,
                    cache_read: 0,
                    cache_write: 0,
                    cache_write_1h: None,
                    reasoning: None,
                    total_tokens: in_tok + out_tok,
                    cost: UsageCost::default(),
                };
                calculate_cost(&m.cost, &mut partial.usage);
            }

            writer.push(AssistantMessageEvent::Done {
                reason: partial.stop_reason,
                message: partial,
            });
            writer.end(None);
        });

        stream
    };

    let run_stream_arc = Arc::new(run_stream);
    let run_simple_arc = run_stream_arc.clone();
    let streams = ProviderStreams::new(
        Arc::new(move |m, c, o| run_stream_arc(m, c, o)),
        Arc::new(move |m, c, o: Option<&SimpleStreamOptions>| {
            run_simple_arc(m, c, o.map(|s| &s.base))
        }),
    );

    let provider = create_provider(CreateProviderOptions {
        id: DEFAULT_FAUX_PROVIDER.to_owned(),
        name: Some("Faux Provider".to_owned()),
        base_url: Some("http://localhost:0".to_owned()),
        headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("Faux Key", "faux-key")),
        models: vec![AnyModel::Chat(model.clone())],
        fetch_models: None,
        filter_models: None,
        api: Some(ProviderApiConfig::ByApi(BTreeMap::from([(
            DEFAULT_FAUX_API.to_owned(),
            streams,
        )]))),
        classifiers: None,
    })
    .expect("faux provider is valid");

    FauxProviderHandle {
        provider,
        model,
        state,
    }
}

pub fn register_faux_provider(models: &Models) -> FauxProviderHandle {
    let handle = create_faux_provider();
    models.set_provider(handle.provider.clone());
    handle
}
