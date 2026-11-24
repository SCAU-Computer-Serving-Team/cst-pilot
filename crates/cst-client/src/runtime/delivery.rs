use std::sync::Arc;

use serde_json::Value;
use tokio::sync::watch;

use crate::error::ClientError;
use crate::outbox::OutboxItem;
use crate::workspace::{ComposerDraft, DraftKey};

use super::{ReconcileReport, SendSubmissionOutcome, SharedClientCore, bump_revision};

fn build_message_post_body(item: &OutboxItem) -> Value {
    let mut msg_body = serde_json::Map::new();
    msg_body.insert("id".into(), Value::String(item.client_message_id.clone()));
    msg_body.insert("text".into(), Value::String(item.text.clone()));
    msg_body.insert(
        "delivery".into(),
        Value::String(item.send_mode.as_api_delivery().to_owned()),
    );
    if let Some(ref skill) = item.skill {
        msg_body.insert("skill".into(), Value::String(skill.clone()));
    }
    if !item.images.is_empty() {
        let imgs: Vec<Value> = item.images.iter().map(|h| h.to_base64_payload()).collect();
        msg_body.insert("images".into(), Value::Array(imgs));
    }
    Value::Object(msg_body)
}

pub(super) async fn execute_send_draft(
    core: &Arc<SharedClientCore>,
    revision_tx: &watch::Sender<u64>,
    target: DraftKey,
    draft: &ComposerDraft,
    previous: Option<watch::Receiver<()>>,
    preserve_workspace_draft: bool,
) -> Result<SendSubmissionOutcome, ClientError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let initial_session_id = match &target {
        DraftKey::Session(sid) => Some(sid.clone()),
        DraftKey::NewSession => None,
    };
    let item = OutboxItem::from_draft(initial_session_id.clone(), draft, now_ms)?;
    let client_message_id = item.client_message_id.clone();

    let (session_id, bound_item, _created_tail) = match initial_session_id {
        Some(sid) => {
            core.outbox
                .lock()
                .expect("outbox lock")
                .enqueue_session(&sid, item.clone());
            bump_revision(core, revision_tx);
            if let Some(mut previous) = previous {
                let _ = previous.changed().await;
            }
            if core
                .outbox
                .lock()
                .expect("outbox lock")
                .is_session_paused(&sid)
            {
                return Err(ClientError::StorageFull(
                    "本机磁盘空间已满，当前会话后续消息已暂停发送。".into(),
                ));
            }
            (sid, item, None)
        }
        None => {
            let req_key = item
                .creation_request_key
                .clone()
                .expect("home item has creation_request_key");
            if let Err(err) = core
                .outbox
                .lock()
                .expect("outbox lock")
                .enqueue_home(item.clone())
            {
                let _ = core
                    .workspace
                    .lock()
                    .expect("workspace lock")
                    .restore_draft_if_empty(DraftKey::NewSession, draft.clone());
                return Err(err);
            }
            bump_revision(core, revision_tx);

            match core
                .http
                .create_session(
                    &req_key,
                    draft.selected_provider.as_deref(),
                    draft.selected_model_id.as_deref(),
                    draft.selected_thinking_level.as_deref(),
                )
                .await
            {
                Ok(created_sid) => {
                    // Register a send-chain tail BEFORE switching `active_session` so a rapid
                    // follow-up send in the new session waits for this first POST `/messages`.
                    let (tail_tx, prev_tail) = core.enqueue_send_tail(&created_sid);
                    if let Some(mut prev) = prev_tail {
                        let _ = prev.changed().await;
                    }
                    let bound = core
                        .outbox
                        .lock()
                        .expect("outbox lock")
                        .bind_home_to_session(&created_sid)
                        .unwrap_or(item);
                    core.workspace
                        .lock()
                        .expect("workspace lock")
                        .set_active_session(Some(created_sid.clone()));
                    bump_revision(core, revision_tx);
                    (created_sid, bound, Some(tail_tx))
                }
                Err(err) if err.is_disconnected() => {
                    core.outbox
                        .lock()
                        .expect("outbox lock")
                        .mark_home_uncertain(err.to_string());
                    bump_revision(core, revision_tx);
                    return Err(err);
                }
                Err(err) => {
                    // Restore to NewSession composer ONLY if the user hasn't started typing new content;
                    // otherwise retain the failed item in `home_pending` so neither draft is lost!
                    let restored = core
                        .workspace
                        .lock()
                        .expect("workspace lock")
                        .restore_draft_if_empty(DraftKey::NewSession, draft.clone());
                    {
                        let mut ob = core.outbox.lock().expect("outbox lock");
                        if restored {
                            ob.clear_home();
                        } else {
                            ob.mark_home_failed(&err);
                        }
                    }
                    bump_revision(core, revision_tx);
                    return Err(err);
                }
            }
        }
    };

    let msg_body = build_message_post_body(&bound_item);
    match core
        .http
        .post_session_message(&session_id, &client_message_id, &msg_body)
        .await
    {
        Ok(_) => {
            core.outbox
                .lock()
                .expect("outbox lock")
                .mark_accepted(&session_id, &client_message_id);
            bump_revision(core, revision_tx);
            Ok(SendSubmissionOutcome {
                session_id,
                client_message_id,
            })
        }
        Err(err) => {
            if err.is_credentials_required() && !preserve_workspace_draft {
                let target_for_restore = match &target {
                    DraftKey::NewSession => DraftKey::Session(session_id.clone()),
                    other => other.clone(),
                };
                let restored = core
                    .workspace
                    .lock()
                    .expect("workspace lock")
                    .restore_draft_if_empty(target_for_restore, draft.clone());
                let mut ob = core.outbox.lock().expect("outbox lock");
                if restored {
                    ob.remove_item(&session_id, &client_message_id);
                } else {
                    ob.mark_session_error(&session_id, &client_message_id, &err);
                }
            } else {
                core.outbox.lock().expect("outbox lock").mark_session_error(
                    &session_id,
                    &client_message_id,
                    &err,
                );
            }
            bump_revision(core, revision_tx);
            Err(err)
        }
    }
}

pub(super) async fn execute_retry_outbox(
    core: &Arc<SharedClientCore>,
    revision_tx: &watch::Sender<u64>,
    session_id: Option<String>,
    client_message_id: &str,
    previous: Option<watch::Receiver<()>>,
) -> Result<SendSubmissionOutcome, ClientError> {
    let (target_sid, item, check_receipt_first, _created_tail) = match session_id {
        None => {
            let (home_item, was_uncertain) = core
                .outbox
                .lock()
                .expect("outbox lock")
                .prepare_home_retry(client_message_id)?;
            bump_revision(core, revision_tx);
            let req_key = home_item
                .creation_request_key
                .clone()
                .expect("home retry has request_key");

            let created_sid = if was_uncertain {
                match core.http.check_session_creation(&req_key).await {
                    Ok((status, Some(sid))) if status == "done" => Some(sid),
                    Ok((status, _)) if status == "started" => {
                        core.outbox
                            .lock()
                            .expect("outbox lock")
                            .mark_home_uncertain("会话仍在创建中，请稍后重试。".into());
                        bump_revision(core, revision_tx);
                        return Err(ClientError::Conflict {
                            code: "session_creation_in_progress".into(),
                            message: "会话仍在创建中，请稍后重试。".into(),
                        });
                    }
                    Ok(_) => None,
                    Err(err) => {
                        if err.is_disconnected() {
                            core.outbox
                                .lock()
                                .expect("outbox lock")
                                .mark_home_uncertain(err.to_string());
                        } else {
                            core.outbox
                                .lock()
                                .expect("outbox lock")
                                .mark_home_failed(&err);
                        }
                        bump_revision(core, revision_tx);
                        return Err(err);
                    }
                }
            } else {
                None
            };

            let sid = match created_sid {
                Some(sid) => sid,
                None => match core
                    .http
                    .create_session(
                        &req_key,
                        home_item.provider.as_deref(),
                        home_item.model_id.as_deref(),
                        home_item.thinking_level.as_deref(),
                    )
                    .await
                {
                    Ok(sid) => sid,
                    Err(err) if err.is_disconnected() => {
                        core.outbox
                            .lock()
                            .expect("outbox lock")
                            .mark_home_uncertain(err.to_string());
                        bump_revision(core, revision_tx);
                        return Err(err);
                    }
                    Err(err) => {
                        core.outbox
                            .lock()
                            .expect("outbox lock")
                            .mark_home_failed(&err);
                        bump_revision(core, revision_tx);
                        return Err(err);
                    }
                },
            };

            let (tail_tx, prev_tail) = core.enqueue_send_tail(&sid);
            if let Some(mut prev) = prev_tail {
                let _ = prev.changed().await;
            }
            let bound = core
                .outbox
                .lock()
                .expect("outbox lock")
                .bind_home_to_session(&sid)
                .unwrap_or(home_item);
            {
                let mut ws = core.workspace.lock().expect("workspace lock");
                if ws.active_session_id().is_none()
                    && !ws
                        .draft(&DraftKey::NewSession)
                        .is_some_and(ComposerDraft::has_unsent_content)
                {
                    ws.set_active_session(Some(sid.clone()));
                }
            }
            bump_revision(core, revision_tx);
            (sid, bound, was_uncertain, Some(tail_tx))
        }
        Some(sid) => {
            let (sess_item, was_uncertain) = core
                .outbox
                .lock()
                .expect("outbox lock")
                .prepare_session_retry(&sid, client_message_id)?;
            bump_revision(core, revision_tx);
            if let Some(mut previous) = previous {
                let _ = previous.changed().await;
            }
            (sid, sess_item, was_uncertain, None)
        }
    };

    if check_receipt_first
        && let Ok(Some(_)) = core
            .http
            .check_message_receipt(&target_sid, &item.client_message_id)
            .await
    {
        core.outbox
            .lock()
            .expect("outbox lock")
            .mark_accepted(&target_sid, &item.client_message_id);
        bump_revision(core, revision_tx);
        return Ok(SendSubmissionOutcome {
            session_id: target_sid,
            client_message_id: item.client_message_id,
        });
    }

    let msg_body = build_message_post_body(&item);
    match core
        .http
        .post_session_message(&target_sid, &item.client_message_id, &msg_body)
        .await
    {
        Ok(_) => {
            core.outbox
                .lock()
                .expect("outbox lock")
                .mark_accepted(&target_sid, &item.client_message_id);
            bump_revision(core, revision_tx);
            Ok(SendSubmissionOutcome {
                session_id: target_sid,
                client_message_id: item.client_message_id,
            })
        }
        Err(err) => {
            core.outbox.lock().expect("outbox lock").mark_session_error(
                &target_sid,
                &item.client_message_id,
                &err,
            );
            bump_revision(core, revision_tx);
            Err(err)
        }
    }
}

/// Read-only receipt reconciliation after a disconnect (`GET /api/sessions/creation/<key>`
/// and `GET /api/sessions/<id>/messages/<msgId>`). Never auto-resends unaccepted payloads!
pub(super) async fn reconcile_uncertain_items(
    core: &Arc<SharedClientCore>,
) -> Result<ReconcileReport, ClientError> {
    let items = core.outbox.lock().expect("outbox lock").uncertain_items();
    let mut report = ReconcileReport::default();
    let mut first_error: Option<ClientError> = None;

    for (maybe_sid, item) in items {
        match maybe_sid {
            None => {
                if let Some(req_key) = &item.creation_request_key {
                    let (status, created_sid) =
                        match core.http.check_session_creation(req_key).await {
                            Ok(res) => res,
                            Err(err) => {
                                if first_error.is_none() {
                                    first_error = Some(err);
                                }
                                continue;
                            }
                        };
                    if status == "done"
                        && let Some(sid) = created_sid
                    {
                        let already_accepted = match core
                            .http
                            .check_message_receipt(&sid, &item.client_message_id)
                            .await
                        {
                            Ok(found) => found.is_some(),
                            Err(err @ ClientError::NotFound { .. }) => {
                                let applied = core
                                    .outbox
                                    .lock()
                                    .expect("outbox lock")
                                    .reconcile_home_bound(
                                        &item.client_message_id,
                                        &sid,
                                        false,
                                        &err,
                                    );
                                if applied {
                                    report.resolved_count += 1;
                                }
                                continue;
                            }
                            Err(err) => {
                                if first_error.is_none() {
                                    first_error = Some(err);
                                }
                                continue;
                            }
                        };
                        let not_delivered = ClientError::NotFound {
                            code: "message_not_delivered".into(),
                            message: "会话已创建，首条消息未送达，请点击重试。".into(),
                        };
                        let applied = core
                            .outbox
                            .lock()
                            .expect("outbox lock")
                            .reconcile_home_bound(
                                &item.client_message_id,
                                &sid,
                                already_accepted,
                                &not_delivered,
                            );
                        if applied {
                            {
                                let mut ws = core.workspace.lock().expect("workspace lock");
                                if ws.active_session_id().is_none()
                                    && !ws
                                        .draft(&DraftKey::NewSession)
                                        .is_some_and(ComposerDraft::has_unsent_content)
                                {
                                    ws.set_active_session(Some(sid.clone()));
                                }
                            }
                            report.recovered_home_session_id = Some(sid);
                            report.resolved_count += 1;
                        }
                    } else if status == "missing" {
                        // Guard against races: only restore draft & clear home if the exact item is still Uncertain.
                        let still_uncertain = core
                            .outbox
                            .lock()
                            .expect("outbox lock")
                            .is_home_uncertain(&item.client_message_id);
                        if !still_uncertain {
                            continue;
                        }
                        let restored = core
                            .workspace
                            .lock()
                            .expect("workspace lock")
                            .restore_draft_if_empty(DraftKey::NewSession, item.to_composer_draft());
                        let not_created = ClientError::NotFound {
                            code: "session_not_created".into(),
                            message: "连接中断前会话尚未创建，可重试或移除。".into(),
                        };
                        let applied = core
                            .outbox
                            .lock()
                            .expect("outbox lock")
                            .reconcile_home_missing(
                                &item.client_message_id,
                                restored,
                                &not_created,
                            );
                        if applied {
                            if restored {
                                report.restored_home_draft = true;
                            }
                            report.resolved_count += 1;
                        }
                    }
                }
            }
            Some(sid) => match core
                .http
                .check_message_receipt(&sid, &item.client_message_id)
                .await
            {
                Ok(Some(_server_item)) => {
                    if core
                        .outbox
                        .lock()
                        .expect("outbox lock")
                        .reconcile_session_uncertain(&sid, &item.client_message_id, true, None)
                    {
                        report.resolved_count += 1;
                    }
                }
                Ok(None) => {
                    let not_received = ClientError::NotFound {
                        code: "message_not_received".into(),
                        message: "网络中断前服务端未收到该消息，可手动重试或编辑。".into(),
                    };
                    if core
                        .outbox
                        .lock()
                        .expect("outbox lock")
                        .reconcile_session_uncertain(
                            &sid,
                            &item.client_message_id,
                            false,
                            Some(&not_received),
                        )
                    {
                        report.resolved_count += 1;
                    }
                }
                Err(err @ ClientError::NotFound { .. }) => {
                    if core
                        .outbox
                        .lock()
                        .expect("outbox lock")
                        .reconcile_session_uncertain(
                            &sid,
                            &item.client_message_id,
                            false,
                            Some(&err),
                        )
                    {
                        report.resolved_count += 1;
                    }
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            },
        }
    }

    if report.resolved_count > 0 {
        Ok(report)
    } else if let Some(err) = first_error {
        Err(err)
    } else {
        Ok(report)
    }
}
