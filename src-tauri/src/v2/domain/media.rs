use serde::{Deserialize, Serialize};

use super::errors::SafeError;
use super::ids::{ArtifactId, AssetId, RunId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    #[default]
    Asset,
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaObject {
    pub id: MediaObjectId,
    pub kind: MediaKind,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub storage_key: Option<String>,
    pub provider_refs: Vec<ProviderObjectRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MediaObjectId {
    Asset(AssetId),
    Artifact(ArtifactId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderObjectRef {
    pub provider: String,
    pub remote_id: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub asset_id: AssetId,
    pub checksum: String,
}

impl AssetRef {
    pub fn new(asset_id: AssetId, checksum: impl Into<String>) -> Self {
        Self {
            asset_id,
            checksum: checksum.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub run_id: RunId,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub storage_key: String,
    pub provider_refs: Vec<ProviderObjectRef>,
}

impl Artifact {
    pub fn validate(&self) -> Result<(), SafeError> {
        if self.filename.trim().is_empty() {
            return Err(SafeError::validation(
                "ARTIFACT_FILENAME_REQUIRED",
                "Artifact 必须包含文件名",
            ));
        }
        if self.checksum.trim().is_empty() {
            return Err(SafeError::validation(
                "ARTIFACT_CHECKSUM_REQUIRED",
                "Artifact 必须包含校验和",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_validation_requires_filename_and_checksum() {
        let artifact = Artifact {
            id: ArtifactId(1),
            run_id: RunId(1),
            filename: String::new(),
            mime_type: "image/png".to_string(),
            byte_size: 0,
            checksum: String::new(),
            storage_key: "key".to_string(),
            provider_refs: vec![],
        };
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn artifact_validation_accepts_valid_artifact() {
        let artifact = Artifact {
            id: ArtifactId(1),
            run_id: RunId(1),
            filename: "out.png".to_string(),
            mime_type: "image/png".to_string(),
            byte_size: 10,
            checksum: "abc".to_string(),
            storage_key: "key".to_string(),
            provider_refs: vec![],
        };
        assert!(artifact.validate().is_ok());
    }
}
