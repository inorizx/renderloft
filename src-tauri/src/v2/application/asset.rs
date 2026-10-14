//! Asset 写操作用例：本地创建（校验、大小限制、checksum）。

use std::sync::Arc;

use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::hash::fnv1a_checksum;
use crate::v2::domain::ids::AssetId;
use crate::v2::domain::media::{MediaKind, MediaObject, MediaObjectId};
use crate::v2::ports::repositories::AssetRepository;

pub const MAX_ASSET_BYTES: usize = 100 * 1024 * 1024;

pub struct AssetDependencies {
    pub assets: Arc<dyn AssetRepository>,
}

pub struct CreateAssetRequest {
    pub actor: ActorRef,
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

pub struct CreateAsset {
    pub dependencies: AssetDependencies,
}

impl CreateAsset {
    pub fn new(dependencies: AssetDependencies) -> Self {
        Self { dependencies }
    }

    pub async fn execute(&self, request: CreateAssetRequest) -> DomainResult<AssetId> {
        if !request.actor.has_scope(Scope::AssetsWrite) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "缺少 assets:write 权限",
            ));
        }
        if request.filename.trim().is_empty() {
            return Err(SafeError::validation(
                "FILENAME_REQUIRED",
                "filename 不能为空",
            ));
        }
        if request.bytes.len() > MAX_ASSET_BYTES {
            return Err(SafeError::validation(
                "ASSET_TOO_LARGE",
                "Asset 不能超过 100 MB",
            ));
        }
        let checksum = format!("sha1-{:x}", fnv1a_checksum(&request.bytes));
        let media = MediaObject {
            id: MediaObjectId::Asset(AssetId(0)),
            kind: MediaKind::Asset,
            filename: request.filename,
            mime_type: request.mime_type,
            byte_size: request.bytes.len() as u64,
            checksum: checksum.clone(),
            storage_key: None,
            provider_refs: vec![],
        };
        self.dependencies
            .assets
            .create_asset(media, request.bytes)
            .await
    }
}
