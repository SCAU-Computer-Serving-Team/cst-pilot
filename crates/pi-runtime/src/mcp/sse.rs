use pi_ai::JsonValue;

pub(crate) const MAX_MCP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct SseDecoder {
    pending: Vec<u8>,
    data: String,
    event: String,
    event_id: Option<String>,
    pub last_event_id: Option<String>,
    pub retry_ms: Option<u64>,
    first_line: bool,
}

impl SseDecoder {
    pub fn with_cursor(last_event_id: Option<String>) -> Self {
        Self {
            last_event_id,
            ..Default::default()
        }
    }
    pub fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<JsonValue>, String> {
        if self.pending.len().saturating_add(bytes.len()) > MAX_MCP_MESSAGE_BYTES {
            return Err("MCP SSE message exceeds size limit".to_owned());
        }
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        let mut start = 0;
        while let Some(relative) = self.pending[start..]
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            let end = start + relative;
            if self.pending[end] == b'\r' && end + 1 == self.pending.len() && !eof {
                break;
            }
            let mut line = std::str::from_utf8(&self.pending[start..end])
                .map_err(|_| "Invalid UTF-8 in MCP SSE event")?
                .to_owned();
            if !self.first_line {
                self.first_line = true;
                line = line.trim_start_matches('\u{feff}').to_owned();
            }
            start = end + 1;
            if self.pending[end] == b'\r' && self.pending.get(start) == Some(&b'\n') {
                start += 1;
            }
            self.line(&line, &mut messages)?;
        }
        self.pending.drain(..start);
        if eof {
            let tail = std::mem::take(&mut self.pending);
            if !tail.is_empty() {
                self.line(
                    std::str::from_utf8(&tail).map_err(|_| "Invalid UTF-8 in MCP SSE event")?,
                    &mut messages,
                )?;
            }
            self.line("", &mut messages)?;
        }
        Ok(messages)
    }

    fn line(&mut self, line: &str, messages: &mut Vec<JsonValue>) -> Result<(), String> {
        if line.is_empty() {
            if !self.data.is_empty() && (self.event.is_empty() || self.event == "message") {
                messages.push(
                    serde_json::from_str(self.data.trim_end_matches('\n'))
                        .map_err(|_| "Invalid JSON in MCP SSE event")?,
                );
            }
            if let Some(id) = self.event_id.take() {
                self.last_event_id = (!id.is_empty()).then_some(id);
            }
            self.data.clear();
            self.event.clear();
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                if self
                    .data
                    .len()
                    .saturating_add(value.len())
                    .saturating_add(1)
                    > MAX_MCP_MESSAGE_BYTES
                {
                    return Err("MCP SSE message exceeds size limit".to_owned());
                }
                self.data.push_str(value);
                self.data.push('\n');
            }
            "event" => self.event = value.to_owned(),
            "id" if !value.contains('\0') => self.event_id = Some(value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.retry_ms = value.parse().ok()
            }
            _ => {}
        }
        Ok(())
    }
}
