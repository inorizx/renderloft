//! Actor token 服务端口：签发、认证、列示、撤销。
//!
//! `infrastructure::token_service::TokenService` 实现本端口；HTTP
//! transport 与 daemon 组合根只依赖本 trait，不直接 import
//! infrastructure 的具体类型。

use async_trait::async_trait;

use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
use crate::v2::domain::errors::DomainResult;
use crate::v2::ports::repositories::ActorTokenRecord;

#[async_trait]
pub trait TokenService: Send + Sync {
    /// 签发新令牌，返回明文令牌与记录（记录只存哈希）。
    async fn issue(
        &self,
        actor_id: &str,
        kind: ActorKind,
        client_id: &str,
        scopes: Vec<Scope>,
        label: Option<String>,
        ttl_seconds: Option<u64>,
    ) -> DomainResult<(String, ActorTokenRecord)>;

    /// 校验令牌并还原 actor。
    async fn authenticate(&self, token: &str) -> DomainResult<ActorRef>;

    /// 列出全部令牌记录。
    async fn list(&self) -> DomainResult<Vec<ActorTokenRecord>>;

    /// 撤销令牌。
    async fn revoke(&self, token_id: i64) -> DomainResult<()>;
}
