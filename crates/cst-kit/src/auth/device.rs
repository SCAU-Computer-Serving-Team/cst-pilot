use super::storage::JsonFiles;

pub(crate) async fn get(files: &JsonFiles) -> Result<String, String> {
    let tx = files.transaction().await?;
    let mut value = tx.read("device.json")?;
    if let Some(id) = value.get("device_id") {
        if let Some(id) = id.as_str().filter(|id| valid(id)) {
            return Ok(id.to_owned());
        }
        return Err("device.json 的设备标识无效，原文件已保留，请修正后重试。".into());
    }
    if !value.as_object().is_some_and(|v| v.is_empty()) {
        return Err("device.json 缺少设备标识，原文件已保留。".into());
    }
    let id = format!("dev_{}", uuid::Uuid::new_v4().simple());
    value["device_id"] = id.clone().into();
    value["created_at"] = pi_ai::now_ms().into();
    tx.write("device.json", &value)?;
    Ok(id)
}
fn valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
