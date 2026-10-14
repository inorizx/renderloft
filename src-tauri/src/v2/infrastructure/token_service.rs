use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::ClientId;
use crate::v2::domain::model::Timestamp;
use crate::v2::ports::repositories::{ActorTokenRecord, TokenStore};

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub struct TokenService {
    pub store: Arc<dyn TokenStore>,
}

impl TokenService {
    pub fn new(store: Arc<dyn TokenStore>) -> Self {
        Self { store }
    }

    pub async fn issue(
        &self,
        actor_id: &str,
        kind: ActorKind,
        client_id: &str,
        scopes: Vec<Scope>,
        label: Option<String>,
        ttl_seconds: Option<u64>,
    ) -> DomainResult<(String, ActorTokenRecord)> {
        let token = format!("rh-{}-{}", actor_id, uuid::Uuid::new_v4().simple());
        let expires_at = ttl_seconds
            .map(|seconds| Timestamp::from_millis(Timestamp::now_millis() + seconds * 1000));
        let record = ActorTokenRecord {
            id: 0,
            token_hash: sha256_hex(&token),
            actor_id: actor_id.to_string(),
            kind,
            client_id: client_id.to_string(),
            scopes,
            label,
            expires_at,
            revoked: false,
            created_at: Timestamp::now(),
        };
        let mut record = record;
        record.id = self.store.create(&record).await?;
        Ok((token, record))
    }

    pub async fn authenticate(&self, token: &str) -> DomainResult<ActorRef> {
        let record = self
            .store
            .find_by_hash(&sha256_hex(token))
            .await?
            .ok_or_else(|| SafeError {
                code: "TOKEN_INVALID".to_string(),
                kind: crate::v2::domain::errors::ErrorKind::Auth,
                message: "认证令牌无效".to_string(),
                retryable: false,
                details: Default::default(),
            })?;
        if record.revoked {
            return Err(SafeError {
                code: "TOKEN_REVOKED".to_string(),
                kind: crate::v2::domain::errors::ErrorKind::Auth,
                message: "认证令牌已被撤销".to_string(),
                retryable: false,
                details: Default::default(),
            });
        }
        if let Some(expires_at) = &record.expires_at {
            if expires_at.is_expired() {
                return Err(SafeError {
                    code: "TOKEN_EXPIRED".to_string(),
                    kind: crate::v2::domain::errors::ErrorKind::Auth,
                    message: "认证令牌已过期".to_string(),
                    retryable: false,
                    details: Default::default(),
                });
            }
        }
        Ok(ActorRef {
            id: record.actor_id,
            kind: record.kind,
            client_id: ClientId(record.client_id),
            scopes: record.scopes,
            label: record.label,
        })
    }
}

#[async_trait::async_trait]
impl crate::v2::ports::token::TokenService for TokenService {
    async fn issue(
        &self,
        actor_id: &str,
        kind: ActorKind,
        client_id: &str,
        scopes: Vec<Scope>,
        label: Option<String>,
        ttl_seconds: Option<u64>,
    ) -> DomainResult<(String, ActorTokenRecord)> {
        TokenService::issue(self, actor_id, kind, client_id, scopes, label, ttl_seconds).await
    }

    async fn authenticate(&self, token: &str) -> DomainResult<ActorRef> {
        TokenService::authenticate(self, token).await
    }

    async fn list(&self) -> DomainResult<Vec<ActorTokenRecord>> {
        self.store.list().await
    }

    async fn revoke(&self, token_id: i64) -> DomainResult<()> {
        self.store.revoke(token_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::ActorKind;
    use crate::v2::infrastructure::fakes::InMemoryRepositories;

    #[tokio::test]
    async fn issued_token_authenticates_with_scopes() {
        let repositories = InMemoryRepositories::new();
        let service = TokenService::new(Arc::new(repositories.clone()));
        let (token, record) = service
            .issue(
                "actor-1",
                ActorKind::Cli,
                "client-1",
                vec![Scope::OperationsExecute],
                Some("cli".to_string()),
                None,
            )
            .await
            .expect("签发应成功");
        assert!(!record.token_hash.starts_with("rh"));
        let actor = service.authenticate(&token).await.expect("认证应成功");
        assert_eq!(actor.id, "actor-1");
        assert!(actor.has_scope(Scope::OperationsExecute));
    }

    #[tokio::test]
    async fn revoked_token_is_rejected() {
        let repositories = InMemoryRepositories::new();
        let service = TokenService::new(Arc::new(repositories.clone()));
        let (token, record) = service
            .issue("actor-1", ActorKind::Cli, "client-1", vec![], None, None)
            .await
            .expect("签发应成功");
        repositories.revoke(record.id).await.expect("撤销应成功");
        let error = service
            .authenticate(&token)
            .await
            .expect_err("已撤销令牌应被拒绝");
        assert_eq!(error.code, "TOKEN_REVOKED");
    }

    #[tokio::test]
    async fn ttl_sets_expiry_on_issued_token() {
        let repositories = InMemoryRepositories::new();
        let service = TokenService::new(Arc::new(repositories));
        let (_, record) = service
            .issue(
                "actor-1",
                ActorKind::Cli,
                "client-1",
                vec![],
                None,
                Some(3600),
            )
            .await
            .expect("签发应成功");
        let expires_at = record.expires_at.expect("有 ttl 时应设置过期时间");
        assert!(expires_at.millis() > Timestamp::now_millis());
    }

    #[tokio::test]
    async fn unknown_token_is_rejected() {
        let repositories = InMemoryRepositories::new();
        let service = TokenService::new(Arc::new(repositories));
        let error = service
            .authenticate("rh-unknown-token")
            .await
            .expect_err("未知令牌应被拒绝");
        assert_eq!(error.code, "TOKEN_INVALID");
    }
}
