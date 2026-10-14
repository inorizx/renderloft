use async_trait::async_trait;
use std::sync::Arc;

use crate::v2::domain::errors::DomainResult;
use crate::v2::domain::ids::ArtifactId;
use crate::v2::domain::model::SecretBinding;

#[derive(Debug, Clone)]
pub struct ArtifactObject {
    pub id: ArtifactId,
    pub run_id: Option<u64>,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub storage_key: String,
    /// 远端结果 URL。存在时字节按需从远端拉取（中转站定位，不落本地/数据库）；
    /// 为 None 时字节以本地缓存文件形式保存。
    pub url: Option<String>,
    /// 本地字节（仅 url 为 None 时使用，由存储层写入本地缓存文件，不进数据库）。
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait ArtifactStore: Send + Sync {
    async fn store(&self, artifact: ArtifactObject) -> DomainResult<()>;
    async fn load(&self, id: ArtifactId) -> DomainResult<Option<ArtifactObject>>;
    /// 读取本地缓存的文件字节（仅 url 为 None 的产物）。
    async fn read_bytes(&self, id: ArtifactId) -> DomainResult<Option<Vec<u8>>>;
    async fn exists(&self, checksum: &str) -> DomainResult<bool>;
}

#[derive(Debug, Clone)]
pub struct SecretValue {
    pub secret: String,
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn resolve(&self, binding: &SecretBinding) -> DomainResult<Option<SecretValue>>;
    async fn set(&self, slot: &str, value: &str) -> DomainResult<()>;
    async fn delete(&self, slot: &str) -> DomainResult<()>;
}

/// 允许把共享的 `Arc<dyn ArtifactStore>` 作为 `dyn ArtifactStore` 复用。
#[async_trait]
impl ArtifactStore for Arc<dyn ArtifactStore> {
    async fn store(&self, artifact: ArtifactObject) -> DomainResult<()> {
        self.as_ref().store(artifact).await
    }
    async fn load(&self, id: ArtifactId) -> DomainResult<Option<ArtifactObject>> {
        self.as_ref().load(id).await
    }
    async fn read_bytes(&self, id: ArtifactId) -> DomainResult<Option<Vec<u8>>> {
        self.as_ref().read_bytes(id).await
    }
    async fn exists(&self, checksum: &str) -> DomainResult<bool> {
        self.as_ref().exists(checksum).await
    }
}

/// 允许把共享的 `Arc<dyn SecretStore>` 作为 `Box<dyn SecretStore>` 传入多个 driver。
#[async_trait]
impl SecretStore for Arc<dyn SecretStore> {
    async fn resolve(&self, binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
        self.as_ref().resolve(binding).await
    }
    async fn set(&self, slot: &str, value: &str) -> DomainResult<()> {
        self.as_ref().set(slot, value).await
    }
    async fn delete(&self, slot: &str) -> DomainResult<()> {
        self.as_ref().delete(slot).await
    }
}
