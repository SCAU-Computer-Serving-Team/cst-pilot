use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::locks::{StorageError, atomic_write_bytes};

pub const MAX_TOTAL_IMAGE_BYTES: usize = 12 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    pub id: String,
    pub mime_type: String,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub enum AttachmentError {
    InvalidInput(String),
    NotFound,
    Storage(StorageError),
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(msg) => f.write_str(msg),
            Self::NotFound => f.write_str("图片不存在"),
            Self::Storage(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AttachmentError {}

pub fn is_valid_session_id_key(session_id: &str) -> bool {
    (1..=128).contains(&session_id.len())
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn is_valid_sha256_hex(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_strict_standard_base64(input: &str) -> bool {
    if input.is_empty() || !input.len().is_multiple_of(4) {
        return false;
    }
    let bytes = input.as_bytes();
    let pad_count = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if pad_count > 2 {
        return false;
    }
    let body = &bytes[..bytes.len() - pad_count];
    body.iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

pub fn matches_image_magic(bytes: &[u8], mime_type: &str) -> bool {
    match mime_type {
        "image/png" => bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

/// Content-addressed session image store (`agent/home/web-inbox/attachments/<sessionId>/<sha256>`).
/// Enforces PNG/JPEG/GIF/WebP magic signatures and a 12 MiB per-request raw byte cap.
#[derive(Clone)]
pub struct ImageStore {
    root: PathBuf,
}

impl ImageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id)
    }

    pub async fn accept(
        &self,
        session_id: &str,
        images_value: &Value,
    ) -> Result<Vec<ImageRef>, AttachmentError> {
        if !is_valid_session_id_key(session_id) {
            return Err(AttachmentError::InvalidInput("图片会话 ID 无效".to_owned()));
        }
        let Some(items) = images_value.as_array() else {
            return Err(AttachmentError::InvalidInput("图片格式无效".to_owned()));
        };

        let mut remaining = MAX_TOTAL_IMAGE_BYTES;
        let mut decoded_items: Vec<(String, String, Vec<u8>)> = Vec::with_capacity(items.len());

        for item in items {
            let Some(obj) = item.as_object() else {
                return Err(AttachmentError::InvalidInput(
                    "图片格式或体积无效".to_owned(),
                ));
            };
            let Some(mime_type) = obj.get("mimeType").and_then(Value::as_str) else {
                return Err(AttachmentError::InvalidInput(
                    "图片格式或体积无效".to_owned(),
                ));
            };
            if !matches!(
                mime_type,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ) {
                return Err(AttachmentError::InvalidInput(
                    "图片格式或体积无效".to_owned(),
                ));
            }
            let Some(data_b64) = obj.get("data").and_then(Value::as_str) else {
                return Err(AttachmentError::InvalidInput(
                    "图片格式或体积无效".to_owned(),
                ));
            };
            let max_b64_len = (remaining.div_ceil(3)) * 4 + 4;
            if data_b64.len() > max_b64_len || !is_strict_standard_base64(data_b64) {
                return Err(AttachmentError::InvalidInput(
                    "图片格式或体积无效".to_owned(),
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_b64)
                .map_err(|_| AttachmentError::InvalidInput("图片格式或体积无效".to_owned()))?;
            if bytes.is_empty()
                || bytes.len() > remaining
                || !matches_image_magic(&bytes, mime_type)
            {
                return Err(AttachmentError::InvalidInput(
                    "图片格式或体积无效".to_owned(),
                ));
            }
            remaining -= bytes.len();
            let sha_id = format!("{:x}", Sha256::digest(&bytes));
            decoded_items.push((sha_id, mime_type.to_owned(), bytes));
        }

        let mut refs = Vec::with_capacity(decoded_items.len());
        let folder = self.session_dir(session_id);
        for (id, mime_type, bytes) in decoded_items {
            let len = bytes.len();
            let file_path = folder.join(&id);
            atomic_write_bytes(&file_path, bytes, "保存会话图片附件")
                .await
                .map_err(AttachmentError::Storage)?;
            refs.push(ImageRef {
                id,
                mime_type,
                bytes: len,
            });
        }

        Ok(refs)
    }

    pub async fn read(&self, session_id: &str, id: &str) -> Result<Vec<u8>, AttachmentError> {
        if !is_valid_session_id_key(session_id) || !is_valid_sha256_hex(id) {
            return Err(AttachmentError::InvalidInput("图片 ID 无效".to_owned()));
        }
        let path = self.session_dir(session_id).join(id);
        tokio::task::spawn_blocking(move || match fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(AttachmentError::NotFound),
            Err(err) => Err(AttachmentError::Storage(StorageError::from_io(
                &err,
                "读取会话图片附件",
            ))),
        })
        .await
        .map_err(|_| {
            AttachmentError::Storage(StorageError {
                kind: crate::locks::StorageErrorKind::StorageUnavailable,
                message: "读取会话图片附件任务异常中断".to_owned(),
            })
        })?
    }

    pub async fn delete_session_attachments(&self, session_id: &str) {
        if !is_valid_session_id_key(session_id) {
            return;
        }
        let dir = self.session_dir(session_id);
        let _ = tokio::task::spawn_blocking(move || fs::remove_dir_all(dir)).await;
    }
}
