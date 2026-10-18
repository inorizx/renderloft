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

    /// **提交结果不明**吗？= 网络/超时类失败**且我们证明不了字节没出去**，**或者**被显式标成
    /// "提交结果不明"（`SUBMISSION_UNKNOWN_DETAIL`，见下）。
    ///
    /// ⚠️ 这条判据决定钱，而且**只能有一处**：`ErrorKind::Network` 把两件事混在一起——
    /// 「连接被拒」（对端**主动拒绝**握手 ⇒ 一个字节都没发出去 ⇒ 平台什么都没收到）与
    /// 「发出去后超时」（请求可能已经在平台上跑起来了）。前者重投**不可能**重复扣费，
    /// 后者重投就是再花一次钱（`AGENTS.md` 铁律二）。
    ///
    /// 两个消费者，必须给出同一个答案：
    /// * `application::scheduler` 决定"落 `SUBMISSION_UNCERTAIN` 等对账"还是"退避重试"；
    /// * 驱动（如 ComfyUI）决定"报一个可按 id 回查的不确定提交"还是"把错误交回去重试"。
    ///
    /// 此前这两处**各写了一份**判据，实测踩到过：只改一处 ⇒ run 进了 `RETRY_WAIT` 而 tick 的计数
    /// 报 `uncertain`（库里的状态与计数各说各话）。所以判据放在这里，谁都不许再抄一份。
    ///
    /// ⚠️ 第二个来源（2026-10-18 补）：**HTTP 5xx 也是"结果不明"**。它是"平台回了话"，
    /// 但 500 可能是**网关/内部错误发生在受理之后** ⇒ 平台可能已经收下并计费。
    /// 按铁律二它和"发出去后超时"同类，所以**不许盲投**。标记由提交路径上的
    /// `infrastructure::providers::safety::map_submit_http_error` 打（4xx 不打：那是明确拒绝）。
    pub fn submission_is_unknown(&self) -> bool {
        if self
            .details
            .get(SUBMISSION_UNKNOWN_DETAIL)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return true;
        }
        matches!(self.kind, ErrorKind::Network | ErrorKind::Timeout)
            && !self.proves_nothing_was_sent()
    }
}

/// `details` 里"字节没出去"那个键的**唯一**定义处（生产者在
/// `infrastructure::providers::safety::map_network_error`，消费者是
/// `SafeError::proves_nothing_was_sent`）。
pub const NOTHING_SENT_DETAIL: &str = "nothingSent";

/// `details` 里"**这次提交的结果不明**"那个键的**唯一**定义处（生产者在
/// `infrastructure::providers::safety::map_submit_http_error` —— 只给 HTTP 5xx 打，
/// 消费者是 `SafeError::submission_is_unknown`）。
///
/// 与 `NOTHING_SENT_DETAIL` 是**两个相反的证据**，别混：那个说"一个字节都没出去"（⇒ 可以放心重试），
/// 这个说"**可能已经收下了**"（⇒ 不许自动重投）。
pub const SUBMISSION_UNKNOWN_DETAIL: &str = "submissionUnknown";

impl std::fmt::Display for SafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SafeError {}

pub type DomainResult<T> = Result<T, SafeError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn with_nothing_sent(proven: bool) -> SafeError {
        let mut details = BTreeMap::new();
        details.insert(NOTHING_SENT_DETAIL.to_string(), Value::Bool(proven));
        SafeError::network("PROVIDER_UNREACHABLE_SUBMIT", "连不上").with_details(details)
    }

    /// 「提交结果不明」这条判据**决定钱**，所以它自己的三档必须钉死。
    ///
    /// 负对照：把 `submission_is_unknown` 里的 `&& !self.proves_nothing_was_sent()` 删掉
    /// ⇒ 第一条（证明得了"没出去"）会红 —— 那一档本该走自动重试，而不是等死线。
    #[test]
    fn submission_is_unknown_only_when_the_bytes_might_have_left() {
        // ① 网络类 + **证明得了**字节没出去（连接被拒）⇒ 不算"结果不明"：可以安全重试。
        assert!(
            !with_nothing_sent(true).submission_is_unknown(),
            "字节证明没出去就该走退避重试，而不是落等确认"
        );
        // ② 网络类 + 证明不了（发送后超时）⇒ **算**"结果不明"：绝不自动重投。
        assert!(
            with_nothing_sent(false).submission_is_unknown(),
            "可能已经收下就必须落等确认"
        );
        // ③ 超时类 + 证明不了 ⇒ 同上（超时最典型）。
        assert!(SafeError::timeout("PROVIDER_TIMEOUT_SUBMIT", "超时").submission_is_unknown());
        // ④ 非网络类（如被平台明确拒绝的 4xx）⇒ 不是"结果不明"：平台给了答复。
        assert!(!SafeError::validation("PROVIDER_REJECTED", "参数不对").submission_is_unknown());
        assert!(
            !SafeError::provider("RUNNINGHUB_REJECTED", "TASK_QUEUE_MAXED").submission_is_unknown()
        );
    }
}
