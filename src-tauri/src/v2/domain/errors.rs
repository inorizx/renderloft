use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorKind {
    Validation,
    Auth,
    Authorization,
    NotFound,
    Conflict,
    Storage,
    Network,
    Timeout,
    Provider,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeError {
    pub code: String,
    pub kind: ErrorKind,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub details: BTreeMap<String, Value>,
}

impl SafeError {
    pub fn validation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::Validation,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::NotFound,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::Conflict,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self {
            code: "STORAGE_ERROR".to_string(),
            kind: ErrorKind::Storage,
            message: message.into(),
            retryable: true,
            details: BTreeMap::new(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "INTERNAL_ERROR".to_string(),
            kind: ErrorKind::Internal,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn provider(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::Provider,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn network(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::Network,
            message: message.into(),
            retryable: true,
            details: BTreeMap::new(),
        }
    }

    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::Validation,
            message: message.into(),
            retryable: true,
            details: BTreeMap::new(),
        }
    }

    pub fn timeout(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            kind: ErrorKind::Timeout,
            message: message.into(),
            retryable: true,
            details: BTreeMap::new(),
        }
    }

    /// 附加结构化细节（如 Provider 的节点信息、远端状态码等）。
    pub fn with_details(mut self, details: BTreeMap<String, Value>) -> Self {
        self.details = details;
        self
    }
}

impl std::fmt::Display for SafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SafeError {}

pub type DomainResult<T> = Result<T, SafeError>;
