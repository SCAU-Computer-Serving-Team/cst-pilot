use super::*;

impl AgentSession {
    // =========================================================================
    // Tree Navigation, Fork, NewSession, SwitchSession & ContextUsage
    // =========================================================================

    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, String> {
        self.navigate_tree_admitted(target_id, options, None).await
    }

    /// Select an exact saved leaf for search without stopping a run or rewinding a user entry.
    /// `expected_leaf` is the leaf observed before the request, including an empty branch.
    pub async fn navigate_tree_to_leaf(
        &self,
        target_id: &str,
        expected_leaf: Option<&str>,
    ) -> Result<NavigateTreeResult, String> {
        self.navigate_tree_admitted(
            target_id,
            NavigateTreeOptions::default(),
            Some(expected_leaf),
        )
        .await
    }

    async fn navigate_tree_admitted(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
        expected_leaf: Option<Option<&str>>,
    ) -> Result<NavigateTreeResult, String> {
        let cancel = CancellationToken::new();
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            if self.is_streaming() {
                return Err(
                    "Wait for the current response to finish before navigating the session tree."
                        .to_owned(),
                );
            }
            if self.is_compacting() {
                return Err("Wait for the current compaction or tree navigation to finish before navigating the session tree.".to_owned());
            }
            if let Some(expected) = expected_leaf {
                if !self.is_idle() {
                    return Err("会话正在执行".to_owned());
                }
                if !self.with_session_manager(|sm| sm.get_leaf_id() == expected) {
                    return Err("当前分支已变更，请重新定位".to_owned());
                }
            }
            *self.inner.branch_summary_cancel.lock().expect("lock") = Some(cancel.clone());
        }
        let session = self.clone();
        let target_id = target_id.to_owned();
        let expected_leaf = expected_leaf.map(|leaf| leaf.map(str::to_owned));
        tokio::spawn(async move {
            let _active = ActiveAuxiliaryGuard {
                session: session.downgrade(),
                compaction: false,
            };
            RUN_SESSION
                .scope(
                    Arc::as_ptr(&session.inner) as usize,
                    session.navigate_tree_internal(&target_id, options, cancel, expected_leaf),
                )
                .await
        })
        .await
        .map_err(|_| "Session tree navigation task failed".to_owned())?
    }

    async fn navigate_tree_internal(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
        cancel: CancellationToken,
        expected_leaf: Option<Option<String>>,
    ) -> Result<NavigateTreeResult, String> {
        let old_leaf_id = self.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned));
        let exact_leaf = expected_leaf.is_some();
        if expected_leaf
            .as_ref()
            .is_some_and(|expected| expected != &old_leaf_id)
        {
            return Err("当前分支已变更，请重新定位".to_owned());
        }
        let target_entry = self
            .with_session_manager(|sm| sm.get_entry(target_id).cloned())
            .ok_or_else(|| format!("Entry {target_id} not found"))?;
        // Editing a user/custom message at the current leaf still rewinds to its parent.
        let rewinds_to_parent = !exact_leaf
            && (matches!(&target_entry, SessionEntry::Message(m) if m.message.role() == "user")
                || matches!(&target_entry, SessionEntry::CustomMessage(_)));
        if old_leaf_id.as_deref() == Some(target_id) && !rewinds_to_parent {
            return Ok(NavigateTreeResult::default());
        }

        let collected = self.with_session_manager(|sm| {
            collect_entries_for_branch_summary(sm, old_leaf_id.as_deref(), Some(target_id))
        });

        let mut custom_instructions = options.custom_instructions.clone();
        let mut replace_instructions = options.replace_instructions;
        let mut label = options.label.clone();

        let prep = TreePreparation {
            target_id: target_id.to_owned(),
            old_leaf_id: old_leaf_id.clone(),
            common_ancestor_id: collected.common_ancestor_id.clone(),
            entries_to_summarize: collected.entries.clone(),
            user_wants_summary: options.summarize,
            custom_instructions: custom_instructions.clone(),
            replace_instructions,
            label: label.clone(),
        };

        let mut ext_summary: Option<BranchSummaryResult> = None;
        let mut from_extension = false;

        let runner = self.extension_runner();
        let before = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(NavigateTreeResult { cancelled: true, aborted: true, ..Default::default() }),
            result = runner.emit_lifecycle(SessionLifecycleEvent::SessionBeforeTree { preparation: prep }) => result,
        };
        if let SessionBeforeEventOutput::Tree(before_res) = before {
            if before_res.cancel {
                return Ok(NavigateTreeResult {
                    cancelled: true,
                    ..NavigateTreeResult::default()
                });
            }
            if before_res.summary.is_some() && options.summarize {
                ext_summary = before_res.summary;
                from_extension = true;
            }
            if before_res.custom_instructions.is_some() {
                custom_instructions = before_res.custom_instructions;
            }
            if let Some(rep) = before_res.replace_instructions {
                replace_instructions = rep;
            }
            if before_res.label.is_some() {
                label = before_res.label;
            }
        }

        let mut summary_text: Option<String> = None;
        let mut summary_details: Option<JsonValue> = None;
        let mut summary_usage: Option<Usage> = None;

        if options.summarize && !collected.entries.is_empty() && ext_summary.is_none() {
            if !self.has_configured_model() {
                return Err(self.no_models_error_message());
            }
            let mut gen_opts = GenerateBranchSummaryOptions::new(self.limits_model());
            gen_opts.cancel_token = Some(cancel.clone());
            gen_opts.custom_instructions = custom_instructions;
            gen_opts.replace_instructions = replace_instructions;
            gen_opts.stream_fn = Some(self.inner.agent.stream_fn());
            gen_opts.retry = Some(self.retry_policy());
            gen_opts.block_images = self.is_block_images_enabled();

            let gen_res = generate_branch_summary(&collected.entries, &gen_opts).await;
            if gen_res.aborted == Some(true) {
                return Ok(NavigateTreeResult {
                    cancelled: true,
                    aborted: true,
                    ..NavigateTreeResult::default()
                });
            }
            if let Some(err) = gen_res.error {
                return Err(err);
            }
            summary_text = gen_res.summary;
            summary_usage = gen_res.usage;
            summary_details = serde_json::to_value(BranchSummaryDetails {
                read_files: gen_res.read_files.unwrap_or_default(),
                modified_files: gen_res.modified_files.unwrap_or_default(),
            })
            .ok();
        } else if let Some(ext_s) = ext_summary {
            summary_text = ext_s.summary;
            summary_usage = ext_s.usage;
        }

        if cancel.is_cancelled() {
            return Ok(NavigateTreeResult {
                cancelled: true,
                aborted: true,
                ..Default::default()
            });
        }

        // 若目标节点为 user 或 custom_message，将叶节点移至其 parent_id 并回填编辑器文本
        let (new_leaf_id, editor_text): (Option<String>, Option<String>) = match &target_entry {
            _ if exact_leaf => (Some(target_id.to_owned()), None),
            SessionEntry::Message(m) if m.message.role() == "user" => {
                let text = match &m.message {
                    AgentMessage::User(u) => match &u.content {
                        UserContent::Text(s) => s.clone(),
                        UserContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| match b {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(""),
                    },
                    _ => String::new(),
                };
                (m.parent_id.clone(), Some(text))
            }
            SessionEntry::CustomMessage(cm) => {
                (cm.parent_id.clone(), Some(cm.content.text_summary()))
            }
            _ => (Some(target_id.to_owned()), None),
        };

        let mut summary_entry: Option<BranchSummaryEntry> = None;
        if let Some(text) = &summary_text {
            let sid = self
                .with_session_manager_internal(|sm| {
                    sm.branch_with_summary(
                        new_leaf_id.as_deref(),
                        text.clone(),
                        summary_details,
                        Some(from_extension),
                        summary_usage,
                    )
                })
                .map_err(|e| e.to_string())?;
            if let Some(lbl) = &label {
                self.with_session_manager_internal(|sm| {
                    sm.append_label_change(&sid, Some(lbl.clone()))
                })
                .map_err(|e| e.to_string())?;
            }
            if let Some(SessionEntry::BranchSummary(bs)) =
                self.with_session_manager(|sm| sm.get_entry(&sid).cloned())
            {
                summary_entry = Some(bs);
            }
        } else if let Some(nl) = &new_leaf_id {
            self.with_session_manager_internal(|sm| {
                if expected_leaf
                    .as_ref()
                    .is_some_and(|expected| sm.get_leaf_id() != expected.as_deref())
                {
                    return Err(SessionStoreError::InvalidOperation(
                        "当前分支已变更，请重新定位".to_owned(),
                    ));
                }
                sm.branch(nl)
            })
            .map_err(|e| e.to_string())?;
        } else {
            self.with_session_manager_internal(SessionManager::reset_leaf);
        }

        if summary_text.is_none()
            && let Some(lbl) = label
        {
            self.with_session_manager_internal(|sm| sm.append_label_change(target_id, Some(lbl)))
                .map_err(|e| e.to_string())?;
        }

        self.refresh_finalized_context();
        self.restore_tools_from_transcript();

        let current_leaf = self.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned));
        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionTree {
                new_leaf_id: current_leaf,
                old_leaf_id,
                from_extension: summary_text.as_ref().map(|_| from_extension),
            })
            .await;

        Ok(NavigateTreeResult {
            editor_text,
            cancelled: false,
            aborted: false,
            summary_entry,
        })
    }

    /// 从指定节点派生新会话（同时使旧 `ExtensionContext` 失效并重新绑定新会话上下文）。
    pub async fn fork(&self, entry_id: &str, options: ForkSessionOptions) -> Result<bool, String> {
        self.ensure_external_open()?;
        let position = options
            .position
            .clone()
            .unwrap_or_else(|| "before".to_owned());

        if let SessionBeforeEventOutput::Fork(before_res) = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionBeforeFork {
                entry_id: entry_id.to_owned(),
                position: position.clone(),
            })
            .await
            && before_res.cancel
        {
            return Ok(false);
        }

        self.abort().await;

        let target_leaf = if position == "at" {
            let _ = self
                .with_session_manager(|sm| sm.get_entry(entry_id).cloned())
                .ok_or_else(|| format!("Entry {entry_id} not found"))?;
            Some(entry_id.to_owned())
        } else {
            let entry = self
                .with_session_manager(|sm| sm.get_entry(entry_id).cloned())
                .ok_or_else(|| format!("Entry {entry_id} not found"))?;
            entry.parent_id().map(ToOwned::to_owned)
        };

        let prev_file = self.session_file();
        if let Some(leaf) = &target_leaf {
            self.with_session_manager_internal(|sm| sm.create_branched_session(leaf))
                .map_err(|e| e.to_string())?;
        } else {
            let cwd = self.inner.cwd.clone();
            let dir = self.with_session_manager(|sm| sm.get_session_dir().to_path_buf());
            let new_sm = if self.with_session_manager(SessionManager::is_persisted) {
                SessionManager::create(
                    cwd,
                    dir,
                    Some(NewSessionOptions {
                        id: None,
                        parent_session: prev_file
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned()),
                    }),
                )
                .map_err(|e| e.to_string())?
            } else {
                SessionManager::in_memory(cwd, None, None).map_err(|e| e.to_string())?
            };
            *self.inner.session_manager.lock().expect("sm lock") = new_sm;
        }

        self.rotate_extension_runner_after_session_replacement("fork", prev_file)
            .await?;
        Ok(true)
    }

    pub async fn new_session(&self, parent_session: Option<String>) -> Result<bool, String> {
        self.ensure_external_open()?;
        if let SessionBeforeEventOutput::Switch(before_res) = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionBeforeSwitch {
                reason: "new".to_owned(),
                target_session_file: None,
            })
            .await
            && before_res.cancel
        {
            return Ok(false);
        }

        self.abort().await;

        let prev_file = self.session_file();
        let cwd = self.inner.cwd.clone();
        let (is_persisted, dir) =
            self.with_session_manager(|sm| (sm.is_persisted(), sm.get_session_dir().to_path_buf()));
        let new_sm = if is_persisted {
            SessionManager::create(
                cwd,
                dir,
                Some(NewSessionOptions {
                    id: None,
                    parent_session,
                }),
            )
            .map_err(|e| e.to_string())?
        } else {
            SessionManager::in_memory(cwd, None, None).map_err(|e| e.to_string())?
        };
        *self.inner.session_manager.lock().expect("sm lock") = new_sm;

        self.rotate_extension_runner_after_session_replacement("new", prev_file)
            .await?;
        Ok(true)
    }

    pub async fn switch_session(&self, session_path: impl Into<PathBuf>) -> Result<bool, String> {
        self.ensure_external_open()?;
        let target_path = session_path.into();

        if let SessionBeforeEventOutput::Switch(before_res) = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionBeforeSwitch {
                reason: "resume".to_owned(),
                target_session_file: Some(target_path.clone()),
            })
            .await
            && before_res.cancel
        {
            return Ok(false);
        }

        self.abort().await;

        let prev_file = self.session_file();
        let dir = self.with_session_manager(|sm| sm.get_session_dir().to_path_buf());
        let new_sm = SessionManager::open(&target_path, Some(dir), Some(self.inner.cwd.clone()))
            .map_err(|e| e.to_string())?;
        *self.inner.session_manager.lock().expect("sm lock") = new_sm;

        self.rotate_extension_runner_after_session_replacement("resume", prev_file)
            .await?;
        Ok(true)
    }

    /// Reload resources through the host's repeatable factories.
    pub async fn reload_resources(&self) -> Result<(), String> {
        self.start_reload(None).await
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.reload_resources().await
    }

    pub async fn reload_with_extensions(
        &self,
        extensions: Vec<Box<dyn Extension>>,
    ) -> Result<(), String> {
        self.start_reload(Some(extensions)).await
    }

    async fn start_reload(
        &self,
        extensions: Option<Vec<Box<dyn Extension>>>,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let id = Arc::as_ptr(&self.inner) as usize;
        if RUN_SESSION
            .try_with(|current| *current == id)
            .unwrap_or(false)
            || SHUTDOWN_SESSION
                .try_with(|current| *current == id)
                .unwrap_or(false)
        {
            return Err("Cannot await reload from its own session callback".to_owned());
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
            let mut slot = self.inner.reload_cancel.lock().expect("reload cancel lock");
            if slot.is_some() {
                return Err("Session reload in progress".to_owned());
            }
            *slot = Some(cancel.clone());
        }
        let session = self.clone();
        tokio::spawn(async move {
            let _active = ActiveReloadGuard {
                session: session.downgrade(),
            };
            RUN_SESSION
                .scope(id, session.reload_internal(extensions, cancel))
                .await
        })
        .await
        .map_err(|_| "Session reload task failed".to_owned())?
    }

    async fn reload_internal(
        &self,
        extensions: Option<Vec<Box<dyn Extension>>>,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let hook = self.inner.on_reload.read().expect("on reload lock").clone();
        let outcome = if let Some(extensions) = extensions {
            SessionReloadOutcome {
                extension_runner: ExtensionRunner::from_extensions(
                    self.inner.cwd.clone(),
                    extensions,
                )
                .map_err(|e| e.to_string())?,
                on_committed: None,
                skills: self.skills(),
                prompt_templates: self.prompt_templates(),
                enable_skill_commands: None,
                system_prompt_options: None,
            }
        } else if let Some(hook) = hook {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("Session reload cancelled".to_owned()),
                result = hook() => result?,
            }
        } else {
            SessionReloadOutcome {
                extension_runner: self.extension_runner().clone_with_fresh_generation(),
                on_committed: None,
                skills: self.skills(),
                prompt_templates: self.prompt_templates(),
                enable_skill_commands: None,
                system_prompt_options: None,
            }
        };
        self.ensure_external_open()?;
        if cancel.is_cancelled() {
            return Err("Session reload cancelled".to_owned());
        }
        self.swap_extension_runner(outcome).await
    }

    async fn swap_extension_runner(&self, outcome: SessionReloadOutcome) -> Result<(), String> {
        let on_committed = outcome.on_committed;
        let before_invalidate = self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock")
            .clone();
        if let Some(callback) = before_invalidate {
            callback();
        }
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            self.set_skills(outcome.skills);
            self.set_prompt_templates(outcome.prompt_templates);
            if let Some(enable) = outcome.enable_skill_commands {
                self.set_enable_skill_commands(enable);
            }
            if let Some(options) = outcome.system_prompt_options {
                self.set_base_system_prompt_options(options);
            }
            let new_runner = outcome.extension_runner;
            let old_runner = self.extension_runner();
            new_runner.inherit_ui_from(&old_runner);
            old_runner.invalidate(None);
            old_runner.detach_models();
            new_runner.bind_models(self.inner.models.clone());
            *self.inner.extension_runner.write().expect("runner lock") = new_runner;
            self.inner.session_generation.fetch_add(1, Ordering::SeqCst);
            self.inner
                .selection_generation
                .fetch_add(1, Ordering::SeqCst);
            self.bind_extension_runner();
            self.refresh_tool_registry(None);
            self.restore_tools_from_transcript();
            self.refresh_finalized_context();
        }
        if let Some(on_committed) = on_committed {
            on_committed(&self.extension_runner());
        }
        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionStart {
                reason: "reload".to_owned(),
                previous_session_file: self.session_file(),
            })
            .await;
        Ok(())
    }

    async fn rotate_extension_runner_after_session_replacement(
        &self,
        reason: &str,
        previous_session_file: Option<PathBuf>,
    ) -> Result<(), String> {
        // Invalidate captured handles before opening or reconciling the target log.
        self.extension_runner().invalidate(None);
        let log = self
            .with_session_manager(|sm| {
                self.inner
                    .execution_log_factory
                    .as_ref()
                    .map_or_else(|| Self::default_execution_log(sm), |factory| factory(sm))
            })
            .map_err(|err| {
                let msg = format!("Failed to open replacement session execution log: {err}");
                self.set_fatal_runtime_error(msg.clone());
                msg
            })?;
        *self.inner.execution_log.lock().expect("execution log lock") = log;
        let report = self.reconcile_startup().map_err(|err| {
            let msg = format!("Failed to reconcile replacement session: {err}");
            self.set_fatal_runtime_error(msg.clone());
            msg
        })?;
        *self
            .inner
            .startup_report
            .lock()
            .expect("startup report lock") = report;
        *self.inner.fatal_runtime_error.lock().expect("fatal lock") = None;
        *self
            .inner
            .last_assistant_message
            .lock()
            .expect("assistant lock") = None;
        *self
            .inner
            .failed_response
            .lock()
            .expect("failed response lock") = None;
        self.inner
            .pending_input_correlations
            .lock()
            .expect("input correlations lock")
            .clear();
        self.inner
            .pending_next_turn_messages
            .lock()
            .expect("next turn lock")
            .clear();
        self.inner
            .overflow_recovery_attempted
            .store(false, Ordering::SeqCst);
        self.inner.retry_attempt.store(0, Ordering::SeqCst);
        if let Some(runner) = self.inner.nested_runner.get() {
            runner.clear();
        }
        let (projection, branch) =
            self.with_session_manager(|sm| (sm.build_session_projection(), sm.get_branch(None)));
        if let Some(selection) =
            get_branch_selection(&branch, |p, m| self.inner.models.get_model(p, m))
            && let Some(model) = self
                .inner
                .models
                .get_model(&selection.provider, &selection.model_id)
        {
            self.inner.agent.set_model(model);
        }
        if let Some(level) = parse_model_thinking_level(&projection.thinking_level) {
            self.inner.agent.set_thinking_level(level);
        }
        // 使先前被捕获的旧 ExtensionContext / ExtensionRuntime 立即失效！
        let old_runner = self.extension_runner();
        old_runner.invalidate(None);

        let new_runner = old_runner.clone_with_fresh_generation();
        *self.inner.extension_runner.write().expect("runner lock") = new_runner;
        self.inner.session_generation.fetch_add(1, Ordering::SeqCst);
        self.inner
            .selection_generation
            .fetch_add(1, Ordering::SeqCst);
        self.bind_extension_runner();
        self.refresh_tool_registry(None);
        self.restore_tools_from_transcript();
        self.refresh_finalized_context();
        self.inner.agent.set_session_id(Some(self.session_id()));

        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionStart {
                reason: reason.to_owned(),
                previous_session_file,
            })
            .await;
        Ok(())
    }

    /// 获取当前上下文用量（刚完成压缩且尚无压缩后新助手回复时，`tokens` 与 `percent` 返回 `None`）。
    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        self.with_context_branch(|_, _, usage| usage)
    }

    /// Read the leaf, branch and capacity estimate from one session-manager snapshot.
    pub fn with_context_branch<R>(
        &self,
        read: impl FnOnce(Option<&str>, Vec<SessionEntry>, Option<ContextUsage>) -> R,
    ) -> R {
        let context_window = if self.has_configured_model() {
            self.limits_model().context_window
        } else {
            0
        };
        let (leaf, branch, projection) = self.with_session_manager(|sm| {
            (
                sm.get_leaf_id().map(str::to_owned),
                sm.get_branch(None),
                (context_window > 0).then(|| sm.build_session_projection()),
            )
        });
        let context_usage = projection.as_ref().and_then(|projection| {
            crate::compaction::context_usage_for_branch(projection, &branch, context_window)
        });
        read(leaf.as_deref(), branch, context_usage)
    }

    pub fn get_session_stats(&self) -> SessionStats {
        let entries = self.with_session_manager(SessionManager::get_entries);
        let mut user_messages = 0usize;
        let mut assistant_messages = 0usize;
        let mut tool_results = 0usize;
        let mut total_messages = 0usize;
        let mut tool_calls = 0usize;
        let mut totals = create_usage_totals();

        for entry in &entries {
            match entry {
                SessionEntry::Usage(u) => add_usage_to_totals(&mut totals, &u.usage),
                SessionEntry::BranchSummary(bs) => {
                    if let Some(u) = &bs.usage {
                        add_usage_to_totals(&mut totals, u);
                    }
                }
                SessionEntry::Compaction(c) => {
                    if let Some(u) = &c.usage {
                        add_usage_to_totals(&mut totals, u);
                    }
                }
                SessionEntry::Message(m) => {
                    total_messages += 1;
                    match &m.message {
                        AgentMessage::User(_) => user_messages += 1,
                        AgentMessage::ToolResult(tr) => {
                            tool_results += 1;
                            if let Some(u) = &tr.usage {
                                add_usage_to_totals(&mut totals, u);
                            }
                        }
                        AgentMessage::Assistant(a) => {
                            assistant_messages += 1;
                            tool_calls += a
                                .content
                                .iter()
                                .filter(|b| matches!(b, ContentBlock::ToolCall(_)))
                                .count();
                            add_usage_to_totals(&mut totals, &a.usage);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        SessionStats {
            session_file: self.session_file(),
            session_id: self.session_id(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages,
            input_tokens: totals.input,
            output_tokens: totals.output,
            cache_read_tokens: totals.cache_read,
            cache_write_tokens: totals.cache_write,
            total_tokens: totals.input + totals.output + totals.cache_read + totals.cache_write,
            cost: totals.cost,
            context_usage: self.get_context_usage(),
        }
    }

    pub fn get_slash_commands(&self) -> Vec<SlashCommandInfo> {
        let mut list: Vec<SlashCommandInfo> = self
            .extension_runner()
            .get_registered_commands()
            .into_iter()
            .map(|rc| SlashCommandInfo {
                name: rc.invocation_name,
                description: rc.command.description,
                source: SlashCommandSource::Extension,
                source_info: rc.command.source_info,
            })
            .collect();
        for tpl in self.prompt_templates() {
            list.push(SlashCommandInfo {
                name: tpl.name,
                description: Some(tpl.description),
                source: SlashCommandSource::Prompt,
                source_info: tpl.source_info,
            });
        }
        if self.enable_skill_commands() {
            for skill in self.skills() {
                list.push(SlashCommandInfo {
                    name: format!("skill:{}", skill.name),
                    description: Some(skill.description),
                    source: SlashCommandSource::Skill,
                    source_info: skill.source_info,
                });
            }
        }
        for b in BUILTIN_SLASH_COMMANDS {
            list.push(SlashCommandInfo {
                name: b.name.to_owned(),
                description: Some(b.description.to_owned()),
                source: SlashCommandSource::Prompt,
                source_info: SourceInfo::builtin(b.name),
            });
        }
        list
    }

    pub fn export_to_jsonl(
        &self,
        output_path: Option<&Path>,
    ) -> Result<PathBuf, SessionStoreError> {
        self.with_session_manager(|sm| export_session_to_jsonl(sm, output_path, None))
    }
}
