use std::sync::Arc;

use pi_ai::Models;
use pi_ai::faux::{FauxResponseStep, faux_text_message, register_faux_provider};
use pi_session::{
    AgentSession, AgentSessionOptions, ForkSessionOptions, InlineExtension, NavigateTreeOptions,
    PromptDisposition, SessionManager,
};

#[tokio::main]
async fn main() {
    let temp_dir =
        std::env::temp_dir().join(format!("pi-session-example-fork-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_text_message("First branch answer")),
        FauxResponseStep::Static(faux_text_message("Second turn on branch A")),
        FauxResponseStep::Static(faux_text_message("## Goal\nSummarized abandoned branch A")),
    ]);

    let ext = InlineExtension::new("<example:commands-tree>", |api| {
        api.register_simple_command(
            "bookmark",
            "Bookmark current leaf",
            |args, ctx| async move {
                if let Some(leaf_id) = ctx.session_leaf_id() {
                    let label = if args.trim().is_empty() {
                        "starred".to_owned()
                    } else {
                        args.trim().to_owned()
                    };
                    ctx.runtime().set_label(leaf_id, Some(label))?;
                }
                Ok(())
            },
        );

        api.register_simple_command(
            "jump-first",
            "Navigate back to first assistant reply with summary",
            |args, ctx| async move {
                let branch = ctx.session_branch();
                let first_asst_id = branch
                    .iter()
                    .find_map(|e| match e {
                        pi_session::SessionEntry::Message(m) if m.message.role() == "assistant" => {
                            Some(m.id.clone())
                        }
                        _ => None,
                    })
                    .ok_or_else(|| "No assistant entry found".to_owned())?;
                ctx.navigate_tree(
                    first_asst_id,
                    NavigateTreeOptions {
                        summarize: true,
                        custom_instructions: (!args.trim().is_empty())
                            .then(|| args.trim().to_owned()),
                        replace_instructions: false,
                        label: Some("returned-here".to_owned()),
                    },
                )
                .await?;
                Ok(())
            },
        );

        api.register_simple_command(
            "fork-current",
            "Fork current leaf",
            |_args, ctx| async move {
                if let Some(leaf_id) = ctx.session_leaf_id() {
                    ctx.fork(
                        leaf_id,
                        ForkSessionOptions {
                            position: Some("at".to_owned()),
                        },
                    )
                    .await?;
                }
                Ok(())
            },
        );
        Ok(())
    });

    let sm = SessionManager::create(".", &temp_dir, None).unwrap();
    let mut opts = AgentSessionOptions::new(".", reg.model.clone(), models, sm);
    opts.extensions = vec![Box::new(ext)];

    let session = AgentSession::new(opts).unwrap();
    session.prompt("Question 1", None).await.unwrap();
    session.prompt("Question 2", None).await.unwrap();

    // 1. 执行 /bookmark 命令打标签
    let disp = session.prompt("/bookmark milestone-a", None).await.unwrap();
    assert_eq!(disp, PromptDisposition::HandledByExtension);

    // 2. 执行 /jump-first 带分支总结回跳到第一轮回复
    let disp2 = session
        .prompt("/jump-first focus on key decisions", None)
        .await
        .unwrap();
    assert_eq!(disp2, PromptDisposition::HandledByExtension);

    // 3. 执行 /fork-current 派生新会话文件
    let old_session_id = session.session_id();
    let disp3 = session.prompt("/fork-current", None).await.unwrap();
    assert_eq!(disp3, PromptDisposition::HandledByExtension);
    assert_ne!(session.session_id(), old_session_id);
    assert!(session.session_file().unwrap().exists());

    let _ = std::fs::remove_dir_all(&temp_dir);
    println!("example slash_commands_and_tree_fork passed");
}
