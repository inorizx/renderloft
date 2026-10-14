use async_trait::async_trait;

use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::errors::DomainResult;
use crate::v2::domain::media::AssetRef;

#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub reason: Option<String>,
}

#[async_trait]
pub trait AuthorizationPolicy: Send + Sync {
    async fn require_scope(&self, actor: &ActorRef, scope: Scope) -> DomainResult<PolicyDecision>;
}

#[async_trait]
pub trait AssetPolicy: Send + Sync {
    async fn validate_asset_ref(&self, asset_ref: &AssetRef) -> DomainResult<PolicyDecision>;
}
