use super::*;

impl AgentSession {
    // =========================================================================
    // Auto-Retry, Auto-Compaction & Context Overflow Recovery
    // =========================================================================

    pub(super) fn find_last_assistant_message(&self) -> Option<AssistantMessage> {
        let state = self.inner.agent.state();
        for msg in state.messages.iter().rev() {
            if let AgentMessage::Assistant(a) = msg {
                return Some(a.clone());
            }
        }
        None
    }

    fn model_for_message(&self, msg: &AssistantMessage) -> Option<Model> {
        let current = self.model();
        if current.provider == msg.provider && current.id == msg.model {
            return Some(current);
        }
        if is_virtual_model(&current) {
            return self.inner.models.get_model(&msg.provider, &msg.model);
        }
        None
    }

    pub(super) fn limits_model(&self) -> Model {
        let current = self.model();
        if is_virtual_model(&current)
            && let Some(latest) = find_latest_response(&self.inner.agent.state().messages)
            && let Some(phys) = self.inner.models.get_model(&latest.provider, &latest.model)
        {
            return phys;
        }
        current
    }

    pub(super) fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        if self.fatal_runtime_error().is_some() {
            return false;
        }
        let cw = self
            .model_for_message(message)
            .unwrap_or_else(|| self.model())
            .context_window;
        if is_context_overflow(message, (cw > 0).then_some(cw)) {
            return false;
        }
        is_retryable_assistant_error(message)
    }

    pub(super) fn finish_cancelled_retry(&self) {
        let attempt = self.inner.retry_attempt.swap(0, Ordering::SeqCst);
        if attempt == 0 {
            return;
        }
        self.emit(AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt: attempt as u32,
            final_error: Some("Retry cancelled".to_owned()),
        });
    }

    async fn prepare_retry(&self, message: &AssistantMessage) -> bool {
        let policy = self.retry_policy();
        if !policy.enabled {
            return false;
        }
        let next_attempt = self.inner.retry_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        if (next_attempt as u32) > policy.max_retries {
            self.inner.retry_attempt.fetch_sub(1, Ordering::SeqCst);
            return false;
        }

        let delay_ms = retry_delay_ms(&policy, next_attempt as u32);
        let err_msg = message
            .error_message
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Unknown error".to_owned());

        let token = CancellationToken::new();
        *self.inner.retry_cancel.lock().expect("lock") = Some(token.clone());
        if self.is_closing_or_closed()
            || self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
        {
            token.cancel();
        }
        self.emit(AgentSessionEvent::AutoRetryStart {
            attempt: next_attempt as u32,
            max_attempts: policy.max_retries,
            delay_ms,
            error_message: err_msg,
        });

        if let Err(err) = self.omit_recovery_attempt(message) {
            self.inner.retry_cancel.lock().expect("lock").take();
            self.set_fatal_runtime_error(err);
            return false;
        }

        let aborted = tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => false,
            _ = token.cancelled() => true,
        };
        *self.inner.retry_cancel.lock().expect("lock") = None;

        if aborted {
            self.finish_cancelled_retry();
            return false;
        }
        true
    }

    fn find_persisted_message_entry_id(&self, target: &AgentMessage) -> Option<String> {
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        for entry in branch.iter().rev() {
            if let SessionEntry::Message(m) = entry
                && &m.message == target
            {
                return Some(m.id.clone());
            }
        }
        let guard = self
            .inner
            .entry_ids_by_timestamp_and_role
            .lock()
            .expect("entry map lock");
        guard
            .iter()
            .rev()
            .find(|(role, ts, _)| role == target.role() && *ts == target.timestamp())
            .map(|(_, _, id)| id.clone())
    }

    /// 持久化隐去失败的 `AssistantMessage` 及其紧随的配对 `ToolResultMessage`（对齐上游 `#11227`）。
    fn omit_recovery_attempt(&self, assistant_msg: &AssistantMessage) -> Result<(), String> {
        let tool_call_ids: BTreeSet<String> = assistant_msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall(tc) => Some(tc.id.clone()),
                _ => None,
            })
            .collect();

        let asst_agent_msg = AgentMessage::Assistant(assistant_msg.clone());
        let asst_entry_id = self.find_persisted_message_entry_id(&asst_agent_msg);
        let state_msgs = self.inner.agent.state().messages;

        let idx = state_msgs.iter().rposition(|m| m == &asst_agent_msg);
        let mut tool_results: Vec<AgentMessage> = Vec::new();
        if let Some(start_idx) = idx {
            for m in &state_msgs[start_idx + 1..] {
                if m.role() == "assistant" {
                    break;
                }
                if let AgentMessage::ToolResult(tr) = m
                    && tool_call_ids.contains(&tr.tool_call_id)
                {
                    tool_results.push(m.clone());
                }
            }
        }

        let mut target_ids: Vec<String> = Vec::with_capacity(1 + tool_results.len());
        if let Some(aid) = asst_entry_id {
            target_ids.push(aid);
        } else if idx.is_some() {
            return Err(
                "Cannot persist recovery omission because assistant message has no source entry"
                    .to_owned(),
            );
        }

        for tr_msg in &tool_results {
            let Some(tid) = self.find_persisted_message_entry_id(tr_msg) else {
                return Err(
                    "Cannot persist recovery omission because a projected toolResult message has no source entry"
                        .to_owned(),
                );
            };
            target_ids.push(tid);
        }

        for target_id in target_ids {
            let edit_id = self
                .with_session_manager_internal(|sm| sm.append_context_edit(&target_id, None))
                .map_err(|e| e.to_string())?;
            if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&edit_id).cloned()) {
                self.emit(AgentSessionEvent::EntryAppended { entry });
            }
        }

        self.refresh_finalized_context();
        Ok(())
    }

    pub(super) async fn handle_post_agent_run(&self) -> bool {
        if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            || self.fatal_runtime_error().is_some()
        {
            self.finish_cancelled_retry();
            return false;
        }

        let last_asst = self
            .inner
            .last_assistant_message
            .lock()
            .expect("lock")
            .take()
            .map(|(m, _)| m);

        let Some(message) = last_asst else {
            return self.inner.agent.has_queued_messages();
        };

        if self.is_retryable_error(&message) && self.prepare_retry(&message).await {
            if self.inner.agent_run_abort_requested.load(Ordering::SeqCst) {
                self.finish_cancelled_retry();
                return false;
            }
            let phys_model = self
                .model_for_message(&message)
                .unwrap_or_else(|| self.model());
            *self.inner.failed_response.lock().expect("lock") =
                Some((phys_model, self.thinking_level(), message));
            return !self.inner.agent_run_abort_requested.load(Ordering::SeqCst);
        }

        if self.inner.agent_run_abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
            return false;
        }

        if message.stop_reason == StopReason::Error {
            let attempt = self.inner.retry_attempt.swap(0, Ordering::SeqCst);
            if attempt > 0 {
                self.emit(AgentSessionEvent::AutoRetryEnd {
                    success: false,
                    attempt: attempt as u32,
                    final_error: message.error_message.clone(),
                });
            }
        }

        if self.check_compaction(&message, true).await {
            return !self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                && self.fatal_runtime_error().is_none();
        }

        !self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            && self.fatal_runtime_error().is_none()
            && self.inner.agent.has_queued_messages()
    }

    pub(super) fn exceeds_compaction_threshold(
        &self,
        model: &Model,
        projection: &crate::session_manager::SessionProjection,
    ) -> bool {
        if model.context_window == 0 {
            return false;
        }
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        let tokens = estimate_projected_context_tokens(projection, &branch).tokens;
        should_compact(tokens, model.context_window, &self.compaction_settings())
    }

    pub(super) async fn check_compaction(
        &self,
        assistant_message: &AssistantMessage,
        skip_aborted_check: bool,
    ) -> bool {
        if self.fatal_runtime_error().is_some() {
            return false;
        }
        let settings = self.compaction_settings();
        if !settings.enabled {
            return false;
        }
        if skip_aborted_check && assistant_message.stop_reason == StopReason::Aborted {
            return false;
        }

        let message_model = self.model_for_message(assistant_message);
        let same_model = message_model.is_some();
        let effective_model = message_model.clone().unwrap_or_else(|| self.model());
        let context_window = effective_model.context_window;

        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        if let Some(latest_comp) = get_latest_compaction_entry(&branch)
            && let Some(comp_ms) = parse_iso_timestamp_ms(&latest_comp.timestamp)
            && assistant_message.timestamp > 0
            && assistant_message.timestamp <= comp_ms
        {
            return false;
        }

        let current_projection =
            self.with_session_manager(SessionManager::build_session_projection);
        let asst_agent_msg = AgentMessage::Assistant(assistant_message.clone());
        let asst_entry_id = self.find_persisted_message_entry_id(&asst_agent_msg);
        let asst_is_projected = asst_entry_id.as_ref().is_none_or(|id| {
            current_projection.entries.iter().any(|e| {
                e.source_entry.id() == id && e.messages.iter().any(|m| m.role() == "assistant")
            })
        });

        let explicit_overflow = assistant_message.stop_reason == StopReason::Error
            && is_context_overflow(assistant_message, None);
        let silent_or_length_overflow = asst_is_projected
            && is_context_overflow(
                assistant_message,
                (context_window > 0).then_some(context_window),
            );
        let context_overflow = same_model && (explicit_overflow || silent_or_length_overflow);
        let recoverable_length = same_model
            && asst_is_projected
            && is_recoverable_length(assistant_message, effective_model.max_tokens);

        if context_overflow || recoverable_length {
            let will_retry = assistant_message.stop_reason != StopReason::Stop;
            if !will_retry {
                // Case 2: 静默超窗但回复已完成（stop_reason == Stop），仅压缩不重试
                return self
                    .run_auto_compaction(CompactionReason::Overflow, false)
                    .await;
            }

            if self
                .inner
                .overflow_recovery_attempted
                .swap(true, Ordering::SeqCst)
            {
                let err_msg = if context_overflow {
                    "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_owned()
                } else {
                    "Truncated response recovery failed after one compact-and-retry attempt."
                        .to_owned()
                };
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Overflow,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(err_msg.clone()),
                });
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::SessionCompactFailed {
                        reason: CompactionReason::Overflow,
                        error_message: err_msg,
                        will_retry: false,
                    })
                    .await;
                return false;
            }

            if let Err(err) = self.omit_recovery_attempt(assistant_message) {
                self.set_fatal_runtime_error(err);
                return false;
            }

            let retry = self
                .run_auto_compaction(CompactionReason::Overflow, true)
                .await;
            if retry {
                *self.inner.failed_response.lock().expect("lock") = Some((
                    effective_model,
                    self.thinking_level(),
                    assistant_message.clone(),
                ));
            }
            return retry;
        }

        // Case 3: 阈值触发自动压缩
        let has_context_edits = current_projection
            .entries
            .iter()
            .any(|e| matches!(e.source_entry, SessionEntry::ContextEdit(_)));
        let direct_tokens = calculate_context_tokens(&assistant_message.usage);
        let context_tokens = if has_context_edits {
            estimate_projected_context_tokens(&current_projection, &branch).tokens
        } else if assistant_message.stop_reason == StopReason::Error || direct_tokens == 0 {
            estimate_context_tokens(&self.inner.agent.state().messages).tokens
        } else {
            direct_tokens
        };

        if should_compact(context_tokens, context_window, &settings) {
            return self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
        }

        false
    }

    pub(super) async fn run_auto_compaction(
        &self,
        reason: CompactionReason,
        will_retry: bool,
    ) -> bool {
        let model = self.limits_model();
        let settings = self.compaction_settings();
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        let Some(preparation) = prepare_compaction(&branch, settings) else {
            return false;
        };

        let cancel = CancellationToken::new();
        *self.inner.compaction_cancel.lock().expect("lock") = Some(cancel.clone());
        self.emit(AgentSessionEvent::CompactionStart { reason });

        let outcome = self
            .execute_compaction_core(
                &preparation,
                &branch,
                &model,
                None,
                reason,
                will_retry,
                cancel.clone(),
            )
            .await;
        *self.inner.compaction_cancel.lock().expect("lock") = None;
        self.inner.idle_notify.notify_waiters();

        match outcome {
            Ok(result) => {
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason,
                    result: Some(result),
                    aborted: false,
                    will_retry,
                    error_message: None,
                });
                if will_retry {
                    true
                } else {
                    self.inner.agent.has_queued_messages()
                }
            }
            Err((aborted, err)) => {
                let err_msg = (!aborted).then(|| match reason {
                    CompactionReason::Overflow => {
                        format!("Context overflow recovery failed: {err}")
                    }
                    _ => format!("Auto-compaction failed: {err}"),
                });
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: err_msg.clone(),
                });
                if let Some(em) = err_msg {
                    let _ = self
                        .extension_runner()
                        .emit_lifecycle(SessionLifecycleEvent::SessionCompactFailed {
                            reason,
                            error_message: em,
                            will_retry: false,
                        })
                        .await;
                }
                false
            }
        }
    }

    /// 手动压缩上下文（`/compact`，支持短会话自适应折半缩减保留预算与 `session_before_compact` 钩子拦截）。
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<CompactionResult, String> {
        self.ensure_external_open()?;
        if !self.has_configured_model() {
            return Err(self.no_models_error_message());
        }
        self.abort().await;
        let cancel = CancellationToken::new();
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            if self.is_compacting() {
                return Err("Compaction already in progress".to_owned());
            }
            *self.inner.compaction_cancel.lock().expect("lock") = Some(cancel.clone());
        }
        let session = self.clone();
        tokio::spawn(async move {
            let _active = ActiveAuxiliaryGuard {
                session: session.downgrade(),
                compaction: true,
            };
            RUN_SESSION
                .scope(
                    Arc::as_ptr(&session.inner) as usize,
                    session.compact_internal(custom_instructions, cancel),
                )
                .await
        })
        .await
        .map_err(|_| "Session compaction task failed".to_owned())?
    }

    async fn compact_internal(
        &self,
        custom_instructions: Option<String>,
        cancel: CancellationToken,
    ) -> Result<CompactionResult, String> {
        self.emit_async(AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        })
        .await;

        let model = self.limits_model();
        let settings = self.compaction_settings();
        let branch = self.with_session_manager(|sm| sm.get_branch(None));

        let Some(preparation) = prepare_compaction_with_adaptive_budget(&branch, settings) else {
            let err = if matches!(branch.last(), Some(SessionEntry::Compaction(_))) {
                "Already compacted".to_owned()
            } else {
                "Nothing to compact (session too small)".to_owned()
            };
            self.emit_async(AgentSessionEvent::CompactionEnd {
                reason: CompactionReason::Manual,
                result: None,
                aborted: false,
                will_retry: false,
                error_message: Some(format!("Compaction failed: {err}")),
            })
            .await;
            return Err(err);
        };

        let outcome = self
            .execute_compaction_core(
                &preparation,
                &branch,
                &model,
                custom_instructions,
                CompactionReason::Manual,
                false,
                cancel,
            )
            .await;

        match outcome {
            Ok(result) => {
                self.emit_async(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: Some(result.clone()),
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                })
                .await;
                Ok(result)
            }
            Err((aborted, err)) => {
                let error_message = (!aborted).then(|| format!("Compaction failed: {err}"));
                self.emit_async(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: error_message.clone(),
                })
                .await;
                if let Some(em) = error_message {
                    let _ = self
                        .extension_runner()
                        .emit_lifecycle(SessionLifecycleEvent::SessionCompactFailed {
                            reason: CompactionReason::Manual,
                            error_message: em,
                            will_retry: false,
                        })
                        .await;
                }
                Err(err)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_compaction_core(
        &self,
        preparation: &CompactionPreparation,
        branch_entries: &[SessionEntry],
        model: &Model,
        custom_instructions: Option<String>,
        _reason: CompactionReason,
        _will_retry: bool,
        cancel_token: CancellationToken,
    ) -> Result<CompactionResult, (bool, String)> {
        let mut from_extension = false;
        let mut ext_compaction: Option<CompactionResult> = None;

        let runner = self.extension_runner();
        let before = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => return Err((true, "Compaction cancelled".to_owned())),
            result = runner.emit_lifecycle(SessionLifecycleEvent::SessionBeforeCompact {
                preparation: preparation.clone(),
                branch_entries: branch_entries.to_vec(),
                custom_instructions: custom_instructions.clone(),
            }) => result,
        };
        if let SessionBeforeEventOutput::Compact(before_res) = before {
            if before_res.cancel {
                return Err((true, "Compaction cancelled".to_owned()));
            }
            if let Some(custom_comp) = before_res.compaction {
                from_extension = true;
                ext_compaction = Some(custom_comp);
            }
        }

        if cancel_token.is_cancelled() {
            return Err((true, "Compaction cancelled".to_owned()));
        }

        let base_result = if let Some(ext_res) = ext_compaction {
            ext_res
        } else {
            let sum_opts = SummarizationCallOptions {
                api_key: None,
                headers: None,
                cancel_token: Some(cancel_token.clone()),
                custom_instructions,
                thinking_level: None,
                stream_fn: Some(self.inner.agent.stream_fn()),
                retry: Some(self.retry_policy()),
                callbacks: None,
                session_id: None,
                block_images: self.is_block_images_enabled(),
            };
            compact(preparation, model, &sum_opts)
                .await
                .map_err(|e| (cancel_token.is_cancelled(), e))?
        };

        if cancel_token.is_cancelled() {
            return Err((true, "Compaction cancelled".to_owned()));
        }

        let comp_id = self
            .with_session_manager_internal(|sm| {
                sm.append_compaction(
                    &base_result.summary,
                    Some(base_result.first_kept_entry_id.clone()),
                    base_result.tokens_before,
                    base_result.details.clone(),
                    Some(from_extension),
                    base_result.usage.clone(),
                )
            })
            .map_err(|e| (false, e.to_string()))?;

        self.refresh_finalized_context();
        let proj = self.with_session_manager(SessionManager::build_session_projection);
        let estimated_after: u64 = proj.messages.iter().map(estimate_tokens).sum();

        if let Some(SessionEntry::Compaction(saved)) =
            self.with_session_manager(|sm| sm.get_entry(&comp_id).cloned())
        {
            let _ = self
                .extension_runner()
                .emit_lifecycle(SessionLifecycleEvent::SessionCompact {
                    compaction_entry: saved.clone(),
                    from_extension,
                })
                .await;
            self.emit(AgentSessionEvent::EntryAppended {
                entry: SessionEntry::Compaction(saved),
            });
        }

        Ok(CompactionResult {
            summary: base_result.summary,
            first_kept_entry_id: base_result.first_kept_entry_id,
            tokens_before: base_result.tokens_before,
            estimated_tokens_after: Some(estimated_after),
            usage: base_result.usage,
            details: base_result.details,
        })
    }
}
