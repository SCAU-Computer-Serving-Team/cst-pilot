use super::*;

impl AgentSession {
    pub(super) fn bind_extension_runner(&self) {
        let runner = self.extension_runner();
        let w = self.downgrade();
        let s1 = w.clone();
        let s2 = w.clone();
        let s3 = w.clone();
        let s4 = w.clone();
        let s5 = w.clone();
        let s6 = w.clone();
        let s7 = w.clone();
        let s8 = w.clone();
        let s9 = w.clone();
        let s10 = w.clone();
        let s11 = w.clone();
        let s12 = w.clone();
        let s13 = w.clone();
        let s14 = w.clone();
        let requested_session = w.clone();

        let actions = ExtensionActions {
            send_message: Arc::new(move |draft, opts| {
                let s = s1.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                tokio::spawn(async move {
                    let _ = s.send_custom_message(draft, opts).await;
                });
                Ok(())
            }),
            send_user_message: Arc::new(move |text, images, deliver_as| {
                let s = s2.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                tokio::spawn(async move {
                    let _ = s
                        .prompt(
                            text,
                            Some(PromptOptions {
                                images,
                                streaming_behavior: deliver_as,
                                source: Some(InputSource::Extension),
                                expand_prompt_templates: Some(false),
                                ..PromptOptions::default()
                            }),
                        )
                        .await;
                });
                Ok(())
            }),
            append_entry: Arc::new(move |custom_type, data| {
                let s = s3.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                let id = s
                    .with_session_manager_mut(|sm| sm.append_custom_entry(custom_type, data))
                    .map_err(|e| e.to_string())?;
                if let Some(entry) = s.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
                    s.emit(AgentSessionEvent::EntryAppended { entry });
                }
                Ok(id)
            }),
            set_session_name: Arc::new(move |name| {
                let s = s4.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                s.set_session_name(name).map_err(|e| e.to_string())
            }),
            get_session_name: Arc::new(move || s5.upgrade().and_then(|s| s.session_name())),
            set_label: Arc::new(move |target_id, label| {
                let s = s6.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                s.with_session_manager_mut(|sm| sm.append_label_change(&target_id, label))
                    .map_err(|e| e.to_string())
            }),
            get_active_tools: Arc::new(move || {
                s7.upgrade()
                    .map_or_else(Vec::new, |s| s.get_active_tool_names())
            }),
            get_requested_tools: Arc::new(move || {
                requested_session
                    .upgrade()
                    .map(|s| {
                        s.inner
                            .requested_tool_names
                            .read()
                            .expect("requested tools lock")
                            .clone()
                    })
                    .unwrap_or_default()
            }),
            get_all_tools: Arc::new(move || {
                s8.upgrade().map_or_else(Vec::new, |s| s.get_all_tools())
            }),
            set_active_tools: Arc::new(move |names| {
                s9.upgrade()
                    .ok_or_else(|| SESSION_CLOSED_ERROR.to_owned())?
                    .set_active_tools_by_name(names)
            }),
            refresh_tools: Arc::new(move || {
                if let Some(s) = s10.upgrade()
                    && !s.is_closing_or_closed()
                {
                    s.refresh_tool_registry(None);
                }
            }),
            get_commands: Arc::new(move || {
                s11.upgrade()
                    .map_or_else(Vec::new, |s| s.get_slash_commands())
            }),
            set_model: Arc::new(move |model| {
                let s_opt = s12.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.set_model(model).await?;
                    Ok(true)
                })
            }),
            get_thinking_level: Arc::new(move || {
                s13.upgrade()
                    .map_or(ModelThinkingLevel::Off, |s| s.thinking_level())
            }),
            set_thinking_level: Arc::new(move |lvl| {
                if let Some(s) = s14.upgrade() {
                    let _ = s.set_thinking_level(lvl);
                }
            }),
        };

        let c1 = w.clone();
        let c2 = w.clone();
        let c3 = w.clone();
        let c4 = w.clone();
        let c5 = w.clone();
        let c6 = w.clone();
        let c7 = w.clone();
        let c8 = w.clone();
        let c9 = w.clone();
        let c10 = w.clone();
        let c11 = w.clone();
        let c12 = w.clone();
        let c13 = w.clone();
        let c14 = w.clone();
        let c15 = w.clone();
        let c16 = w.clone();
        let c17 = w.clone();

        let context_actions = ExtensionContextActions {
            get_model: Arc::new(move || c1.upgrade().and_then(|s| s.configured_model())),
            is_idle: Arc::new(move || c2.upgrade().is_none_or(|s| s.is_idle())),
            get_cancel_token: Arc::new(move || c3.upgrade().and_then(|s| s.agent().cancel_token())),
            abort: Arc::new(move || {
                if let Some(s) = c4.upgrade() {
                    tokio::spawn(async move {
                        s.abort().await;
                    });
                }
            }),
            has_pending_messages: Arc::new(move || {
                c5.upgrade().is_some_and(|s| s.pending_message_count() > 0)
            }),
            shutdown: Arc::new(move || {
                if let Some(s) = c6.upgrade() {
                    s.dispose();
                }
            }),
            get_context_usage: Arc::new(move || c7.upgrade().and_then(|s| s.get_context_usage())),
            compact: Arc::new(move |opts| {
                if let Some(s) = c8.upgrade() {
                    let instructions = opts.and_then(|o| o.custom_instructions);
                    tokio::spawn(async move {
                        let _ = s.compact(instructions).await;
                    });
                }
            }),
            get_system_prompt: Arc::new(move || {
                c9.upgrade().map_or_else(String::new, |s| s.system_prompt())
            }),
            get_system_prompt_options: Arc::new(move || {
                c10.upgrade()
                    .map_or_else(BuildSystemPromptOptions::default, |s| {
                        s.inner
                            .base_system_prompt_options
                            .read()
                            .expect("lock")
                            .to_build_options()
                    })
            }),
            get_callable_tools: Arc::new(move || {
                c11.upgrade()
                    .map_or_else(Vec::new, |s| s.get_callable_tools())
            }),
            execute_tool: Some(Arc::new(move |caller_id, name, args, opts| {
                let s_opt = c12.upgrade();
                Box::pin(async move {
                    let Some(s) = s_opt else {
                        return AgentToolCallOutcome {
                            tool_call: ToolCall {
                                id: format!("{caller_id}/0"),
                                name,
                                arguments: serde_json::Map::new(),
                                thought_signature: None,
                                namespace: None,
                                arguments_error: None,
                            },
                            result: pi_agent::AgentToolResult::error_text("Session dropped"),
                            is_error: true,
                            duration_ms: None,
                        };
                    };
                    s.execute_nested_tool_call(&caller_id, &name, args, opts)
                        .await
                })
            })),
            get_session_tree: Arc::new(move || {
                c13.upgrade().map_or_else(Vec::new, |s| {
                    s.with_session_manager(SessionManager::get_tree)
                })
            }),
            get_session_branch: Arc::new(move || {
                c14.upgrade().map_or_else(Vec::new, |s| {
                    s.with_session_manager(|sm| sm.get_branch(None))
                })
            }),
            get_session_leaf_id: Arc::new(move || {
                c15.upgrade().and_then(|s| {
                    s.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned))
                })
            }),
            get_session_id: Arc::new(move || {
                c17.upgrade().map_or_else(String::new, |s| s.session_id())
            }),
            get_session_file: Arc::new(move || c16.upgrade().and_then(|s| s.session_file())),
        };

        let m1 = w.clone();
        let m2 = w.clone();
        let m3 = w.clone();
        let m4 = w.clone();
        let m5 = w.clone();
        let m6 = w;
        let command_actions = ExtensionCommandContextActions {
            wait_for_idle: Arc::new(move || {
                let s_opt = m1.upgrade();
                Box::pin(async move {
                    if let Some(s) = s_opt {
                        s.wait_for_idle().await;
                    }
                })
            }),
            new_session: Arc::new(move |parent| {
                let s_opt = m2.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.new_session(parent).await
                })
            }),
            fork: Arc::new(move |entry_id, opts| {
                let s_opt = m3.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.fork(&entry_id, opts).await
                })
            }),
            navigate_tree: Arc::new(move |target_id, opts| {
                let s_opt = m4.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    let res = s.navigate_tree(&target_id, opts).await?;
                    Ok(!res.cancelled)
                })
            }),
            switch_session: Arc::new(move |path| {
                let s_opt = m5.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.switch_session(path).await
                })
            }),
            reload: Some(Arc::new(move || {
                let s_opt = m6.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.reload_resources().await
                })
            })),
        };

        runner.bind_core(actions, context_actions);
        runner.bind_command_context(Some(command_actions));
    }

    pub(super) fn install_agent_hooks(&self) {
        let w = self.downgrade();
        let raw_stream = self.inner.agent.stream_fn();
        let stream_session = w.clone();
        self.inner
            .agent
            .set_stream_fn(Arc::new(move |model, mut transcript, mut options| {
                let raw = raw_stream.clone();
                let session = stream_session.upgrade();
                Box::pin(async move {
                    if is_unconfigured_model(&model) {
                        let err_msg = session
                            .as_ref()
                            .map_or_else(format_no_models_available_message, |s| {
                                s.no_models_error_message()
                            });
                        let stream = pi_ai::create_assistant_message_event_stream();
                        let mut message = AssistantMessage::new_pending(&model);
                        message.stop_reason = StopReason::Error;
                        message.error_message = Some(err_msg);
                        stream.push(pi_ai::AssistantMessageEvent::Error {
                            reason: StopReason::Error,
                            error: message.clone(),
                        });
                        stream.end(Some(message));
                        return stream;
                    }
                    let failure = match session.as_ref() {
                        Some(s) if s.inner.agent_run_abort_requested.load(Ordering::SeqCst) => {
                            Some("Session run cancelled".to_owned())
                        }
                        Some(s) => s.ensure_external_open().err(),
                        None => Some("Session has been dropped".to_owned()),
                    };
                    if let Some(err) = failure {
                        let stream = pi_ai::create_assistant_message_event_stream();
                        let mut message = AssistantMessage::new_pending(&model);
                        message.stop_reason = StopReason::Aborted;
                        message.error_message = Some(err);
                        stream.push(pi_ai::AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: message.clone(),
                        });
                        stream.end(Some(message));
                        return stream;
                    }
                    let session = session.expect("checked session");
                    if session.is_block_images_enabled() {
                        transcript = pi_ai::normalize_context(&pi_ai::Context {
                            system_prompt: None,
                            messages: filter_blocked_images(transcript.messages().to_vec()),
                            tools: None,
                        });
                    }
                    let runner = session.extension_runner();
                    let token = options.base.cancel_token.get_or_insert_default().clone();
                    let header_token = token.clone();
                    let header_session = session.downgrade();
                    let previous = options.base.on_headers.take();
                    options.base.on_headers = Some(Arc::new(move |headers, model| {
                        let runner = runner.clone();
                        let previous = previous.clone();
                        let token = header_token.clone();
                        let session = header_session.upgrade();
                        let id = session
                            .as_ref()
                            .map_or(0, |s| Arc::as_ptr(&s.inner) as usize);
                        Box::pin(RUN_SESSION.scope(id, async move {
                            let headers = match previous {
                                Some(cb) => {
                                    let changed = cb(headers.clone(), model).await;
                                    pi_ai::api::normalize_header_changes(&headers, changed)
                                }
                                None => headers,
                            };
                            let headers = runner.emit_before_provider_headers(headers).await;
                            if runner.runtime().check_active().is_err()
                                || session.is_none_or(|s| s.ensure_runtime_writable().is_err())
                            {
                                token.cancel();
                            }
                            headers
                        }))
                    }));
                    let previous_payload = options.base.on_payload.take();
                    let payload_session = session.downgrade();
                    let payload_runner = session.extension_runner();
                    options.base.on_payload = Some(Arc::new(move |payload, model| {
                        let previous = previous_payload.clone();
                        let token = token.clone();
                        let session = payload_session.upgrade();
                        let runner = payload_runner.clone();
                        let id = session
                            .as_ref()
                            .map_or(0, |s| Arc::as_ptr(&s.inner) as usize);
                        Box::pin(RUN_SESSION.scope(id, async move {
                            let result = match previous {
                                Some(cb) => cb(payload, model).await,
                                None => None,
                            };
                            if runner.runtime().check_active().is_err()
                                || session.is_none_or(|s| s.ensure_runtime_writable().is_err())
                            {
                                token.cancel();
                            }
                            result
                        }))
                    }));

                    let sid = session.session_id();
                    if options.base.session_id.as_deref() == Some(sid.as_str())
                        && let Some(obs) = session
                            .inner
                            .on_resolved_session_request
                            .read()
                            .expect("resolved req lock")
                            .clone()
                    {
                        let req_seq = session.inner.request_seq.fetch_add(1, Ordering::SeqCst);
                        obs(ResolvedPhysicalRequest {
                            session_id: sid,
                            session_generation: session.session_generation(),
                            selection_generation: session.selection_generation(),
                            request_seq: req_seq,
                            selected_model: session.model(),
                            physical_model: model.clone(),
                            transcript: transcript.clone(),
                            options: options.clone(),
                        });
                    }

                    session.in_callback(raw(model, transcript, options)).await
                })
            }));

        let s_transform = w.clone();
        self.inner
            .agent
            .set_transform_context(Some(Arc::new(move |messages, _cancel| {
                let s_opt = s_transform.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_transform_context(messages)).await,
                        None => messages,
                    }
                })
            })));

        let s_before = w.clone();
        self.inner
            .agent
            .set_before_tool_call(Some(Arc::new(move |ctx, cancel| {
                let s_opt = s_before.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => {
                            s.in_callback(s.handle_before_tool_call(ctx, None, cancel))
                                .await
                        }
                        None => Ok(None),
                    }
                })
            })));

        let s_after = w.clone();
        self.inner
            .agent
            .set_after_tool_call(Some(Arc::new(move |ctx, cancel| {
                let s_opt = s_after.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => {
                            s.in_callback(s.handle_after_tool_call(ctx, None, cancel))
                                .await
                        }
                        None => Ok(None),
                    }
                })
            })));

        let s_finalize = w.clone();
        self.inner
            .agent
            .set_finalize_message(Some(Arc::new(move |msg, cancel| {
                let s_opt = s_finalize.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_finalize_message(msg, cancel)).await,
                        None => Ok(msg),
                    }
                })
            })));

        let s_prep_req = w.clone();
        self.inner
            .agent
            .set_prepare_request(Some(Arc::new(move |req_ctx, cancel| {
                let s_opt = s_prep_req.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => {
                            s.in_callback(s.handle_prepare_request(req_ctx, cancel))
                                .await
                        }
                        None => None,
                    }
                })
            })));

        let s_prep_next = w.clone();
        self.inner
            .agent
            .set_prepare_next_turn(Some(Arc::new(move |turn_ctx| {
                let s_opt = s_prep_next.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_prepare_next_turn(turn_ctx)).await,
                        None => None,
                    }
                })
            })));

        let s_finish = w.clone();
        self.inner
            .agent
            .set_finish_turn(Some(Arc::new(move |turn_ctx, cancel| {
                let s_opt = s_finish.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_finish_turn(turn_ctx, cancel)).await,
                        None => None,
                    }
                })
            })));

        let s_payload = w.clone();
        self.inner
            .agent
            .set_on_payload(Some(Arc::new(move |payload, _model| {
                let s_opt = s_payload.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => Some(
                            s.in_callback(
                                s.extension_runner().emit_before_provider_request(payload),
                            )
                            .await,
                        ),
                        None => Some(payload),
                    }
                })
            })));

        let s_resp = w.clone();
        self.inner
            .agent
            .set_on_response(Some(Arc::new(move |resp, _model| {
                let s_opt = s_resp.upgrade();
                Box::pin(async move {
                    if let Some(s) = s_opt {
                        s.in_callback(s.extension_runner().emit_after_provider_response(
                            AfterProviderResponseEvent {
                                status: resp.status,
                                headers: resp.headers,
                            },
                        ))
                        .await;
                    }
                })
            })));

        let s_stream_ev = w.clone();
        self.inner
            .agent
            .set_on_provider_stream_event(Some(Arc::new(move |ev, _model| {
                let s_opt = s_stream_ev.upgrade();
                Box::pin(async move {
                    if let Some(s) = s_opt {
                        s.in_callback(s.extension_runner().emit_provider_stream_event(ev))
                            .await;
                    }
                })
            })));

        let s_sub = w;
        let _ = self.inner.agent.subscribe(Arc::new(move |ev, _cancel| {
            let s_opt = s_sub.upgrade();
            Box::pin(async move {
                if let Some(s) = s_opt {
                    s.in_callback(s.handle_agent_event(ev)).await;
                }
            })
        }));
    }

    async fn handle_transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let mut transformed = self.extension_runner().emit_context(messages).await;

        let hidden = self
            .inner
            .hidden_declarations
            .read()
            .expect("hidden lock")
            .clone();
        if !hidden.is_empty() {
            transformed = transformed
                .into_iter()
                .map(|msg| match msg {
                    AgentMessage::System(mut sys) => {
                        if let Some(added) = &mut sys.tools_added {
                            added.retain(|t| !hidden.contains(&t.name));
                            if added.is_empty() {
                                sys.tools_added = None;
                            }
                        }
                        if let Some(removed) = &mut sys.tools_removed {
                            removed.retain(|t| !hidden.contains(&t.name));
                            if removed.is_empty() {
                                sys.tools_removed = None;
                            }
                        }
                        AgentMessage::System(sys)
                    }
                    other => other,
                })
                .collect();
        }

        let forced = self
            .inner
            .run_system_prompt_options
            .read()
            .expect("lock")
            .as_ref()
            .and_then(|o| o.force_system_prompt.clone());
        if let Some(forced_text) = forced {
            let current = get_agent_current_system_message(&transformed);
            let head = SystemMessage {
                content: SystemContent::Text(forced_text),
                sections: None,
                tools_added: current.as_ref().and_then(|c| c.tools_added.clone()),
                tools_removed: None,
                timestamp: current.map_or_else(now_ms, |c| c.timestamp),
            };
            let mut out = vec![AgentMessage::System(head)];
            out.extend(transformed.into_iter().filter(|m| m.role() != "system"));
            return out;
        }

        transformed
    }

    pub(super) async fn handle_before_tool_call(
        &self,
        ctx: &mut BeforeToolCallContext,
        parent_tool_call_id: Option<String>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<Option<BeforeToolCallResult>, String> {
        if self.is_closing_or_closed() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(SESSION_CLOSED_ERROR.to_owned()),
                terminate: Some(true),
                args: None,
            }));
        }
        if let Some(fatal) = self.fatal_runtime_error() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(fatal),
                terminate: Some(true),
                args: None,
            }));
        }

        let runner = self.extension_runner();
        let ext_res = runner
            .emit_tool_call(ToolCallEvent {
                tool_call_id: ctx.tool_call.id.clone(),
                tool_name: ctx.tool_call.name.clone(),
                input: ctx.args.clone(),
                parent_tool_call_id: parent_tool_call_id.clone(),
            })
            .await?;

        if let Some(r) = ext_res
            && r.block
        {
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: r.reason,
                terminate: None,
                args: None,
            }));
        }

        let _admission = self
            .inner
            .operation_admission
            .lock()
            .expect("admission lock");
        if self.is_closing_or_closed() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(SESSION_CLOSED_ERROR.to_owned()),
                terminate: Some(true),
                args: None,
            }));
        }
        if let Some(err) = self.fatal_runtime_error() {
            return Err(err);
        }
        if self.with_session_manager(SessionManager::is_poisoned) {
            let err = "Session storage is poisoned; refusing tool execution".to_owned();
            self.set_fatal_runtime_error(err.clone());
            return Err(err);
        }

        // 工具执行前可失败地将 InFlightToolRecord 写入状态日志（默认 ToolReplayPolicy::Never）
        let replay_policy = self
            .inner
            .tool_definitions
            .read()
            .expect("defs lock")
            .get(&ctx.tool_call.name)
            .map_or(ToolReplayPolicy::Never, |(d, _)| d.replay_policy);

        let record = InFlightToolRecord {
            tool_call_id: ctx.tool_call.id.clone(),
            parent_tool_call_id,
            tool_name: ctx.tool_call.name.clone(),
            replay_policy,
            started_at_ms: now_ms(),
        };

        if let Err(err) = self.with_execution_log_internal(|log| log.record_tool_start(record)) {
            let err_msg = format!("Failed to persist in-flight tool state before execution: {err}");
            self.set_fatal_runtime_error(err_msg.clone());
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(err_msg),
                terminate: Some(true),
                args: None,
            }));
        }

        Ok(None)
    }

    pub(super) async fn handle_after_tool_call(
        &self,
        ctx: AfterToolCallContext,
        parent_tool_call_id: Option<String>,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<Option<AfterToolCallResult>, String> {
        let orig_content = ctx.result.content.clone();
        let orig_structured = ctx.result.structured_content.clone();
        let runner = self.extension_runner();
        let hook_res = runner
            .emit_tool_result(ToolResultEvent {
                tool_call_id: ctx.tool_call.id,
                tool_name: ctx.tool_call.name,
                input: ctx.args,
                content: ctx.result.content,
                details: Some(ctx.result.details),
                structured_content: ctx.result.structured_content,
                is_error: ctx.is_error,
                usage: ctx.result.usage,
                parent_tool_call_id,
            })
            .await;

        let current_content = hook_res
            .as_ref()
            .and_then(|r| r.content.clone())
            .unwrap_or_else(|| orig_content.clone());

        let normalize_fn = self
            .inner
            .normalize_tool_result_content
            .read()
            .expect("normalize_tool_result_content lock")
            .clone();
        let normalized_content = match normalize_fn {
            Some(cb) => cb(current_content.clone(), self.model()).await,
            None => current_content.clone(),
        };

        if hook_res.is_none() && normalized_content == current_content {
            return Ok(None);
        }

        Ok(Some(AfterToolCallResult {
            content: Some(normalized_content),
            details: hook_res.as_ref().and_then(|r| r.details.clone()),
            structured_content: if let Some(ref hr) = hook_res {
                hr.structured_content.clone()
            } else {
                orig_structured
            },
            is_error: hook_res.as_ref().and_then(|r| r.is_error),
            usage: hook_res.and_then(|r| r.usage),
            terminate: None,
        }))
    }

    async fn handle_finalize_message(
        &self,
        mut message: AgentMessage,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<AgentMessage, String> {
        if let Some(fatal) = self.fatal_runtime_error() {
            return Err(fatal);
        }

        let input_correlation = if message.role() == "user" {
            self.inner
                .pending_input_correlations
                .lock()
                .expect("input correlations lock")
                .remove(&message.timestamp())
        } else {
            None
        };
        let admission_cb = if message.role() == "user" {
            self.inner
                .pending_admission_callbacks
                .lock()
                .expect("admission lock")
                .remove(&message.timestamp())
        } else {
            None
        };

        // 若是父工具结果消息：在进入 message_end 处理器及发出 message_start 前合入 nested_calls 与汇总 usage
        let orig_tool_call_id = if let AgentMessage::ToolResult(tr) = &mut message {
            let tc_id = tr.tool_call_id.clone();
            if let Some(nested_runner) = self.inner.nested_runner.get()
                && let Some(summary) = nested_runner.take_record(&tc_id)
            {
                if summary.calls.is_some() {
                    tr.nested_calls = summary.calls;
                }
                if let Some(nested_usage) = summary.usage {
                    tr.usage = Some(match &tr.usage {
                        Some(existing) => combine_usage(existing, &nested_usage),
                        None => nested_usage,
                    });
                }
            }
            Some((tc_id, tr.nested_calls.clone()))
        } else {
            None
        };

        // 触发扩展 message_end 钩子，允许链式返回同角色新消息
        if let Some(replaced) = self
            .extension_runner()
            .emit_message_end(message.clone())
            .await
        {
            message = replaced;
            if let (Some((tc_id, merged_nested)), AgentMessage::ToolResult(tr)) =
                (orig_tool_call_id, &mut message)
            {
                tr.tool_call_id = tc_id;
                if tr.nested_calls.is_none() {
                    tr.nested_calls = merged_nested;
                }
            }
        }

        if let Some(policy) = &self.inner.data_policy {
            let role = message.role().to_owned();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                policy.persisted_message(message)
            }));
            message = match result {
                Ok(Ok(projected)) if projected.role() == role => projected,
                _ => {
                    let error = "Session message projection failed".to_owned();
                    if let Some(cb) = &admission_cb {
                        cb(Err(error.clone()));
                    }
                    self.set_fatal_runtime_error(error.clone());
                    return Err(error);
                }
            };
        }

        let queue_item_id = input_correlation.as_ref().and_then(|(client_id, _)| {
            self.with_execution_log(|log| {
                log.get_input(client_id)
                    .map(|record| record.queue_item_id.clone())
            })
        });
        // 将最终消息持久化到 SessionManager；失败时设置会话级终止标记并停止运行
        let persist_res: Result<String, SessionStoreError> =
            self.with_session_manager_internal(|sm| {
                if let Some((client_id, _)) = &input_correlation {
                    sm.append_queued_user_message(
                        message.clone(),
                        client_id,
                        queue_item_id.as_deref(),
                    )
                } else if let Some(cm) = CustomMessage::from_agent_message(&message) {
                    sm.append_custom_message_entry(
                        cm.custom_type,
                        cm.content,
                        cm.display,
                        cm.details,
                    )
                } else {
                    sm.append_message(message.clone())
                }
            });

        let entry_id = match persist_res {
            Ok(id) => id,
            Err(err) => {
                let err_msg = format!("Session persistence failed: {err}");
                if let Some(cb) = &admission_cb {
                    cb(Err(err_msg.clone()));
                }
                self.set_fatal_runtime_error(err_msg.clone());
                return Err(err_msg);
            }
        };

        if let Some((client_id, disposition)) = input_correlation
            && let Err(err) = self.accept_input(&client_id, disposition, Some(entry_id.clone()))
        {
            if let Some(cb) = &admission_cb {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        if let Some(cb) = admission_cb {
            cb(Ok(PromptDisposition::Started));
        }

        self.inner
            .entry_ids_by_timestamp_and_role
            .lock()
            .expect("entry map lock")
            .push((
                message.role().to_owned(),
                message.timestamp(),
                entry_id.clone(),
            ));

        if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&entry_id).cloned()) {
            self.emit_async(AgentSessionEvent::EntryAppended { entry })
                .await;
        }

        // 若是 ToolResultMessage：仅在最终 ToolResultMessage 已成功写入并同步后，才清理状态日志中的未决记录
        if let AgentMessage::ToolResult(tr) = &message
            && let Err(err) =
                self.with_execution_log_internal(|log| log.clear_persisted_tool_result(tr))
        {
            let err_msg =
                format!("Failed to clear in-flight tool state after persisting ToolResult: {err}");
            self.set_fatal_runtime_error(err_msg.clone());
            return Err(err_msg);
        }

        if let AgentMessage::Assistant(a) = &message {
            *self
                .inner
                .last_assistant_message
                .lock()
                .expect("last assistant lock") = Some((a.clone(), Some(entry_id)));

            if !matches!(a.stop_reason, StopReason::Error | StopReason::Length) {
                self.inner
                    .overflow_recovery_attempted
                    .store(false, Ordering::SeqCst);
            }

            if a.stop_reason != StopReason::Error {
                let prev_attempt = self.inner.retry_attempt.swap(0, Ordering::SeqCst);
                if prev_attempt > 0 {
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt: prev_attempt as u32,
                        final_error: None,
                    });
                }
            }
        }

        Ok(message)
    }

    async fn handle_prepare_request(
        &self,
        req_ctx: PrepareRequestContext,
        cancel_token: Option<CancellationToken>,
    ) -> Option<AgentRequestUpdate> {
        if self.fatal_runtime_error().is_some() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return None;
        }

        let failed = self.inner.failed_response.lock().expect("lock").take();
        let projection = self.with_session_manager(SessionManager::build_session_projection);
        let mut canonical_context = AgentContext {
            messages: projection.messages.clone(),
            tools: self.inner.agent.state().tools,
        };
        let model = self.model();
        let thinking_level = self.thinking_level();

        if !is_virtual_model(&model) {
            let _ = req_ctx;
            return Some(AgentRequestUpdate {
                context: Some(canonical_context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        }

        // 虚拟模型路由（pi-virtual）
        let vdef = self
            .extension_runner()
            .get_registered_virtual_models()
            .into_iter()
            .find(|def| def.provider == model.provider && def.id == model.id);
        let Some(vdef) = vdef else {
            return Some(AgentRequestUpdate {
                context: Some(canonical_context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        };

        let last_asst_idx = canonical_context
            .messages
            .iter()
            .rposition(|m| m.role() == "assistant");
        let tail_slice = match last_asst_idx {
            Some(idx) => &canonical_context.messages[idx + 1..],
            None => &canonical_context.messages[..],
        };
        let is_user_turn = tail_slice.iter().any(|m| m.role() == "user");
        let reason = if failed.is_some() {
            ModelRouteReason::Retry
        } else if is_user_turn {
            ModelRouteReason::User
        } else {
            ModelRouteReason::Continuation
        };

        let previous = find_latest_response(&canonical_context.messages).and_then(|resp| {
            self.inner
                .models
                .get_model(&resp.provider, &resp.model)
                .map(|m| PreviousModelRoute {
                    model: m,
                    thinking_level: resp.thinking_level,
                })
        });

        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        let prev_state = get_virtual_model_state(&branch, &model.provider, &model.id);

        let route_req = ModelRouteRequest {
            model: model.clone(),
            thinking_level,
            reason,
            previous,
            failed: failed.map(|(fm, fl, fmsg)| FailedModelRoute {
                model: fm,
                thinking_level: Some(fl),
                message: fmsg,
            }),
            state: prev_state.clone(),
            messages: convert_to_llm_with_options(
                &canonical_context.messages,
                self.is_block_images_enabled(),
            ),
            cancel_token,
        };

        let Ok(route) = (vdef.route)(route_req).await else {
            return Some(AgentRequestUpdate {
                context: Some(canonical_context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        };

        if route.state.is_some() && route.state != prev_state {
            let state_data = VirtualModelStateData {
                provider: model.provider.clone(),
                model_id: model.id.clone(),
                state: route.state,
            };
            let val = serde_json::to_value(state_data).expect("virtual state serialization");
            match self.with_session_manager_internal(|sm| {
                sm.append_custom_entry(VIRTUAL_MODEL_STATE_ENTRY, Some(val))
            }) {
                Ok(entry_id) => {
                    if let Some(entry) =
                        self.with_session_manager(|sm| sm.get_entry(&entry_id).cloned())
                    {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to persist virtual model routing state: {err}"
                    ));
                    return None;
                }
            }
        }

        if self.exceeds_compaction_threshold(&route.model, &projection) {
            let _ = self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
            let refreshed = self.with_session_manager(SessionManager::build_session_projection);
            canonical_context.messages = refreshed.messages;
        }

        Some(AgentRequestUpdate {
            context: Some(canonical_context),
            model: Some(route.model),
            thinking_level: Some(route.thinking_level),
        })
    }

    async fn handle_prepare_next_turn(
        &self,
        turn_ctx: PrepareNextTurnContext,
    ) -> Option<AgentLoopTurnUpdate> {
        let mut next_context = turn_ctx.context;
        let projection = self.with_session_manager(SessionManager::build_session_projection);
        let model = self.model();
        if !is_virtual_model(&model) && self.exceeds_compaction_threshold(&model, &projection) {
            let _ = self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
            next_context.messages =
                self.with_session_manager(|sm| sm.build_session_projection().messages);
        } else {
            next_context.messages = projection.messages;
        }

        let mut opts = self
            .inner
            .run_system_prompt_options
            .read()
            .expect("lock")
            .clone()
            .unwrap_or_else(|| {
                self.inner
                    .base_system_prompt_options
                    .read()
                    .expect("lock")
                    .clone()
            });
        opts.selected_tools = self.get_active_tool_names();
        let update_msg = self.prepare_prompt_and_tool_loadout(&mut opts, &next_context.messages);
        *self.inner.run_system_prompt_options.write().expect("lock") = Some(opts);

        next_context.tools = self.inner.agent.state().tools;
        Some(AgentLoopTurnUpdate {
            context: Some(next_context),
            messages: update_msg.map(|sys| vec![AgentMessage::System(sys)]),
            model: Some(self.model()),
            thinking_level: Some(self.thinking_level()),
        })
    }

    async fn handle_finish_turn(
        &self,
        turn_ctx: pi_agent::AgentTurnContext,
        _cancel_token: Option<CancellationToken>,
    ) -> Option<AgentTurnDecision> {
        if self.fatal_runtime_error().is_some() {
            return Some(AgentTurnDecision::End);
        }

        // 记录轮次精确耗时（cst-web-work-duration）
        let started_ms = self.inner.turn_started_at_ms.load(Ordering::SeqCst);
        let ended_ms = now_ms();
        if started_ms > 0 {
            let duration_ms = ended_ms.saturating_sub(started_ms);
            let _ = self.with_session_manager_internal(|sm| {
                sm.append_custom_entry(
                    WORK_DURATION_CUSTOM_TYPE,
                    Some(json!({
                        "turnIndex": self.inner.turn_index.load(Ordering::SeqCst),
                        "startedAtMs": started_ms,
                        "endedAtMs": ended_ms,
                        "durationMs": duration_ms,
                    })),
                )
            });
        }

        let turn_idx = self.inner.turn_index.load(Ordering::SeqCst) as usize;
        let boundary = self
            .extension_runner()
            .emit_boundary(
                BoundaryEventKind::TurnEnd {
                    turn_index: turn_idx,
                    message: AgentMessage::Assistant(turn_ctx.message),
                    tool_results: turn_ctx.tool_results,
                },
                |drafts| self.build_boundary_context_preview(drafts, true),
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
                        "Failed to commit turn_end boundary drafts: {err}"
                    ));
                    return Some(AgentTurnDecision::End);
                }
            }
        }

        self.flush_pending_custom_messages();

        if boundary.r#continue {
            match self.build_boundary_context_preview(&[], true) {
                Ok(preview) if preview.can_continue => return Some(AgentTurnDecision::Continue),
                _ => {
                    self.extension_runner().emit_error(ExtensionError {
                        extension_path: "<boundary>".to_owned(),
                        event: "turn_end".to_owned(),
                        error: "turn_end requested continuation without runnable model context"
                            .to_owned(),
                    });
                }
            }
        }

        None
    }

    pub(super) fn build_boundary_context_preview(
        &self,
        drafts: &[SessionBoundaryDraft],
        is_turn_end: bool,
    ) -> Result<BoundaryContextPreview, String> {
        let mut preview_sm = self.with_session_manager(SessionManager::clone_in_memory_preview);
        preview_sm
            .commit_boundary_drafts(drafts)
            .map_err(|e| e.to_string())?;
        let projection = preview_sm.build_session_projection();
        let branch = preview_sm.get_branch(None);
        let estimated_tokens = estimate_projected_context_tokens(&projection, &branch).tokens;
        let llm_messages = convert_to_llm(&projection.messages);
        let final_role = llm_messages.last().map(|m| match m {
            pi_ai::Message::System(_) => "system",
            pi_ai::Message::User(_) => "user",
            pi_ai::Message::Assistant(_) => "assistant",
            pi_ai::Message::ToolResult(_) => "toolResult",
        });
        let has_non_system = llm_messages
            .iter()
            .any(|m| !matches!(m, pi_ai::Message::System(_)));
        let context_can_continue = has_non_system && final_role != Some("assistant");
        let has_pending_custom = !self
            .inner
            .pending_custom_messages
            .lock()
            .expect("lock")
            .is_empty();
        let has_queued = self.inner.agent.has_queued_messages();

        let can_continue = context_can_continue
            || has_pending_custom
            || if is_turn_end {
                has_queued
            } else {
                final_role == Some("assistant") && has_queued
            };

        Ok(BoundaryContextPreview {
            messages: projection.messages,
            estimated_tokens,
            can_continue,
        })
    }

    async fn handle_agent_event(&self, event: AgentEvent) {
        match &event {
            AgentEvent::AgentStart => {
                if self.inner.agent_run_abort_requested.load(Ordering::SeqCst) {
                    self.inner.agent.abort();
                }
                self.inner.turn_index.store(0, Ordering::SeqCst);
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::AgentStart)
                    .await;
                self.emit_async(AgentSessionEvent::AgentStart).await;
            }
            AgentEvent::TurnStart => {
                let now = now_ms();
                self.inner.turn_started_at_ms.store(now, Ordering::SeqCst);
                let idx = self.inner.turn_index.load(Ordering::SeqCst) as usize;
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::TurnStart {
                        turn_index: idx,
                        timestamp: now,
                    })
                    .await;
                self.emit_async(AgentSessionEvent::TurnStart).await;
            }
            AgentEvent::MessageStart { message } => {
                if let AgentMessage::User(u) = message {
                    self.inner
                        .overflow_recovery_attempted
                        .store(false, Ordering::SeqCst);
                    let text = match &u.content {
                        UserContent::Text(s) => s.clone(),
                        UserContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| match b {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(""),
                    };
                    if !text.is_empty() {
                        let mut removed = false;
                        {
                            let mut steer = self.inner.steering_messages.lock().expect("lock");
                            if let Some(pos) = steer.iter().position(|s| s == &text) {
                                steer.remove(pos);
                                removed = true;
                            }
                        }
                        if !removed {
                            let mut follow = self.inner.follow_up_messages.lock().expect("lock");
                            if let Some(pos) = follow.iter().position(|s| s == &text) {
                                follow.remove(pos);
                                removed = true;
                            }
                        }
                        if removed {
                            self.emit_queue_update();
                        }
                    }
                }
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::MessageStart {
                        message: message.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::MessageUpdate {
                        message: message.clone(),
                        assistant_message_event: assistant_message_event.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::MessageUpdate {
                    message: message.clone(),
                    assistant_message_event: assistant_message_event.clone(),
                })
                .await;
            }
            AgentEvent::MessageEnd { message } => {
                self.emit_async(AgentSessionEvent::MessageEnd {
                    message: message.clone(),
                })
                .await;
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::ToolExecutionStart {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        parent_tool_call_id: parent_tool_call_id.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::ToolExecutionStart {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    parent_tool_call_id: parent_tool_call_id.clone(),
                })
                .await;
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
                parent_tool_call_id,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::ToolExecutionUpdate {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        partial_result: partial_result.clone(),
                        parent_tool_call_id: parent_tool_call_id.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::ToolExecutionUpdate {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    partial_result: partial_result.clone(),
                    parent_tool_call_id: parent_tool_call_id.clone(),
                })
                .await;
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
                parent_tool_call_id,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::ToolExecutionEnd {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: result.clone(),
                        is_error: *is_error,
                        duration_ms: *duration_ms,
                        parent_tool_call_id: parent_tool_call_id.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::ToolExecutionEnd {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    result: result.clone(),
                    is_error: *is_error,
                    duration_ms: *duration_ms,
                    parent_tool_call_id: parent_tool_call_id.clone(),
                })
                .await;
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                self.inner.turn_index.fetch_add(1, Ordering::SeqCst);
                self.emit_async(AgentSessionEvent::TurnEnd {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                })
                .await;
            }
            AgentEvent::AgentEnd { messages } => {
                if let Some(nested) = self.inner.nested_runner.get() {
                    nested.clear();
                }
                let will_retry = self.will_retry_after_agent_end(messages);
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::AgentEnd {
                        messages: messages.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::AgentEnd {
                    messages: messages.clone(),
                    will_retry,
                })
                .await;
            }
        }
    }

    fn will_retry_after_agent_end(&self, messages: &[AgentMessage]) -> bool {
        if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            || self.fatal_runtime_error().is_some()
        {
            return false;
        }
        let policy = self.retry_policy();
        let current_attempt = self.inner.retry_attempt.load(Ordering::SeqCst) as u32;
        if !policy.enabled || current_attempt >= policy.max_retries {
            return false;
        }
        for msg in messages.iter().rev() {
            if let AgentMessage::Assistant(a) = msg {
                return self.is_retryable_error(a);
            }
        }
        false
    }
}
