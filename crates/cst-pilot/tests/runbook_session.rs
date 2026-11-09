use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cst_pilot::{Kit, SKILL_NAMES};
use pi_ai::faux::{
    FauxResponseStep, create_faux_provider, faux_assistant_message, faux_text_message,
    faux_tool_call,
};
use pi_ai::{Message, StopReason, UserContentBlock};
use pi_runtime::{
    CreateAgentSessionOptions, CreateModelRuntimeOptions, ModelRuntime, create_agent_session,
};
use pi_session::SessionManager;
use serde_json::{Value, json};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn real_sdk_session_plans_and_exports_two_revisions_using_complete_tool_results() {
    let root = std::env::temp_dir().join(format!("cst-step3-session-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let fixture = Fixture(root.canonicalize().unwrap());
    for name in SKILL_NAMES {
        let skill = fixture.0.join(format!("agent/home/skills/{name}/SKILL.md"));
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(
            skill,
            format!("---\nname: {name}\ndescription: 测试工具规范\n---\n仅测试。\n"),
        )
        .unwrap();
    }
    fs::write(
        fixture.0.join("agent/home/APPEND_SYSTEM.md"),
        "测试用维修交付规则。",
    )
    .unwrap();
    let faux = create_faux_provider();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(faux.provider.clone());
    let options = Kit::new(&fixture.0)
        .unwrap()
        .session_options(CreateAgentSessionOptions {
            cwd: Some(fixture.0.clone()),
            model_runtime: Some(runtime),
            model: Some(faux.model.clone()),
            session_manager: Some(
                SessionManager::in_memory(fixture.0.to_string_lossy(), None, None).unwrap(),
            ),
            ..Default::default()
        })
        .unwrap();
    let created = create_agent_session(options).await.unwrap();
    for name in ["command_plan", "runbook_deliver", "runbook"] {
        assert!(
            created
                .session
                .get_active_tool_names()
                .iter()
                .any(|tool| tool == name)
        );
    }
    let exported = Arc::new(Mutex::new(Vec::<Value>::new()));
    let collected = exported.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let response = FauxResponseStep::Dynamic(Arc::new(move |context, _, _| {
        let index = calls.fetch_add(1, Ordering::SeqCst);
        let previous = context.messages().iter().rev().find_map(|message| {
            let Message::ToolResult(result) = message else {
                return None;
            };
            assert!(!result.is_error);
            result.content.iter().find_map(|block| {
                let UserContentBlock::Text(text) = block else {
                    return None;
                };
                serde_json::from_str::<Value>(&text.text)
                    .ok()
                    .map(|v| v["runbook"].clone())
            })
        });
        if index % 3 == 2 {
            let previous = previous.unwrap();
            assert!(previous["commands"][0]["command"].is_string());
            collected.lock().unwrap().push(previous);
            return faux_text_message("请打开文本，逐条复制执行。");
        }
        let (name, params) = if index.is_multiple_of(3) {
            let mut params = json!({"title":"测试命令","level":"safe","date":"2026-11-08","items":[{
                "summary":"输出测试文字，无系统改动。","command":if index==0{"echo first"}else{"echo second"},"shell":"cmd"
            }]});
            if let Some(previous) = previous {
                params["runbookId"] = previous["runbookId"].clone();
                params["baseVersion"] = previous["version"].clone();
            }
            ("command_plan", params)
        } else {
            let previous = previous.unwrap();
            assert_eq!(previous["action"], "planned");
            assert_eq!(previous["commands"][0]["admin"], false);
            (
                "runbook_deliver",
                json!({"runbookId":previous["runbookId"],"version":previous["version"]}),
            )
        };
        faux_assistant_message(
            vec![faux_tool_call(
                name,
                params.as_object().unwrap().clone(),
                Some(format!("call-{index}")),
            )],
            Some(StopReason::ToolUse),
        )
    }));
    faux.set_responses(vec![response; 6]);
    created
        .session
        .prompt("准备并交付第一批命令，已核对。", None)
        .await
        .unwrap();
    created
        .session
        .prompt("修订第二版并交付，已核对。", None)
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 6);
    let results = exported.lock().unwrap().clone();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["runbookId"], results[1]["runbookId"]);
    assert_eq!(results[0]["version"], 1);
    assert_eq!(results[1]["version"], 2);
    for (plan, expected) in results.iter().zip(["echo first", "echo second"]) {
        let file = PathBuf::from(plan["file"].as_str().unwrap());
        assert!(file.starts_with(&fixture.0));
        assert!(fs::read_to_string(file).unwrap().contains(expected));
    }
    created.session.shutdown().await.unwrap();
}
