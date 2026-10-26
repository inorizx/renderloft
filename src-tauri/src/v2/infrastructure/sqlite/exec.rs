use async_trait::async_trait;
use rusqlite::params;
use serde_json::json;

use super::*;
use crate::v2::domain::ids::{ArtifactId, AssetId, AttemptId};
use crate::v2::domain::media::MediaObject;
use crate::v2::domain::operation::{AttemptStatus, TrackingState, Transition};
use crate::v2::ports::callback::{CallbackDeliverer, DeliveryOutcome, DeliveryRequest};
use crate::v2::ports::repositories::{
    ActiveRun, AssetRepository, AttemptResolution, CallbackDeliveryRecord,
    CallbackDeliveryRepository, ClaimedRun, EventSpec, LeaseRecord, LeaseStore,
};
use crate::v2::ports::stores::{ArtifactObject, ArtifactStore};

const SUBMISSION_RECOVERY_WINDOW_MILLIS: u64 = 60_000;
// 「默认跟踪死线」那个数字的所有者是 `domain::connection_options`（原来这里也抄了一份）。

fn now_millis() -> u64 {
    Timestamp::now_millis()
}

#[async_trait]
impl ConnectionRepository for SqliteRepositories {
    async fn find_connection(&self, id: ConnectionId) -> DomainResult<Option<DomainConnection>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, vendor, protocol, display_name, current_revision_id, status
                 FROM v2_connections WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 Connection"))?;
        Ok(row.map(
            |(id, vendor, protocol, display_name, current_revision_id, status)| DomainConnection {
                id: ConnectionId(id as u64),
                vendor,
                protocol,
                display_name,
                current_revision_id: ConnectionRevisionId(current_revision_id as u64),
                status: serde_json::from_str(&status).unwrap_or_default(),
            },
        ))
    }

    async fn find_revision(
        &self,
        id: ConnectionRevisionId,
    ) -> DomainResult<Option<ConnectionRevision>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, connection_id, config_json, secret_bindings_json,
                        effective_capabilities_json, lifecycle_features_json, created_at
                 FROM v2_connection_revisions WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 Connection revision"))?;
        Ok(row.map(
            |(
                id,
                connection_id,
                config_json,
                secret_bindings_json,
                effective_capabilities_json,
                lifecycle_features_json,
                created_at,
            )| {
                ConnectionRevision {
                    id: ConnectionRevisionId(id as u64),
                    connection_id: ConnectionId(connection_id as u64),
                    config: serde_json::from_str(&config_json).unwrap_or_default(),
                    secret_bindings: serde_json::from_str(&secret_bindings_json)
                        .unwrap_or_default(),
                    effective_capabilities: serde_json::from_str(&effective_capabilities_json)
                        .unwrap_or_default(),
                    lifecycle_features: serde_json::from_str(&lifecycle_features_json)
                        .unwrap_or_default(),
                    created_at: Timestamp(created_at),
                }
            },
        ))
    }

    async fn list_revisions(&self) -> DomainResult<Vec<(String, String, ConnectionRevision)>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT c.vendor, c.protocol, r.id, r.connection_id, r.config_json,
                        r.secret_bindings_json, r.effective_capabilities_json,
                        r.lifecycle_features_json, r.created_at
                 FROM v2_connection_revisions r JOIN v2_connections c ON c.id = r.connection_id
                 ORDER BY r.id ASC",
            )
            .map_err(|_| SafeError::storage("无法读取 V2 Connection revisions"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 V2 Connection revisions"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 V2 Connection revisions"))?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    vendor,
                    protocol,
                    id,
                    connection_id,
                    config_json,
                    secret_bindings_json,
                    effective_capabilities_json,
                    lifecycle_features_json,
                    created_at,
                )| {
                    (
                        vendor,
                        protocol,
                        ConnectionRevision {
                            id: ConnectionRevisionId(id as u64),
                            connection_id: ConnectionId(connection_id as u64),
                            config: serde_json::from_str(&config_json).unwrap_or_default(),
                            secret_bindings: serde_json::from_str(&secret_bindings_json)
                                .unwrap_or_default(),
                            effective_capabilities: serde_json::from_str(
                                &effective_capabilities_json,
                            )
                            .unwrap_or_default(),
                            lifecycle_features: serde_json::from_str(&lifecycle_features_json)
                                .unwrap_or_default(),
                            created_at: Timestamp(created_at),
                        },
                    )
                },
            )
            .collect())
    }

    async fn list_connections(&self) -> DomainResult<Vec<DomainConnection>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id, vendor, protocol, display_name, current_revision_id, status
                 FROM v2_connections ORDER BY id ASC",
            )
            .map_err(|_| SafeError::storage("无法读取 V2 Connections"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 V2 Connections"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 V2 Connections"))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, vendor, protocol, display_name, current_revision_id, status)| {
                    DomainConnection {
                        id: ConnectionId(id as u64),
                        vendor,
                        protocol,
                        display_name,
                        current_revision_id: ConnectionRevisionId(current_revision_id as u64),
                        status: serde_json::from_str(&status).unwrap_or_default(),
                    }
                },
            )
            .collect())
    }

    async fn create_connection(
        &self,
        connection: DomainConnection,
        revision: ConnectionRevision,
    ) -> DomainResult<(DomainConnection, ConnectionRevision)> {
        let lock = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = lock
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let status = serde_json::to_string(&connection.status)
            .map_err(|_| SafeError::storage("无法序列化状态"))?;
        transaction
            .execute(
                "INSERT INTO v2_connections(
                    vendor, protocol, display_name, current_revision_id, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    connection.vendor,
                    connection.protocol,
                    connection.display_name,
                    revision.id.0,
                    status
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Connection"))?;
        let connection_id = ConnectionId(transaction.last_insert_rowid() as u64);
        let config = serde_json::to_string(&revision.config)
            .map_err(|_| SafeError::storage("无法序列化 config"))?;
        let secret_bindings = serde_json::to_string(&revision.secret_bindings)
            .map_err(|_| SafeError::storage("无法序列化 secret bindings"))?;
        let capabilities = serde_json::to_string(&revision.effective_capabilities)
            .map_err(|_| SafeError::storage("无法序列化 capabilities"))?;
        let lifecycle = serde_json::to_string(&revision.lifecycle_features)
            .map_err(|_| SafeError::storage("无法序列化 lifecycle"))?;
        transaction
            .execute(
                "INSERT INTO v2_connection_revisions(
                    connection_id, config_json, secret_bindings_json,
                    effective_capabilities_json, lifecycle_features_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    connection_id.0,
                    config,
                    secret_bindings,
                    capabilities,
                    lifecycle,
                    revision.created_at.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Connection revision"))?;
        let revision_id = ConnectionRevisionId(transaction.last_insert_rowid() as u64);
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        let mut connection = connection;
        connection.id = connection_id;
        connection.current_revision_id = revision_id;
        let mut revision = revision;
        revision.id = revision_id;
        revision.connection_id = connection_id;
        Ok((connection, revision))
    }

    async fn update_connection(&self, connection: &DomainConnection) -> DomainResult<()> {
        let lock = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let status = serde_json::to_string(&connection.status)
            .map_err(|_| SafeError::storage("无法序列化状态"))?;
        let changed = lock
            .execute(
                "UPDATE v2_connections SET vendor = ?1, protocol = ?2, display_name = ?3,
                        current_revision_id = ?4, status = ?5 WHERE id = ?6",
                params![
                    connection.vendor,
                    connection.protocol,
                    connection.display_name,
                    connection.current_revision_id.0,
                    status,
                    connection.id.0
                ],
            )
            .map_err(|_| SafeError::storage("无法更新 V2 Connection"))?;
        if changed == 0 {
            return Err(SafeError::not_found(
                "CONNECTION_NOT_FOUND",
                "Connection 不存在",
            ));
        }
        Ok(())
    }

    async fn delete_connection(&self, id: ConnectionId) -> DomainResult<bool> {
        let mut lock = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = lock
            .transaction()
            .map_err(|_| SafeError::storage("无法开启删除事务"))?;
        // 若该连接的 revision 已被 Run 引用，删除会破坏历史任务的可追溯性。
        let referenced: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM v2_runs
                 WHERE connection_revision_id IN (
                     SELECT id FROM v2_connection_revisions WHERE connection_id = ?1
                 )",
                params![id.0],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法检查 Connection 引用"))?;
        if referenced > 0 {
            return Err(SafeError::conflict(
                "CONNECTION_IN_USE",
                "该连接已被历史任务引用，无法删除",
            ));
        }
        // 删除连接只被**历史任务**引用挡住（上面那道 `CONNECTION_IN_USE`）：任务与产物的
        // 外键指着连接，删了它们就成了悬空引用。别在这里再加"被别的什么引用"的守卫——
        // 每加一道都要有真实的写入方，否则它守的是一张空表，却仍会对用户喊一句
        // 他根本无法执行的指令。
        transaction
            .execute(
                "DELETE FROM v2_connection_revisions WHERE connection_id = ?1",
                params![id.0],
            )
            .map_err(|_| SafeError::storage("无法清理 Connection 版本"))?;
        let changed = transaction
            .execute("DELETE FROM v2_connections WHERE id = ?1", params![id.0])
            .map_err(|_| SafeError::storage("无法删除 V2 Connection"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交删除事务"))?;
        Ok(changed > 0)
    }

    async fn create_revision(
        &self,
        connection_id: ConnectionId,
        revision: ConnectionRevision,
    ) -> DomainResult<ConnectionRevision> {
        let lock = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = lock
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let config = serde_json::to_string(&revision.config)
            .map_err(|_| SafeError::storage("无法序列化 config"))?;
        let secret_bindings = serde_json::to_string(&revision.secret_bindings)
            .map_err(|_| SafeError::storage("无法序列化 secret bindings"))?;
        let capabilities = serde_json::to_string(&revision.effective_capabilities)
            .map_err(|_| SafeError::storage("无法序列化 capabilities"))?;
        let lifecycle = serde_json::to_string(&revision.lifecycle_features)
            .map_err(|_| SafeError::storage("无法序列化 lifecycle"))?;
        transaction
            .execute(
                "INSERT INTO v2_connection_revisions(
                    connection_id, config_json, secret_bindings_json,
                    effective_capabilities_json, lifecycle_features_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    connection_id.0,
                    config,
                    secret_bindings,
                    capabilities,
                    lifecycle,
                    revision.created_at.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Connection revision"))?;
        let revision_id = ConnectionRevisionId(transaction.last_insert_rowid() as u64);
        transaction
            .execute(
                "UPDATE v2_connections SET current_revision_id = ?1 WHERE id = ?2",
                params![revision_id.0, connection_id.0],
            )
            .map_err(|_| SafeError::storage("无法更新 V2 Connection"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        let mut revision = revision;
        revision.id = revision_id;
        revision.connection_id = connection_id;
        Ok(revision)
    }
}

fn load_recipe_revision(
    transaction: &Transaction,
    id: RecipeRevisionId,
) -> DomainResult<RecipeRevision> {
    let row = transaction
        .query_row(
            "SELECT id, recipe_id, revision, input_schema_json, output_schema_json,
                    binding_json, defaults_json, source, created_at
             FROM v2_recipe_revisions WHERE id = ?1",
            params![id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|_| SafeError::storage("无法读取 V2 Recipe revision"))?
        .ok_or_else(|| {
            SafeError::not_found("RECIPE_REVISION_NOT_FOUND", "Recipe revision 不存在")
        })?;
    Ok(RecipeRevision {
        id: RecipeRevisionId(row.0 as u64),
        recipe_id: RecipeId(row.1 as u64),
        revision: row.2 as u32,
        input_schema: serde_json::from_str(&row.3).unwrap_or(Value::Null),
        output_schema: serde_json::from_str(&row.4).unwrap_or(Value::Null),
        binding: serde_json::from_str(&row.5).unwrap_or_default(),
        defaults: serde_json::from_str(&row.6).unwrap_or_default(),
        source: serde_json::from_str(&row.7).unwrap_or_default(),
        created_at: Timestamp(row.8),
    })
}

fn load_connection_revision(
    transaction: &Transaction,
    id: ConnectionRevisionId,
) -> DomainResult<(String, String, ConnectionRevision)> {
    let row = transaction
        .query_row(
            "SELECT c.vendor, c.protocol, r.id, r.connection_id, r.config_json,
                    r.secret_bindings_json, r.effective_capabilities_json,
                    r.lifecycle_features_json, r.created_at
             FROM v2_connection_revisions r JOIN v2_connections c ON c.id = r.connection_id
             WHERE r.id = ?1",
            params![id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|_| SafeError::storage("无法读取 V2 Connection revision"))?
        .ok_or_else(|| {
            SafeError::not_found(
                "CONNECTION_REVISION_NOT_FOUND",
                "Connection revision 不存在",
            )
        })?;
    Ok((
        row.0,
        row.1,
        ConnectionRevision {
            id: ConnectionRevisionId(row.2 as u64),
            connection_id: ConnectionId(row.3 as u64),
            config: serde_json::from_str(&row.4).unwrap_or_default(),
            secret_bindings: serde_json::from_str(&row.5).unwrap_or_default(),
            effective_capabilities: serde_json::from_str(&row.6).unwrap_or_default(),
            lifecycle_features: serde_json::from_str(&row.7).unwrap_or_default(),
            created_at: Timestamp(row.8),
        },
    ))
}

type RunRow = (
    i64,
    i64,
    String,
    i64,
    i64,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
);

fn run_from_row(row: RunRow, operation_id: OperationId) -> Run {
    Run {
        id: RunId(row.0 as u64),
        operation_id,
        client_run_id: row.2,
        recipe_revision_id: RecipeRevisionId(row.4 as u64),
        connection_revision_id: ConnectionRevisionId(row.5 as u64),
        sequence: row.3 as u32,
        inputs: serde_json::from_str(&row.6).unwrap_or_default(),
        assets: serde_json::from_str(&row.7).unwrap_or_default(),
        status: RunStatus::parse(&row.8).unwrap_or(RunStatus::Queued),
        tracking_state: TrackingState::parse(&row.9).unwrap_or(TrackingState::Inactive),
        tracking_deadline: row.10.map(Timestamp),
        next_poll_at: row.11.map(Timestamp),
        last_polled_at: row.12.map(Timestamp),
        error: row.15.and_then(|value| serde_json::from_str(&value).ok()),
        // 这条路径（认领/完成/转移/列活跃）不喂详情接口，所以不读那一列——
        // 详情走 `mod.rs` 的 PersistedOperation 装载，那里才填 usage。
        // 哪天有客户端要从这些路径看到用量，就把 `usage_json` 加进 RunRow 与那几条 SELECT。
        usage: None,
        created_at: Timestamp(row.13),
        updated_at: Timestamp(row.14),
    }
}

#[async_trait]
impl OperationRepository for SqliteRepositories {
    async fn find_by_idempotency(
        &self,
        actor_id: &str,
        client_id: &str,
        idempotency_key: &str,
    ) -> DomainResult<Option<PersistedOperation>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let operation_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM v2_operations
                 WHERE actor_id = ?1 AND client_id = ?2 AND idempotency_key = ?3",
                params![actor_id, client_id, idempotency_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 幂等记录"))?;
        let Some(operation_id) = operation_id else {
            return Ok(None);
        };
        let result = Self::load_operation(&transaction, OperationId(operation_id as u64))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(result)
    }

    async fn create_operation(&self, request: NewOperation) -> DomainResult<PersistedOperation> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let idempotency_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM v2_operations
                    WHERE actor_id = ?1 AND client_id = ?2 AND idempotency_key = ?3
                 )",
                params![
                    request.operation.actor.id,
                    request.operation.actor.client_id.0,
                    request.operation.idempotency_key.0
                ],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法检查 V2 幂等记录"))?;
        if idempotency_exists {
            return Err(SafeError::conflict(
                "IDEMPOTENCY_CONFLICT",
                "idempotencyKey 已存在",
            ));
        }

        let metadata_json = serde_json::to_string(&request.operation.metadata)
            .map_err(|_| SafeError::storage("无法序列化 metadata"))?;
        let callback_json = request
            .operation
            .callback
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| SafeError::storage("无法序列化 callback"))?;
        let status = request.operation.status.as_str().to_string();
        let now = Timestamp::now().0;

        transaction
            .execute(
                "INSERT INTO v2_operations(
                    actor_id, actor_kind, client_id, actor_label, origin, idempotency_key,
                    request_hash, status, metadata_json, callback_json, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    request.operation.actor.id,
                    serde_json::to_string(&request.operation.actor.kind)
                        .map_err(|_| SafeError::storage("无法序列化 Actor 类型"))?,
                    request.operation.actor.client_id.0,
                    request.operation.actor.label,
                    request.operation.origin,
                    request.operation.idempotency_key.0,
                    request.operation.request_hash,
                    status,
                    metadata_json,
                    callback_json,
                    now
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Operation"))?;
        let operation_id = OperationId(transaction.last_insert_rowid() as u64);

        let mut runs = Vec::with_capacity(request.runs.len());
        for (sequence, new_run) in request.runs.into_iter().enumerate() {
            let run_status = RunStatus::Queued.as_str().to_string();
            let tracking = TrackingState::Inactive.as_str().to_string();
            let inputs_json = serde_json::to_string(&new_run.inputs)
                .map_err(|_| SafeError::storage("无法序列化 Run inputs"))?;
            let assets_json = serde_json::to_string(&new_run.assets)
                .map_err(|_| SafeError::storage("无法序列化 Run assets"))?;
            transaction
                .execute(
                    "INSERT INTO v2_runs(
                        operation_id, client_run_id, sequence, recipe_revision_id,
                        connection_revision_id, inputs_json, assets_json, status,
                        tracking_state, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                    params![
                        operation_id.0,
                        new_run.client_run_id,
                        sequence as i64,
                        new_run.recipe_revision_id.0,
                        new_run.connection_revision_id.0,
                        inputs_json,
                        assets_json,
                        run_status,
                        tracking,
                        now
                    ],
                )
                .map_err(|_| SafeError::storage("无法写入 V2 Run"))?;
            runs.push(Run {
                // 刚创建，平台还没报任何用量。
                usage: None,
                id: RunId(transaction.last_insert_rowid() as u64),
                operation_id,
                client_run_id: new_run.client_run_id,
                recipe_revision_id: new_run.recipe_revision_id,
                connection_revision_id: new_run.connection_revision_id,
                sequence: sequence as u32,
                inputs: new_run.inputs,
                assets: new_run.assets,
                status: RunStatus::Queued,
                tracking_state: TrackingState::Inactive,
                tracking_deadline: None,
                next_poll_at: None,
                last_polled_at: None,
                error: None,
                created_at: Timestamp(now.clone()),
                updated_at: Timestamp(now.clone()),
            });
        }

        let mut operation = request.operation;
        operation.id = operation_id;
        operation.updated_at = Timestamp(now.clone());

        for (index, run) in runs.iter().enumerate() {
            insert_event(
                &transaction,
                operation_id,
                Some(run.id),
                &operation.actor,
                index as u64,
                "run.created",
                &operation.created_at,
                serde_json::json!({}),
            )?;
        }
        insert_event(
            &transaction,
            operation_id,
            None,
            &operation.actor,
            runs.len() as u64,
            "operation.created",
            &operation.created_at,
            serde_json::json!({}),
        )?;

        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(PersistedOperation { operation, runs })
    }

    async fn find_operation(&self, id: OperationId) -> DomainResult<Option<PersistedOperation>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let result = Self::load_operation(&transaction, id)?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(result)
    }

    async fn update_run(&self, run: &Run) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let status = run.status.as_str().to_string();
        let tracking = run.tracking_state.as_str().to_string();
        let error_json = run
            .error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| SafeError::storage("无法序列化 Run 错误"))?;
        connection
            .execute(
                "UPDATE v2_runs SET status = ?1, tracking_state = ?2, tracking_deadline = ?3,
                        next_poll_at = ?4, last_polled_at = ?5, error_json = ?6, updated_at = ?7
                        WHERE id = ?8",
                params![
                    status,
                    tracking,
                    run.tracking_deadline.as_ref().map(|t| t.0.clone()),
                    run.next_poll_at.as_ref().map(|t| t.0.clone()),
                    run.last_polled_at.as_ref().map(|t| t.0.clone()),
                    error_json,
                    run.updated_at.0,
                    run.id.0
                ],
            )
            .map_err(|_| SafeError::storage("无法更新 V2 Run"))?;
        Ok(())
    }

    async fn append_attempt(&self, attempt: &RunAttempt) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let status = serde_json::to_string(&attempt.status)
            .map_err(|_| SafeError::storage("无法序列化 Attempt 状态"))?;
        let remote_handle_json = attempt
            .remote_handle
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| SafeError::storage("无法序列化 remote handle"))?;
        let error_json = attempt
            .error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| SafeError::storage("无法序列化 Attempt 错误"))?;
        connection
            .execute(
                "INSERT INTO v2_run_attempts(
                    run_id, number, status, request_fingerprint, fencing_token,
                    remote_handle_json, error_json, started_at, completed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    attempt.run_id.0,
                    attempt.number,
                    status,
                    attempt.request_fingerprint,
                    attempt.fencing_token,
                    remote_handle_json,
                    error_json,
                    attempt.started_at.0,
                    attempt.completed_at.as_ref().map(|t| t.0.clone()),
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Attempt"))?;
        Ok(())
    }

    async fn fence_attempt(&self, run_id: RunId) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .execute(
                "UPDATE v2_run_attempts SET status = 'CANCELLED', completed_at = ?1
                 WHERE run_id = ?2
                   AND status NOT IN ('SUCCEEDED', 'FAILED', 'UNCERTAIN', 'CANCELLED')",
                params![Timestamp::now().0, run_id.0],
            )
            .map_err(|_| SafeError::storage("无法撤销 V2 Attempt"))?;
        Ok(())
    }

    async fn claim_next_run(
        &self,
        owner: &str,
        fencing_token: &str,
        connection_revision_id: ConnectionRevisionId,
        now: Timestamp,
    ) -> DomainResult<Option<ClaimedRun>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let row: Option<(
            i64,
            i64,
            String,
            i64,
            i64,
            i64,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
        )> = transaction
            .query_row(
                "SELECT r.id, r.operation_id, r.client_run_id, r.sequence, r.recipe_revision_id,
                            r.connection_revision_id, r.inputs_json, r.assets_json, r.status,
                            r.tracking_state, r.tracking_deadline, r.next_poll_at, r.last_polled_at,
                            r.created_at, r.updated_at, r.error_json
                     FROM v2_runs r
                     WHERE r.status IN ('QUEUED', 'RETRY_WAIT')
                       AND r.connection_revision_id = ?1
                       AND (r.status = 'QUEUED'
                            OR r.next_poll_at IS NULL
                            OR r.next_poll_at <= ?2)
                     ORDER BY r.created_at ASC, r.sequence ASC LIMIT 1",
                params![connection_revision_id.0, now.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取待领取 Run"))?;
        let Some(row) = row else {
            transaction
                .commit()
                .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
            return Ok(None);
        };
        let run_id = RunId(row.0 as u64);
        let operation_id = OperationId(row.1 as u64);
        transaction
            .execute(
                "UPDATE v2_runs SET status = 'SUBMITTING', updated_at = ?1
                 WHERE id = ?2 AND status IN ('QUEUED', 'RETRY_WAIT')",
                params![now.0, run_id.0],
            )
            .map_err(|_| SafeError::storage("无法领取 Run"))?;
        let number: i64 = transaction
            .query_row(
                "SELECT COUNT(*) + 1 FROM v2_run_attempts WHERE run_id = ?1",
                params![run_id.0],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法读取尝试编号"))?;
        transaction
            .execute(
                "INSERT INTO v2_run_attempts(
                    run_id, number, status, request_fingerprint, fencing_token, started_at
                 ) VALUES (?1, ?2, 'SUBMITTING', ?3, ?4, ?5)",
                params![
                    run_id.0,
                    number,
                    format!("run-{}:attempt-{number}", run_id.0),
                    fencing_token,
                    now.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 Attempt"))?;
        let attempt = RunAttempt {
            id: AttemptId(0),
            run_id,
            number: number as u32,
            status: AttemptStatus::Submitting,
            request_fingerprint: format!("run-{}:attempt-{number}", run_id.0),
            fencing_token: fencing_token.to_string(),
            remote_handle: None,
            error: None,
            started_at: now.clone(),
            completed_at: None,
        };
        let recipe_revision = load_recipe_revision(&transaction, RecipeRevisionId(row.4 as u64))?;
        // 与 `list_active_runs` 同一取法：同一个事务里按主键取一次，取不到留空，
        // 由 `compose_filename` 退回原名。
        let recipe_name: String = transaction
            .query_row(
                "SELECT name FROM v2_recipes WHERE id = ?1",
                params![recipe_revision.recipe_id.0 as i64],
                |row| row.get(0),
            )
            .unwrap_or_default();
        let (_, _, connection_revision) =
            load_connection_revision(&transaction, connection_revision_id)?;
        let mut run = run_from_row(row, operation_id);
        run.status = RunStatus::Submitting;
        run.updated_at = now.clone();
        let claimed = ClaimedRun {
            run,
            recipe_name,
            recipe_revision,
            connection_revision,
            attempt,
        };
        let now = now.clone();
        let actor = ActorRef {
            id: owner.to_string(),
            kind: crate::v2::domain::actor::ActorKind::System,
            client_id: crate::v2::domain::ids::ClientId("scheduler".to_string()),
            scopes: vec![],
            label: None,
        };
        insert_event(
            &transaction,
            operation_id,
            Some(run_id),
            &actor,
            now.millis(),
            "run.claimed",
            &now,
            serde_json::json!({}),
        )?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(Some(claimed))
    }

    async fn complete_attempt(
        &self,
        run_id: RunId,
        fencing_token: &str,
        resolution: AttemptResolution,
    ) -> DomainResult<Option<Run>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let matches: Option<(i64, String, String, i64)> = transaction
            .query_row(
                "SELECT id, status, fencing_token, run_id FROM v2_run_attempts
                 WHERE run_id = ?1 ORDER BY number DESC LIMIT 1",
                params![run_id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Attempt"))?;
        let Some((attempt_id, status, token, _)) = matches else {
            return Ok(None);
        };
        if token != fencing_token || is_terminal_attempt(&status) {
            return Ok(None);
        }
        let (new_status, remote_handle_json, error_json): (String, Option<String>, Option<String>) =
            match &resolution {
                AttemptResolution::Succeeded { remote_handle } => (
                    "SUCCEEDED".to_string(),
                    remote_handle
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|_| SafeError::storage("无法序列化 remote handle"))?,
                    None,
                ),
                AttemptResolution::Failed { error, .. } => (
                    "FAILED".to_string(),
                    None,
                    Some(
                        serde_json::to_string(error)
                            .map_err(|_| SafeError::storage("无法序列化错误"))?,
                    ),
                ),
                AttemptResolution::Uncertain {
                    remote_handle,
                    error,
                } => (
                    "UNCERTAIN".to_string(),
                    remote_handle
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|_| SafeError::storage("无法序列化 remote handle"))?,
                    // 「为什么不明」必须落库：提交失败进不确定态时，这是**唯一**会留下来的证据
                    //（事件载荷只有一句 message，日志那一行要等到死线才写）。少了它，
                    //「连接被拒」与「发出去后超时」在库里长得一模一样。
                    error
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|_| SafeError::storage("无法序列化错误"))?,
                ),
            };
        transaction
            .execute(
                "UPDATE v2_run_attempts SET status = ?1, remote_handle_json = ?2, error_json = ?3,
                        completed_at = ?4 WHERE id = ?5",
                params![
                    new_status,
                    remote_handle_json,
                    error_json,
                    Timestamp::now().0,
                    attempt_id
                ],
            )
            .map_err(|_| SafeError::storage("无法更新 Attempt"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        drop(connection);
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT r.id, r.operation_id, r.client_run_id, r.sequence, r.recipe_revision_id,
                        r.connection_revision_id, r.inputs_json, r.assets_json, r.status,
                        r.tracking_state, r.tracking_deadline, r.next_poll_at, r.last_polled_at,
                        r.created_at, r.updated_at, r.error_json
                 FROM v2_runs r WHERE r.id = ?1",
                params![run_id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Run"))?;
        Ok(row.map(|row| {
            let operation_id = OperationId(row.1 as u64);
            run_from_row(row, operation_id)
        }))
    }

    async fn transition_run(
        &self,
        run_id: RunId,
        transition: Transition,
        events: Vec<EventSpec>,
    ) -> DomainResult<Run> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let row = transaction
            .query_row(
                "SELECT r.id, r.operation_id, r.client_run_id, r.sequence, r.recipe_revision_id,
                        r.connection_revision_id, r.inputs_json, r.assets_json, r.status,
                        r.tracking_state, r.tracking_deadline, r.next_poll_at, r.last_polled_at,
                        r.created_at, r.updated_at, r.error_json
                 FROM v2_runs r WHERE r.id = ?1",
                params![run_id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Run"))?
            .ok_or_else(|| SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"))?;
        let operation_id = OperationId(row.1 as u64);
        let mut run = run_from_row(row, operation_id);
        let now = Timestamp::now();
        run.apply(transition, now.clone())?;
        run.apply_tracking(now.clone());
        let status_json = run.status.as_str().to_string();
        let tracking_json = run.tracking_state.as_str().to_string();
        transaction
            .execute(
                "UPDATE v2_runs SET status = ?1, tracking_state = ?2, tracking_deadline = ?3,
                        next_poll_at = ?4, updated_at = ?5 WHERE id = ?6",
                params![
                    status_json,
                    tracking_json,
                    run.tracking_deadline.as_ref().map(|t| t.0.clone()),
                    run.next_poll_at.as_ref().map(|t| t.0.clone()),
                    run.updated_at.0,
                    run.id.0
                ],
            )
            .map_err(|_| SafeError::storage("无法更新 Run"))?;
        for event in events {
            insert_event(
                &transaction,
                operation_id,
                event.run_id,
                &ActorRef {
                    id: "system".to_string(),
                    kind: crate::v2::domain::actor::ActorKind::System,
                    client_id: crate::v2::domain::ids::ClientId("system".to_string()),
                    scopes: vec![],
                    label: None,
                },
                now.millis(),
                &event.event_type,
                &now,
                event.safe_payload,
            )?;
        }
        refresh_operation_status(&transaction, operation_id)?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(run)
    }

    async fn set_run_error(&self, run_id: RunId, error: Option<SafeError>) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let error_json = error
            .map(|error| serde_json::to_string(&error))
            .transpose()
            .map_err(|_| SafeError::storage("无法序列化 Run 错误"))?;
        connection
            .execute(
                "UPDATE v2_runs SET error_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![error_json, Timestamp::now().0, run_id.0],
            )
            .map_err(|_| SafeError::storage("无法写入 Run 错误"))?;
        Ok(())
    }

    async fn set_run_usage(
        &self,
        run_id: RunId,
        usage: Option<serde_json::Value>,
    ) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let usage_json = usage
            .map(|usage| serde_json::to_string(&usage))
            .transpose()
            .map_err(|_| SafeError::storage("无法序列化 Run 用量"))?;
        connection
            .execute(
                "UPDATE v2_runs SET usage_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![usage_json, Timestamp::now().0, run_id.0],
            )
            .map_err(|_| SafeError::storage("无法写入 Run 用量"))?;
        Ok(())
    }

    async fn count_active_operations(&self, actor_id: &str) -> DomainResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = connection
            .query_row(
                // 判据与 `OperationStatus::is_terminal` 一致：终态 = SUCCEEDED / FAILED /
                // CANCELLED / PARTIAL。用 NOT IN 列出终态而不是列出非终态——
                // 后者会在将来新增状态时**静默把新状态算成"未终结"**（闸门越收越紧），
                // 前者最多是漏算一个新终态（闸门偏松），偏松比偏紧安全。
                "SELECT COUNT(*) FROM v2_operations
                 WHERE actor_id = ?1
                   AND status NOT IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'PARTIAL')",
                params![actor_id],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法统计进行中的任务"))?;
        Ok(count as usize)
    }

    async fn count_active_runs_for_connection(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
    ) -> DomainResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = connection
            .query_row(
                // ⚠️ **`SUBMISSION_UNCERTAIN` 刻意不算占位**（2026-10-17 改）。
                //
                // 它占的是"我们不知道平台收没收到"这个**疑问**，不是平台的执行位：把它算成占位，
                // 一个不确定的 run 会把这条连接唯一的执行位**占满到死线（默认 1 小时）**，
                // 后面所有 run 全排队——而真正在平台上跑的可能一个都没有。
                //
                // 为什么让它不占位是安全的（实测依据）：平台侧真被占满时**会拒**，而
                // RunningHub 的拒绝（`TASK_QUEUE_MAXED`）在 `rejection_error` 里是
                // `retryable: true`、且实测"拒了一分钱没花"⇒ 退避 30 秒后自动重试。
                // 所以"多提交一次被拒"的代价是一次退避，而不是"我们自己造成的任务失败"。
                //
                // 方向仍然保守：`ACCEPTED` / `RUNNING` 照旧占位（那是真的在平台上跑）。
                // 对账队列（`list_active_runs`）**必须继续包含** `SUBMISSION_UNCERTAIN`
                // ——那是它被捞出来确认的唯一途径，改那里等于放弃对账。
                "SELECT COUNT(*) FROM v2_runs
                 WHERE connection_revision_id = ?1
                   AND status IN ('ACCEPTED', 'RUNNING')
                   AND tracking_state = 'active'",
                params![connection_revision_id.0],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法统计在跑的 Run"))?;
        Ok(count as usize)
    }

    async fn count_queued_runs_for_connection(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
    ) -> DomainResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM v2_runs
                 WHERE connection_revision_id = ?1 AND status = 'QUEUED'",
                params![connection_revision_id.0],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法统计排队的 Run"))?;
        Ok(count as usize)
    }

    /// 把"sources 里那些**排满**的连接上、从未尝试过"的 QUEUED run 改派到目标连接
    /// （见端口注释的三条边界）。
    ///
    /// 为什么要 `sources`：只剩"目标有空位"这一个条件时，一个空闲连接会把别人的活全吸走，
    /// 另一端反而空转（3+1 的池子上 4 条会全落到 3 个位的那边）。只有**排满的**连接才该被叫号。
    ///
    /// 一次事务（选 + 改）保证不会把同一条 run 选两次；`NOT EXISTS` 查 attempt 表而不是
    /// `status <> 'RETRY_WAIT'`——状态可能被别处改回 QUEUED，尝试记录才是硬证据。
    async fn adopt_queued_runs(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
        sources: &[crate::v2::domain::ids::ConnectionRevisionId],
        max: usize,
    ) -> DomainResult<usize> {
        let sources: Vec<_> = sources
            .iter()
            .copied()
            .filter(|source| *source != connection_revision_id)
            .collect();
        if max == 0 || sources.is_empty() {
            return Ok(0);
        }
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始改派事务"))?;
        let source_placeholders = std::iter::repeat_n("?", sources.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT r.id FROM v2_runs r
             WHERE r.status = 'QUEUED'
               AND r.connection_revision_id IN ({source_placeholders})
               AND NOT EXISTS (SELECT 1 FROM v2_run_attempts a WHERE a.run_id = r.id)
               AND r.recipe_revision_id IN (
                   SELECT recipe_revision_id FROM v2_runs WHERE connection_revision_id = ?
               )
             ORDER BY r.created_at ASC, r.id ASC
             LIMIT ?"
        );
        let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(sources.len() + 2);
        values.extend(
            sources
                .iter()
                .map(|id| rusqlite::types::Value::Integer(id.0 as i64)),
        );
        values.push(rusqlite::types::Value::Integer(connection_revision_id.0 as i64));
        values.push(rusqlite::types::Value::Integer(max as i64));
        let ids: Vec<i64> = {
            let mut statement = transaction
                .prepare(&sql)
                .map_err(|_| SafeError::storage("无法准备改派查询"))?;
            let mut rows = statement
                .query(rusqlite::params_from_iter(values.iter()))
                .map_err(|_| SafeError::storage("无法查询可改派的 Run"))?;
            let mut ids = Vec::new();
            while let Some(row) = rows
                .next()
                .map_err(|_| SafeError::storage("无法读取可改派的 Run"))?
            {
                ids.push(
                    row.get(0)
                        .map_err(|_| SafeError::storage("无法读取可改派的 Run"))?,
                );
            }
            ids
        };
        if ids.is_empty() {
            return Ok(0);
        }
        let update_placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let update = format!(
            "UPDATE v2_runs SET connection_revision_id = ?, updated_at = ?
             WHERE id IN ({update_placeholders})"
        );
        let mut update_values: Vec<rusqlite::types::Value> = Vec::with_capacity(ids.len() + 2);
        update_values.push(rusqlite::types::Value::Integer(connection_revision_id.0 as i64));
        update_values.push(rusqlite::types::Value::Integer(Timestamp::now_millis() as i64));
        update_values.extend(
            ids.iter()
                .map(|id| rusqlite::types::Value::Integer(*id)),
        );
        transaction
            .prepare(&update)
            .and_then(|mut statement| {
                statement.execute(rusqlite::params_from_iter(update_values.iter()))
            })
            .map_err(|_| SafeError::storage("无法改派 Run 的连接"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交改派事务"))?;
        Ok(ids.len())
    }

    async fn list_active_runs(&self, limit: usize) -> DomainResult<Vec<ActiveRun>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let mut statement = transaction
            .prepare(
                "SELECT r.id, r.operation_id, r.client_run_id, r.sequence, r.recipe_revision_id,
                        r.connection_revision_id, r.inputs_json, r.assets_json, r.status,
                        r.tracking_state, r.tracking_deadline, r.next_poll_at, r.last_polled_at,
                        r.created_at, r.updated_at, r.error_json
                 FROM v2_runs r
                 WHERE r.status IN ('ACCEPTED', 'RUNNING', 'SUBMISSION_UNCERTAIN')
                   AND r.tracking_state = 'active'
                 ORDER BY r.created_at ASC LIMIT ?1",
            )
            .map_err(|_| SafeError::storage("无法读取 Active Runs"))?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 Active Runs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 Active Runs"))?;
        let mut result = Vec::new();
        for row in rows {
            let operation_id = OperationId(row.1 as u64);
            let run = run_from_row(row, operation_id);
            let (vendor, protocol, connection_revision) =
                load_connection_revision(&transaction, run.connection_revision_id)?;
            let recipe_revision = load_recipe_revision(&transaction, run.recipe_revision_id)?;
            // 名字在同一个事务里按主键取一次；取不到就留空——`compose_filename` 会退回
            // 平台/驱动的原名，不会因为查不到名字就把产物名弄丢。
            let recipe_name: String = transaction
                .query_row(
                    "SELECT name FROM v2_recipes WHERE id = ?1",
                    params![recipe_revision.recipe_id.0 as i64],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            let attempt = load_latest_attempt(&transaction, run.id)?;
            if let Some(attempt) = attempt {
                result.push(ActiveRun {
                    run,
                    vendor,
                    protocol,
                    recipe_name,
                    recipe_revision,
                    connection_revision,
                    attempt,
                });
            }
        }
        drop(statement);
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(result)
    }

    async fn recover_expired_submissions(&self) -> DomainResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let window = (now_millis() as i64) - SUBMISSION_RECOVERY_WINDOW_MILLIS as i64;
        let mut expired: Vec<i64> = Vec::new();
        {
            let mut statement = transaction
                .prepare(
                    "SELECT a.run_id FROM v2_run_attempts a
                     JOIN v2_runs r ON r.id = a.run_id
                     WHERE a.status = 'SUBMITTING' AND CAST(a.started_at AS INTEGER) <= ?1
                       AND r.status = 'SUBMITTING'",
                )
                .map_err(|_| SafeError::storage("无法读取过期提交"))?;
            let mut rows = statement
                .query(params![window])
                .map_err(|_| SafeError::storage("无法读取过期提交"))?;
            while let Some(row) = rows
                .next()
                .map_err(|_| SafeError::storage("无法读取过期提交"))?
            {
                expired.push(
                    row.get::<_, i64>(0)
                        .map_err(|_| SafeError::storage("无法读取过期提交"))?,
                );
            }
        }
        let now = Timestamp::now();
        for run_id in &expired {
            transaction
                .execute(
                    "UPDATE v2_runs SET status = 'SUBMISSION_UNCERTAIN', tracking_state = 'active',
                            tracking_deadline = ?1, updated_at = ?2
                     WHERE id = ?3 AND status = 'SUBMITTING'",
                    params![
                        Timestamp::from_millis(
                            now_millis()
                                + crate::v2::domain::connection_options::DEFAULT_TRACKING_DEADLINE_MILLIS
                        )
                        .0,
                        now.0,
                        run_id
                    ],
                )
                .map_err(|_| SafeError::storage("无法恢复过期提交"))?;
            transaction
                .execute(
                    "UPDATE v2_run_attempts SET status = 'UNCERTAIN', completed_at = ?1
                     WHERE run_id = ?2 AND status = 'SUBMITTING'",
                    params![now.0, run_id],
                )
                .map_err(|_| SafeError::storage("无法恢复过期 Attempt"))?;
        }
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(expired.len())
    }

    async fn set_tracking(
        &self,
        run_id: RunId,
        tracking_state: TrackingState,
        next_poll_at: Option<Timestamp>,
        deadline: Option<Timestamp>,
    ) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        // 必须以纯字符串写入（与 transition_run 一致），否则 list_active_runs 的
        // WHERE tracking_state = 'active' 无法匹配带 JSON 引号的 "active"。
        let tracking_json = tracking_state.as_str().to_string();
        connection
            .execute(
                "UPDATE v2_runs SET tracking_state = ?1, next_poll_at = ?2, tracking_deadline = ?3,
                        updated_at = ?4 WHERE id = ?5",
                params![
                    tracking_json,
                    next_poll_at.map(|t| t.0),
                    deadline.map(|t| t.0),
                    Timestamp::now().0,
                    run_id.0
                ],
            )
            .map_err(|_| SafeError::storage("无法更新 tracking 状态"))?;
        Ok(())
    }

    async fn append_event(
        &self,
        run_id: RunId,
        event_type: String,
        payload: Value,
    ) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let operation_id: i64 = transaction
            .query_row(
                "SELECT operation_id FROM v2_runs WHERE id = ?1",
                params![run_id.0],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法读取 Run"))?;
        let payload_json = serde_json::to_string(&payload)
            .map_err(|_| SafeError::storage("无法序列化事件 payload"))?;
        let now = Timestamp::now();
        transaction
            .execute(
                "INSERT INTO v2_events(
                    event_type, operation_id, run_id, actor_json, sequence, payload_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event_type,
                    operation_id,
                    run_id.0,
                    serde_json::to_string(&ActorRef {
                        id: "system".to_string(),
                        kind: crate::v2::domain::actor::ActorKind::System,
                        client_id: crate::v2::domain::ids::ClientId("system".to_string()),
                        scopes: vec![],
                        label: None,
                    })
                    .unwrap_or_default(),
                    now.millis(),
                    payload_json,
                    now.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入事件"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(())
    }

    async fn find_latest_attempt(&self, run_id: RunId) -> DomainResult<Option<RunAttempt>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let attempt = load_latest_attempt(&transaction, run_id)?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(attempt)
    }

    async fn list_operations(
        &self,
        limit: usize,
        after: Option<OperationId>,
    ) -> DomainResult<Vec<crate::v2::ports::repositories::OperationSummary>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(&list_operations_sql())
            .map_err(|_| SafeError::storage("无法读取 Operations"))?;
        let rows = statement
            .query_map(params![after.map(|id| id.0 as i64), limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 Operations"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 Operations"))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, status, created_at, run_count, attention_status, attention_deadline)| {
                    // 原因码**不在这里手抄**：SQL 只回原始状态串，翻译一律走 domain
                    // （`attention_reason`）——于是"摘要里的原因码"与"get 里的原因码"
                    // 在结构上不可能不一致。
                    let attention_reason = attention_status
                        .as_deref()
                        .and_then(crate::v2::domain::operation::RunStatus::parse)
                        .and_then(crate::v2::domain::operation::attention_reason)
                        .map(str::to_string);
                    crate::v2::ports::repositories::OperationSummary {
                        id: OperationId(id as u64),
                        status: crate::v2::domain::operation::OperationStatus::parse(&status)
                            .unwrap_or(crate::v2::domain::operation::OperationStatus::Queued),
                        created_at: Timestamp(created_at),
                        run_count: run_count as u64,
                        attention_reason,
                        attention_deadline: attention_deadline.map(Timestamp),
                    }
                },
            )
            .collect())
    }

    async fn list_stale_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id FROM v2_operations
                 WHERE status IN ('QUEUED', 'RUNNING')
                   AND updated_at < ?1
                 ORDER BY updated_at ASC LIMIT ?2",
            )
            .map_err(|_| SafeError::storage("无法读取陈旧 Operations"))?;
        let rows = statement
            .query_map(params![cutoff.0, limit as i64], |row| row.get::<_, i64>(0))
            .map_err(|_| SafeError::storage("无法读取陈旧 Operations"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取陈旧 Operations"))?;
        Ok(rows.into_iter().map(|id| OperationId(id as u64)).collect())
    }

    async fn list_old_terminal_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id FROM v2_operations
                 WHERE status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'PARTIAL')
                   AND updated_at < ?1
                 ORDER BY updated_at ASC LIMIT ?2",
            )
            .map_err(|_| SafeError::storage("无法读取待清理 Operations"))?;
        let rows = statement
            .query_map(params![cutoff.0, limit as i64], |row| row.get::<_, i64>(0))
            .map_err(|_| SafeError::storage("无法读取待清理 Operations"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取待清理 Operations"))?;
        Ok(rows.into_iter().map(|id| OperationId(id as u64)).collect())
    }

    async fn force_fail_operation(
        &self,
        operation_id: OperationId,
        reason: &str,
    ) -> DomainResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let mut statement = transaction
            .prepare("SELECT id, status FROM v2_runs WHERE operation_id = ?1")
            .map_err(|_| SafeError::storage("无法读取陈旧 Run"))?;
        let run_rows = statement
            .query_map(params![operation_id.0], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| SafeError::storage("无法读取陈旧 Run"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取陈旧 Run"))?;
        drop(statement);
        let now = Timestamp::now();
        let mut failed = 0usize;
        let mut has_success = false;
        for (run_id, status) in &run_rows {
            if status == "SUCCEEDED" {
                has_success = true;
            }
            if is_terminal_run_status(status) {
                continue;
            }
            failed += 1;
            transaction
                .execute(
                    "UPDATE v2_runs SET status = 'FAILED', tracking_state = 'EXHAUSTED',
                            tracking_deadline = NULL, next_poll_at = NULL, updated_at = ?1
                     WHERE id = ?2",
                    params![now.0, *run_id],
                )
                .map_err(|_| SafeError::storage("无法将陈旧 Run 置为失败"))?;
            transaction
                .execute(
                    "UPDATE v2_run_attempts SET status = 'FAILED', completed_at = ?1
                     WHERE run_id = ?2
                       AND status NOT IN ('SUCCEEDED', 'FAILED', 'UNCERTAIN', 'CANCELLED')",
                    params![now.0, *run_id],
                )
                .map_err(|_| SafeError::storage("无法将陈旧 Attempt 置为失败"))?;
        }
        insert_event(
            &transaction,
            operation_id,
            None,
            &crate::v2::domain::actor::ActorRef {
                id: "system".to_string(),
                kind: crate::v2::domain::actor::ActorKind::System,
                client_id: crate::v2::domain::ids::ClientId("system".to_string()),
                scopes: vec![],
                label: None,
            },
            now.millis(),
            "operation.hygiene_failed",
            &now,
            json!({ "reason": reason, "failedRuns": failed }),
        )?;
        let projected = if has_success {
            crate::v2::domain::operation::OperationStatus::Partial
        } else {
            crate::v2::domain::operation::OperationStatus::Failed
        };
        transaction
            .execute(
                "UPDATE v2_operations SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![projected.as_str(), now.0, operation_id.0],
            )
            .map_err(|_| SafeError::storage("无法更新 Operation 状态"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(failed)
    }

    async fn delete_operations(&self, operation_ids: &[OperationId]) -> DomainResult<usize> {
        if operation_ids.is_empty() {
            return Ok(0);
        }
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let mut deleted = 0usize;
        // 产物缓存文件的 id：**行删了、文件还在**是这条路径此前的漏——库清了，
        // `artifacts\` 里那些图片没人认领，也永远没人删（只增不减）。所以先记下 id，
        // 事务提交之后再删文件。
        let mut orphan_files: Vec<i64> = Vec::new();
        for id in operation_ids {
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM v2_operations WHERE id = ?1",
                    params![id.0],
                    |_| Ok(true),
                )
                .optional()
                .map_err(|_| SafeError::storage("无法读取 Operation"))?
                .unwrap_or(false);
            if !exists {
                continue;
            }
            // 顺序要紧：这条 SELECT 必须在下面那条 DELETE 之前——删完就问不出来了。
            {
                let mut statement = transaction
                    .prepare(
                        "SELECT id FROM v2_assets
                         WHERE kind = 'artifact'
                           AND run_id IN (SELECT id FROM v2_runs WHERE operation_id = ?1)",
                    )
                    .map_err(|_| SafeError::storage("无法读取产物清单"))?;
                let rows = statement
                    .query_map(params![id.0], |row| row.get::<_, i64>(0))
                    .map_err(|_| SafeError::storage("无法读取产物清单"))?;
                for row in rows {
                    orphan_files.push(row.map_err(|_| SafeError::storage("无法读取产物清单"))?);
                }
            }
            transaction
                .execute(
                    "DELETE FROM v2_run_attempts
                     WHERE run_id IN (SELECT id FROM v2_runs WHERE operation_id = ?1)",
                    params![id.0],
                )
                .map_err(|_| SafeError::storage("无法清理 Run Attempts"))?;
            transaction
                .execute(
                    "DELETE FROM v2_assets
                     WHERE run_id IN (SELECT id FROM v2_runs WHERE operation_id = ?1)",
                    params![id.0],
                )
                .map_err(|_| SafeError::storage("无法清理资产"))?;
            transaction
                .execute(
                    "DELETE FROM v2_events WHERE operation_id = ?1",
                    params![id.0],
                )
                .map_err(|_| SafeError::storage("无法清理事件"))?;
            transaction
                .execute(
                    "DELETE FROM v2_callback_deliveries WHERE operation_id = ?1",
                    params![id.0],
                )
                .map_err(|_| SafeError::storage("无法清理回调"))?;
            transaction
                .execute("DELETE FROM v2_runs WHERE operation_id = ?1", params![id.0])
                .map_err(|_| SafeError::storage("无法清理 Runs"))?;
            transaction
                .execute("DELETE FROM v2_operations WHERE id = ?1", params![id.0])
                .map_err(|_| SafeError::storage("无法清理 Operation"))?;
            deleted += 1;
        }
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        // 提交之后才删文件：反过来（先删文件、事务回滚）等于把还能用的缓存丢掉了；
        // 这个顺序最坏只是留下一个孤儿文件——而 `remove_file` 失败**不会重试**（没有清扫循环），
        // 所以宁可留着也不先删：留下的是空间，删错的是数据。
        if let Some(dir) = &self.artifact_dir {
            for artifact_id in &orphan_files {
                let _ = std::fs::remove_file(dir.join(artifact_id.to_string()));
            }
        }
        Ok(deleted)
    }
}

fn is_terminal_attempt(status: &str) -> bool {
    matches!(status, "SUCCEEDED" | "FAILED" | "UNCERTAIN" | "CANCELLED")
}

/// `list_operations` 的 SQL：摘要 + **一条**相关子查询取"最阻塞的那条 run"的 attention。
///
/// 为什么是一条相关子查询，而不是"每条 operation 再查一次 run"：那是 N+1 —— 一页 50 条
/// 就是 51 次查询，而这个列表是 Agent 每轮都可能打的主路径。这里一条 SQL 出全部。
///
/// ⚠️ **状态集合与严重度排序都从 `domain::operation::ATTENTION_LADDER` 生成**，不在这里手抄：
/// 抄一份的失效模式是"SQL 里的顺序与 Rust 里的顺序不一致，两边测试各自都绿"，而表现是
/// 列表里挑中的那条与详情里挑中的那条不是同一条。SQL 里因此**只有状态名与档位**，
/// 原因码一个都不出现（它由 Rust 侧经 `attention_reason` 翻译，见 `list_operations`）。
fn list_operations_sql() -> String {
    let ladder = crate::v2::domain::operation::ATTENTION_LADDER;
    let statuses = ladder
        .iter()
        .map(|(status, _, _)| format!("'{}'", status.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let ranks = ladder
        .iter()
        .map(|(status, _, severity)| format!("WHEN '{}' THEN {severity}", status.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "SELECT o.id, o.status, o.created_at,
                (SELECT COUNT(*) FROM v2_runs r WHERE r.operation_id = o.id) AS run_count,
                attention.status, attention.tracking_deadline
         FROM v2_operations o
         LEFT JOIN v2_runs attention
                ON attention.id = (
                     SELECT candidate.id FROM v2_runs candidate
                      WHERE candidate.operation_id = o.id
                        AND candidate.status IN ({statuses})
                      ORDER BY CASE candidate.status {ranks} END, candidate.id ASC
                      LIMIT 1)
         WHERE (?1 IS NULL OR o.id < ?1)
         ORDER BY o.id DESC LIMIT ?2"
    )
}

fn is_terminal_run_status(status: &str) -> bool {
    matches!(status, "SUCCEEDED" | "FAILED" | "CANCELLED")
}

fn load_latest_attempt(
    transaction: &Transaction,
    run_id: RunId,
) -> DomainResult<Option<RunAttempt>> {
    let row = transaction
        .query_row(
            "SELECT id, run_id, number, status, request_fingerprint, fencing_token,
                    remote_handle_json, error_json, started_at, completed_at
             FROM v2_run_attempts WHERE run_id = ?1 ORDER BY number DESC LIMIT 1",
            params![run_id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()
        .map_err(|_| SafeError::storage("无法读取 Attempt"))?;
    Ok(row.map(
        |(
            id,
            run_id,
            number,
            status,
            fingerprint,
            token,
            remote_handle_json,
            error_json,
            started_at,
            completed_at,
        )| {
            RunAttempt {
                id: AttemptId(id as u64),
                run_id: RunId(run_id as u64),
                number: number as u32,
                status: AttemptStatus::parse(&status).unwrap_or(AttemptStatus::Submitting),
                request_fingerprint: fingerprint,
                fencing_token: token,
                remote_handle: remote_handle_json
                    .and_then(|value| serde_json::from_str(&value).ok()),
                error: error_json.and_then(|value| serde_json::from_str(&value).ok()),
                started_at: Timestamp(started_at),
                completed_at: completed_at.map(Timestamp),
            }
        },
    ))
}

fn refresh_operation_status(
    transaction: &Transaction,
    operation_id: OperationId,
) -> DomainResult<()> {
    let mut statement = transaction
        .prepare("SELECT r.status FROM v2_runs r WHERE r.operation_id = ?1")
        .map_err(|_| SafeError::storage("无法读取 Operation Runs"))?;
    let statuses = statement
        .query_map(params![operation_id.0], |row| row.get::<_, String>(0))
        .map_err(|_| SafeError::storage("无法读取 Operation Runs"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SafeError::storage("无法读取 Operation Runs"))?;
    let runs: Vec<Run> = statuses
        .into_iter()
        .map(|status| Run {
            // 这条投影只读了 status，没有用量列。
            usage: None,
            id: RunId(0),
            operation_id,
            client_run_id: String::new(),
            recipe_revision_id: RecipeRevisionId(0),
            connection_revision_id: ConnectionRevisionId(0),
            sequence: 0,
            inputs: Default::default(),
            assets: vec![],
            status: RunStatus::parse(&status).unwrap_or(RunStatus::Queued),
            tracking_state: TrackingState::Inactive,
            tracking_deadline: None,
            next_poll_at: None,
            last_polled_at: None,
            error: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        })
        .collect();
    let projected = crate::v2::domain::operation::project_operation_status(&runs);
    let status_json = projected.as_str().to_string();
    transaction
        .execute(
            "UPDATE v2_operations SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status_json, Timestamp::now().0, operation_id.0],
        )
        .map_err(|_| SafeError::storage("无法更新 Operation 状态"))?;
    Ok(())
}

#[async_trait]
impl LeaseStore for SqliteRepositories {
    async fn acquire(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let expires = Timestamp::from_millis(now_millis() + ttl_seconds * 1000);
        let changed = transaction
            .execute(
                "INSERT INTO v2_leases(id, owner, token, expires_at, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                    owner = excluded.owner,
                    token = excluded.token,
                    expires_at = excluded.expires_at,
                    updated_at = excluded.updated_at
                 WHERE v2_leases.owner = excluded.owner
                    OR CAST(v2_leases.expires_at AS INTEGER) <= ?4",
                params![owner, token, expires.0, now_millis() as i64],
            )
            .map_err(|_| SafeError::storage("无法获取调度租约"))?;
        if changed == 0 {
            return Err(SafeError::conflict(
                "LEASE_BUSY",
                "调度租约被其他所有者持有",
            ));
        }
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(())
    }

    async fn renew(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let expires = Timestamp::from_millis(now_millis() + ttl_seconds * 1000);
        let changed = connection
            .execute(
                "UPDATE v2_leases SET expires_at = ?1, updated_at = ?1
                 WHERE owner = ?2 AND token = ?3",
                params![expires.0, owner, token],
            )
            .map_err(|_| SafeError::storage("无法续租"))?;
        if changed == 0 {
            return Err(SafeError::conflict("LEASE_LOST", "调度租约已失效"));
        }
        Ok(())
    }

    async fn release(&self, owner: &str, token: &str) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .execute(
                "DELETE FROM v2_leases WHERE owner = ?1 AND token = ?2",
                params![owner, token],
            )
            .map_err(|_| SafeError::storage("无法释放租约"))?;
        Ok(())
    }

    async fn current(&self) -> DomainResult<Option<LeaseRecord>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT owner, token, expires_at FROM v2_leases WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取租约"))?;
        Ok(row.map(|(owner, token, expires_at)| LeaseRecord {
            owner,
            token,
            expires_at: Timestamp(expires_at),
        }))
    }
}

#[async_trait]
impl crate::v2::ports::repositories::ArtifactRepository for SqliteRepositories {
    async fn list_by_run(
        &self,
        run_id: RunId,
    ) -> DomainResult<Vec<crate::v2::ports::repositories::ArtifactSummary>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id, run_id, filename, mime_type, byte_size, checksum, created_at
                 FROM v2_assets WHERE kind = 'artifact' AND run_id = ?1 ORDER BY id ASC",
            )
            .map_err(|_| SafeError::storage("无法读取 Artifacts"))?;
        let rows = statement
            .query_map(params![run_id.0], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 Artifacts"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 Artifacts"))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, artifact_run_id, filename, mime_type, byte_size, checksum, created_at)| {
                    crate::v2::ports::repositories::ArtifactSummary {
                        id: ArtifactId(id as u64),
                        run_id: RunId(artifact_run_id.unwrap_or_default() as u64),
                        filename,
                        mime_type,
                        byte_size: byte_size as u64,
                        checksum,
                        created_at: Timestamp(created_at),
                    }
                },
            )
            .collect())
    }

    async fn list(
        &self,
        limit: usize,
        cursor: crate::v2::ports::repositories::ArtifactCursor,
    ) -> DomainResult<Vec<crate::v2::ports::repositories::ArtifactSummary>> {
        use crate::v2::ports::repositories::ArtifactCursor;
        // 两个方向各一条 SQL（不是"同一句加个 ORDER BY 变量"）：比较符、排序、游标含义**三者
        // 必须一起变**，拆开写反而容易配错（比如 `id > ?` 配 `ORDER BY id DESC` 会给出最旧的一批）。
        let (sql, bound) = match cursor {
            ArtifactCursor::After(after) => (
                "SELECT id, run_id, filename, mime_type, byte_size, checksum, created_at
                 FROM v2_assets WHERE kind = 'artifact' AND (?1 IS NULL OR id > ?1)
                 ORDER BY id ASC LIMIT ?2",
                after,
            ),
            ArtifactCursor::Before(before) => (
                "SELECT id, run_id, filename, mime_type, byte_size, checksum, created_at
                 FROM v2_assets WHERE kind = 'artifact' AND (?1 IS NULL OR id < ?1)
                 ORDER BY id DESC LIMIT ?2",
                before,
            ),
        };
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(sql)
            .map_err(|_| SafeError::storage("无法读取 Artifacts"))?;
        let rows = statement
            .query_map(params![bound.map(|id| id.0 as i64), limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 Artifacts"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 Artifacts"))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, artifact_run_id, filename, mime_type, byte_size, checksum, created_at)| {
                    crate::v2::ports::repositories::ArtifactSummary {
                        id: ArtifactId(id as u64),
                        run_id: RunId(artifact_run_id.unwrap_or_default() as u64),
                        filename,
                        mime_type,
                        byte_size: byte_size as u64,
                        checksum,
                        created_at: Timestamp(created_at),
                    }
                },
            )
            .collect())
    }
}

#[async_trait]
impl AssetRepository for SqliteRepositories {
    async fn find_asset(&self, id: AssetId) -> DomainResult<Option<MediaObject>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, kind, filename, mime_type, byte_size, checksum, storage_key
                 FROM v2_assets WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Asset"))?;
        Ok(row.map(
            |(id, kind, filename, mime_type, byte_size, checksum, storage_key)| MediaObject {
                id: crate::v2::domain::media::MediaObjectId::Asset(AssetId(id as u64)),
                kind: serde_json::from_str(&kind).unwrap_or_default(),
                filename,
                mime_type,
                byte_size: byte_size as u64,
                checksum,
                storage_key,
                provider_refs: vec![],
            },
        ))
    }

    async fn read_bytes(&self, id: AssetId) -> DomainResult<Option<Vec<u8>>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Option<Vec<u8>> = connection
            .query_row(
                "SELECT bytes FROM v2_assets WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Asset 字节"))?;
        Ok(bytes)
    }

    async fn create_asset(&self, media: MediaObject, bytes: Vec<u8>) -> DomainResult<AssetId> {
        let lock = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let kind = serde_json::to_string(&media.kind)
            .map_err(|_| SafeError::storage("无法序列化 media kind"))?;
        lock.execute(
            "INSERT INTO v2_assets(
                kind, filename, mime_type, byte_size, checksum, storage_key, bytes, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                kind,
                media.filename,
                media.mime_type,
                media.byte_size,
                media.checksum,
                media.storage_key,
                bytes,
                Timestamp::now().0
            ],
        )
        .map_err(|_| SafeError::storage("无法写入 V2 Asset"))?;
        Ok(AssetId(lock.last_insert_rowid() as u64))
    }
}

#[async_trait]
impl CallbackDeliveryRepository for SqliteRepositories {
    async fn list_operations_awaiting_callback(
        &self,
        since_millis: u64,
        limit: usize,
    ) -> DomainResult<Vec<(OperationId, u64)>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                // 终态判定与 `OperationStatus::is_terminal` 同一组字符串（写死在这里是有意的：
                // SQL 里没法调 Rust 的判定，改了那边要同步改这里——下面有测试钉着四种终态）。
                "SELECT o.id,
                        COALESCE((SELECT MAX(e.id) FROM v2_events e WHERE e.operation_id = o.id), 0)
                 FROM v2_operations o
                 WHERE o.callback_json IS NOT NULL
                   AND o.status IN ('SUCCEEDED', 'CANCELLED', 'FAILED', 'PARTIAL')
                   AND CAST(o.updated_at AS INTEGER) >= ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM v2_callback_deliveries d WHERE d.operation_id = o.id
                   )
                 ORDER BY o.id ASC LIMIT ?2",
            )
            .map_err(|_| SafeError::storage("无法读取待回调操作"))?;
        let rows = statement
            .query_map(params![since_millis as i64, limit as i64], |row| {
                Ok((
                    OperationId(row.get::<_, i64>(0)? as u64),
                    row.get::<_, i64>(1)? as u64,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取待回调操作"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取待回调操作"))?;
        Ok(rows)
    }

    async fn create_delivery(&self, delivery: CallbackDeliveryRecord) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let payload_json = serde_json::to_string(&delivery.payload)
            .map_err(|_| SafeError::storage("无法序列化回调 payload"))?;
        connection
            .execute(
                "INSERT INTO v2_callback_deliveries(
                    delivery_id, operation_id, event_id, url, payload_json, attempts,
                    max_attempts, next_retry_at, completed, last_error
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    delivery.delivery_id,
                    delivery.operation_id.0,
                    delivery.event_id,
                    delivery.url,
                    payload_json,
                    delivery.attempts,
                    delivery.max_attempts,
                    delivery.next_retry_at.0,
                    delivery.completed as i64,
                    delivery.last_error,
                ],
            )
            .map_err(|_| SafeError::storage("无法写入回调投递"))?;
        Ok(())
    }

    async fn list_due_deliveries(&self, limit: usize) -> DomainResult<Vec<CallbackDeliveryRecord>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT delivery_id, operation_id, event_id, url, payload_json, attempts,
                        max_attempts, next_retry_at, completed, last_error
                 FROM v2_callback_deliveries
                 WHERE completed = 0 AND CAST(next_retry_at AS INTEGER) <= ?1
                 ORDER BY next_retry_at ASC LIMIT ?2",
            )
            .map_err(|_| SafeError::storage("无法读取待投递回调"))?;
        let rows = statement
            .query_map(params![now_millis() as i64, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取待投递回调"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取待投递回调"))?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    delivery_id,
                    operation_id,
                    event_id,
                    url,
                    payload_json,
                    attempts,
                    max_attempts,
                    next_retry_at,
                    completed,
                    last_error,
                )| CallbackDeliveryRecord {
                    delivery_id,
                    operation_id: OperationId(operation_id as u64),
                    event_id: event_id as u64,
                    url,
                    payload: serde_json::from_str(&payload_json).unwrap_or(Value::Null),
                    attempts: attempts as u32,
                    max_attempts: max_attempts as u32,
                    next_retry_at: Timestamp(next_retry_at),
                    completed: completed != 0,
                    last_error,
                },
            )
            .collect())
    }

    async fn mark_delivery_attempt(
        &self,
        delivery_id: &str,
        success: bool,
        error: Option<String>,
    ) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let attempts: i64 = transaction
            .query_row(
                "SELECT attempts FROM v2_callback_deliveries WHERE delivery_id = ?1",
                params![delivery_id],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法读取回调投递"))?;
        let max_attempts: i64 = transaction
            .query_row(
                "SELECT max_attempts FROM v2_callback_deliveries WHERE delivery_id = ?1",
                params![delivery_id],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法读取回调投递"))?;
        let next_attempts = attempts + 1;
        let completed = success || next_attempts >= max_attempts;
        let next_retry = if completed {
            Timestamp::now().0
        } else {
            Timestamp::from_millis(now_millis() + next_attempts as u64 * 30_000).0
        };
        transaction
            .execute(
                "UPDATE v2_callback_deliveries SET attempts = ?1, last_error = ?2,
                        next_retry_at = ?3, completed = ?4 WHERE delivery_id = ?5",
                params![
                    next_attempts,
                    error,
                    next_retry,
                    completed as i64,
                    delivery_id
                ],
            )
            .map_err(|_| SafeError::storage("无法更新回调投递"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        Ok(())
    }
}

#[async_trait]
impl ArtifactStore for SqliteRepositories {
    async fn store(&self, artifact: ArtifactObject) -> DomainResult<()> {
        // 有字节时写入本地缓存文件（不进数据库，避免 DB 随结果膨胀）。
        if !artifact.bytes.is_empty() {
            if let Some(dir) = &self.artifact_dir {
                std::fs::write(dir.join(artifact.id.0.to_string()), &artifact.bytes)
                    .map_err(|_| SafeError::storage("无法写入产物缓存文件"))?;
            }
        }
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .execute(
                "INSERT OR IGNORE INTO v2_assets(
                    id, kind, run_id, filename, mime_type, byte_size, checksum, storage_key, bytes, url, created_at
                 ) VALUES (?1, 'artifact', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    artifact.id.0,
                    artifact.run_id,
                    artifact.filename,
                    artifact.mime_type,
                    artifact.byte_size,
                    artifact.checksum,
                    artifact.storage_key,
                    // bytes 列保留（旧版 NOT NULL，写空值），真实产物字节走文件或远端 URL。
                    Vec::<u8>::new(),
                    artifact.url,
                    Timestamp::now().0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 Artifact"))?;
        Ok(())
    }

    async fn load(&self, id: ArtifactId) -> DomainResult<Option<ArtifactObject>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, filename, mime_type, byte_size, checksum, storage_key, url
                 FROM v2_assets WHERE id = ?1 AND kind = 'artifact'",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Artifact"))?;
        Ok(row.map(
            |(id, filename, mime_type, byte_size, checksum, storage_key, url)| ArtifactObject {
                id: ArtifactId(id as u64),
                run_id: None,
                filename,
                mime_type,
                byte_size: byte_size as u64,
                checksum,
                storage_key,
                url,
                bytes: Vec::new(),
            },
        ))
    }

    async fn read_bytes(&self, id: ArtifactId) -> DomainResult<Option<Vec<u8>>> {
        // 优先读本地缓存文件。
        if let Some(dir) = &self.artifact_dir {
            let path = dir.join(id.0.to_string());
            if path.is_file() {
                return std::fs::read(&path)
                    .map(Some)
                    .map_err(|_| SafeError::storage("无法读取产物缓存文件"));
            }
        }
        // 兼容迁移前历史数据：字节仍在数据库里的旧产物。
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Option<Vec<u8>> = connection
            .query_row(
                "SELECT bytes FROM v2_assets WHERE id = ?1 AND kind = 'artifact'",
                params![id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 Artifact 字节"))?;
        Ok(bytes.filter(|bytes| !bytes.is_empty()))
    }

    async fn cache_bytes(&self, id: ArtifactId, bytes: &[u8]) -> DomainResult<()> {
        // 与 `read_bytes` 的读取顺序**对称**：有缓存目录就落文件（大字节不进库），
        // 没有目录（内存库 / 测试）就退回库里的 bytes 列。
        match &self.artifact_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)
                    .map_err(|_| SafeError::storage("无法创建产物缓存目录"))?;
                std::fs::write(dir.join(id.0.to_string()), bytes)
                    .map_err(|_| SafeError::storage("无法写入产物缓存文件"))?;
            }
            None => {
                let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
                connection
                    .execute(
                        "UPDATE v2_assets SET bytes = ?2 WHERE id = ?1 AND kind = 'artifact'",
                        params![id.0, bytes],
                    )
                    .map_err(|_| SafeError::storage("无法写入产物字节"))?;
            }
        }
        // 字节到手了才刷新大小：`byte_size = 0` 原本的意思是"只知道字节在平台上"，
        // 现在它的意思是"还没取过"。已有大小（>0）不动——那是平台当时的报价。
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .execute(
                "UPDATE v2_assets SET byte_size = ?2
                 WHERE id = ?1 AND kind = 'artifact' AND byte_size = 0",
                params![id.0, bytes.len() as i64],
            )
            .map_err(|_| SafeError::storage("无法更新产物大小"))?;
        Ok(())
    }

    async fn exists(&self, checksum: &str) -> DomainResult<bool> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM v2_assets WHERE checksum = ?1 AND kind = 'artifact')",
                params![checksum],
                |row| row.get(0),
            )
            .map_err(|_| SafeError::storage("无法检查 Artifact"))?;
        Ok(exists)
    }
}

#[allow(dead_code)]
pub struct HttpCallbackDeliverer {
    client: reqwest::Client,
}

#[allow(dead_code)]
impl HttpCallbackDeliverer {
    pub fn new() -> Result<Self, String> {
        // 回调也是出站 HTTP，走同一条代理装配：**没有例外**。
        // （这条以前是"刻意直连"，理由是回调地址是用户自己的内网服务；但规则一旦有例外，
        // 下一个新增的出网路径就会漏掉代理。回环地址由判据本身保证直连，
        // 内网地址由系统绕过列表管——那正是浏览器处理自己内网服务的方式。）
        let client = crate::v2::infrastructure::providers::proxy::configure(
            reqwest::Client::builder(),
        )
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("无法创建回调客户端：{error}"))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl CallbackDeliverer for HttpCallbackDeliverer {
    async fn deliver(&self, request: DeliveryRequest) -> DomainResult<DeliveryOutcome> {
        let parsed = reqwest::Url::parse(&request.url)
            .map_err(|_| SafeError::validation("CALLBACK_URL_INVALID", "回调 URL 无效"))?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            return Ok(DeliveryOutcome::Rejected {
                message: "回调 URL 必须使用 http 或 https".to_string(),
            });
        }
        if parsed.username() != "" || parsed.password().is_some() {
            return Ok(DeliveryOutcome::Rejected {
                message: "回调 URL 不允许携带用户信息".to_string(),
            });
        }
        // 签名头由 application 层算好带下来（密钥不在这一层碰）。没配密钥就没有这个头。
        let mut builder = self.client.post(request.url).json(&request.payload);
        if let Some(signature) = request.signature.as_deref() {
            builder = builder.header(crate::v2::ports::callback::SIGNATURE_HEADER, signature);
        }
        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                return Ok(DeliveryOutcome::NetworkError {
                    message: error.to_string(),
                    retryable: error.is_timeout() || error.is_connect(),
                })
            }
        };
        if response.status().is_success() {
            Ok(DeliveryOutcome::Accepted)
        } else {
            Ok(DeliveryOutcome::Rejected {
                message: format!("回调返回 {}", response.status()),
            })
        }
    }
}
