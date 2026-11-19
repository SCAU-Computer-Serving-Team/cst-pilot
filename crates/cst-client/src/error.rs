use serde_json::Value;
use thiserror::Error;

/// Unified client-side error classification for `cst-desktop` and Phase 10 `cst-tui`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClientError {
    #[error("{0}")]
    NotElevated(String),
    #[error("{0}")]
    PrivilegeMismatch(String),
    #[error("{0}")]
    CrossSessionInstance(String),
    #[error("{0}")]
    InvalidServerIdentity(String),
    #[error("{0}")]
    UpgradeRecoveryFailed(String),
    #[error("{0}")]
    BootstrapFailed(String),
    #[error("{0}")]
    Disconnected(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    WebModeActive(String),
    #[error("{0}")]
    UpdateInProgress(String),
    #[error("{message}")]
    CredentialsRequired {
        message: String,
        session_id: Option<String>,
    },
    #[error("{0}")]
    StorageFull(String),
    #[error("{0}")]
    ConfigurationUnavailable(String),
    #[error("{message}")]
    Conflict { code: String, message: String },
    #[error("{message}")]
    BadRequest { code: String, message: String },
    #[error("{message}")]
    NotFound { code: String, message: String },
    #[error("{0}")]
    InvalidImage(String),
    #[error("{0}")]
    ImageBudgetExceeded(String),
    #[error("{message}")]
    Server {
        status: u16,
        code: String,
        message: String,
    },
}

impl ClientError {
    pub fn code(&self) -> &str {
        match self {
            Self::NotElevated(_) => "not_elevated",
            Self::PrivilegeMismatch(_) => "privilege_mismatch",
            Self::CrossSessionInstance(_) => "cross_session_instance",
            Self::InvalidServerIdentity(_) => "invalid_server_identity",
            Self::UpgradeRecoveryFailed(_) => "upgrade_recovery_failed",
            Self::BootstrapFailed(_) => "bootstrap_failed",
            Self::Disconnected(_) => "disconnected",
            Self::Unauthorized(_) => "unauthorized",
            Self::WebModeActive(_) => "web_mode_active",
            Self::UpdateInProgress(_) => "update_in_progress",
            Self::CredentialsRequired { .. } => "credentials_required",
            Self::StorageFull(_) => "storage_full",
            Self::ConfigurationUnavailable(_) => "configuration_unavailable",
            Self::Conflict { code, .. }
            | Self::BadRequest { code, .. }
            | Self::NotFound { code, .. }
            | Self::Server { code, .. } => code.as_str(),
            Self::InvalidImage(_) => "invalid_image",
            Self::ImageBudgetExceeded(_) => "image_budget_exceeded",
        }
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected(_))
    }

    pub fn is_storage_full(&self) -> bool {
        matches!(self, Self::StorageFull(_))
    }

    pub fn is_credentials_required(&self) -> bool {
        matches!(self, Self::CredentialsRequired { .. })
    }

    pub fn from_http_response(status: u16, body: Option<&Value>) -> Self {
        let err_obj = body.and_then(|v| v.get("error")).and_then(Value::as_object);
        let code = err_obj
            .and_then(|o| o.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("server_error")
            .to_owned();
        let message = err_obj
            .and_then(|o| o.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("请求失败（HTTP {status}）"));
        let session_id = err_obj
            .and_then(|o| o.get("sessionId"))
            .and_then(Value::as_str)
            .map(str::to_owned);

        match (status, code.as_str()) {
            (401, _) => Self::Unauthorized(message),
            (_, "web_mode_active") => Self::WebModeActive(message),
            (_, "update_in_progress") => Self::UpdateInProgress(message),
            (_, "credentials_required") => Self::CredentialsRequired {
                message,
                session_id,
            },
            (507, _) | (_, "storage_full" | "storage_unavailable") => Self::StorageFull(message),
            (503, "configuration_unavailable") => Self::ConfigurationUnavailable(message),
            (400, _) => Self::BadRequest { code, message },
            (404, _) => Self::NotFound { code, message },
            (409, _) => Self::Conflict { code, message },
            _ => Self::Server {
                status,
                code,
                message,
            },
        }
    }
}
