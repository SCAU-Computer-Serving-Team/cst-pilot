use super::sse::SseDecoder;
use serde_json::json;

#[test]
fn sse_split_utf8_crlf_bom_and_empty_cursor_are_handled() {
    let mut decoder = SseDecoder::with_cursor(Some("old".to_owned()));
    let wire = "\u{feff}: comment\r\nid: next\r\nevent: message\r\ndata: {\"text\":\"中文\"}\r\n\r\nid:\r\n\r\n";
    let mut messages = Vec::new();
    for byte in wire.as_bytes() {
        messages.extend(decoder.push(&[*byte], false).unwrap());
    }
    assert_eq!(messages, vec![json!({"text":"中文"})]);
    assert_eq!(decoder.last_event_id, None);
    let messages = decoder
        .push(b"retry: 1200\ndata: {\"tail\":true}", true)
        .unwrap();
    assert_eq!(messages, vec![json!({"tail":true})]);
    assert_eq!(decoder.retry_ms, Some(1200));
}
