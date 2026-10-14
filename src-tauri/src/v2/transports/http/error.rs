use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::v2::domain::errors::{ErrorKind, SafeError};

pub fn status_for(error: &SafeError) -> StatusCode {
    match error.code.as_str() {
        "IDEMPOTENCY_CONFLICT" | "LEASE_BUSY" | "LEASE_LOST" => StatusCode::CONFLICT,
        "SCHEMA_CHANGED" => StatusCode::PRECONDITION_FAILED,
        "PROVIDER_RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "SCOPE_REQUIRED" => StatusCode::FORBIDDEN,
        "TOKEN_INVALID" | "TOKEN_EXPIRED" | "TOKEN_REVOKED" => StatusCode::UNAUTHORIZED,
        _ => match error.kind {
            ErrorKind::Validation => StatusCode::BAD_REQUEST,
            ErrorKind::Auth => StatusCode::UNAUTHORIZED,
            ErrorKind::Authorization => StatusCode::FORBIDDEN,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorKind::Provider => StatusCode::BAD_GATEWAY,
            ErrorKind::Network => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Storage | ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

pub fn error_response(error: SafeError) -> axum::response::Response {
    let status = status_for(&error);
    (status, crate::v2::transports::http::dto::error_json(&error)).into_response()
}

pub fn unauthorized(message: impl Into<String>) -> axum::response::Response {
    error_response(SafeError {
        code: "TOKEN_INVALID".to_string(),
        kind: ErrorKind::Auth,
        message: message.into(),
        retryable: false,
        details: Default::default(),
    })
}

pub fn forbidden(message: impl Into<String>) -> axum::response::Response {
    error_response(SafeError {
        code: "SCOPE_REQUIRED".to_string(),
        kind: ErrorKind::Authorization,
        message: message.into(),
        retryable: false,
        details: Default::default(),
    })
}

pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> axum::response::Response {
    error_response(SafeError::not_found(code, message))
}

pub fn validation(code: impl Into<String>, message: impl Into<String>) -> axum::response::Response {
    error_response(SafeError::validation(code, message))
}

pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> axum::response::Response {
    error_response(SafeError::conflict(code, message))
}
