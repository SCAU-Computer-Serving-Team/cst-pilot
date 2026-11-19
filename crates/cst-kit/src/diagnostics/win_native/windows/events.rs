use super::{Control, EventQuery, EventRead, EventVisitor, NativeError, stopped, wide};
use crate::diagnostics::win_native::EventRecord;
use serde::Deserialize;
use std::collections::HashMap;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::EventLog::*;
use windows::core::PCWSTR;
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

#[derive(Deserialize)]
struct XmlEvent {
    #[serde(rename = "System")]
    system: XmlSystem,
}
#[derive(Deserialize)]
struct XmlSystem {
    #[serde(rename = "Provider")]
    provider: XmlProvider,
    #[serde(rename = "EventID")]
    id: u16,
    #[serde(rename = "EventRecordID")]
    record_id: u64,
    #[serde(rename = "Level", default)]
    level: u8,
    #[serde(rename = "TimeCreated")]
    time: XmlTime,
    #[serde(rename = "Channel")]
    channel: String,
    #[serde(rename = "Computer", default)]
    computer: Option<String>,
}
#[derive(Deserialize)]
struct XmlProvider {
    #[serde(rename = "@Name")]
    name: String,
}
#[derive(Deserialize)]
struct XmlTime {
    #[serde(rename = "@SystemTime")]
    time: String,
}

fn xml(handle: EVT_HANDLE) -> Result<XmlEvent, NativeError> {
    let mut buffer = vec![0u16; 8192];
    let mut used = 0;
    let mut properties = 0;
    let render = |buffer: &mut Vec<u16>, used: &mut u32, properties: &mut u32| unsafe {
        EvtRender(
            None,
            handle,
            EvtRenderEventXml.0,
            (buffer.len() * 2) as u32,
            Some(buffer.as_mut_ptr().cast()),
            used,
            properties,
        )
    };
    if let Err(e) = render(&mut buffer, &mut used, &mut properties) {
        if !is_code(&e, ERROR_INSUFFICIENT_BUFFER.0) || used > 2 * 1024 * 1024 {
            return Err(error("EvtRender", e));
        }
        buffer.resize((used as usize).div_ceil(2), 0);
        render(&mut buffer, &mut used, &mut properties).map_err(|e| error("EvtRender", e))?;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    quick_xml::de::from_str(&String::from_utf16_lossy(&buffer[..end])).map_err(|_| NativeError {
        source: "EvtRender".into(),
        code: None,
        message: "事件 XML 字段无法解析。".into(),
    })
}
fn message(metadata: EVT_HANDLE, event: EVT_HANDLE) -> Option<String> {
    let mut used = 0;
    let mut buffer = vec![0u16; 4096];
    let format = |buffer: &mut [u16], used: &mut u32| unsafe {
        EvtFormatMessage(
            Some(metadata),
            Some(event),
            0,
            None,
            EvtFormatMessageEvent.0,
            Some(buffer),
            used,
        )
    };
    if let Err(e) = format(&mut buffer, &mut used) {
        if !is_code(&e, ERROR_INSUFFICIENT_BUFFER.0) || used > 1024 * 1024 {
            return None;
        }
        buffer.resize(used as usize, 0);
        format(&mut buffer, &mut used).ok()?;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

pub fn query(
    query: &EventQuery,
    visitor: &mut EventVisitor<'_>,
    control: &Control,
) -> Result<EventRead, NativeError> {
    stopped("EventLog", control)?;
    let log = wide(&query.log);
    let xpath = wide(&query.xpath);
    let result = EventHandle(
        unsafe {
            EvtQuery(
                None,
                PCWSTR(log.as_ptr()),
                PCWSTR(xpath.as_ptr()),
                EvtQueryChannelPath.0 | EvtQueryReverseDirection.0,
            )
        }
        .map_err(|e| error(&query.log, e))?,
    );
    let mut publishers: HashMap<String, Option<EventHandle>> = HashMap::new();
    let mut read = EventRead::default();
    let mut scanned = 0;
    loop {
        stopped("EventLog", control)?;
        let mut raw = [0isize; 16];
        let mut returned = 0;
        let capacity = (query.max_events - scanned).min(raw.len());
        match unsafe { EvtNext(result.0, &mut raw[..capacity], 1000, 0, &mut returned) } {
            Ok(()) => {}
            Err(e) if is_code(&e, ERROR_NO_MORE_ITEMS.0) => break,
            Err(e) => return Err(error(&query.log, e)),
        }
        let handles: Vec<_> = raw[..returned as usize]
            .iter()
            .map(|h| EventHandle(EVT_HANDLE(*h)))
            .collect();
        for event in handles {
            stopped("EventLog", control)?;
            scanned += 1;
            let xml = match xml(event.0) {
                Ok(event) => event.system,
                Err(_) => {
                    read.unreadable += 1;
                    continue;
                }
            };
            let publisher = publishers
                .entry(xml.provider.name.clone())
                .or_insert_with(|| {
                    let name = wide(&xml.provider.name);
                    unsafe {
                        EvtOpenPublisherMetadata(None, PCWSTR(name.as_ptr()), PCWSTR::null(), 0, 0)
                    }
                    .ok()
                    .map(EventHandle)
                });
            let msg = publisher
                .as_ref()
                .and_then(|publisher| message(publisher.0, event.0));
            if msg.is_none() {
                read.unreadable += 1;
            }
            visitor(EventRecord {
                log: xml.channel,
                time: xml.time.time,
                record_id: xml.record_id,
                level: xml.level,
                provider: xml.provider.name,
                id: xml.id,
                machine: xml.computer,
                message: msg,
            })
            .map_err(|message| NativeError {
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
    #[test]
    fn native_event_xml_handles_namespaces_and_attributes() {
        let text = r#"<Event xmlns="http://schemas.microsoft.com/win/2004/08/events/event"><System><Provider Name="Application Error" Guid="id"/><EventID Qualifiers="0">1000</EventID><Level>2</Level><TimeCreated SystemTime="2026-11-08T01:02:03.123Z"/><EventRecordID>42</EventRecordID><Channel>Application</Channel><Computer>test</Computer></System></Event>"#;
        let event: super::XmlEvent = quick_xml::de::from_str(text).unwrap();
        assert_eq!(event.system.id, 1000);
        assert_eq!(event.system.record_id, 42);
        assert_eq!(event.system.provider.name, "Application Error");
    }
}
