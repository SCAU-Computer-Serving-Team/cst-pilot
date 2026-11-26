use pi_agent::AgentToolResult;
use pi_ai::{TextContent, UserContentBlock};
use serde_json::{Map, Value, json};

use crate::tools::MAX_OUTPUT_BYTES;

fn is_protected_key(key: &str) -> bool {
    matches!(
        key,
        "elapsedMs"
            | "outputTruncated"
            | "originalBytes"
            | "outputNotice"
            | "degraded"
            | "degradedFrom"
            | "error"
            | "notice"
            | "file"
            | "dir"
            | "name"
            | "runbookId"
            | "version"
            | "sequence"
            | "action"
            | "level"
            | "levelLabel"
            | "items"
            | "path"
            | "kind"
            | "mimeType"
            | "imageAttached"
            | "responseId"
            | "provider"
            | "mode"
    )
}

pub fn serialize_bounded_json(value: &Value) -> Result<String, String> {
    let original = serde_json::to_string(value).map_err(|_| "诊断结果无法编码。")?;
    if original.len() <= MAX_OUTPUT_BYTES {
        return Ok(original);
    }
    for limit in [256, 128, 64, 32, 16, 8, 4, 2, 1, 0] {
        let mut reduced = reduce(value, limit, 0, false, true);
        attach_truncation_meta(&mut reduced, original.len());
        let text = serde_json::to_string(&reduced).map_err(|_| "诊断结果无法编码。")?;
        if text.len() <= MAX_OUTPUT_BYTES {
            return Ok(text);
        }
    }
    // Second pass: if protected strings themselves caused overflow at limit=0, allow trimming
    // protected strings while keeping essential paths/IDs/elapsedMs.
    for limit in [64, 32, 16, 8, 4, 2, 1, 0] {
        let mut reduced = reduce(value, limit, 0, false, false);
        attach_truncation_meta(&mut reduced, original.len());
        let text = serde_json::to_string(&reduced).map_err(|_| "诊断结果无法编码。")?;
        if text.len() <= MAX_OUTPUT_BYTES {
            return Ok(text);
        }
    }
    let mut fallback = Map::new();
    fallback.insert("outputTruncated".into(), json!(true));
    fallback.insert("originalBytes".into(), json!(original.len()));
    fallback.insert(
        "outputNotice".into(),
        json!("结果字段过多，请缩小查询范围。"),
    );
    if let Some(obj) = value.as_object() {
        for key in ["elapsedMs", "degraded", "degradedFrom", "error"] {
            if let Some(val) = obj.get(key) {
                fallback.insert(key.into(), val.clone());
            }
        }
        if let Some(rb) = obj.get("runbook").and_then(Value::as_object) {
            let mut rb_min = Map::new();
            for key in [
                "runbookId",
                "sequence",
                "version",
                "action",
                "file",
                "dir",
                "name",
                "level",
                "levelLabel",
                "items",
            ] {
                if let Some(val) = rb.get(key) {
                    rb_min.insert(key.into(), val.clone());
                }
            }
            if !rb_min.is_empty() {
                fallback.insert("runbook".into(), Value::Object(rb_min));
            }
        }
    }
    let text = serde_json::to_string(&Value::Object(fallback)).map_err(|_| "诊断结果无法编码。")?;
    if text.len() <= MAX_OUTPUT_BYTES {
        return Ok(text);
    }
    let mut minimal = json!({
        "outputTruncated": true,
        "originalBytes": original.len(),
        "outputNotice": "结果字段过多，请缩小查询范围。"
    });
    if let Some(ms) = value.get("elapsedMs") {
        minimal["elapsedMs"] = ms.clone();
    }
    Ok(minimal.to_string())
}

fn attach_truncation_meta(reduced: &mut Value, original_bytes: usize) {
    if let Some(object) = reduced.as_object_mut() {
        object.insert("outputTruncated".into(), json!(true));
        object.insert("originalBytes".into(), json!(original_bytes));
        object.insert(
            "outputNotice".into(),
            json!("模型文本已裁剪；完整结果保留在界面中。"),
        );
    }
}

/// UI details retain the source object; only model-facing JSON is reduced.
pub fn diagnostic_result(value: Value) -> Result<AgentToolResult, String> {
    let text = serialize_bounded_json(&value)?;
    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details: value,
        structured_content: None,
        usage: None,
        is_error: false,
        terminate: None,
    })
}

pub fn error_result(message: impl Into<String>, elapsed_ms: u64) -> AgentToolResult {
    let msg = message.into();
    let value = json!({
        "error": msg,
        "elapsedMs": elapsed_ms,
    });
    let text = serialize_bounded_json(&value).unwrap_or_else(|_| {
        json!({
            "error": "工具执行失败。",
            "elapsedMs": elapsed_ms,
        })
        .to_string()
    });
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details: value,
        structured_content: None,
        usage: None,
        is_error: true,
        terminate: None,
    }
}

pub fn inject_elapsed_ms(mut result: AgentToolResult, elapsed_ms: u64) -> AgentToolResult {
    match &mut result.details {
        Value::Object(obj) => {
            obj.insert("elapsedMs".into(), json!(elapsed_ms));
        }
        Value::Null => {
            result.details = json!({ "elapsedMs": elapsed_ms });
        }
        _ => {}
    }

    if let Some(UserContentBlock::Text(first)) = result.content.first_mut() {
        if let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(&first.text) {
            obj.insert("elapsedMs".into(), json!(elapsed_ms));
            if let Ok(bounded) = serialize_bounded_json(&Value::Object(obj)) {
                first.text = bounded;
            }
        } else if result.is_error {
            let msg = first.text.clone();
            return error_result(msg, elapsed_ms);
        }
    }
    result
}

fn reduce(
    value: &Value,
    limit: usize,
    depth: usize,
    protected: bool,
    keep_protected_strings: bool,
) -> Value {
    if depth > 32 {
        return Value::Null;
    }
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .take(limit)
                .map(|v| reduce(v, limit, depth + 1, false, keep_protected_strings))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| {
                    let child_protected = is_protected_key(k);
                    (
                        k.clone(),
                        reduce(v, limit, depth + 1, child_protected, keep_protected_strings),
                    )
                })
                .collect(),
        ),
        Value::String(text)
            if (!protected || !keep_protected_strings) && text.chars().count() > limit * 8 =>
        {
            json!(text.chars().take(limit * 8).collect::<String>() + "…")
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::ImageContent;

    #[test]
    fn protected_keys_survive_50kib_reduction() {
        let long_cmd = "Get-Process | Where-Object { $_.CPU -gt 10 } # ".repeat(150);
        let commands: Vec<Value> = (0..20)
            .map(|i| {
                json!({
                    "summary": format!("步骤 {i}"),
                    "command": long_cmd,
                    "shell": "powershell",
                    "admin": true
                })
            })
            .collect();
        let value = json!({
            "runbook": {
                "runbookId": "rb-20261126-01",
                "sequence": 1,
                "version": 2,
                "action": "exported",
                "level": "safe",
                "levelLabel": "安全",
                "items": 20,
                "file": "E:\\Learning\\artifacts\\2026-11-26\\runbook\\01-安全-清理缓存-v2.txt",
                "dir": "E:\\Learning\\artifacts\\2026-11-26\\runbook",
                "name": "01-安全-清理缓存-v2.txt",
                "notice": "命令清单已导出，请队员核对后手动执行。",
                "commands": commands
            },
            "elapsedMs": 18
        });
        let result = diagnostic_result(value).unwrap();
        let UserContentBlock::Text(text) = &result.content[0] else {
            panic!("expected text block");
        };
        assert!(text.text.len() <= MAX_OUTPUT_BYTES);
        let parsed: Value = serde_json::from_str(&text.text).unwrap();
        assert_eq!(parsed["outputTruncated"], true);
        assert_eq!(parsed["elapsedMs"], 18);
        assert_eq!(parsed["runbook"]["runbookId"], "rb-20261126-01");
        assert_eq!(
            parsed["runbook"]["file"],
            "E:\\Learning\\artifacts\\2026-11-26\\runbook\\01-安全-清理缓存-v2.txt"
        );
        assert_eq!(parsed["runbook"]["name"], "01-安全-清理缓存-v2.txt");
        assert_eq!(parsed["runbook"]["items"], 20);
        assert_eq!(
            parsed["runbook"]["notice"],
            "命令清单已导出，请队员核对后手动执行。"
        );
    }

    #[test]
    fn inject_elapsed_ms_preserves_image_block_and_formats_errors() {
        let img = AgentToolResult {
            content: vec![
                UserContentBlock::Text(TextContent::new(
                    json!({"path": "C:\\a.png", "kind": "image"}).to_string(),
                )),
                UserContentBlock::Image(ImageContent {
                    data: "AA==".into(),
                    mime_type: "image/png".into(),
                }),
            ],
            details: json!({"path": "C:\\a.png", "kind": "image"}),
            structured_content: None,
            usage: None,
            is_error: false,
            terminate: None,
        };
        let injected = inject_elapsed_ms(img, 42);
        assert_eq!(injected.content.len(), 2);
        assert!(matches!(injected.content[1], UserContentBlock::Image(_)));
        assert_eq!(injected.details["elapsedMs"], 42);
        let UserContentBlock::Text(t) = &injected.content[0] else {
            panic!("expected text");
        };
        let parsed: Value = serde_json::from_str(&t.text).unwrap();
        assert_eq!(parsed["elapsedMs"], 42);

        let err = error_result("权限不足", 7);
        assert!(err.is_error);
        assert_eq!(err.details["error"], "权限不足");
        assert_eq!(err.details["elapsedMs"], 7);
    }
}
