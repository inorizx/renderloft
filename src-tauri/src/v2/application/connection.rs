//! Connection 写操作用例：创建与更新（含密钥绑定持久化）。
//!
//! 收口自 HTTP handler 内的手工编排：密钥写入、binding 解析、revision
//! 创建逻辑统一在此，多个 transport（HTTP/CLI/MCP）可复用。

use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::v2::application::credential_reference;
use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::{ConnectionId, ConnectionRevisionId};
use crate::v2::domain::model::{
    Connection, ConnectionRevision, ConnectionStatus, LifecycleFeatures, SecretBinding,
    SecretResolution, Timestamp,
};
use crate::v2::ports::repositories::ConnectionRepository;
use crate::v2::ports::stores::SecretStore;

/// 密钥绑定条目的传输形态：`value` 仅用于写入 keyring，绝不回显。
#[derive(Debug, Clone)]
pub struct SecretBindingEntry {
    pub slot: String,
    pub resolution: SecretResolution,
    pub value: Option<String>,
}

/// 拒绝"既引用别人、又自己填一把 Key"。
///
/// 为什么必须拒：执行前解析引用时，被引用连接的 `secret_bindings` 会**替换**掉自己的，
/// 于是自己填的那把永远用不上——而用户看到的是"保存成功"（CLI 的 `set-key` 尤其像成功了）。
/// 这是典型的静默失效，宁可当场报错并告诉他怎么改。
fn reject_key_next_to_a_reference(
    config: &BTreeMap<String, Value>,
    secret_bindings: &[SecretBindingEntry],
) -> DomainResult<()> {
    if !secret_bindings.is_empty() && credential_reference::source_connection_id(config)?.is_some()
    {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_TAKES_PRECEDENCE",
            "这条连接正在引用另一条连接的密钥，自己填的 Key 不会被使用；\
             要自己填请先把 config.sourceConnectionId 去掉（界面上是「API Key 来源」改为自己填）",
        ));
    }
    Ok(())
}

impl SecretBindingEntry {
    /// 从请求字段严格解析；坏条目显式报错（替代 `.ok()` 静默丢弃）。
    pub fn parse(
        slot: String,
        resolution: Option<String>,
        value: Option<String>,
    ) -> DomainResult<Self> {
        if slot.trim().is_empty() {
            return Err(SafeError::validation(
                "SECRET_BINDING_INVALID",
                "secretBindings 的 slot 不能为空",
            ));
        }
        let resolution = match resolution.as_deref() {
            None | Some("current") => SecretResolution::Current,
            Some("pinned") => SecretResolution::Pinned(value.clone().unwrap_or_default()),
            Some(other) => {
                return Err(SafeError::validation(
                    "SECRET_BINDING_INVALID",
                    format!("secretBindings 的 resolution 非法：{other}"),
                ))
            }
        };
        Ok(Self {
            slot,
            resolution,
            value,
        })
    }
}

pub struct ConnectionDependencies {
    pub connections: Arc<dyn ConnectionRepository>,
    pub secrets: Arc<dyn SecretStore>,
}

pub struct CreateConnectionRequest {
    pub actor: ActorRef,
    pub vendor: String,
    pub protocol: String,
    pub display_name: String,
    pub config: BTreeMap<String, Value>,
    pub capabilities: Vec<String>,
    pub lifecycle_features: LifecycleFeatures,
    pub secret_bindings: Vec<SecretBindingEntry>,
}

pub struct CreateConnection {
    pub dependencies: ConnectionDependencies,
}

impl CreateConnection {
    pub fn new(dependencies: ConnectionDependencies) -> Self {
        Self { dependencies }
    }

    pub async fn execute(&self, request: CreateConnectionRequest) -> DomainResult<Connection> {
        if !request.actor.has_scope(Scope::ConnectionsManage) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "缺少 connections:manage 权限",
            ));
        }
        if request.vendor.trim().is_empty() {
            return Err(SafeError::validation("VENDOR_REQUIRED", "vendor 不能为空"));
        }
        if request.protocol.trim().is_empty() {
            return Err(SafeError::validation(
                "PROTOCOL_REQUIRED",
                "protocol 不能为空",
            ));
        }
        // 凭据引用：在**写之前**校验（此时用户还能当场改），见 credential_reference 的模块说明。
        credential_reference::validate_reference(
            self.dependencies.connections.as_ref(),
            &request.vendor,
            None,
            &request.config,
        )
        .await?;
        reject_key_next_to_a_reference(&request.config, &request.secret_bindings)?;
        let now = Timestamp::now();
        let connection = Connection {
            id: ConnectionId(0),
            vendor: request.vendor,
            protocol: request.protocol,
            display_name: request.display_name,
            current_revision_id: ConnectionRevisionId(0),
            status: ConnectionStatus::Active,
        };
        let revision = ConnectionRevision {
            id: ConnectionRevisionId(0),
            connection_id: connection.id,
            config: request.config,
            secret_bindings: vec![],
            effective_capabilities: request.capabilities,
            lifecycle_features: request.lifecycle_features,
            created_at: now,
        };
        let (mut connection, mut revision) = self
            .dependencies
            .connections
            .create_connection(connection, revision)
            .await?;
        // 创建后才获得 connectionId，凭据槽位必须按连接隔离，避免多个平台连接互相覆盖。
        if !request.secret_bindings.is_empty() {
            let scoped = scoped_entries(connection.id, &request.secret_bindings);
            persist_secret_values(&self.dependencies.secrets, &scoped).await?;
            let scoped_revision = ConnectionRevision {
                id: ConnectionRevisionId(0),
                connection_id: connection.id,
                config: revision.config.clone(),
                secret_bindings: scoped
                    .iter()
                    .map(|entry| SecretBinding {
                        slot: entry.slot.clone(),
                        resolution: entry.resolution.clone(),
                    })
                    .collect(),
                effective_capabilities: revision.effective_capabilities.clone(),
                lifecycle_features: revision.lifecycle_features.clone(),
                created_at: Timestamp::now(),
            };
            revision = self
                .dependencies
                .connections
                .create_revision(connection.id, scoped_revision)
                .await?;
        }
        connection.current_revision_id = revision.id;
        self.dependencies
            .connections
            .update_connection(&connection)
            .await?;
        Ok(connection)
    }
}

pub struct UpdateConnectionRequest {
    pub actor: ActorRef,
    pub connection_id: ConnectionId,
    pub display_name: Option<String>,
    pub config: Option<BTreeMap<String, Value>>,
    pub capabilities: Option<Vec<String>>,
    pub lifecycle_features: Option<LifecycleFeatures>,
    pub secret_bindings: Option<Vec<SecretBindingEntry>>,
}

pub struct UpdateConnection {
    pub dependencies: ConnectionDependencies,
}

impl UpdateConnection {
    pub fn new(dependencies: ConnectionDependencies) -> Self {
        Self { dependencies }
    }

    pub async fn execute(&self, request: UpdateConnectionRequest) -> DomainResult<Connection> {
        if !request.actor.has_scope(Scope::ConnectionsManage) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "缺少 connections:manage 权限",
            ));
        }
        let mut connection = self
            .dependencies
            .connections
            .find_connection(request.connection_id)
            .await?
            .ok_or_else(|| SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
        // 凭据引用：**在任何写入之前**校验——否则"名字改了、配置没改"会留下一半成功的状态。
        if let Some(config) = &request.config {
            credential_reference::validate_reference(
                self.dependencies.connections.as_ref(),
                &connection.vendor,
                Some(connection.id),
                config,
            )
            .await?;
        }
        // 引用与"自己填的 Key"不能并存：解析时引用**覆盖**自己的绑定，所以那把 Key
        // 会静静地永远用不上。`connection set-key` 正是只发 secretBindings、不发 config 的，
        // 所以这里必须回读当前 config 才能判出来。
        if request
            .secret_bindings
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        {
            let config = match &request.config {
                Some(config) => config.clone(),
                None => self
                    .dependencies
                    .connections
                    .find_revision(connection.current_revision_id)
                    .await?
                    .map(|revision| revision.config)
                    .unwrap_or_default(),
            };
            reject_key_next_to_a_reference(
                &config,
                request.secret_bindings.as_deref().unwrap_or(&[]),
            )?;
        }
        if let Some(display_name) = request.display_name {
            connection.display_name = display_name;
        }
        self.dependencies
            .connections
            .update_connection(&connection)
            .await?;

        // 配置/密钥变更 → 创建不可变新 revision（不影响历史 Run）。
        let has_config_update = request.config.is_some()
            || request.secret_bindings.is_some()
            || request.capabilities.is_some()
            || request.lifecycle_features.is_some();
        if has_config_update {
            let current = self
                .dependencies
                .connections
                .find_revision(connection.current_revision_id)
                .await?
                .ok_or_else(|| {
                    SafeError::not_found(
                        "CONNECTION_REVISION_NOT_FOUND",
                        "Connection revision 不存在",
                    )
                })?;
            let scoped = request
                .secret_bindings
                .as_ref()
                .map(|entries| scoped_entries(connection.id, entries));
            if let Some(entries) = &scoped {
                persist_secret_values(&self.dependencies.secrets, entries).await?;
            }
            let revision = ConnectionRevision {
                id: ConnectionRevisionId(0),
                connection_id: connection.id,
                config: request
                    .config
                    .clone()
                    .unwrap_or_else(|| current.config.clone()),
                secret_bindings: scoped
                    .as_ref()
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|entry| SecretBinding {
                                slot: entry.slot.clone(),
                                resolution: entry.resolution.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_else(|| current.secret_bindings.clone()),
                effective_capabilities: request
                    .capabilities
                    .clone()
                    .unwrap_or_else(|| current.effective_capabilities.clone()),
                lifecycle_features: request
                    .lifecycle_features
                    .unwrap_or_else(|| current.lifecycle_features.clone()),
                created_at: Timestamp::now(),
            };
            let revision = self
                .dependencies
                .connections
                .create_revision(connection.id, revision)
                .await?;
            connection.current_revision_id = revision.id;
            self.dependencies
                .connections
                .update_connection(&connection)
                .await?;
        }
        Ok(connection)
    }
}

fn scoped_entries(
    connection_id: ConnectionId,
    entries: &[SecretBindingEntry],
) -> Vec<SecretBindingEntry> {
    entries
        .iter()
        .cloned()
        .map(|mut entry| {
            if entry.slot == "api_key" {
                entry.slot = format!("connection-{}-api_key", connection_id.0);
            }
            entry
        })
        .collect()
}

/// 写入请求携带的 secret 值（仅存 keyring，不回显）。create/update 共用。
async fn persist_secret_values(
    secrets: &Arc<dyn SecretStore>,
    entries: &[SecretBindingEntry],
) -> DomainResult<()> {
    for entry in entries {
        if let Some(value) = &entry.value {
            if !value.trim().is_empty() {
                secrets.set(&entry.slot, value).await?;
            }
        }
    }
    Ok(())
}

/// 一次性迁移：把旧版引用共享字面量 `api_key` 槽位的连接改写为连接级 scoped slot。
///
/// 旧模型所有连接共用同一个 `api_key` keyring 槽位，多次保存会互相覆盖。
/// 本迁移把当前存在值复制到 `connection-{id}-api_key`，并创建指向 scoped slot
/// 的新 revision，最后删除已无人引用的旧 `api_key` 槽位。幂等：第二次运行时
/// 已无连接引用 `api_key`，不会产生任何写入。
pub async fn migrate_legacy_api_key_slots(
    connections: &Arc<dyn ConnectionRepository>,
    secrets: &Arc<dyn SecretStore>,
) -> DomainResult<usize> {
    let all = connections.list_connections().await?;
    let mut migrated = 0usize;
    for connection in all {
        let Some(revision) = connections
            .find_revision(connection.current_revision_id)
            .await?
        else {
            continue;
        };
        let references_legacy = revision
            .secret_bindings
            .iter()
            .any(|binding| binding.slot == "api_key");
        if !references_legacy {
            continue;
        }
        let scoped_slot = format!("connection-{}-api_key", connection.id.0);
        // 解析旧共享槽位当前值并复制到 scoped slot。
        let legacy = revision
            .secret_bindings
            .iter()
            .find(|binding| binding.slot == "api_key")
            .cloned();
        if let Some(binding) = legacy {
            if let Some(value) = secrets.resolve(&binding).await? {
                secrets.set(&scoped_slot, &value.secret).await?;
            }
        }
        // 创建新 revision：把 `api_key` 绑定改写为 scoped slot。
        let new_bindings: Vec<SecretBinding> = revision
            .secret_bindings
            .iter()
            .map(|binding| SecretBinding {
                slot: if binding.slot == "api_key" {
                    scoped_slot.clone()
                } else {
                    binding.slot.clone()
                },
                resolution: binding.resolution.clone(),
            })
            .collect();
        let new_revision = ConnectionRevision {
            id: ConnectionRevisionId(0),
            connection_id: connection.id,
            config: revision.config.clone(),
            secret_bindings: new_bindings,
            effective_capabilities: revision.effective_capabilities.clone(),
            lifecycle_features: revision.lifecycle_features.clone(),
            created_at: Timestamp::now(),
        };
        let created = connections
            .create_revision(connection.id, new_revision)
            .await?;
        let mut updated = connection.clone();
        updated.current_revision_id = created.id;
        connections.update_connection(&updated).await?;
        migrated += 1;
    }
    // 迁移了任意连接后，旧共享槽位不再被引用，删除避免孤儿凭据残留。
    if migrated > 0 {
        secrets.delete("api_key").await?;
    }
    Ok(migrated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::infrastructure::fakes::InMemoryRepositories;

    #[derive(Clone)]
    struct InMemorySecrets(
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    );

    #[async_trait::async_trait]
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

    type RepoRef = Arc<dyn ConnectionRepository>;
    type SecretsRef = Arc<dyn SecretStore>;

    fn repos_ref(repos: &Arc<InMemoryRepositories>) -> RepoRef {
        repos.clone() as RepoRef
    }

    fn legacy_connection(repos: &Arc<InMemoryRepositories>, secrets: &InMemorySecrets) {
        let connection = Connection {
            id: ConnectionId(0),
            vendor: "runninghub".to_string(),
            protocol: "runninghub".to_string(),
            display_name: "legacy".to_string(),
            current_revision_id: ConnectionRevisionId(0),
            status: ConnectionStatus::Active,
        };
        let revision = ConnectionRevision {
            id: ConnectionRevisionId(0),
            connection_id: ConnectionId(0),
            config: BTreeMap::from([(
                "baseUrl".to_string(),
                Value::String("https://x.example".into()),
            )]),
            secret_bindings: vec![SecretBinding {
                slot: "api_key".to_string(),
                resolution: SecretResolution::Current,
            }],
            effective_capabilities: vec![],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        };
        secrets
            .0
            .lock()
            .unwrap()
            .insert("api_key".to_string(), "shared-legacy-key".to_string());
        futures::executor::block_on(repos.create_connection(connection, revision)).expect("create");
    }

    fn current_secret_slot(repos: &Arc<InMemoryRepositories>, connection: &Connection) -> String {
        futures::executor::block_on(repos.find_revision(connection.current_revision_id))
            .expect("revision")
            .expect("exists")
            .secret_bindings[0]
            .slot
            .clone()
    }

    #[tokio::test]
    async fn migrates_legacy_shared_slot_to_scoped_per_connection() {
        let repos = Arc::new(InMemoryRepositories::new());
        let secrets = InMemorySecrets(std::sync::Arc::new(std::sync::Mutex::new(
            Default::default(),
        )));
        let secrets_ref: SecretsRef = Arc::new(secrets.clone());

        legacy_connection(&repos, &secrets);
        legacy_connection(&repos, &secrets);

        let migrated = migrate_legacy_api_key_slots(&repos_ref(&repos), &secrets_ref)
            .await
            .unwrap();
        assert_eq!(migrated, 2);

        let connections = repos.list_connections().await.unwrap();
        assert_eq!(connections.len(), 2);
        let scoped: Vec<String> = connections
            .iter()
            .map(|connection| current_secret_slot(&repos, connection))
            .collect();
        assert!(scoped.iter().all(|slot| slot.ends_with("-api_key")));
        // 旧共享槽位已删除；每个连接在 scoped slot 下有其副本。
        let map = secrets.0.lock().unwrap();
        assert!(!map.contains_key("api_key"));
        assert_eq!(
            map.get(&scoped[0]).map(String::as_str),
            Some("shared-legacy-key")
        );
        assert_eq!(
            map.get(&scoped[1]).map(String::as_str),
            Some("shared-legacy-key")
        );
    }

    #[tokio::test]
    async fn migration_is_idempotent_and_skips_scoped_connections() {
        let repos = Arc::new(InMemoryRepositories::new());
        let secrets = InMemorySecrets(std::sync::Arc::new(std::sync::Mutex::new(
            Default::default(),
        )));
        let secrets_ref: SecretsRef = Arc::new(secrets.clone());

        legacy_connection(&repos, &secrets);
        let first = migrate_legacy_api_key_slots(&repos_ref(&repos), &secrets_ref)
            .await
            .unwrap();
        assert_eq!(first, 1);
        // 已 scoped，不产生新迁移
        let second = migrate_legacy_api_key_slots(&repos_ref(&repos), &secrets_ref)
            .await
            .unwrap();
        assert_eq!(second, 0);
        let counts = repos.list_connections().await.unwrap();
        assert_eq!(counts.len(), 1);
    }

    #[tokio::test]
    async fn no_connections_means_no_legacy_delete_noop() {
        let repos = Arc::new(InMemoryRepositories::new());
        let secrets = InMemorySecrets(std::sync::Arc::new(std::sync::Mutex::new(
            Default::default(),
        )));
        let secrets_ref: SecretsRef = Arc::new(secrets.clone());
        secrets
            .0
            .lock()
            .unwrap()
            .insert("api_key".to_string(), "orphan".to_string());
        let migrated = migrate_legacy_api_key_slots(&repos_ref(&repos), &secrets_ref)
            .await
            .unwrap();
        assert_eq!(migrated, 0);
        // 无连接时不移除旧槽位（安全：不误删未识别数据）
        assert!(secrets.0.lock().unwrap().contains_key("api_key"));
    }
}
