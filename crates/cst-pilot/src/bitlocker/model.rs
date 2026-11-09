use crate::diagnostics::win_native::NativeError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Status,
    Unlock,
    Decrypt,
}
impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Unlock => "unlock",
            Self::Decrypt => "decrypt",
        }
    }
}
// No Debug implementation: tool parameters may contain a credential.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    #[serde(default)]
    pub scope: Scope,
    pub drive: Option<String>,
    pub recovery_password: Option<String>,
}
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Protector {
    pub id: String,
    pub r#type: Option<u32>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub status: String,
    pub reason: String,
    pub return_code: Option<u32>,
    pub error: Option<NativeError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
}
impl Operation {
    pub fn new(status: &str, reason: &str) -> Self {
        Self {
            status: status.into(),
            reason: reason.into(),
            return_code: None,
            error: None,
            credential_source: None,
        }
    }
    pub fn code(scope: Scope, code: u32) -> Self {
        let method = if scope == Scope::Unlock {
            "UnlockWithNumericalPassword"
        } else {
            "Decrypt"
        };
        Self {
            return_code: Some(code),
            error: (code != 0).then(|| NativeError {
                source: method.into(),
                code: Some(code),
                message: "Windows 操作返回失败".into(),
            }),
            ..Self::new(
                if code == 0 { "accepted" } else { "rejected" },
                if code == 0 {
                    "Windows 已接受请求"
                } else {
                    "Windows 拒绝请求"
                },
            )
        }
    }
}
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub volume_id: String,
    pub drive: Option<String>,
    pub volume_type: Option<u32>,
    pub conversion_status: Option<u32>,
    pub encryption_percentage: Option<u32>,
    pub encryption_method: Option<u32>,
    pub protection_status: Option<u32>,
    pub lock_status: Option<u32>,
    pub auto_unlock_enabled: Option<bool>,
    pub auto_unlock_keys_stored: Option<bool>,
    pub key_protectors: Vec<Protector>,
    pub collection_errors: Vec<NativeError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<Operation>,
}
impl Volume {
    pub fn unavailable(&self) -> Self {
        Self {
            volume_id: self.volume_id.clone(),
            drive: self.drive.clone(),
            volume_type: self.volume_type,
            collection_errors: vec![NativeError {
                source: "postOperation".into(),
                code: None,
                message: "操作后未获取目标卷状态，请重新查询。".into(),
            }],
            ..Self::default()
        }
    }
    pub fn reason(&self, scope: Scope) -> Option<Operation> {
        let reason = match scope {
            Scope::Status => return None,
            Scope::Unlock => match self.lock_status {
                Some(0) => ("skipped", "卷已解锁"),
                Some(1) => return None,
                _ => ("rejected", "锁定状态采集失败"),
            },
            Scope::Decrypt => match (self.conversion_status, self.lock_status) {
                (Some(0), _) => ("skipped", "卷已完全解密"),
                (Some(3), _) => ("skipped", "卷正在解密"),
                (_, Some(1)) => ("rejected", "卷已锁定"),
                (Some(1 | 2 | 4 | 5), Some(0)) => return None,
                _ => ("rejected", "执行所需状态缺失或不支持"),
            },
        };
        Some(Operation::new(reason.0, reason.1))
    }
}
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub collected_at: String,
    pub admin: bool,
    pub volumes: Vec<Volume>,
    pub collection_errors: Vec<NativeError>,
}
pub fn normalize_id(id: &str) -> Result<String, String> {
    let id = id.trim().trim_end_matches(['\\', '/']).to_uppercase();
    if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
        return Err("目标卷身份无效。".into());
    }
    Ok(id)
}
pub fn normalize_drive(drive: Option<&str>) -> Result<Option<String>, String> {
    drive
        .map(|d| {
            let bytes = d.as_bytes();
            if (bytes.len() == 1 || bytes.len() == 2 && bytes[1] == b':')
                && bytes[0].is_ascii_alphabetic()
            {
                Ok(format!("{}:", char::from(bytes[0].to_ascii_uppercase())))
            } else {
                Err("drive 必须为盘符，如 C 或 C:。".into())
            }
        })
        .transpose()
}
