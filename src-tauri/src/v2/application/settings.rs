//! 本地工作区设置与 RunningHub 账号/队列状态。
//!
//! 设置只保存非机密偏好（下载目录）。**平台凭据一律由 Connection 管理**
//! （密钥进系统凭据库），此处没有任何密钥入口；密钥绝不落库、绝不出现
//! 在日志或错误响应。

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(test)]
use async_trait::async_trait;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::ConnectionId;
use crate::v2::domain::model::{is_api_key_slot, ConnectionRevision};
use crate::v2::ports::provider::{driver_key, DriverRegistry};
use crate::v2::ports::repositories::{ConnectionRepository, SettingsRecord, SettingsRepository};
use crate::v2::ports::stores::SecretStore;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub download_dir: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    #[serde(default)]
    pub download_dir: String,
}

pub struct SettingsService {
    settings: Arc<dyn SettingsRepository>,
}

impl SettingsService {
    pub fn new(settings: Arc<dyn SettingsRepository>) -> Self {
        Self { settings }
    }

    pub async fn view(&self) -> DomainResult<SettingsView> {
        let record = self.settings.load_settings().await?;
        Ok(SettingsView {
            download_dir: record.download_dir,
        })
    }

    pub async fn save(&self, input: SettingsInput) -> DomainResult<SettingsView> {
        let download_dir = if input.download_dir.trim().is_empty() {
            default_download_dir()
        } else {
            input.download_dir.trim().to_string()
        };
        let record = SettingsRecord { download_dir };
        self.settings.save_settings(&record).await?;
        self.view().await
    }

    /// 打开下载目录；`reveal` 给了一个**目录内**的文件路径时，顺带把它选中。
    ///
    /// 选中那个文件是主流做法（浏览器、VS Code 的"在文件夹中显示"都是选中而不是只开目录）。
    /// Windows 下的写法是 `explorer.exe /select,<路径>`——**逗号后不能有空格**，有空格它就
    /// 当成两个参数、只开默认位置。路径必须落在下载目录里才认：这个参数来自界面，
    /// 不该让它变成"打开任意路径"的开关。
    pub async fn open_download_dir(&self, reveal: Option<&str>) -> DomainResult<String> {
        let record = self.settings.load_settings().await?;
        let directory = if record.download_dir.trim().is_empty() {
            default_download_dir()
        } else {
            record.download_dir
        };
        std::fs::create_dir_all(&directory).map_err(|_| SafeError::storage("无法创建下载目录"))?;
        let reveal = reveal
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .filter(|path| {
                let candidate = std::path::Path::new(path);
                candidate.starts_with(std::path::Path::new(&directory))
            });
        let opened = if cfg!(target_os = "windows") {
            match reveal {
                Some(path) => std::process::Command::new("explorer.exe")
                    .arg(format!("/select,{path}"))
                    .spawn(),
                None => std::process::Command::new("explorer.exe")
                    .arg(&directory)
                    .spawn(),
            }
        } else if cfg!(target_os = "macos") {
            match reveal {
                Some(path) => std::process::Command::new("open")
                    .arg("-R")
                    .arg(path)
                    .spawn(),
                None => std::process::Command::new("open").arg(&directory).spawn(),
            }
        } else {
            // Linux 的文件管理器没有统一的"选中"约定，退回打开目录。
            std::process::Command::new("xdg-open")
                .arg(&directory)
                .spawn()
        };
        opened.map_err(|_| SafeError::storage("无法打开下载目录"))?;
        Ok(directory)
    }
}

pub fn mask_secret(value: &str) -> Option<String> {
    let characters = value.trim().chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return None;
    }
    if characters.len() <= 8 {
        return Some("*".repeat(characters.len()));
    }
    Some(format!(
        "{}****{}",
        characters[..4].iter().collect::<String>(),
        characters[characters.len() - 4..]
            .iter()
            .collect::<String>()
    ))
}

pub fn default_download_dir() -> String {
    dirs::download_dir()
        .or_else(dirs::document_dir)
        .or_else(dirs::home_dir)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

// ---------- RunningHub 账号 / 队列状态（基于 Connection） ----------

/// 状态查询用到的 RunningHub 连接摘要（供诊断展示）。
#[derive(Debug, Clone)]
pub struct RunningHubBrief {
    pub base_url: String,
    pub api_key_configured: bool,
    pub region: Option<String>,
    pub account: Option<String>,
}

pub struct RunningHubStatusService {
    connections: Arc<dyn ConnectionRepository>,
    drivers: Arc<DriverRegistry>,
    secrets: Arc<dyn SecretStore>,
}

impl RunningHubStatusService {
    pub fn new(
        connections: Arc<dyn ConnectionRepository>,
        drivers: Arc<DriverRegistry>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            connections,
            drivers,
            secrets,
        }
    }

    async fn runninghub_revision(
        &self,
        requested: Option<ConnectionId>,
    ) -> DomainResult<ConnectionRevision> {
        let connection = if let Some(id) = requested {
            let connection =
                self.connections.find_connection(id).await?.ok_or_else(|| {
                    SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在")
                })?;
            if connection.vendor != "runninghub" || connection.protocol != "runninghub" {
                return Err(SafeError::validation(
                    "NOT_RUNNINGHUB_CONNECTION",
                    "该 Connection 不是 RunningHub",
                ));
            }
            connection
        } else {
            let connections = self.connections.list_connections().await?;
            connections
                .into_iter()
                .find(|connection| {
                    connection.vendor == "runninghub" || connection.protocol == "runninghub"
                })
                .ok_or_else(|| {
                    SafeError::validation(
                        "RUNNINGHUB_CONNECTION_MISSING",
                        "尚未配置 RunningHub Connection，请到「连接」页添加",
                    )
                })?
        };
        self.connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })
    }

    pub async fn account_status(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> DomainResult<serde_json::Value> {
        let revision = self.runninghub_revision(connection_id).await?;
        let driver = self.drivers.get(&driver_key("runninghub", "runninghub"))?;
        driver.fetch_account_status(&revision).await
    }

    pub async fn queue_status(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> DomainResult<serde_json::Value> {
        let revision = self.runninghub_revision(connection_id).await?;
        let driver = self.drivers.get(&driver_key("runninghub", "runninghub"))?;
        driver.fetch_queue_status(&revision).await
    }

    /// 首个 RunningHub 连接的摘要；未配置时返回 None（用于诊断，不报错）。
    pub async fn runninghub_brief(&self) -> DomainResult<Option<RunningHubBrief>> {
        let connections = self.connections.list_connections().await?;
        let Some(connection) = connections.into_iter().find(|connection| {
            connection.vendor == "runninghub" && connection.protocol == "runninghub"
        }) else {
            return Ok(None);
        };
        let Some(revision) = self
            .connections
            .find_revision(connection.current_revision_id)
            .await?
        else {
            return Ok(None);
        };
        let api_key_configured = if let Some(binding) = revision
            .secret_bindings
            .iter()
            .find(|binding| is_api_key_slot(&binding.slot))
        {
            self.secrets
                .resolve(binding)
                .await?
                .is_some_and(|value| !value.secret.trim().is_empty())
        } else {
            false
        };
        Ok(Some(RunningHubBrief {
            base_url: revision
                .config
                .get("baseUrl")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            api_key_configured,
            region: revision
                .config
                .get("region")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            account: revision
                .config
                .get("account")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::model::SecretBinding;
    use crate::v2::infrastructure::fakes::InMemoryRepositories;

    struct InMemorySecrets(std::sync::Mutex<std::collections::HashMap<String, String>>);

    #[async_trait]
    impl SecretStore for InMemorySecrets {
        async fn resolve(
            &self,
            binding: &SecretBinding,
        ) -> DomainResult<Option<crate::v2::ports::stores::SecretValue>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&binding.slot)
                .cloned()
                .map(|secret| crate::v2::ports::stores::SecretValue { secret }))
        }
        async fn set(&self, slot: &str, value: &str) -> DomainResult<()> {
            self.0
                .lock()
                .unwrap()
                .insert(slot.to_string(), value.to_string());
            Ok(())
        }
        async fn delete(&self, slot: &str) -> DomainResult<()> {
            self.0.lock().unwrap().remove(slot);
            Ok(())
        }
    }

    fn service() -> SettingsService {
        let repos = Arc::new(InMemoryRepositories::new());
        SettingsService::new(repos)
    }

    #[tokio::test]
    async fn save_then_view_roundtrip_download_dir() {
        let service = service();
        let view = service
            .save(SettingsInput {
                download_dir: "D:/tmp/downloads".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(view.download_dir, "D:/tmp/downloads");
    }

    #[tokio::test]
    async fn empty_download_dir_falls_back_to_default() {
        let service = service();
        let view = service
            .save(SettingsInput {
                download_dir: "  ".to_string(),
            })
            .await
            .unwrap();
        assert!(!view.download_dir.is_empty());
    }

    #[test]
    fn masked_secret_formats() {
        assert_eq!(mask_secret("abcdef"), Some("******".to_string()));
        assert_eq!(
            mask_secret("abcdefghijklmnop"),
            Some("abcd****mnop".to_string())
        );
        assert_eq!(mask_secret(""), None);
    }

    #[tokio::test]
    async fn status_without_runninghub_connection_is_clear() {
        let repositories = Arc::new(InMemoryRepositories::new());
        let service = RunningHubStatusService::new(
            repositories.clone(),
            Arc::new(DriverRegistry::new()),
            Arc::new(InMemorySecrets(std::sync::Mutex::new(Default::default()))),
        );
        let error = service.account_status(None).await.unwrap_err();
        assert_eq!(error.code, "RUNNINGHUB_CONNECTION_MISSING");
        assert!(service.runninghub_brief().await.unwrap().is_none());
    }
}
