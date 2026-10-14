use async_trait::async_trait;
use serde_json::Value;

use crate::v2::domain::errors::DomainResult;

/// 签名头的名字。值形如 `t=<毫秒>,v1=<hex>`，被签名的串是 `"{t}.{请求体原始字节}"`。
///
/// 完整契约（接收端要照着实现的那份）见 [`crate::v2::domain::signing`] 的模块文档。
/// 没配密钥时**不带这个头**——宁可不签，也不签一个假的。
pub const SIGNATURE_HEADER: &str = "x-renderloft-signature";

#[derive(Debug, Clone)]
pub struct DeliveryRequest {
    pub delivery_id: String,
    pub url: String,
    pub payload: Value,
    /// 签名头的值（由 application 层算好；密钥不在这一层碰）。
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Accepted,
    Rejected { message: String },
    NetworkError { message: String, retryable: bool },
}

#[async_trait]
pub trait CallbackDeliverer: Send + Sync {
    async fn deliver(&self, request: DeliveryRequest) -> DomainResult<DeliveryOutcome>;
}
