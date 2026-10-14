use async_trait::async_trait;
use serde_json::Value;

use crate::v2::domain::errors::DomainResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    pub path: String,
    pub reason: String,
}

#[async_trait]
pub trait SchemaValidator: Send + Sync {
    async fn validate(
        &self,
        schema: &Value,
        instance: &Value,
    ) -> DomainResult<Vec<SchemaViolation>>;
}
