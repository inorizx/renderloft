pub mod async_task;
pub mod comfyui;
pub mod openai_images;
pub mod proxy;
pub mod runninghub;
pub mod safety;

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::v2::domain::errors::DomainResult;
    use crate::v2::domain::model::SecretBinding;
    use crate::v2::ports::provider::ProviderDriver;
    use crate::v2::ports::stores::{SecretStore, SecretValue};

    struct NoSecrets;

    #[async_trait::async_trait]
    impl SecretStore for NoSecrets {
        async fn resolve(&self, _binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
            Ok(None)
        }
        async fn set(&self, _slot: &str, _value: &str) -> DomainResult<()> {
            Ok(())
        }
        async fn delete(&self, _slot: &str) -> DomainResult<()> {
            Ok(())
        }
    }

    async fn assert_shared_contract(driver: &dyn ProviderDriver) {
        let descriptor = driver.descriptor();
        assert!(!descriptor.vendor_id.is_empty(), "vendorId 不能为空");
        assert!(!descriptor.protocol_id.is_empty(), "protocolId 不能为空");
        assert!(!descriptor.display_name.is_empty(), "displayName 不能为空");
        assert!(
            !descriptor.business_capabilities.is_empty(),
            "businessCapabilities 不能为空"
        );
        assert!(
            descriptor.connection_schema.is_object(),
            "connectionSchema 必须是 JSON Schema 对象"
        );
        assert!(
            descriptor.binding_schema.is_object(),
            "bindingSchema 必须是 JSON Schema 对象"
        );
        assert!(
            !descriptor.security_requirements.is_empty(),
            "securityRequirements 必须声明"
        );
        // 协议身份必须与 driver key 前缀一致
        assert_eq!(
            crate::v2::ports::provider::driver_key(&descriptor.vendor_id, &descriptor.protocol_id),
            format!("{}/{}", descriptor.vendor_id, descriptor.protocol_id)
        );
    }

    #[tokio::test]
    async fn openai_images_shared_contract() {
        let driver = openai_images::OpenAiImagesDriver::new(Box::new(NoSecrets));
        assert_shared_contract(&driver).await;
    }

    #[tokio::test]
    async fn comfyui_shared_contract() {
        let driver = comfyui::ComfyUiDriver::new(Box::new(NoSecrets));
        assert_shared_contract(&driver).await;
    }

    #[tokio::test]
    async fn runninghub_shared_contract() {
        let driver = runninghub::RunningHubDriver::new(Box::new(NoSecrets));
        assert_shared_contract(&driver).await;
    }
}
