use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, Weak};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ClientError;

/// Maximum compressed payload size per single message submission (`12 MiB`).
pub const MAX_SUBMISSION_IMAGE_BYTES: usize = 12 * 1024 * 1024;
/// Maximum width or height of a single image in pixels (`8192 px`).
pub const MAX_IMAGE_DIMENSION_PX: u32 = 8192;
/// Maximum decoded RGBA frame bytes of a single image (`36 MiB`, leaving headroom inside the `48 MiB` global budget).
pub const MAX_SINGLE_DECODED_BYTES: usize = 36 * 1024 * 1024;
/// Global memory budget across all live client image handles (`48 MiB`).
pub const DEFAULT_GLOBAL_IMAGE_BUDGET_BYTES: usize = 48 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceView {
    #[default]
    Gui,
    Tui,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DraftKey {
    NewSession,
    Session(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    #[default]
    Direct,
    Queue,
    Steer,
}

impl SendMode {
    pub fn as_api_delivery(self) -> &'static str {
        match self {
            Self::Direct | Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InlineToken {
    Command { raw: String, name: String },
    FileRef { path: String, display_name: String },
}

struct ImageEntryInner {
    sha256: String,
    mime_type: String,
    file_name: Option<String>,
    compressed_bytes: Arc<[u8]>,
    width: u32,
    height: u32,
    charged_bytes: usize,
    pool: Weak<Mutex<PoolInner>>,
}

impl Drop for ImageEntryInner {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade()
            && let Ok(mut guard) = pool.lock()
        {
            if guard
                .entries
                .get(&self.sha256)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                guard.entries.remove(&self.sha256);
            }
            guard.used_bytes = guard.used_bytes.saturating_sub(self.charged_bytes);
        }
    }
}

/// Reference-counted image buffer shared across `ComposerDraft`, `SessionOutbox`, and image preview.
/// Dropping the last clone automatically releases its compressed + decoded RGBA reservation from `ImageBudgetPool`.
#[derive(Clone)]
pub struct SharedImageHandle(Arc<ImageEntryInner>);

impl std::fmt::Debug for SharedImageHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedImageHandle")
            .field("sha256", &self.0.sha256)
            .field("mime_type", &self.0.mime_type)
            .field("width", &self.0.width)
            .field("height", &self.0.height)
            .field("compressed_len", &self.0.compressed_bytes.len())
            .field("charged_bytes", &self.0.charged_bytes)
            .finish()
    }
}

impl PartialEq for SharedImageHandle {
    fn eq(&self, other: &Self) -> bool {
        self.0.sha256 == other.0.sha256
    }
}
impl Eq for SharedImageHandle {}

impl SharedImageHandle {
    pub fn sha256(&self) -> &str {
        &self.0.sha256
    }

    pub fn mime_type(&self) -> &str {
        &self.0.mime_type
    }

    pub fn file_name(&self) -> Option<&str> {
        self.0.file_name.as_deref()
    }

    pub fn compressed_bytes(&self) -> &[u8] {
        &self.0.compressed_bytes
    }

    pub fn compressed_len(&self) -> usize {
        self.0.compressed_bytes.len()
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.0.width, self.0.height)
    }

    pub fn decoded_rgba_bytes(&self) -> usize {
        (self.0.width as usize)
            .saturating_mul(self.0.height as usize)
            .saturating_mul(4)
    }

    pub fn charged_bytes(&self) -> usize {
        self.0.charged_bytes
    }

    pub fn to_base64_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "mimeType": self.0.mime_type,
            "data": STANDARD.encode(&self.0.compressed_bytes),
        })
    }
}

struct PoolInner {
    budget_bytes: usize,
    used_bytes: usize,
    entries: BTreeMap<String, Weak<ImageEntryInner>>,
}

/// Enforces format validation, single-image dimension caps, and the global `48 MiB` image memory budget.
pub struct ImageBudgetPool {
    inner: Arc<Mutex<PoolInner>>,
}

impl Default for ImageBudgetPool {
    fn default() -> Self {
        Self::with_budget(DEFAULT_GLOBAL_IMAGE_BUDGET_BYTES)
    }
}

impl ImageBudgetPool {
    pub fn with_budget(budget_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                budget_bytes,
                used_bytes: 0,
                entries: BTreeMap::new(),
            })),
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.inner.lock().expect("image pool lock").used_bytes
    }

    pub fn budget_bytes(&self) -> usize {
        self.inner.lock().expect("image pool lock").budget_bytes
    }

    pub fn admit_image(
        &self,
        bytes: Vec<u8>,
        file_name: Option<String>,
    ) -> Result<SharedImageHandle, ClientError> {
        if bytes.is_empty() {
            return Err(ClientError::InvalidImage("图片内容为空。".into()));
        }
        if bytes.len() > MAX_SUBMISSION_IMAGE_BYTES {
            return Err(ClientError::ImageBudgetExceeded(
                "单张图片原始体积超过 12 MiB 上限。".into(),
            ));
        }

        let (format, mime_type) = detect_supported_image_format(&bytes)?;
        let reader = ImageReader::with_format(Cursor::new(&bytes), format);
        let (width, height) = reader
            .into_dimensions()
            .map_err(|_| ClientError::InvalidImage("图片头损坏或尺寸无法解析。".into()))?;
        if width == 0 || height == 0 {
            return Err(ClientError::InvalidImage("图片尺寸无效。".into()));
        }
        if width > MAX_IMAGE_DIMENSION_PX || height > MAX_IMAGE_DIMENSION_PX {
            return Err(ClientError::ImageBudgetExceeded(format!(
                "图片像素尺寸（{width}×{height}）超过 {MAX_IMAGE_DIMENSION_PX}×{MAX_IMAGE_DIMENSION_PX} 安全上限。"
            )));
        }

        let decoded_rgba_bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| ClientError::ImageBudgetExceeded("图片解码内存溢出。".into()))?;
        if decoded_rgba_bytes > MAX_SINGLE_DECODED_BYTES {
            return Err(ClientError::ImageBudgetExceeded(
                "单张图片解码位图内存超出上限，请缩小分辨率后再添加。".into(),
            ));
        }

        let charged_bytes = bytes
            .len()
            .checked_add(decoded_rgba_bytes)
            .ok_or_else(|| ClientError::ImageBudgetExceeded("图片总内存计算溢出。".into()))?;

        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let mut guard = self.inner.lock().expect("image pool lock");
        if let Some(existing) = guard.entries.get(&sha256).and_then(Weak::upgrade) {
            return Ok(SharedImageHandle(existing));
        }

        if guard.used_bytes.saturating_add(charged_bytes) > guard.budget_bytes {
            return Err(ClientError::ImageBudgetExceeded(
                "待发与预览图片占用的总内存已达 48 MiB 上限，请先发送或移除部分图片。".into(),
            ));
        }

        let entry = Arc::new(ImageEntryInner {
            sha256: sha256.clone(),
            mime_type: mime_type.to_owned(),
            file_name,
            compressed_bytes: Arc::from(bytes.into_boxed_slice()),
            width,
            height,
            charged_bytes,
            pool: Arc::downgrade(&self.inner),
        });
        guard.used_bytes += charged_bytes;
        guard.entries.insert(sha256, Arc::downgrade(&entry));
        Ok(SharedImageHandle(entry))
    }
}

fn detect_supported_image_format(bytes: &[u8]) -> Result<(ImageFormat, &'static str), ClientError> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Ok((ImageFormat::Png, "image/png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Ok((ImageFormat::Jpeg, "image/jpeg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok((ImageFormat::Gif, "image/gif"))
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Ok((ImageFormat::WebP, "image/webp"))
    } else {
        Err(ClientError::InvalidImage(
            "仅支持 PNG、JPEG、GIF 或 WebP 格式图片。".into(),
        ))
    }
}

/// UI-agnostic composer draft shared between GUI (`cst-desktop`) and Phase 10 `cst-tui`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerDraft {
    pub text: String,
    pub tokens: Vec<InlineToken>,
    pub images: Vec<SharedImageHandle>,
    pub send_mode: SendMode,
    pub cursor_char_offset: usize,
    pub selected_provider: Option<String>,
    pub selected_model_id: Option<String>,
    pub selected_thinking_level: Option<String>,
}

impl ComposerDraft {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.tokens.is_empty() && self.images.is_empty()
    }

    pub fn has_unsent_content(&self) -> bool {
        !self.is_empty()
    }

    pub fn total_compressed_image_bytes(&self) -> usize {
        self.images
            .iter()
            .map(SharedImageHandle::compressed_len)
            .sum()
    }

    pub fn attach_image(&mut self, handle: SharedImageHandle) -> Result<(), ClientError> {
        let next_compressed = self
            .total_compressed_image_bytes()
            .saturating_add(handle.compressed_len());
        if next_compressed > MAX_SUBMISSION_IMAGE_BYTES {
            return Err(ClientError::ImageBudgetExceeded(
                "单次发送的图片原始字节合计不能超过 12 MiB。".into(),
            ));
        }
        self.images.push(handle);
        Ok(())
    }

    pub fn skill_name(&self) -> Option<&str> {
        self.tokens.iter().find_map(|tok| match tok {
            InlineToken::Command { name, .. } => name.strip_prefix("/skill:"),
            InlineToken::FileRef { .. } => None,
        })
    }
}

/// Shared workspace state owned across GUI and built-in TUI views within one process window.
pub struct WorkspaceState {
    active_view: WorkspaceView,
    active_session_id: Option<String>,
    drafts: BTreeMap<DraftKey, ComposerDraft>,
    edit_revisions: BTreeMap<DraftKey, u64>,
    next_edit_rev: u64,
    image_pool: Arc<ImageBudgetPool>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new(Arc::new(ImageBudgetPool::default()))
    }
}

impl WorkspaceState {
    pub fn new(image_pool: Arc<ImageBudgetPool>) -> Self {
        Self {
            active_view: WorkspaceView::Gui,
            active_session_id: None,
            drafts: BTreeMap::new(),
            edit_revisions: BTreeMap::new(),
            next_edit_rev: 1,
            image_pool,
        }
    }

    fn bump_edit_rev(&mut self, key: &DraftKey) -> u64 {
        let rev = self.next_edit_rev;
        self.next_edit_rev = self.next_edit_rev.wrapping_add(1);
        self.edit_revisions.insert(key.clone(), rev);
        rev
    }

    pub fn draft_edit_revision(&self, key: &DraftKey) -> u64 {
        self.edit_revisions.get(key).copied().unwrap_or(0)
    }

    pub fn image_pool(&self) -> &Arc<ImageBudgetPool> {
        &self.image_pool
    }

    pub fn active_view(&self) -> WorkspaceView {
        self.active_view
    }

    pub fn active_session_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }

    pub fn set_active_session(&mut self, session_id: Option<String>) {
        self.active_session_id = session_id;
    }

    pub fn active_draft_key(&self) -> DraftKey {
        match &self.active_session_id {
            Some(id) => DraftKey::Session(id.clone()),
            None => DraftKey::NewSession,
        }
    }

    pub fn draft(&self, key: &DraftKey) -> Option<&ComposerDraft> {
        self.drafts.get(key)
    }

    pub fn draft_mut(&mut self, key: DraftKey) -> &mut ComposerDraft {
        self.bump_edit_rev(&key);
        self.drafts.entry(key).or_default()
    }

    pub fn take_draft(&mut self, key: &DraftKey) -> ComposerDraft {
        self.bump_edit_rev(key);
        self.drafts.remove(key).unwrap_or_default()
    }

    pub fn restore_draft(&mut self, key: DraftKey, draft: ComposerDraft) {
        self.bump_edit_rev(&key);
        if draft.is_empty() {
            self.drafts.remove(&key);
        } else {
            self.drafts.insert(key, draft);
        }
    }

    /// Restores `draft` into `key` ONLY if `key` currently holds no unsent user edits.
    /// Returns `true` if restored into the composer, or `false` if the user has already started
    /// typing a new draft while the previous request was in flight.
    pub fn restore_draft_if_empty(&mut self, key: DraftKey, draft: ComposerDraft) -> bool {
        if self
            .draft(&key)
            .is_some_and(ComposerDraft::has_unsent_content)
        {
            return false;
        }
        self.restore_draft(key, draft);
        true
    }

    pub fn has_non_empty_draft(&self, session_id: &str) -> bool {
        self.drafts
            .get(&DraftKey::Session(session_id.to_owned()))
            .is_some_and(ComposerDraft::has_unsent_content)
    }

    pub fn has_any_unsent_draft(&self) -> bool {
        self.drafts.values().any(ComposerDraft::has_unsent_content)
    }

    pub fn unsent_draft_count(&self) -> usize {
        self.drafts
            .values()
            .filter(|d| d.has_unsent_content())
            .count()
    }

    /// Commits settled IME text/cursor from the outgoing view before switching to `target`.
    pub fn switch_view(
        &mut self,
        target: WorkspaceView,
        settled_editor_state: Option<(DraftKey, String, usize)>,
    ) {
        if let Some((key, text, cursor_char_offset)) = settled_editor_state {
            let draft = self.draft_mut(key);
            draft.text = text;
            draft.cursor_char_offset = cursor_char_offset.min(draft.text.chars().count());
        }
        self.active_view = target;
    }
}
