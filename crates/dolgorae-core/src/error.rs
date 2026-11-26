//! Canonical error codes and envelopes for Dolgorae v2.

use dolgorae_spi::DriverError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Canonical 10-code error taxonomy shared across core, HTTP/SSE, and MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    ForbiddenScope,
    NotFound,
    SnapshotConflict,
    DelegationDepthExceeded,
    DelegationCycle,
    PayloadTooLarge,
    DriverUnavailable,
    InternalError,
}

impl ErrorCode {
    /// Returns the canonical `SCREAMING_SNAKE_CASE` wire code string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "BAD_REQUEST",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::ForbiddenScope => "FORBIDDEN_SCOPE",
            Self::NotFound => "NOT_FOUND",
            Self::SnapshotConflict => "SNAPSHOT_CONFLICT",
            Self::DelegationDepthExceeded => "DELEGATION_DEPTH_EXCEEDED",
            Self::DelegationCycle => "DELEGATION_CYCLE",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::DriverUnavailable => "DRIVER_UNAVAILABLE",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }

    /// Returns the canonical HTTP status code for this error class.
    pub fn http_status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::ForbiddenScope => 403,
            Self::NotFound => 404,
            Self::SnapshotConflict | Self::DelegationDepthExceeded | Self::DelegationCycle => 409,
            Self::PayloadTooLarge => 413,
            Self::DriverUnavailable => 503,
            Self::InternalError => 500,
        }
    }
}

/// Wire JSON error body matching `openapi-v2.yaml` `ErrorEnvelope`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Top-level JSON error envelope returned by HTTP and MCP adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

/// Typed domain error returned by all `dolgorae-core` operations.
#[derive(Debug, Error)]
#[error("{}: {message}", code.as_str())]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Value>,
}

impl CoreError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, msg)
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, msg)
    }

    pub fn forbidden_scope(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::ForbiddenScope, msg)
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, msg)
    }

    pub fn snapshot_conflict(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::SnapshotConflict, msg)
    }

    pub fn delegation_depth_exceeded(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::DelegationDepthExceeded, msg)
    }

    pub fn delegation_cycle(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::DelegationCycle, msg)
    }

    pub fn payload_too_large(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::PayloadTooLarge, msg)
    }

    pub fn driver_unavailable(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::DriverUnavailable, msg)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, msg)
    }

    pub fn to_envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message.clone(),
                details: self.details.clone(),
            },
        }
    }
}

impl From<DriverError> for CoreError {
    fn from(err: DriverError) -> Self {
        match err {
            DriverError::Unavailable(m) => Self::driver_unavailable(m),
            DriverError::InvalidConfig(m) | DriverError::Unsupported(m) => Self::bad_request(m),
            DriverError::Protocol(m) | DriverError::ProcessExited(m) | DriverError::Timeout(m) => {
                Self::internal(m)
            }
            DriverError::Io(e) => Self::internal(format!("driver io error: {e}")),
            DriverError::Cancelled => Self::snapshot_conflict("driver operation cancelled"),
        }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(err: std::io::Error) -> Self {
        Self::internal(format!("io error: {err}"))
    }
}
