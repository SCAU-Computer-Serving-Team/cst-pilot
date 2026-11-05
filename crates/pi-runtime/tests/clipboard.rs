use std::collections::BTreeMap;
use std::sync::Mutex;

use pi_runtime::{
    ClipboardTextWriter, MAX_OSC52_BYTES, copy_to_clipboard_with, parse_clipboard_file_paths,
    try_encode_osc52_clipboard_sequence,
};

struct RecordingClipboard {
    text: Mutex<Option<String>>,
    available: bool,
}
impl ClipboardTextWriter for RecordingClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        if !self.available {
            return Err("test clipboard unavailable".to_owned());
        }
        *self.text.lock().unwrap() = Some(text.to_owned());
        Ok(())
    }
}

#[test]
fn remote_copy_writes_native_text_and_the_exact_osc52_payload() {
    let clipboard = RecordingClipboard {
        text: Mutex::new(None),
        available: true,
    };
    let mut terminal = Vec::new();
    copy_to_clipboard_with(
        "中文 😀\ntext",
        &clipboard,
        &mut terminal,
        &BTreeMap::from([("SSH_CONNECTION".to_owned(), "test".to_owned())]),
    )
    .unwrap();
    assert_eq!(
        clipboard.text.lock().unwrap().as_deref(),
        Some("中文 😀\ntext")
    );
    assert_eq!(
        String::from_utf8(terminal).unwrap(),
        try_encode_osc52_clipboard_sequence("中文 😀\ntext").unwrap()
    );
}

#[test]
fn osc52_only_copy_rejects_oversized_encoded_data_without_truncating() {
    let clipboard = RecordingClipboard {
        text: Mutex::new(None),
        available: false,
    };
    let env = BTreeMap::from([("PI_CLIPBOARD_OSC52".to_owned(), "1".to_owned())]);
    let mut terminal = Vec::new();
    copy_to_clipboard_with("exact copy", &clipboard, &mut terminal, &env).unwrap();
    assert!(!terminal.is_empty());
    terminal.clear();
    assert!(
        copy_to_clipboard_with(
            &"x".repeat(MAX_OSC52_BYTES),
            &clipboard,
            &mut terminal,
            &env
        )
        .is_err()
    );
    assert!(terminal.is_empty());
    assert!(try_encode_osc52_clipboard_sequence(&"x".repeat(MAX_OSC52_BYTES / 4 * 3)).is_ok());
    assert!(try_encode_osc52_clipboard_sequence(&"x".repeat(MAX_OSC52_BYTES / 4 * 3 + 1)).is_err());
}

#[test]
fn desktop_copy_does_not_write_control_sequences_when_native_write_succeeds() {
    let clipboard = RecordingClipboard {
        text: Mutex::new(None),
        available: true,
    };
    let mut terminal = Vec::new();
    copy_to_clipboard_with(
        "local",
        &clipboard,
        &mut terminal,
        &BTreeMap::from([("DISPLAY".to_owned(), ":0".to_owned())]),
    )
    .unwrap();
    assert!(terminal.is_empty());
}

#[test]
fn copied_file_lists_accept_local_file_uris_only() {
    #[cfg(windows)]
    let uri = "file:///C:/Users/test/%E4%B8%AD%E6%96%87%20file.txt";
    #[cfg(not(windows))]
    let uri = "file:///home/test/%E4%B8%AD%E6%96%87%20file.txt";
    let paths = parse_clipboard_file_paths(&format!(
        "copy\n# comment\n{uri}\nhttps://example.com/remote\nfile://remote.invalid/secret\n"
    ));
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("中文 file.txt"));
}
