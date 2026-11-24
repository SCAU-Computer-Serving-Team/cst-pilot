use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ClientError;
use crate::state::LifecycleReportDto;
use crate::workspace::{ComposerDraft, InlineToken, SendMode, SharedImageHandle};

/// Validates that `id` matches `^[a-zA-Z0-9_-]{1,128}$`.
pub fn is_valid_client_key(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn generate_client_message_id() -> String {
    format!("msg-{}", Uuid::now_v7().simple())
}

pub fn generate_creation_request_key() -> String {
    format!("create-{}", Uuid::now_v7().simple())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxItemState {
    CreatingSession {
        request_key: String,
    },
    Sending,
    PausedStorageFull {
        message: String,
    },
    Uncertain {
        message: String,
    },
    Failed {
        code: String,
        message: String,
        credentials_required: bool,
    },
}

impl OutboxItemState {
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::CreatingSession { .. } | Self::Sending)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub client_message_id: String,
    pub session_id: Option<String>,
    pub creation_request_key: Option<String>,
    pub text: String,
    pub skill: Option<String>,
    pub images: Vec<SharedImageHandle>,
    pub send_mode: SendMode,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
    pub created_at_ms: u64,
    pub state: OutboxItemState,
}

impl OutboxItem {
    pub fn from_draft(
        session_id: Option<String>,
        draft: &ComposerDraft,
        now_ms: u64,
    ) -> Result<Self, ClientError> {
        let client_message_id = generate_client_message_id();
        if !is_valid_client_key(&client_message_id) {
            return Err(ClientError::BadRequest {
                code: "invalid_message_id".into(),
                message: "生成的客户端消息标识无效。".into(),
            });
        }
        let creation_request_key = if session_id.is_none() {
            Some(generate_creation_request_key())
        } else {
            None
        };
        let state = match &creation_request_key {
            Some(req_key) => OutboxItemState::CreatingSession {
                request_key: req_key.clone(),
            },
            None => OutboxItemState::Sending,
        };
        Ok(Self {
            client_message_id,
            session_id,
            creation_request_key,
            text: draft.text.clone(),
            skill: draft.skill_name().map(str::to_owned),
            images: draft.images.clone(),
            send_mode: draft.send_mode,
            provider: draft.selected_provider.clone(),
            model_id: draft.selected_model_id.clone(),
            thinking_level: draft.selected_thinking_level.clone(),
            created_at_ms: now_ms,
            state,
        })
    }

    pub fn to_composer_draft(&self) -> ComposerDraft {
        let mut tokens = Vec::new();
        if let Some(ref skill) = self.skill {
            let cmd = format!("/skill:{skill}");
            tokens.push(InlineToken::Command {
                raw: cmd.clone(),
                name: cmd,
            });
        }
        let cursor_char_offset = self.text.chars().count();
        ComposerDraft {
            text: self.text.clone(),
            tokens,
            images: self.images.clone(),
            send_mode: self.send_mode,
            cursor_char_offset,
            selected_provider: self.provider.clone(),
            selected_model_id: self.model_id.clone(),
            selected_thinking_level: self.thinking_level.clone(),
        }
    }

    pub fn is_unresolved(&self) -> bool {
        true
    }
}

/// Process-local optimistic outbox state machine (never persists secrets or files to disk).
#[derive(Debug, Default)]
pub struct SessionOutbox {
    home_pending: Option<OutboxItem>,
    by_session: BTreeMap<String, Vec<OutboxItem>>,
    paused_sessions: BTreeMap<String, String>,
}

impl SessionOutbox {
    pub fn home_item(&self) -> Option<&OutboxItem> {
        self.home_pending.as_ref()
    }

    pub fn session_items(&self, session_id: &str) -> &[OutboxItem] {
        self.by_session
            .get(session_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn is_session_paused(&self, session_id: &str) -> bool {
        self.paused_sessions.contains_key(session_id)
    }

    pub fn has_unresolved_for_session(&self, session_id: &str) -> bool {
        self.by_session
            .get(session_id)
            .is_some_and(|items| !items.is_empty())
    }

    pub fn total_unresolved_count(&self) -> usize {
        usize::from(self.home_pending.is_some())
            + self.by_session.values().map(Vec::len).sum::<usize>()
    }

    pub fn enqueue_home(&mut self, item: OutboxItem) -> Result<(), ClientError> {
        if self.home_pending.is_some() {
            return Err(ClientError::Conflict {
                code: "home_send_in_progress".into(),
                message: "正在创建新对话，请稍候。".into(),
            });
        }
        self.home_pending = Some(item);
        Ok(())
    }

    pub fn bind_home_to_session(&mut self, session_id: &str) -> Option<OutboxItem> {
        let mut item = self.home_pending.take()?;
        item.session_id = Some(session_id.to_owned());
        item.state = OutboxItemState::Sending;
        self.by_session
            .entry(session_id.to_owned())
            .or_default()
            .push(item.clone());
        Some(item)
    }

    pub fn clear_home(&mut self) -> Option<OutboxItem> {
        self.home_pending.take()
    }

    pub fn mark_home_uncertain(&mut self, message: String) {
        if let Some(item) = &mut self.home_pending {
            item.state = OutboxItemState::Uncertain { message };
        }
    }

    pub fn mark_home_failed(&mut self, error: &ClientError) {
        if let Some(item) = &mut self.home_pending {
            item.state = OutboxItemState::Failed {
                code: error.code().to_owned(),
                message: error.to_string(),
                credentials_required: error.is_credentials_required(),
            };
        }
    }

    pub fn is_home_uncertain(&self, client_message_id: &str) -> bool {
        self.home_pending.as_ref().is_some_and(|item| {
            item.client_message_id == client_message_id
                && matches!(item.state, OutboxItemState::Uncertain { .. })
        })
    }

    pub fn reconcile_home_bound(
        &mut self,
        client_message_id: &str,
        session_id: &str,
        already_accepted: bool,
        not_delivered_error: &ClientError,
    ) -> bool {
        if !self.is_home_uncertain(client_message_id) {
            return false;
        }
        self.bind_home_to_session(session_id);
        if already_accepted {
            self.mark_accepted(session_id, client_message_id);
        } else {
            self.mark_session_error(session_id, client_message_id, not_delivered_error);
        }
        true
    }

    pub fn reconcile_home_missing(
        &mut self,
        client_message_id: &str,
        restored: bool,
        not_created_error: &ClientError,
    ) -> bool {
        if !self.is_home_uncertain(client_message_id) {
            return false;
        }
        if restored {
            self.clear_home();
        } else {
            self.mark_home_failed(not_created_error);
        }
        true
    }

    pub fn reconcile_session_uncertain(
        &mut self,
        session_id: &str,
        client_message_id: &str,
        accepted: bool,
        error: Option<&ClientError>,
    ) -> bool {
        let still_uncertain = self.by_session.get(session_id).is_some_and(|list| {
            list.iter().any(|item| {
                item.client_message_id == client_message_id
                    && matches!(item.state, OutboxItemState::Uncertain { .. })
            })
        });
        if !still_uncertain {
            return false;
        }
        if accepted {
            self.mark_accepted(session_id, client_message_id);
        } else if let Some(err) = error {
            self.mark_session_error(session_id, client_message_id, err);
        }
        true
    }

    pub fn restore_home_failed_item(&mut self, mut item: OutboxItem, error: &ClientError) {
        item.state = OutboxItemState::Failed {
            code: error.code().to_owned(),
            message: error.to_string(),
            credentials_required: error.is_credentials_required(),
        };
        if self.home_pending.is_none() {
            self.home_pending = Some(item);
        }
    }

    pub fn enqueue_session(&mut self, session_id: &str, mut item: OutboxItem) {
        item.session_id = Some(session_id.to_owned());
        if let Some(reason) = self.paused_sessions.get(session_id) {
            item.state = OutboxItemState::PausedStorageFull {
                message: reason.clone(),
            };
        } else {
            item.state = OutboxItemState::Sending;
        }
        self.by_session
            .entry(session_id.to_owned())
            .or_default()
            .push(item);
    }

    /// Removes the outbox item (releasing its `SharedImageHandle` references!) once accepted by `cst-server`
    /// either via `202 Accepted` or by observing its `client_message_id` in SSE `queue` / `session`.
    pub fn mark_accepted(&mut self, session_id: &str, client_message_id: &str) -> bool {
        let mut removed = false;
        if let Some(list) = self.by_session.get_mut(session_id) {
            let before = list.len();
            list.retain(|item| item.client_message_id != client_message_id);
            removed = list.len() != before;
            if list.is_empty() {
                self.by_session.remove(session_id);
            }
        }
        removed
    }

    /// Reconciles against a server `InboxSnapshot` JSON (`items[*].id`).
    pub fn reconcile_server_inbox_ids(&mut self, session_id: &str, server_ids: &[String]) -> bool {
        let Some(list) = self.by_session.get_mut(session_id) else {
            return false;
        };
        let before = list.len();
        list.retain(|item| !server_ids.iter().any(|sid| sid == &item.client_message_id));
        let changed = list.len() != before;
        if list.is_empty() {
            self.by_session.remove(session_id);
        }
        changed
    }

    pub fn mark_session_error(
        &mut self,
        session_id: &str,
        client_message_id: &str,
        error: &ClientError,
    ) {
        if error.is_storage_full() {
            let msg = error.to_string();
            self.paused_sessions
                .insert(session_id.to_owned(), msg.clone());
            if let Some(list) = self.by_session.get_mut(session_id) {
                for item in list.iter_mut() {
                    if item.client_message_id == client_message_id
                        || matches!(item.state, OutboxItemState::Sending)
                    {
                        item.state = OutboxItemState::PausedStorageFull {
                            message: msg.clone(),
                        };
                    }
                }
            }
            return;
        }

        if error.is_disconnected() {
            if let Some(list) = self.by_session.get_mut(session_id)
                && let Some(item) = list
                    .iter_mut()
                    .find(|i| i.client_message_id == client_message_id)
            {
                item.state = OutboxItemState::Uncertain {
                    message: error.to_string(),
                };
            }
            return;
        }

        if let Some(list) = self.by_session.get_mut(session_id)
            && let Some(item) = list
                .iter_mut()
                .find(|i| i.client_message_id == client_message_id)
        {
            item.state = OutboxItemState::Failed {
                code: error.code().to_owned(),
                message: error.to_string(),
                credentials_required: error.is_credentials_required(),
            };
        }
    }

    /// Transitions a failed/uncertain home item back into `CreatingSession` for retry.
    /// Returns `(item, was_uncertain)` so the caller checks the existing creation receipt before creating a new session.
    pub fn prepare_home_retry(
        &mut self,
        client_message_id: &str,
    ) -> Result<(OutboxItem, bool), ClientError> {
        let Some(item) = &mut self.home_pending else {
            return Err(ClientError::NotFound {
                code: "outbox_item_not_found".into(),
                message: "未找到待重试的主页发送项。".into(),
            });
        };
        if item.client_message_id != client_message_id {
            return Err(ClientError::NotFound {
                code: "outbox_item_not_found".into(),
                message: "待重试的消息标识不匹配。".into(),
            });
        }
        if item.state.is_in_flight() {
            return Err(ClientError::Conflict {
                code: "item_in_flight".into(),
                message: "该消息正在提交中，请勿重复重试。".into(),
            });
        }
        let was_uncertain = matches!(item.state, OutboxItemState::Uncertain { .. });
        let req_key = if was_uncertain {
            item.creation_request_key
                .clone()
                .unwrap_or_else(generate_creation_request_key)
        } else {
            generate_creation_request_key()
        };
        item.creation_request_key = Some(req_key.clone());
        item.state = OutboxItemState::CreatingSession {
            request_key: req_key,
        };
        Ok((item.clone(), was_uncertain))
    }

    pub fn prepare_session_retry(
        &mut self,
        session_id: &str,
        client_message_id: &str,
    ) -> Result<(OutboxItem, bool), ClientError> {
        let Some(list) = self.by_session.get_mut(session_id) else {
            return Err(ClientError::NotFound {
                code: "outbox_item_not_found".into(),
                message: "未找到待重试的消息。".into(),
            });
        };
        let Some(item) = list
            .iter_mut()
            .find(|i| i.client_message_id == client_message_id)
        else {
            return Err(ClientError::NotFound {
                code: "outbox_item_not_found".into(),
                message: "未找到待重试的消息。".into(),
            });
        };
        if item.state.is_in_flight() {
            return Err(ClientError::Conflict {
                code: "item_in_flight".into(),
                message: "该消息正在发送中，请勿重复提交。".into(),
            });
        }
        let was_uncertain = matches!(item.state, OutboxItemState::Uncertain { .. });
        self.paused_sessions.remove(session_id);
        item.state = OutboxItemState::Sending;
        Ok((item.clone(), was_uncertain))
    }

    pub fn prepare_retry(
        &mut self,
        session_id: &str,
        client_message_id: &str,
    ) -> Option<OutboxItem> {
        self.prepare_session_retry(session_id, client_message_id)
            .ok()
            .map(|(item, _)| item)
    }

    pub fn remove_item(&mut self, session_id: &str, client_message_id: &str) -> Option<OutboxItem> {
        let list = self.by_session.get_mut(session_id)?;
        let idx = list
            .iter()
            .position(|i| i.client_message_id == client_message_id)?;
        let removed = list.remove(idx);
        if list.is_empty() {
            self.by_session.remove(session_id);
            self.paused_sessions.remove(session_id);
        }
        Some(removed)
    }

    /// Removes a settled (non-in-flight) outbox item from either Home (`None`) or a specific session (`Some(sid)`).
    pub fn take_settled_item(
        &mut self,
        session_id: Option<&str>,
        client_message_id: &str,
    ) -> Result<OutboxItem, ClientError> {
        match session_id {
            None => {
                let Some(home) = &self.home_pending else {
                    return Err(ClientError::NotFound {
                        code: "outbox_item_not_found".into(),
                        message: "未找到指定的主页待发项。".into(),
                    });
                };
                if home.client_message_id != client_message_id {
                    return Err(ClientError::NotFound {
                        code: "outbox_item_not_found".into(),
                        message: "未找到指定的主页待发项。".into(),
                    });
                }
                if home.state.is_in_flight() {
                    return Err(ClientError::Conflict {
                        code: "item_in_flight".into(),
                        message: "消息正在提交中，暂不可编辑或移除。".into(),
                    });
                }
                Ok(self.home_pending.take().expect("checked Some"))
            }
            Some(sid) => {
                let Some(list) = self.by_session.get_mut(sid) else {
                    return Err(ClientError::NotFound {
                        code: "outbox_item_not_found".into(),
                        message: "未找到指定的待发消息。".into(),
                    });
                };
                let Some(idx) = list
                    .iter()
                    .position(|i| i.client_message_id == client_message_id)
                else {
                    return Err(ClientError::NotFound {
                        code: "outbox_item_not_found".into(),
                        message: "未找到指定的待发消息。".into(),
                    });
                };
                if list[idx].state.is_in_flight() {
                    return Err(ClientError::Conflict {
                        code: "item_in_flight".into(),
                        message: "消息正在发送中，暂不可编辑或移除。".into(),
                    });
                }
                let removed = list.remove(idx);
                if list.is_empty() {
                    self.by_session.remove(sid);
                    self.paused_sessions.remove(sid);
                } else if !list
                    .iter()
                    .any(|i| matches!(i.state, OutboxItemState::PausedStorageFull { .. }))
                {
                    self.paused_sessions.remove(sid);
                }
                Ok(removed)
            }
        }
    }

    pub fn uncertain_items(&self) -> Vec<(Option<String>, OutboxItem)> {
        let mut out = Vec::new();
        if let Some(home) = &self.home_pending
            && matches!(home.state, OutboxItemState::Uncertain { .. })
        {
            out.push((None, home.clone()));
        }
        for (sid, items) in &self.by_session {
            for item in items {
                if matches!(item.state, OutboxItemState::Uncertain { .. }) {
                    out.push((Some(sid.clone()), item.clone()));
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitCheckReport {
    pub running_sessions: usize,
    pub pending_server_inbox: usize,
    pub active_exports: usize,
    pub unresolved_local_outbox: usize,
    pub unsent_drafts: usize,
}

impl ExitCheckReport {
    pub fn from_sources(
        lifecycle: &LifecycleReportDto,
        unresolved_local_outbox: usize,
        unsent_drafts: usize,
    ) -> Self {
        let running_sessions = lifecycle.sessions.iter().filter(|s| s.running).count();
        let pending_server_inbox = lifecycle
            .sessions
            .iter()
            .map(|s| s.pending_inbox_count)
            .sum();
        Self {
            running_sessions,
            pending_server_inbox,
            active_exports: lifecycle.active_exports,
            unresolved_local_outbox,
            unsent_drafts,
        }
    }

    pub fn requires_confirmation(&self) -> bool {
        self.running_sessions > 0
            || self.pending_server_inbox > 0
            || self.active_exports > 0
            || self.unresolved_local_outbox > 0
            || self.unsent_drafts > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSwitchCheck {
    pub unresolved_local_outbox: usize,
    pub unsent_drafts: usize,
}

impl WebSwitchCheck {
    pub fn requires_confirmation(&self) -> bool {
        self.unresolved_local_outbox > 0 || self.unsent_drafts > 0
    }
}
