use crate::mode::ModeRejection;
use crate::session::receipts::ReceiptError;

/// Service failures independent of the HTTP response representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceErrorKind {
    InvalidInput,
    Conflict,
    NotFound,
    AccessDenied,
    TooLarge,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone)]
pub struct ServiceError {
    pub kind: ServiceErrorKind,
    pub code: &'static str,
    pub message: String,
}

impl ServiceError {
    pub fn new(kind: ServiceErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ServiceError {}

/// Preserves an action's own error type while separating admission and receipt failures.
#[derive(Debug)]
pub enum MutationError<E> {
    InvalidKey,
    Admission(ModeRejection),
    Receipt(ReceiptError),
    Action(E),
}
