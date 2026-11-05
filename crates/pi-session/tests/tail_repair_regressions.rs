use pi_session::{JsonlSessionStore, SessionStore, SessionStoreError, TailRepairOutcome};

#[test]
fn tail_repair_only_truncates_incomplete_json() {
    let dir = std::env::temp_dir().join(format!("tail-repair-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let header = serde_json::json!({"type":"session","version":3,"id":"review-probe","timestamp":"2026-11-05T00:00:00.000Z","cwd":"/tmp"});
    for (index, tail) in [
        r#"{"type":"future_entry","id":"keep-me"}"#,
        r#"{"type":"message","id":"invalid-schema"}"#,
        r#"{"broken": invalid}"#,
    ]
    .iter()
    .enumerate()
    {
        for newline in ["", "\n", "\n\n"] {
            let path = dir.join(format!("{index}-{}.jsonl", newline.len()));
            let original = format!("{header}\n{tail}{newline}");
            std::fs::write(&path, &original).unwrap();
            let mut store = JsonlSessionStore::open(&path).unwrap();
            assert!(matches!(
                store.load_entries(),
                Err(SessionStoreError::InvalidSessionFile(_))
            ));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }
    let path = dir.join("incomplete.jsonl");
    std::fs::write(&path, format!("{header}\n{{\"type\":")).unwrap();
    let mut store = JsonlSessionStore::open(&path).unwrap();
    assert_eq!(store.load_entries().unwrap().len(), 1);
    assert!(matches!(
        store.last_tail_repair(),
        Some(TailRepairOutcome::TruncatedIncompleteTail { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{header}\n")
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
