use cst_pilot::{
    Kit, SKILL_NAMES,
    bitlocker::credentials::{HIDDEN, PrivacyPolicy, Vault, redact, validate},
};
use pi_agent::{AgentMessage, AgentToolResult};
use pi_ai::{
    AssistantMessageEvent, ContentBlock, Message, StopReason, UserContentBlock,
    faux::{
        FauxResponseStep, create_faux_provider, faux_assistant_message, faux_text_message,
        faux_tool_call,
    },
};
use pi_runtime::{
    CreateAgentSessionOptions, CreateModelRuntimeOptions, ModelRuntime, NoToolsMode,
    create_agent_session,
};
use pi_session::{
    AgentSessionEvent, Extension, ExtensionInitError, ExtensionRegistrar, ExtensionRunner,
    PromptOptions, SessionDataPolicy, SessionManager, ToolDefinition,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use zeroize::Zeroizing;

const PASSWORD: &str = "123456-234567-345678-456789-567890-678901-789012-890123";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("bitlocker-privacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p.canonicalize().unwrap())
    }
    fn resources(&self) {
        fs::create_dir_all(self.0.join("agent/home")).unwrap();
        fs::write(self.0.join("agent/home/APPEND_SYSTEM.md"), "测试").unwrap();
        for name in SKILL_NAMES {
            let p = self.0.join(format!("agent/home/skills/{name}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {name}\ndescription: 测试\n---\n测试"),
            )
            .unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct PrivacyExtension {
    vault: Vault,
    claimed: Arc<Mutex<Vec<String>>>,
}
impl Extension for PrivacyExtension {
    fn path(&self) -> &str {
        "privacy-test"
    }
    fn register(&self, api: &mut ExtensionRegistrar) -> Result<(), ExtensionInitError> {
        self.vault.register(api);
        let vault = self.vault.clone();
        let claimed = self.claimed.clone();
        api.register_tool(ToolDefinition::new("bitlocker","BitLocker test","Mock only",json!({"type":"object","properties":{"scope":{"type":"string"},"drive":{"type":"string"},"recoveryPassword":{"type":"string"}},"required":["scope","drive","recoveryPassword"]}),Arc::new(move|id,args,_,_,_|{
            let vault=vault.clone();let claimed=claimed.clone();
            Box::pin(async move {
                let value=args["recoveryPassword"].as_str().unwrap();assert_eq!(value,HIDDEN);
                let password=vault.claim(&id,args["scope"].as_str().unwrap(),args["drive"].as_str(),Some(Zeroizing::new(value.into())))?.unwrap();
                assert_eq!(password.as_str(),PASSWORD);claimed.lock().unwrap().push(id);
                // A mock provider error containing the credential must also be scrubbed.
                Ok(AgentToolResult::text(format!("mock result {PASSWORD}")))
            })
        })));
        Ok(())
    }
}
#[test]
fn password_validation_redaction_and_invalid_field_contents_do_not_expose_secrets() {
    assert_eq!(
        validate(Zeroizing::new(format!(" {PASSWORD} ")))
            .unwrap()
            .as_str(),
        PASSWORD
    );
    for invalid in [
        "123456",
        "a23456-234567-345678-456789-567890-678901-789012-890123",
        "123456234567345678456789567890678901789012890123",
    ] {
        let error = validate(Zeroizing::new(invalid.into())).unwrap_err();
        assert!(!error.contains(invalid));
    }
    assert_eq!(
        redact(&format!("key={PASSWORD},{}", "1".repeat(48))),
        format!("key={HIDDEN},{HIDDEN}")
    );
    let message = AgentMessage::Assistant(faux_assistant_message(
        vec![faux_tool_call(
            "bitlocker",
            json!({"recoveryPassword":{"unexpected":PASSWORD}})
                .as_object()
                .unwrap()
                .clone(),
            Some("x".into()),
        )],
        Some(StopReason::ToolUse),
    ));
    let safe = PrivacyPolicy.persisted_message(message).unwrap();
    assert!(!serde_json::to_string(&safe).unwrap().contains(PASSWORD));
}
#[test]
fn streaming_single_digit_chunks_never_reveal_credential_prefixes_or_json_deltas() {
    for end in 1..=PASSWORD.len() {
        let partial = faux_text_message(format!("恢复密码：{}", &PASSWORD[..end]));
        let event = AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Assistant(partial.clone()),
            assistant_message_event: AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: PASSWORD[end - 1..end].into(),
                partial,
            },
        };
        let safe = PrivacyPolicy.project_event(event).unwrap();
        let AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Assistant(message),
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, partial, .. },
        } = safe
        else {
            panic!("shape")
        };
        assert!(delta.is_empty());
        for message in [message, partial] {
            let ContentBlock::Text(text) = &message.content[0] else {
                panic!("text")
            };
            assert_eq!(text.text, format!("恢复密码：{HIDDEN}"));
        }
    }
    for value in ["1", "123456-2", PASSWORD] {
        let partial = faux_assistant_message(
            vec![faux_tool_call(
                "bitlocker",
                json!({"recoveryPassword":value})
                    .as_object()
                    .unwrap()
                    .clone(),
                Some("call".into()),
            )],
            Some(StopReason::ToolUse),
        );
        let event = AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Assistant(partial.clone()),
            assistant_message_event: AssistantMessageEvent::ToolCallDelta {
                content_index: 0,
                delta: format!("{{\"recoveryPassword\":\"{value}"),
                partial,
            },
        };
        let safe = serde_json::to_value(PrivacyPolicy.project_event(event).unwrap()).unwrap();
        assert_eq!(safe["assistantMessageEvent"]["delta"], "");
        assert_eq!(
            safe["message"]["content"][0]["arguments"]["recoveryPassword"],
            HIDDEN
        );
    }
}
fn extension(vault: Vault) -> PrivacyExtension {
    PrivacyExtension {
        vault,
        claimed: Arc::new(Mutex::new(Vec::new())),
    }
}
fn call(id: &str, scope: &str, drive: &str) -> AgentMessage {
    AgentMessage::Assistant(faux_assistant_message(
        vec![faux_tool_call(
            "bitlocker",
            json!({"scope":scope,"drive":drive,"recoveryPassword":PASSWORD})
                .as_object()
                .unwrap()
                .clone(),
            Some(id.into()),
        )],
        Some(StopReason::ToolUse),
    ))
}
#[tokio::test]
async fn transient_call_is_one_use_and_bound_to_normalized_drive_and_unlock_scope() {
    let vault = Vault::default();
    let runner =
        ExtensionRunner::from_extensions("test", vec![Box::new(extension(vault.clone()))]).unwrap();
    runner.emit_message_end(call("a", "unlock", "d")).await;
    assert!(
        vault
            .claim(
                "a",
                "unlock",
                Some("C:"),
                Some(Zeroizing::new(HIDDEN.into()))
            )
            .is_err()
    );
    assert!(
        vault
            .claim(
                "a",
                "unlock",
                Some("D:"),
                Some(Zeroizing::new(HIDDEN.into()))
            )
            .is_err()
    );
    runner.emit_message_end(call("b", "decrypt", "D:")).await;
    assert!(vault.claim("b", "unlock", Some("D:"), None).is_err());
    runner.emit_message_end(call("c", "unlock", "d")).await;
    assert_eq!(
        vault
            .claim("c", "unlock", Some("D:"), None)
            .unwrap()
            .unwrap()
            .as_str(),
        PASSWORD
    );
    assert!(
        vault
            .claim(
                "c",
                "unlock",
                Some("D:"),
                Some(Zeroizing::new(HIDDEN.into()))
            )
            .is_err()
    );
    runner.emit_message_end(call("d", "unlock", "D:")).await;
    vault.clear();
    assert!(
        vault
            .claim(
                "d",
                "unlock",
                Some("D:"),
                Some(Zeroizing::new(HIDDEN.into()))
            )
            .is_err()
    );
}
#[tokio::test]
async fn real_sdk_uses_request_local_user_credential_but_persists_and_emits_only_redacted_copies() {
    let f = Fixture::new();
    let faux = create_faux_provider();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(faux.provider.clone());
    let vault = Vault::default();
    let ext = extension(vault);
    let claimed = ext.claimed.clone();
    let factory: pi_session::ExtensionFactory = Arc::new(move || Ok(Box::new(ext.clone())));
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(f.0.clone()),
        agent_dir: Some(f.0.join("agent")),
        model_runtime: Some(runtime),
        model: Some(faux.model.clone()),
        session_manager: Some(
            SessionManager::create(f.0.to_string_lossy(), f.0.join("sessions"), None).unwrap(),
        ),
        no_tools: Some(NoToolsMode::Builtin),
        data_policy: Some(Arc::new(PrivacyPolicy)),
        extension_factories: vec![factory],
        ..Default::default()
    })
    .await
    .unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let copy = received.clone();
    let _unsub = created
        .session
        .subscribe(Arc::new(move |e| copy.lock().unwrap().push(e)));
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let response = FauxResponseStep::Dynamic(Arc::new(move |ctx, _, _| {
        let index = calls.fetch_add(1, Ordering::SeqCst);
        let serialized = serde_json::to_string(ctx.messages()).unwrap();
        if index < 2 {
            assert!(serialized.contains(PASSWORD));
        } else {
            assert!(!serialized.contains(PASSWORD));
        }
        if index == 0 {
            return faux_assistant_message(
                vec![faux_tool_call(
                    "bitlocker",
                    json!({"scope":"unlock","drive":"D:","recoveryPassword":PASSWORD})
                        .as_object()
                        .unwrap()
                        .clone(),
                    Some("actual-call".into()),
                )],
                Some(StopReason::ToolUse),
            );
        }
        if index == 1 {
            let Message::ToolResult(result) = ctx
                .messages()
                .iter()
                .rev()
                .find(|m| matches!(m, Message::ToolResult(_)))
                .unwrap()
            else {
                panic!("tool result")
            };
            assert!(!result.is_error);
            let UserContentBlock::Text(text) = &result.content[0] else {
                panic!("text")
            };
            assert!(text.text.contains(HIDDEN));
        }
        faux_text_message(if index == 1 { PASSWORD } else { "next round" })
    }));
    faux.set_responses(vec![response; 3]);
    created
        .session
        .prompt(
            format!("我选择 AI 填参，解锁 D:，{PASSWORD}"),
            Some(PromptOptions {
                client_message_id: Some("sensitive-input".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    created
        .session
        .prompt("下一轮不再提供密钥", None)
        .await
        .unwrap();
    assert_eq!(claimed.lock().unwrap().as_slice(), ["actual-call"]);
    let path = created.session.session_file().unwrap();
    let state = fs::read_to_string(format!("{}.state.json", path.display())).unwrap();
    for content in [
        fs::read_to_string(&path).unwrap(),
        state,
        serde_json::to_string(&*received.lock().unwrap()).unwrap(),
        serde_json::to_string(&created.session.state().messages).unwrap(),
    ] {
        assert!(!content.contains(PASSWORD));
        assert!(content.contains(HIDDEN));
    }
    created.session.shutdown().await.unwrap();
}
#[tokio::test]
async fn kit_enforces_privacy_policy_even_when_host_supplies_its_own_projection() {
    struct Identity;
    impl SessionDataPolicy for Identity {}
    let f = Fixture::new();
    f.resources();
    let faux = create_faux_provider();
    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    rt.register_provider(faux.provider.clone());
    let opts = Kit::new(&f.0)
        .unwrap()
        .session_options(CreateAgentSessionOptions {
            cwd: Some(f.0.clone()),
            model_runtime: Some(rt),
            model: Some(faux.model.clone()),
            data_policy: Some(Arc::new(Identity)),
            session_manager: Some(
                SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
            ),
            ..Default::default()
        })
        .unwrap();
    let session = create_agent_session(opts).await.unwrap().session;
    faux.set_responses(vec![
        FauxResponseStep::Static(faux_text_message(PASSWORD));
        2
    ]);
    session.prompt("仅测试输出", None).await.unwrap();
    session.reload().await.unwrap();
    session.prompt("重载后", None).await.unwrap();
    assert!(
        !serde_json::to_string(&session.state().messages)
            .unwrap()
            .contains(PASSWORD)
    );
    session.shutdown().await.unwrap();
}
