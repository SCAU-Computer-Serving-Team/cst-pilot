use pi_agent::AgentToolResult;
use pi_ai::{TextContent, UserContentBlock};
use serde_json::{Value, json};

use crate::tools::MAX_OUTPUT_BYTES;

/// UI details retain the source object; only model-facing JSON is reduced.
pub fn diagnostic_result(value: Value) -> Result<AgentToolResult, String> {
    let original = serde_json::to_string(&value).map_err(|_| "诊断结果无法编码。")?;
    let mut text = original.clone();
    if text.len() > MAX_OUTPUT_BYTES {
        for limit in [256, 128, 64, 32, 16, 8, 4, 2, 1, 0] {
            let mut reduced = reduce(&value, limit, 0);
            if let Some(object) = reduced.as_object_mut() {
                object.insert("outputTruncated".into(), json!(true));
                object.insert("originalBytes".into(), json!(original.len()));
                object.insert(
                    "outputNotice".into(),
                    json!("模型文本已裁剪；完整结果保留在界面中。"),
                );
            }
            text = serde_json::to_string(&reduced).map_err(|_| "诊断结果无法编码。")?;
            if text.len() <= MAX_OUTPUT_BYTES {
                break;
            }
        }
        if text.len() > MAX_OUTPUT_BYTES {
            text = json!({"outputTruncated":true,"originalBytes":original.len(),"outputNotice":"结果字段过多，请缩小查询范围。"}).to_string();
        }
    }
    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details: value,
        structured_content: None,
        usage: None,
        is_error: false,
        terminate: None,
    })
}

fn reduce(value: &Value, limit: usize, depth: usize) -> Value {
    if depth > 32 {
        return Value::Null;
    }
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .take(limit)
                .map(|v| reduce(v, limit, depth + 1))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), reduce(v, limit, depth + 1)))
                .collect(),
        ),
        Value::String(text) if text.chars().count() > limit * 8 => {
            json!(text.chars().take(limit * 8).collect::<String>() + "…")
        }
        other => other.clone(),
    }
}
