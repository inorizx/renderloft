//! 受限的远端 HTTP 读取能力（结果/产物拉取）。
//!
//! 实现侧负责 SSRF 防护（协议白名单、内网地址拒绝）、拒绝重定向、
//! 响应体大小上限与错误映射。transports / application 只依赖本 trait，
//! 不直接依赖 infrastructure 与 reqwest，避免越层。

use crate::v2::domain::errors::DomainResult;

#[async_trait::async_trait]
pub trait SafeHttpClient: Send + Sync {
    /// 校验 URL 后拉取响应体；失败时返回已脱敏的 `SafeError`。
    async fn fetch_limited(&self, url: &str, max_bytes: usize) -> DomainResult<Vec<u8>>;

    /// 单条结果允许的最大字节数（与实现侧常量对齐）。
    fn max_result_bytes(&self) -> usize;
}
