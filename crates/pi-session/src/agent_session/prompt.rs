use super::*;

impl AgentSession {
    pub async fn execute_nested_tool_call(
        &self,
        caller_id: &str,
        name: &str,
        args: JsonValue,
        options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        let runner = self
            .inner
            .nested_runner
            .get()
            .expect("nested_runner initialized");
        runner.execute(caller_id, name, args, options).await
    }

    // =========================================================================
    // Prompting, Queues, Custom Messages & Bash Execution
    // =========================================================================

    pub async fn prompt(
        &self,
        text: impl Into<String>,
        options: Option<PromptOptions>,
    ) -> Result<PromptDisposition, String> {
        let text = text.into();
        let opts = options.unwrap_or_default();

        if let Err(err) = self.ensure_external_open() {
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        // 若正在广播 agent_settled，推入延迟队列待广播结束后再执行
        if self.inner.is_emitting_agent_settled.load(Ordering::SeqCst) {
            let s = self.clone();
            let t = text;
            let o = opts;
            self.inner
                .deferred_settled_actions
                .lock()
                .expect("deferred lock")
                .push(Box::new(move || {
                    Box::pin(async move {
                        let _ = s.prompt_after_settled(t, o).await;
                    })
                }));
            return Ok(PromptDisposition::Started);
        }

        self.prompt_after_settled(text, opts).await
    }

    pub(super) fn accept_input(
        &self,
        client_id: &str,
        disposition: PromptDisposition,
        entry_id: Option<String>,
    ) -> Result<(), String> {
        let result = self.with_session_manager_mut(|sm| {
            self.with_execution_log_internal(|log| {
                log.mark_input_accepted(sm, client_id, disposition, entry_id)
            })
        });
        result.map(|_| ()).map_err(|err| {
            let msg = format!("Failed to persist input acceptance: {err}");
            self.set_fatal_runtime_error(msg.clone());
            msg
        })
    }

    async fn prompt_after_settled(
        &self,
        text: String,
        opts: PromptOptions,
    ) -> Result<PromptDisposition, String> {
        if let Err(err) = self.ensure_external_open() {
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        let _input_lease = match &self.inner.data_policy {
            Some(policy) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                policy.admit_input(&text, opts.expand_prompt_templates.unwrap_or(true))
            })) {
                Ok(Ok(lease)) => lease,
                _ => {
                    let error = "Session input admission declined".to_owned();
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(error.clone()));
                    }
                    return Err(error);
                }
            },
            None => None,
        };

        // 幂等 client_message_id 预检与去重
        if let Some(client_id) = opts.client_message_id.as_deref() {
            let persisted_text = match &self.inner.data_policy {
                Some(policy) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    policy.persisted_input(&text)
                })) {
                    Ok(Ok(text)) => text,
                    _ => {
                        let error = "Session input projection failed".to_owned();
                        if let Some(cb) = &opts.on_admitted {
                            cb(Err(error.clone()));
                        }
                        self.set_fatal_runtime_error(error.clone());
                        return Err(error);
                    }
                },
                None => text.clone(),
            };
            let (record, is_dup) = self
                .with_execution_log_mut(|log| {
                    log.begin_input_delivery(
                        client_id,
                        opts.queue_item_id.as_deref(),
                        &persisted_text,
                        opts.images.clone().unwrap_or_default(),
                    )
                })
                .map_err(|e| {
                    let msg = e.to_string();
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(msg.clone()));
                    }
                    msg
                })?;

            if is_dup {
                let res = match record.status {
                    InputDeliveryStatus::Accepted { disposition, .. } => Ok(disposition),
                    InputDeliveryStatus::Delivering | InputDeliveryStatus::Queued => {
                        Ok(PromptDisposition::Started)
                    }
                    InputDeliveryStatus::Failed { error } => Err(error),
                    InputDeliveryStatus::Uncertain { reason } => Err(reason),
                };
                if let Some(cb) = &opts.on_admitted {
                    cb(res.clone());
                }
                return res;
            }
        }

        let expand_templates = opts.expand_prompt_templates.unwrap_or(true);
        if expand_templates
            && text.starts_with('/')
            && self.try_execute_extension_command(&text).await?
        {
            if let Some(client_id) = opts.client_message_id.as_deref()
                && let Err(err) =
                    self.accept_input(client_id, PromptDisposition::HandledByExtension, None)
            {
                if let Some(cb) = &opts.on_admitted {
                    cb(Err(err.clone()));
                }
                return Err(err);
            }
            if let Some(cb) = &opts.on_admitted {
                cb(Ok(PromptDisposition::HandledByExtension));
            }
            return Ok(PromptDisposition::HandledByExtension);
        }

        if self.is_compacting() {
            let err =
                "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
                    .to_owned();
            if let Some(client_id) = opts.client_message_id.as_deref() {
                let _ =
                    self.with_execution_log_internal(|log| log.mark_input_failed(client_id, &err));
            }
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        let input_res = self
            .emit_input_cancellable(
                text.clone(),
                opts.images.clone(),
                opts.source.unwrap_or(InputSource::Interactive),
                self.is_streaming()
                    .then_some(opts.streaming_behavior)
                    .flatten(),
            )
            .await;
        let input_res =
            match input_res.and_then(|result| self.ensure_external_open().map(|()| result)) {
                Ok(result) => result,
                Err(error) => {
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(error.clone()));
                    }
                    return Err(error);
                }
            };

        let (mut current_text, current_images) = match input_res {
            InputEventResult::Handled => {
                if let Some(client_id) = opts.client_message_id.as_deref()
                    && let Err(err) =
                        self.accept_input(client_id, PromptDisposition::HandledByExtension, None)
                {
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(err.clone()));
                    }
                    return Err(err);
                }
                if let Some(cb) = &opts.on_admitted {
                    cb(Ok(PromptDisposition::HandledByExtension));
                }
                return Ok(PromptDisposition::HandledByExtension);
            }
            InputEventResult::Transform { text: t, images: i } => (t, i.or(opts.images)),
            InputEventResult::Continue => (text, opts.images),
        };

        if expand_templates {
            current_text = self.expand_skill_command(&current_text);
            let templates = self.prompt_templates();
            if !templates.is_empty() {
                current_text = expand_prompt_template_text(&current_text, &templates);
            }
        }

        if self.is_streaming() {
            let Some(behavior) = opts.streaming_behavior else {
                let err = "Agent is already processing. Specify streaming_behavior ('steer' or 'followUp') to queue the message.".to_owned();
                if let Some(client_id) = opts.client_message_id.as_deref() {
                    let _ = self
                        .with_execution_log_internal(|log| log.mark_input_failed(client_id, &err));
                }
                if let Some(cb) = &opts.on_admitted {
                    cb(Err(err.clone()));
                }
                return Err(err);
            };
            let disposition = match behavior {
                StreamingInputBehavior::Steer => {
                    self.queue_steer_correlated(
                        current_text,
                        current_images,
                        opts.client_message_id.clone(),
                    );
                    PromptDisposition::Steered
                }
                StreamingInputBehavior::FollowUp => {
                    self.queue_follow_up_correlated(
                        current_text,
                        current_images,
                        opts.client_message_id.clone(),
                    );
                    PromptDisposition::FollowedUp
                }
            };
            if let Some(cb) = &opts.on_admitted {
                cb(Ok(disposition));
            }
            // Keep Delivering until the queued message and its correlation are committed.
            return Ok(disposition);
        }

        if !self.has_configured_model() {
            let err = self.no_models_error_message();
            if let Some(client_id) = opts.client_message_id.as_deref() {
                let _ =
                    self.with_execution_log_internal(|log| log.mark_input_failed(client_id, &err));
            }
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        self.start_run_with_admission(
            BeforeAgentStartTrigger::User {
                prompt: current_text,
                images: current_images,
            },
            opts.client_message_id,
            opts.on_admitted,
        )
        .await?;

        Ok(PromptDisposition::Started)
    }

    async fn emit_input_cancellable(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: InputSource,
        behavior: Option<StreamingInputBehavior>,
    ) -> Result<InputEventResult, String> {
        let cancel = CancellationToken::new();
        let id = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            let id = self
                .inner
                .next_input_hook_id
                .fetch_add(1, Ordering::Relaxed);
            self.inner
                .active_input_cancels
                .lock()
                .expect("input cancels lock")
                .insert(id, cancel.clone());
            id
        };
        let session = self.clone();
        tokio::spawn(async move {
            let _guard = ActiveInputGuard {
                session: session.downgrade(),
                id,
            };
            session
                .in_callback(async {
                    let runner = session.extension_runner();
                    tokio::select! { biased;
                        _ = cancel.cancelled() => Err("Session input cancelled".to_owned()),
                        result = runner.emit_input(text, images, source, behavior) => Ok(result),
                    }
                })
                .await
        })
        .await
        .map_err(|_| "Session input task failed".to_owned())?
    }

    fn expand_skill_command(&self, text: &str) -> String {
        if !self.enable_skill_commands() || !text.starts_with("/skill:") {
            return text.to_owned();
        }
        let after_prefix = &text["/skill:".len()..];
        let (skill_name, args) = match after_prefix.find(' ') {
            Some(idx) => (&after_prefix[..idx], after_prefix[idx + 1..].trim()),
            None => (after_prefix, ""),
        };
        let skills = self.skills();
        let Some(skill) = skills.iter().find(|s| s.name == skill_name) else {
            return text.to_owned();
        };
        match std::fs::read_to_string(&skill.file_path) {
            Ok(raw) => {
                let stripped = strip_yaml_frontmatter(&raw).trim();
                let block = format!(
                    "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
                    skill.name, skill.file_path, skill.base_dir, stripped
                );
                if args.is_empty() {
                    block
                } else {
                    format!("{block}\n\n{args}")
                }
            }
            Err(err) => {
                self.extension_runner().emit_error(ExtensionError {
                    extension_path: skill.file_path.clone(),
                    event: "skill_expansion".to_owned(),
                    error: err.to_string(),
                });
                text.to_owned()
            }
        }
    }

    async fn try_execute_extension_command(&self, text: &str) -> Result<bool, String> {
        let without_slash = &text[1..];
        let (cmd_name, args) = match without_slash.find(' ') {
            Some(idx) => (&without_slash[..idx], without_slash[idx + 1..].to_owned()),
            None => (without_slash, String::new()),
        };

        let runner = self.extension_runner();
        let Some(resolved) = runner.get_command(cmd_name) else {
            return Ok(false);
        };
        let ctx = runner.create_command_context();
        if let Err(err) = (resolved.command.handler)(args, ctx).await {
            runner.emit_error(ExtensionError {
                extension_path: format!("command:{cmd_name}"),
                event: "command".to_owned(),
                error: err,
            });
        }
        Ok(true)
    }

    pub async fn steer(
        &self,
        text: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let text = text.into();
        if let Some(stripped) = text.strip_prefix('/') {
            let cmd_name = stripped.split_whitespace().next().unwrap_or_default();
            if self.extension_runner().get_command(cmd_name).is_some() {
                return Err(format!(
                    "Extension command \"/{cmd_name}\" cannot be queued. Use prompt() when not streaming."
                ));
            }
        }
        self.queue_steer(text, images);
        Ok(())
    }

    pub async fn follow_up(
        &self,
        text: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let text = text.into();
        if let Some(stripped) = text.strip_prefix('/') {
            let cmd_name = stripped.split_whitespace().next().unwrap_or_default();
            if self.extension_runner().get_command(cmd_name).is_some() {
                return Err(format!(
                    "Extension command \"/{cmd_name}\" cannot be queued. Use prompt() when not streaming."
                ));
            }
        }
        self.queue_follow_up(text, images);
        Ok(())
    }

    pub fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        self.inner.agent.clear_all_queues();
        let steering = std::mem::take(&mut *self.inner.steering_messages.lock().expect("lock"));
        let follow_up = std::mem::take(&mut *self.inner.follow_up_messages.lock().expect("lock"));
        self.emit_queue_update();
        (steering, follow_up)
    }

    fn queue_steer(&self, text: String, images: Option<Vec<ImageContent>>) {
        self.queue_steer_correlated(text, images, None);
    }

    fn queue_steer_correlated(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        client_id: Option<String>,
    ) {
        let timestamp = self.input_timestamp();
        if let Some(id) = client_id {
            self.inner
                .pending_input_correlations
                .lock()
                .expect("input correlations lock")
                .insert(timestamp, (id, PromptDisposition::Steered));
        }
        self.inner
            .steering_messages
            .lock()
            .expect("lock")
            .push(text.clone());
        self.emit_queue_update();
        let mut blocks = vec![UserContentBlock::Text(TextContent::new(text))];
        if let Some(imgs) = images {
            blocks.extend(imgs.into_iter().map(UserContentBlock::Image));
        }
        self.inner.agent.steer(AgentMessage::User(UserMessage {
            content: UserContent::Blocks(blocks),
            timestamp,
        }));
    }

    fn queue_follow_up(&self, text: String, images: Option<Vec<ImageContent>>) {
        self.queue_follow_up_correlated(text, images, None);
    }

    fn queue_follow_up_correlated(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        client_id: Option<String>,
    ) {
        let timestamp = self.input_timestamp();
        if let Some(id) = client_id {
            self.inner
                .pending_input_correlations
                .lock()
                .expect("input correlations lock")
                .insert(timestamp, (id, PromptDisposition::FollowedUp));
        }
        self.inner
            .follow_up_messages
            .lock()
            .expect("lock")
            .push(text.clone());
        self.emit_queue_update();
        let mut blocks = vec![UserContentBlock::Text(TextContent::new(text))];
        if let Some(imgs) = images {
            blocks.extend(imgs.into_iter().map(UserContentBlock::Image));
        }
        self.inner.agent.follow_up(AgentMessage::User(UserMessage {
            content: UserContent::Blocks(blocks),
            timestamp,
        }));
    }

    pub(super) fn emit_queue_update(&self) {
        let steering = self.inner.steering_messages.lock().expect("lock").clone();
        let follow_up = self.inner.follow_up_messages.lock().expect("lock").clone();
        self.emit(AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        });
    }

    pub fn pending_message_count(&self) -> usize {
        self.inner.steering_messages.lock().expect("lock").len()
            + self.inner.follow_up_messages.lock().expect("lock").len()
    }

    pub async fn send_custom_message(
        &self,
        draft: BeforeAgentStartCustomMessageDraft,
        options: SendMessageOptions,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let custom_msg = CustomMessage {
            custom_type: draft.custom_type,
            content: draft.content,
            display: draft.display,
            details: draft.details,
            timestamp: now_ms(),
        };

        if self.is_streaming() && options.trigger_turn {
            match options.deliver_as.unwrap_or(StreamingInputBehavior::Steer) {
                StreamingInputBehavior::Steer => {
                    self.inner.agent.steer(custom_msg.to_agent_message());
                }
                StreamingInputBehavior::FollowUp => {
                    self.inner.agent.follow_up(custom_msg.to_agent_message());
                }
            }
            return Ok(());
        }

        if options.trigger_turn {
            if self.inner.is_emitting_agent_settled.load(Ordering::SeqCst) {
                let s = self.clone();
                let d = CustomMessage {
                    custom_type: custom_msg.custom_type.clone(),
                    content: custom_msg.content.clone(),
                    display: custom_msg.display,
                    details: custom_msg.details.clone(),
                    timestamp: custom_msg.timestamp,
                };
                self.inner
                    .deferred_settled_actions
                    .lock()
                    .expect("lock")
                    .push(Box::new(move || {
                        Box::pin(async move {
                            let _ = s
                                .start_run(BeforeAgentStartTrigger::Custom { message: d }, None)
                                .await;
                        })
                    }));
                return Ok(());
            }
            return self
                .start_run(
                    BeforeAgentStartTrigger::Custom {
                        message: custom_msg,
                    },
                    None,
                )
                .await;
        }

        if self.is_streaming() {
            // 流式期间严禁立即插入会话树破坏 toolCall/toolResult 配对顺序，暂存至 turn_end / agent_end 再刷入
            self.inner
                .pending_custom_messages
                .lock()
                .expect("lock")
                .push(custom_msg);
        } else {
            self.append_custom_message_now(&custom_msg)?;
        }
        Ok(())
    }

    fn append_custom_message_now(&self, custom_msg: &CustomMessage) -> Result<(), String> {
        let id = self
            .with_session_manager_internal(|sm| {
                sm.append_custom_message_entry(
                    &custom_msg.custom_type,
                    custom_msg.content.clone(),
                    custom_msg.display,
                    custom_msg.details.clone(),
                )
            })
            .map_err(|e| e.to_string())?;
        if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
            self.emit(AgentSessionEvent::EntryAppended { entry });
        }
        self.refresh_finalized_context();
        let agent_msg = custom_msg.to_agent_message();
        self.emit(AgentSessionEvent::MessageStart {
            message: agent_msg.clone(),
        });
        self.emit(AgentSessionEvent::MessageEnd { message: agent_msg });
        Ok(())
    }

    pub(super) fn flush_pending_custom_messages(&self) {
        let pending: Vec<CustomMessage> = self
            .inner
            .pending_custom_messages
            .lock()
            .expect("lock")
            .drain(..)
            .collect();
        for msg in pending {
            if let Err(err) = self.append_custom_message_now(&msg) {
                self.set_fatal_runtime_error(err);
                break;
            }
        }
    }

    /// Run a user command using host-provided operations or an extension replacement.
    pub async fn execute_bash(
        &self,
        command: impl Into<String>,
        mut options: ExecuteBashOptions,
    ) -> Result<BashResult, String> {
        let command = command.into();
        let id;
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            id = self.inner.next_bash_id.fetch_add(1, Ordering::Relaxed);
            let token = options.cancel_token.get_or_insert_default().clone();
            self.inner
                .active_bash_cancels
                .lock()
                .expect("bash cancels lock")
                .insert(id, token);
        }
        let session = self.clone();
        // The worker owns registration through result persistence, even if its caller is dropped.
        tokio::spawn(async move {
            let _active = ActiveBashGuard {
                session: session.downgrade(),
                id,
            };
            session.execute_bash_internal(command, options).await
        })
        .await
        .map_err(|_| "Bash execution task failed".to_owned())?
    }

    async fn execute_bash_internal(
        &self,
        command: String,
        options: ExecuteBashOptions,
    ) -> Result<BashResult, String> {
        self.ensure_external_open()?;
        let session_id = self.session_id();
        let runner = self.extension_runner();
        let token = options.cancel_token.clone().unwrap_or_default();
        let hook = tokio::select! {
            biased;
            _ = token.cancelled() => return Err("Bash execution cancelled".to_owned()),
            result = runner.emit_user_bash(UserBashEvent {
                command: command.clone(),
                cwd: self.inner.cwd.clone(),
                exclude_from_context: options.exclude_from_context,
            }) => result?,
        };
        runner.runtime().check_active()?;
        self.ensure_external_open()?;
        let fallback_executor = options.executor.or_else(|| {
            self.inner
                .default_bash_executor
                .read()
                .expect("bash executor lock")
                .clone()
        });
        let (result, executor) = match hook {
            Some(UserBashEventResult::Result(result)) => (Some(result), None),
            Some(UserBashEventResult::Operations(executor)) => (None, Some(executor)),
            None => (None, fallback_executor),
        };
        let result = match result {
            Some(result) => result,
            None => {
                let exec = executor.ok_or_else(|| {
                    "A bash executor must be supplied by the host or an extension".to_owned()
                })?;
                exec(BashRequest {
                    command: command.clone(),
                    cwd: self.inner.cwd.clone(),
                    env: {
                        let mut env = BTreeMap::new();
                        env.insert("PI_SESSION_ID".into(), self.session_id());
                        if let Some(path) = self.session_file() {
                            env.insert(
                                "PI_SESSION_FILE".into(),
                                path.to_string_lossy().into_owned(),
                            );
                        }
                        if let Some(model) = self.configured_model() {
                            env.insert("PI_PROVIDER".into(), model.provider);
                            env.insert("PI_MODEL".into(), model.id);
                        }
                        env.insert(
                            "PI_REASONING_LEVEL".into(),
                            self.thinking_level().as_str().into(),
                        );
                        env
                    },
                    cancel_token: token,
                    on_chunk: options.on_chunk,
                })
                .await?
            }
        };
        runner.runtime().check_active()?;
        if self.session_id() != session_id {
            return Err("Session changed while bash was running".to_owned());
        }
        self.record_bash_result_internal(BashExecutionMessage {
            command,
            output: result.output.clone(),
            exit_code: result.exit_code,
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path.clone(),
            timestamp: now_ms(),
            exclude_from_context: Some(options.exclude_from_context),
        })?;
        Ok(result)
    }

    pub fn record_bash_result(&self, bash_msg: BashExecutionMessage) -> Result<(), String> {
        self.record_bash_result_checked(bash_msg, true)
    }

    fn record_bash_result_internal(&self, bash_msg: BashExecutionMessage) -> Result<(), String> {
        self.record_bash_result_checked(bash_msg, false)
    }

    fn record_bash_result_checked(
        &self,
        bash_msg: BashExecutionMessage,
        external: bool,
    ) -> Result<(), String> {
        let id = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            if external {
                self.ensure_external_open()?;
            } else {
                self.ensure_runtime_writable()?;
            }
            if self.is_streaming() {
                self.inner
                    .pending_bash_messages
                    .lock()
                    .expect("lock")
                    .push(bash_msg);
                None
            } else {
                Some(
                    self.with_session_manager_internal(|sm| {
                        sm.append_message(bash_msg.to_agent_message())
                    })
                    .map_err(|e| e.to_string())?,
                )
            }
        };
        if let Some(id) = id {
            if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
                self.emit(AgentSessionEvent::EntryAppended { entry });
            }
            self.refresh_finalized_context();
        }
        Ok(())
    }

    fn flush_pending_bash_messages(&self) {
        let pending: Vec<BashExecutionMessage> = self
            .inner
            .pending_bash_messages
            .lock()
            .expect("lock")
            .drain(..)
            .collect();
        if pending.is_empty() {
            return;
        }
        for bash_msg in pending {
            match self
                .with_session_manager_internal(|sm| sm.append_message(bash_msg.to_agent_message()))
            {
                Ok(id) => {
                    if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned())
                    {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to persist deferred bash result: {err}"
                    ));
                    break;
                }
            }
        }
        self.refresh_finalized_context();
    }

    pub(super) fn prepare_prompt_and_tool_loadout(
        &self,
        options: &mut NormalizedBuildSystemPromptOptions,
        messages: &[AgentMessage],
    ) -> Option<SystemMessage> {
        let active = self.apply_tool_loadout(&options.selected_tools);
        options.selected_tools = active.into_iter().map(|t| t.name).collect();
        options.hidden_tools = self
            .inner
            .hidden_declarations
            .read()
            .expect("hidden lock")
            .iter()
            .cloned()
            .collect();

        let current_sections = get_agent_current_system_message(messages)
            .and_then(|s| s.sections)
            .unwrap_or_default();
        let target_sections = build_system_prompt_sections(&options.to_build_options()).ok()?;
        let patch = diff_system_prompt_sections(&current_sections, &target_sections)?;
        Some(SystemMessage {
            content: SystemContent::Text(String::new()),
            sections: Some(patch),
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        })
    }

    async fn prepare_run(
        &self,
        trigger: BeforeAgentStartTrigger,
    ) -> Result<Vec<AgentMessage>, String> {
        if let Some(last_asst) = self.find_last_assistant_message() {
            let _ = self.check_compaction(&last_asst, false).await;
        }

        let base_opts = self
            .inner
            .base_system_prompt_options
            .read()
            .expect("lock")
            .to_build_options();
        let selected_before = base_opts.selected_tools.clone().unwrap_or_default();
        let mut combined = self
            .extension_runner()
            .emit_before_agent_start(trigger.clone(), &base_opts)
            .await;

        if combined.system_prompt_options.selected_tools == selected_before {
            combined.system_prompt_options.selected_tools = self.get_active_tool_names();
        }

        let mut messages: Vec<AgentMessage> = Vec::new();
        match trigger {
            BeforeAgentStartTrigger::Custom { message } => {
                messages.push(message.to_agent_message());
            }
            BeforeAgentStartTrigger::User { prompt, images } => {
                let mut blocks = vec![UserContentBlock::Text(TextContent::new(prompt))];
                if let Some(imgs) = images {
                    blocks.extend(imgs.into_iter().map(UserContentBlock::Image));
                }
                messages.push(AgentMessage::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    timestamp: self.input_timestamp(),
                }));
            }
        }

        let next_turn_msgs: Vec<CustomMessage> = self
            .inner
            .pending_next_turn_messages
            .lock()
            .expect("lock")
            .drain(..)
            .collect();
        for m in next_turn_msgs {
            messages.push(m.to_agent_message());
        }

        for draft in combined.messages {
            let cm = CustomMessage {
                custom_type: draft.custom_type,
                content: draft.content,
                display: draft.display,
                details: draft.details,
                timestamp: now_ms(),
            };
            messages.push(cm.to_agent_message());
        }

        let state_msgs = self.inner.agent.state().messages;
        let sys_patch =
            self.prepare_prompt_and_tool_loadout(&mut combined.system_prompt_options, &state_msgs);
        *self.inner.run_system_prompt_options.write().expect("lock") =
            Some(combined.system_prompt_options);

        if let Some(sys) = sys_patch {
            messages.insert(0, AgentMessage::System(sys));
        }

        Ok(messages)
    }

    async fn start_run(
        &self,
        trigger: BeforeAgentStartTrigger,
        client_message_id: Option<String>,
    ) -> Result<(), String> {
        self.start_run_with_admission(trigger, client_message_id, None)
            .await
    }

    /// Resume a runnable transcript without appending a synthetic user message.
    pub async fn continue_run(&self) -> Result<(), String> {
        self.ensure_external_open()?;
        if !self.has_configured_model() {
            return Err(self.no_models_error_message());
        }
        if self.is_compacting() {
            return Err("Cannot continue while compaction is in progress".to_owned());
        }
        self.start_session_run(None, None, None).await
    }

    async fn start_run_with_admission(
        &self,
        trigger: BeforeAgentStartTrigger,
        client_message_id: Option<String>,
        on_admitted: Option<PromptAdmittedCallback>,
    ) -> Result<(), String> {
        self.start_session_run(Some(trigger), client_message_id, on_admitted)
            .await
    }

    async fn start_session_run(
        &self,
        trigger: Option<BeforeAgentStartTrigger>,
        client_message_id: Option<String>,
        mut on_admitted: Option<PromptAdmittedCallback>,
    ) -> Result<(), String> {
        let cancel = CancellationToken::new();
        let admission = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open().and_then(|()| {
                if self
                    .inner
                    .reload_cancel
                    .lock()
                    .expect("reload cancel lock")
                    .is_some()
                {
                    return Err("Session reload in progress".to_owned());
                }
                if self.inner.is_agent_run_active.swap(true, Ordering::SeqCst) {
                    return Err("AgentSession already has an active run".to_owned());
                }
                self.inner.active_run_workers.fetch_add(1, Ordering::SeqCst);
                self.inner
                    .agent_run_abort_requested
                    .store(false, Ordering::SeqCst);
                *self.inner.run_cancel.lock().expect("run cancel lock") = Some(cancel.clone());
                Ok(())
            })
        };
        if let Err(err) = admission {
            if let Some(cb) = on_admitted.take() {
                cb(Err(err.clone()));
            }
            return Err(err);
        }
        let session = self.clone();
        // The worker owns transcript and execution-log finalization independently of the waiter.
        tokio::spawn(async move {
            let _active = ActiveRunGuard {
                session: session.downgrade(),
            };
            let id = Arc::as_ptr(&session.inner) as usize;
            RUN_SESSION
                .scope(
                    id,
                    session.run_admitted(trigger, client_message_id, on_admitted, cancel),
                )
                .await
        })
        .await
        .map_err(|_| "Session run task failed".to_owned())?
    }

    async fn run_admitted(
        &self,
        trigger: Option<BeforeAgentStartTrigger>,
        client_message_id: Option<String>,
        mut on_admitted: Option<PromptAdmittedCallback>,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let continuation = trigger.is_none();
        let prepared = if let Some(trigger) = trigger {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err("Session run cancelled".to_owned()),
                result = self.prepare_run(trigger) => result,
            }
        } else {
            Ok(Vec::new())
        };
        let messages = match prepared {
            Ok(msgs) => msgs,
            Err(err) => {
                if let Some(cid) = client_message_id.as_deref() {
                    let _ =
                        self.with_execution_log_internal(|log| log.mark_input_failed(cid, &err));
                }
                if let Some(cb) = on_admitted.take() {
                    cb(Err(err.clone()));
                }
                self.inner
                    .is_agent_run_active
                    .store(false, Ordering::SeqCst);
                self.flush_pending_bash_messages();
                self.flush_pending_custom_messages();
                self.inner.idle_notify.notify_waiters();
                return Err(err);
            }
        };

        self.flush_pending_bash_messages();
        self.flush_pending_custom_messages();
        *self.inner.failed_response.lock().expect("lock") = None;

        let run_seq = self.inner.next_run_seq.fetch_add(1, Ordering::Relaxed);
        if let Err(err) = self.with_execution_log_internal(|log| log.start_run(run_seq)) {
            let err_str = err.to_string();
            if let Some(cb) = on_admitted.take() {
                cb(Err(err_str.clone()));
            }
            self.inner
                .is_agent_run_active
                .store(false, Ordering::SeqCst);
            self.inner.idle_notify.notify_waiters();
            return Err(err_str);
        }

        if let Some(msg) = messages.iter().find(|m| m.role() == "user") {
            let ts = msg.timestamp();
            if let Some(cid) = client_message_id {
                self.inner
                    .pending_input_correlations
                    .lock()
                    .expect("input correlations lock")
                    .insert(ts, (cid, PromptDisposition::Started));
            }
            if let Some(cb) = on_admitted.take() {
                self.inner
                    .pending_admission_callbacks
                    .lock()
                    .expect("admission lock")
                    .insert(ts, cb);
            }
        } else if let Some(cb) = on_admitted.take() {
            cb(Ok(PromptDisposition::Started));
        }

        let mut run_result = if cancel.is_cancelled() {
            Err("Session run cancelled".to_owned())
        } else if continuation {
            self.inner.agent.continue_run().await
        } else {
            self.inner.agent.prompt(messages).await
        };

        while run_result.is_ok()
            && !self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            && self.fatal_runtime_error().is_none()
        {
            if self.handle_post_agent_run().await {
                if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                    || self.fatal_runtime_error().is_some()
                {
                    break;
                }
                run_result = self.inner.agent.continue_run().await;
                continue;
            }
            if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                || self.fatal_runtime_error().is_some()
                || !self.run_before_settle_boundary().await
            {
                break;
            }
            if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                || self.fatal_runtime_error().is_some()
            {
                break;
            }
            run_result = self.inner.agent.continue_run().await;
        }

        let aborted = self.inner.agent_run_abort_requested.load(Ordering::SeqCst);
        if aborted {
            self.finish_cancelled_retry();
        }
        if self.fatal_runtime_error().is_none() {
            let err_for_log = run_result.as_ref().err().cloned();
            if let Err(err) =
                self.with_execution_log_internal(|log| log.finish_run(aborted, err_for_log))
            {
                self.set_fatal_runtime_error(format!("Failed to persist run completion: {err}"));
            }
        }

        *self.inner.failed_response.lock().expect("lock") = None;
        *self.inner.run_system_prompt_options.write().expect("lock") = None;
        self.flush_pending_bash_messages();
        self.flush_pending_custom_messages();
        self.emit_agent_settled(aborted).await;

        match self.fatal_runtime_error() {
            Some(err) => Err(err),
            None => run_result,
        }
    }

    async fn run_before_settle_boundary(&self) -> bool {
        let state_msgs = self.inner.agent.state().messages;
        let boundary = self
            .extension_runner()
            .emit_boundary(
                BoundaryEventKind::AgentBeforeSettle {
                    messages: state_msgs,
                },
                |drafts| self.build_boundary_context_preview(drafts, false),
            )
            .await;

        if !boundary.entries.is_empty() {
            match self
                .with_session_manager_internal(|sm| sm.commit_boundary_drafts(&boundary.entries))
            {
                Ok(appended) => {
                    self.refresh_finalized_context();
                    for entry in appended {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to commit agent_before_settle boundary drafts: {err}"
                    ));
                    return false;
                }
            }
        }

        self.flush_pending_custom_messages();
        let should_continue = boundary.r#continue || self.inner.agent.has_queued_messages();
        if should_continue {
            match self.build_boundary_context_preview(&[], false) {
                Ok(preview) if preview.can_continue => return true,
                _ => {
                    if boundary.r#continue {
                        self.extension_runner().emit_error(ExtensionError {
                            extension_path: "<boundary>".to_owned(),
                            event: "agent_before_settle".to_owned(),
                            error: "agent_before_settle requested continuation without runnable model context".to_owned(),
                        });
                    }
                    return false;
                }
            }
        }
        false
    }

    async fn emit_agent_settled(&self, aborted: bool) {
        self.inner
            .is_agent_run_active
            .store(false, Ordering::SeqCst);
        self.inner
            .is_emitting_agent_settled
            .store(true, Ordering::SeqCst);
        let on_settled = self
            .inner
            .on_agent_settled
            .read()
            .expect("on settled lock")
            .clone();
        if let Some(cb) = on_settled {
            cb();
        }
        let state_msgs = self.inner.agent.state().messages;
        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::AgentSettled {
                messages: state_msgs,
            })
            .await;
        self.emit_async(AgentSessionEvent::AgentSettled { aborted })
            .await;
        self.inner
            .is_emitting_agent_settled
            .store(false, Ordering::SeqCst);

        let deferred: Vec<DeferredSettledAction> = self
            .inner
            .deferred_settled_actions
            .lock()
            .expect("deferred lock")
            .drain(..)
            .collect();
        for action in deferred {
            action().await;
        }
        self.inner.idle_notify.notify_waiters();
    }
}
