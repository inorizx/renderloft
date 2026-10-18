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

    /// 这次失败**能不能证明「请求字节根本没出去」**？
    ///
    /// 证据由 provider 层落进 `details`（见
    /// `infrastructure::providers::safety::nothing_sent_evidence`：只有**连接被拒 / DNS 解析失败 /
    /// 握手阶段超时**才为真，其余一律"证明不了"）。这里只做**一个**翻译，让上层不必知道那个键名
    /// ——`scheduler` 据此决定"直接退避重试"还是"落等确认"，写错一个字符串就是重复扣费。
    ///
    /// ⚠️ **它是"字节没出去"的证据，不是"可以重投"的许可**：对**模糊类**（发送后超时、连接被
    /// 重置…）重投仍然可能重复计费，那需要平台侧去重（`RecoveryProof` 今天没有生产者）。
    /// 这里只对"证明得了"的那一类给 `true`。
    pub fn proves_nothing_was_sent(&self) -> bool {
        self.details
            .get(NOTHING_SENT_DETAIL)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// `details` 里"字节没出去"那个键的**唯一**定义处（生产者在
/// `infrastructure::providers::safety::map_network_error`，消费者是
/// `SafeError::proves_nothing_was_sent`）。
pub const NOTHING_SENT_DETAIL: &str = "nothingSent";

impl std::fmt::Display for SafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SafeError {}

pub type DomainResult<T> = Result<T, SafeError>;
