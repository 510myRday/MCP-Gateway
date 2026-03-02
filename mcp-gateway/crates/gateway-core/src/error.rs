use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthorized,
    NotFound,
    Conflict,
    ValidationFailed,
    BadRequest,
    UpstreamFailed,
    Internal,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("upstream failed: {0}")]
    Upstream(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Unauthorized(_) => ErrorCode::Unauthorized,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::Validation(_) => ErrorCode::ValidationFailed,
            Self::BadRequest(_) => ErrorCode::BadRequest,
            Self::Upstream(_) => ErrorCode::UpstreamFailed,
            Self::Io(_) | Self::Json(_) | Self::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unauthorized(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::Validation(m)
            | Self::BadRequest(m)
            | Self::Upstream(m)
            | Self::Internal(m) => m.clone(),
            Self::Io(err) => err.to_string(),
            Self::Json(err) => err.to_string(),
        }
    }
}
