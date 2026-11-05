use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_agent::AgentMessage;
use pi_ai::{ContentBlock, ImageContent, StopReason};
use pi_session::{
    ExtensionCommandContextActions, ExtensionError, ForkSessionOptions, NavigateTreeOptions,
    PromptOptions,
};
use std::sync::atomic::AtomicBool;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Semaphore, mpsc};

use crate::modes::rpc_mode::{BoundedStdoutQueue, MAX_STDOUT_QUEUE_BYTES, MAX_STDOUT_QUEUE_EVENTS};

use crate::modes::json_event::to_json_event;
use crate::runtime::{
    AgentSessionRuntime, ForkPosition, ForkSessionRuntimeOptions, NewSessionRuntimeOptions,
    SwitchSessionRuntimeOptions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintOutputMode {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Default)]
pub struct PrintModeOptions {
    pub mode: PrintOutputMode,
    pub initial_message: Option<String>,
    pub initial_images: Option<Vec<ImageContent>>,
    pub messages: Vec<String>,
}

pub fn bind_runtime_command_actions(runtime_host: &AgentSessionRuntime) {
    let session = match runtime_host.try_session() {
        Ok(s) => s,
        Err(_) => return,
    };
    let runner = session.extension_runner();

    let s_idle = session.clone();
    let rt_new = runtime_host.clone();
    let rt_fork = runtime_host.clone();
    let s_nav = session.clone();
    let rt_switch = runtime_host.clone();
    let s_reload = session.clone();

    runner.bind_command_context(Some(ExtensionCommandContextActions {
        wait_for_idle: Arc::new(move || {
            let s = s_idle.clone();
            Box::pin(async move {
                s.wait_for_idle().await;
            })
        }),
        new_session: Arc::new(move |parent_session| {
            let rt = rt_new.clone();
            Box::pin(async move {
                let res = rt
                    .new_session(Some(NewSessionRuntimeOptions {
                        parent_session,
                        setup: None,
                        with_session: None,
                    }))
                    .await?;
                Ok(!res.cancelled)
            })
        }),
        fork: Arc::new(move |entry_id: String, opts: ForkSessionOptions| {
            let rt = rt_fork.clone();
            Box::pin(async move {
                let pos = match opts.position.as_deref() {
                    Some("at") => ForkPosition::At,
                    _ => ForkPosition::Before,
                };
                let res = rt
                    .fork(
                        &entry_id,
                        Some(ForkSessionRuntimeOptions {
                            position: Some(pos),
                            with_session: None,
                        }),
                    )
                    .await?;
                Ok(!res.cancelled)
            })
        }),
        navigate_tree: Arc::new(move |target_id: String, opts: NavigateTreeOptions| {
            let s = s_nav.clone();
            Box::pin(async move {
                let res = s.navigate_tree(&target_id, opts).await?;
                Ok(!res.cancelled)
            })
        }),
        switch_session: Arc::new(move |path: PathBuf| {
            let rt = rt_switch.clone();
            Box::pin(async move {
                let res = rt
                    .switch_session(path, Some(SwitchSessionRuntimeOptions::default()))
                    .await?;
                Ok(!res.cancelled)
            })
        }),
        reload: Some(Arc::new(move || {
            let s = s_reload.clone();
            Box::pin(async move { s.reload_resources().await })
        })),
    }));
}

/// 运行单次打印或 JSON 事件流模式（对照 `runPrintMode`）。
pub async fn run_print_mode<W, E>(
    runtime_host: &AgentSessionRuntime,
    options: PrintModeOptions,
    stdout: W,
    stderr: E,
) -> i32
where
    W: AsyncWrite + Unpin + Send + 'static,
    E: AsyncWrite + Unpin + Send + 'static,
{
    let mode = options.mode;
    let (out_tx, out_rx) = mpsc::channel(MAX_STDOUT_QUEUE_EVENTS);
    let out = BoundedStdoutQueue::new(
        out_tx,
        Arc::new(Semaphore::new(MAX_STDOUT_QUEUE_BYTES)),
        Arc::new(AtomicBool::new(false)),
    );
    let (err_tx, err_rx) = mpsc::channel(64);
    let err = BoundedStdoutQueue::new(
        err_tx,
        Arc::new(Semaphore::new(MAX_STDOUT_QUEUE_BYTES)),
        Arc::new(AtomicBool::new(false)),
    );
    let failure = out.failure_token();
    let err_failure = err.failure_token();

    let finish = tokio_util::sync::CancellationToken::new();
    let writer_handle = tokio::spawn(drain_output(
        stdout,
        out_rx,
        out.clone(),
        err.failure_token(),
        finish.clone(),
    ));
    let err_writer_handle = tokio::spawn(drain_output(
        stderr,
        err_rx,
        err.clone(),
        out.failure_token(),
        finish.clone(),
    ));
    let last_assistant = Arc::new(Mutex::new(None));

    let unsub_holder: Arc<Mutex<Option<super::Unsubscribe>>> = Arc::new(Mutex::new(None));

    let rebind_fn = {
        let rt = runtime_host.clone();
        let tx = out.clone();
        let etx = err.clone();
        let holder = unsub_holder.clone();
        let last = last_assistant.clone();
        Arc::new(move |_new_session: pi_session::AgentSession| {
            let rt = rt.clone();
            let tx = tx.clone();
            let etx = etx.clone();
            let holder = holder.clone();
            let last = last.clone();
            Box::pin(async move {
                if let Some(prev_unsub) = holder.lock().expect("unsub lock").take() {
                    prev_unsub();
                }
                bind_runtime_command_actions(&rt);
                if let Ok(s) = rt.try_session() {
                    let etx_err = etx.clone();
                    s.extension_runner()
                        .on_error(Arc::new(move |err: &ExtensionError| {
                            let _ = etx_err.try_send_text(format!(
                                "Extension error ({:.1024}): {:.4096}\n",
                                err.extension_path, err.error
                            ));
                        }));
                    let tx_ev = tx.clone();
                    let unsub = s.subscribe_async(Arc::new(move |event| {
                        let tx_inner = tx_ev.clone();
                        let last = last.clone();
                        Box::pin(async move {
                            if let pi_session::AgentSessionEvent::MessageEnd {
                                message: AgentMessage::Assistant(message),
                            } = &event
                            {
                                *last.lock().expect("last assistant lock") = Some(message.clone());
                            }
                            if mode == PrintOutputMode::Json {
                                match to_json_event(&event) {
                                    Ok(val) => {
                                        let _ = tx_inner.send_json(&val).await;
                                    }
                                    Err(_) => tx_inner.fail(),
                                }
                            }
                        })
                    }));
                    *holder.lock().expect("unsub lock") = Some(unsub);
                }
                Ok(())
            }) as pi_agent::BoxFuture<'static, Result<(), String>>
        })
    };

    runtime_host.set_rebind_session(Some(rebind_fn.clone()));

    let mut exit_code = 0i32;

    let run = async {
        let initial_session = runtime_host.try_session()?;
        if mode == PrintOutputMode::Json
            && let Some(header) =
                initial_session.with_session_manager(|sm| sm.get_header().cloned())
        {
            let value = serde_json::to_value(header)
                .map_err(|_| "Cannot serialize session header".to_owned())?;
            out.send_json(&value).await?;
        }

        rebind_fn(initial_session).await?;

        if let Some(init_msg) = options.initial_message {
            let s = runtime_host.try_session()?;
            s.prompt(
                init_msg,
                Some(PromptOptions {
                    images: options.initial_images,
                    ..PromptOptions::default()
                }),
            )
            .await?;
        }

        for msg in options.messages {
            let s = runtime_host.try_session()?;
            s.prompt(msg, None).await?;
        }

        if mode == PrintOutputMode::Text {
            let s = runtime_host.try_session()?;
            let state = s.state();
            let last = last_assistant
                .lock()
                .expect("last assistant lock")
                .clone()
                .or_else(|| {
                    state.messages.iter().rev().find_map(|m| {
                        if let AgentMessage::Assistant(a) = m {
                            Some(a.clone())
                        } else {
                            None
                        }
                    })
                });
            if let Some(last_asst) = last {
                if matches!(
                    last_asst.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    let msg = last_asst.error_message.clone().unwrap_or_else(|| {
                        format!(
                            "Request {}",
                            match last_asst.stop_reason {
                                StopReason::Aborted => "aborted",
                                _ => "error",
                            }
                        )
                    });
                    send_text_chunks(&err, &msg).await?;
                    err.send_text("\n".into()).await?;
                    exit_code = 1;
                } else {
                    for block in &last_asst.content {
                        if let ContentBlock::Text(t) = block {
                            send_text_chunks(&out, &t.text).await?;
                            out.send_text("\n".into()).await?;
                        }
                    }
                    if last_asst.stop_reason == StopReason::Length {
                        err.send_text(
                            "Response was truncated: the model reached its output token limit.\n"
                                .to_owned(),
                        )
                        .await?;
                        exit_code = 1;
                    }
                }
            }
        }

        Ok::<(), String>(())
    };
    let run_res = tokio::select! {
        biased;
        () = failure.cancelled() => Err("stdout output failed".to_owned()),
        () = err_failure.cancelled() => Err("stderr output failed".to_owned()),
        result = run => result,
    };

    if let Err(message) = run_res {
        let _ = err.try_send_text(format!("{message:.4096}\n"));
        exit_code = 1;
    }
    if failure.is_cancelled() || err_failure.is_cancelled() {
        out.fail();
        err.fail();
        if let Ok(s) = runtime_host.try_session() {
            s.request_abort();
        }
    }

    if let Some(prev_unsub) = unsub_holder.lock().expect("unsub lock").take() {
        prev_unsub();
    }
    if let Err(message) = runtime_host.dispose().await {
        let _ = err.try_send_text(format!("{message:.4096}\n"));
        exit_code = 1;
    }
    runtime_host.set_rebind_session(None);
    finish.cancel();

    drop(out);
    drop(err);
    let (written, err_written) = tokio::join!(writer_handle, err_writer_handle);

    if written.unwrap_or(true) || err_written.unwrap_or(true) {
        1
    } else {
        exit_code
    }
}

async fn send_text_chunks(queue: &BoundedStdoutQueue, mut text: &str) -> Result<(), String> {
    // Chunk without copying an unbounded text block into the output queue.
    while !text.is_empty() {
        let mut end = text.len().min(64 * 1024);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        queue.send_text(text[..end].to_owned()).await?;
        text = &text[end..];
    }
    Ok(())
}

async fn drain_output<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut receiver: mpsc::Receiver<(String, Option<tokio::sync::OwnedSemaphorePermit>)>,
    queue: BoundedStdoutQueue,
    other_failure: tokio_util::sync::CancellationToken,
    finish: tokio_util::sync::CancellationToken,
) -> bool {
    let failure = queue.failure_token();
    loop {
        let next = tokio::select! {
            biased;
            () = failure.cancelled() => return true,
            () = other_failure.cancelled() => { queue.fail(); return true; }
            () = finish.cancelled(), if !receiver.is_closed() => {
                receiver.close();
                continue;
            }
            next = receiver.recv() => next,
        };
        let Some((line, _permit)) = next else {
            return false;
        };
        let result = tokio::select! {
            biased;
            () = failure.cancelled() => return true,
            () = other_failure.cancelled() => { queue.fail(); return true; }
            result = async {
                writer.write_all(line.as_bytes()).await?;
                writer.flush().await
            } => result,
        };
        if result.is_err() {
            queue.fail();
            return true;
        }
    }
}
