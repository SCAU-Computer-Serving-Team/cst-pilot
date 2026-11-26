use super::{Control, EventQuery, EventRead, LazyEventVisitor, NativeError, stopped, wide};
use crate::diagnostics::win_native::EventRecord;
use chrono::{Local, SecondsFormat, TimeZone};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::EventLog::*;
use windows::core::PCWSTR;

const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;

struct EventHandle(EVT_HANDLE);
impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = EvtClose(self.0);
        }
    }
}

fn error(source: &str, error: windows::core::Error) -> NativeError {
    let code = error.code().0 as u32;
    NativeError {
        source: source.into(),
        code: Some(if code & 0xffff0000 == 0x80070000 {
            code & 0xffff
        } else {
            code
        }),
        message: error.message(),
    }
}

fn is_code(error: &windows::core::Error, code: u32) -> bool {
    error.code().0 as u32 & 0xffff == code
}

pub(crate) fn filetime_to_local_rfc3339(ticks: u64) -> String {
    let Some(since_epoch) = ticks.checked_sub(EPOCH_DIFF_100NS) else {
        return String::new();
    };
    let secs = (since_epoch / 10_000_000) as i64;
    let nanos = ((since_epoch % 10_000_000) * 100) as u32;
    match Local.timestamp_opt(secs, nanos).single() {
        Some(dt) => dt.to_rfc3339_opts(SecondsFormat::Secs, false),
        None => String::new(),
    }
}

fn variant_string(v: &EVT_VARIANT) -> Option<String> {
    if v.Type == EvtVarTypeNull.0 as u32 {
        return None;
    }
    if v.Type == EvtVarTypeString.0 as u32 {
        let ptr = unsafe { v.Anonymous.StringVal };
        if ptr.is_null() {
            return None;
        }
        return unsafe { ptr.to_string() }.ok();
    }
    None
}

fn variant_u64(v: &EVT_VARIANT) -> Option<u64> {
    let t = v.Type;
    if t == EvtVarTypeNull.0 as u32 {
        return None;
    }
    unsafe {
        if t == EvtVarTypeUInt64.0 as u32 {
            Some(v.Anonymous.UInt64Val)
        } else if t == EvtVarTypeUInt32.0 as u32 {
            Some(u64::from(v.Anonymous.UInt32Val))
        } else if t == EvtVarTypeUInt16.0 as u32 {
            Some(u64::from(v.Anonymous.UInt16Val))
        } else if t == EvtVarTypeByte.0 as u32 {
            Some(u64::from(v.Anonymous.ByteVal))
        } else if t == EvtVarTypeFileTime.0 as u32 {
            Some(v.Anonymous.FileTimeVal)
        } else {
            None
        }
    }
}

fn render_system_values(
    ctx: EVT_HANDLE,
    handle: EVT_HANDLE,
    buffer: &mut Vec<u8>,
) -> Result<EventRecord, NativeError> {
    let mut used = 0u32;
    let mut prop_count = 0u32;
    let render = |buf: &mut Vec<u8>, used: &mut u32, count: &mut u32| unsafe {
        EvtRender(
            Some(ctx),
            handle,
            EvtRenderEventValues.0,
            buf.len() as u32,
            Some(buf.as_mut_ptr().cast()),
            used,
            count,
        )
    };
    if let Err(e) = render(buffer, &mut used, &mut prop_count) {
        if !is_code(&e, ERROR_INSUFFICIENT_BUFFER.0) || used > 2 * 1024 * 1024 {
            return Err(error("EvtRender", e));
        }
        buffer.resize(used as usize + 256, 0);
        render(buffer, &mut used, &mut prop_count).map_err(|e| error("EvtRender", e))?;
    }
    let count = prop_count as usize;
    if count < EvtSystemPropertyIdEND.0 as usize
        || count * std::mem::size_of::<EVT_VARIANT>() > buffer.len()
    {
        return Err(NativeError {
            source: "EvtRender".into(),
            code: None,
            message: "事件系统属性数量异常。".into(),
        });
    }
    let vals = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<EVT_VARIANT>(), count) };
    let provider = variant_string(&vals[EvtSystemProviderName.0 as usize]).unwrap_or_default();
    let id = variant_u64(&vals[EvtSystemEventID.0 as usize]).unwrap_or(0) as u16;
    let level = variant_u64(&vals[EvtSystemLevel.0 as usize]).unwrap_or(0) as u8;
    let time_ticks = variant_u64(&vals[EvtSystemTimeCreated.0 as usize]).unwrap_or(0);
    let record_id = variant_u64(&vals[EvtSystemEventRecordId.0 as usize]).unwrap_or(0);
    let channel = variant_string(&vals[EvtSystemChannel.0 as usize]).unwrap_or_default();
    let computer = variant_string(&vals[EvtSystemComputer.0 as usize]);

    Ok(EventRecord {
        log: channel,
        time: filetime_to_local_rfc3339(time_ticks),
        record_id,
        level,
        provider,
        id,
        machine: computer,
        message: None,
    })
}

fn format_message_reused(
    metadata: EVT_HANDLE,
    event: EVT_HANDLE,
    buffer: &mut Vec<u16>,
) -> Option<String> {
    if buffer.len() < 4096 {
        buffer.resize(4096, 0);
    }
    let mut used = 0u32;
    let fmt = |buf: &mut [u16], used: &mut u32| unsafe {
        EvtFormatMessage(
            Some(metadata),
            Some(event),
            0,
            None,
            EvtFormatMessageEvent.0,
            Some(buf),
            used,
        )
    };
    if let Err(e) = fmt(buffer.as_mut_slice(), &mut used) {
        if !is_code(&e, ERROR_INSUFFICIENT_BUFFER.0) || used > 1024 * 1024 {
            return None;
        }
        buffer.resize(used as usize, 0);
        fmt(buffer.as_mut_slice(), &mut used).ok()?;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..end]);
    if buffer.capacity() > 64 * 1024 {
        buffer.clear();
        buffer.shrink_to(4096);
    }
    Some(text)
}

type Publishers = Rc<RefCell<HashMap<String, Option<EventHandle>>>>;
type MsgBuffer = Rc<RefCell<Vec<u16>>>;

struct PendingMessage {
    // EvtClose on a query closes its child handles. Drop the event first.
    event: EventHandle,
    _query: Rc<EventHandle>,
    publishers: Publishers,
    msg_buf: MsgBuffer,
    provider: String,
}

impl PendingMessage {
    fn format(self) -> Option<String> {
        let mut publishers = self.publishers.borrow_mut();
        if publishers.len() >= 128 && !publishers.contains_key(&self.provider) {
            publishers.clear();
        }
        let publisher = publishers.entry(self.provider.clone()).or_insert_with(|| {
            let name = wide(&self.provider);
            unsafe { EvtOpenPublisherMetadata(None, PCWSTR(name.as_ptr()), PCWSTR::null(), 0, 0) }
                .ok()
                .map(EventHandle)
        });
        let mut buf = self.msg_buf.borrow_mut();
        publisher
            .as_ref()
            .and_then(|p| format_message_reused(p.0, self.event.0, &mut buf))
    }
}

pub fn query(
    query: &EventQuery,
    visitor: &mut LazyEventVisitor<'_>,
    control: &Control,
) -> Result<EventRead, NativeError> {
    stopped("EventLog", control)?;
    let log = wide(&query.log);
    let xpath = wide(&query.xpath);
    let result = Rc::new(EventHandle(
        unsafe {
            EvtQuery(
                None,
                PCWSTR(log.as_ptr()),
                PCWSTR(xpath.as_ptr()),
                EvtQueryChannelPath.0 | EvtQueryReverseDirection.0,
            )
        }
        .map_err(|e| error(&query.log, e))?,
    ));
    let render_ctx = EventHandle(
        unsafe { EvtCreateRenderContext(None, EvtRenderContextSystem.0) }
            .map_err(|e| error("EvtCreateRenderContext", e))?,
    );
    let publishers = Publishers::default();
    let msg_buf: MsgBuffer = Rc::new(RefCell::new(vec![0u16; 4096]));
    let mut render_buf = vec![0u8; 4096];
    let mut read = EventRead::default();
    let mut scanned = 0;

    loop {
        stopped("EventLog", control)?;
        let mut raw = [0isize; 64];
        let mut returned = 0;
        let capacity = (query.max_events - scanned).min(raw.len());
        match unsafe { EvtNext(result.0, &mut raw[..capacity], 1000, 0, &mut returned) } {
            Ok(()) => {}
            Err(e) if is_code(&e, ERROR_NO_MORE_ITEMS.0) => break,
            Err(e) if is_code(&e, windows::Win32::Foundation::ERROR_TIMEOUT.0) => continue,
            Err(e) => return Err(error(&query.log, e)),
        }
        let handles: Vec<_> = raw[..returned as usize]
            .iter()
            .map(|h| EventHandle(EVT_HANDLE(*h)))
            .collect();
        for event in handles {
            stopped("EventLog", control)?;
            scanned += 1;
            let rec = match render_system_values(render_ctx.0, event.0, &mut render_buf) {
                Ok(rec) => rec,
                Err(_) => {
                    read.unreadable += 1;
                    continue;
                }
            };
            let provider_name = rec.provider.clone();
            let pending = PendingMessage {
                event,
                _query: result.clone(),
                publishers: publishers.clone(),
                msg_buf: msg_buf.clone(),
                provider: provider_name,
            };
            visitor(rec, Box::new(move || pending.format())).map_err(|message| NativeError {
                source: "EventLog".into(),
                code: None,
                message,
            })?;
        }
        if scanned >= query.max_events {
            read.truncated = query.max_events > 1;
            break;
        }
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_converts_to_local_rfc3339_with_timezone_offset() {
        // 2026-11-26T00:00:00Z in FILETIME ticks
        let unix_secs: u64 = 1_795_564_800;
        let ticks = EPOCH_DIFF_100NS + unix_secs * 10_000_000;
        let formatted = filetime_to_local_rfc3339(ticks);
        assert!(chrono::DateTime::parse_from_rfc3339(&formatted).is_ok());
        assert!(
            !formatted.ends_with('Z'),
            "must use local offset: {formatted}"
        );
    }
}
