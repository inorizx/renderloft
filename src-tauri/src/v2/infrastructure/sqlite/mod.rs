use async_trait::async_trait;
use rusqlite::{params, Connection as SqliteConnection, OptionalExtension, Transaction};
use serde_json::Value;
use std::sync::{Arc, Mutex};

use crate::v2::domain::actor::ActorRef;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::event::EventRecord;
use crate::v2::domain::ids::{
    ConnectionId, ConnectionRevisionId, OperationId, RecipeId, RecipeRevisionId, RunId,
};
use crate::v2::domain::model::{
    Connection as DomainConnection, ConnectionRevision, Recipe, RecipeBinding, RecipeRevision,
    Timestamp,
};
use crate::v2::domain::operation::{
    Operation, OperationStatus, Run, RunAttempt, RunStatus, TrackingState,
};
use crate::v2::ports::repositories::{
    ConnectionRepository, EventStore, NewOperation, OperationRepository, PersistedOperation,
    RecipeRepository, SettingsRecord, SettingsRepository, WorkspaceRepository,
};

pub mod exec;
mod tokens;

pub fn initialize_schema(connection: &SqliteConnection) -> Result<(), String> {
    connection
        .execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS v2_recipes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                labels_json TEXT NOT NULL DEFAULT '[]',
                capability TEXT NOT NULL,
                current_revision_id INTEGER NOT NULL,
                visibility TEXT NOT NULL DEFAULT 'private',
                favorite INTEGER NOT NULL DEFAULT 0,
                group_name TEXT,
                note TEXT,
                last_opened_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS v2_recipe_revisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                recipe_id INTEGER NOT NULL REFERENCES v2_recipes(id),
                revision INTEGER NOT NULL,
                input_schema_json TEXT NOT NULL,
                output_schema_json TEXT NOT NULL,
                binding_json TEXT NOT NULL,
                defaults_json TEXT NOT NULL DEFAULT '{}',
                source TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(recipe_id, revision)
            );

            CREATE TABLE IF NOT EXISTS v2_connections (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                vendor TEXT NOT NULL,
                protocol TEXT NOT NULL,
                display_name TEXT NOT NULL,
                current_revision_id INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'active'
            );

            CREATE TABLE IF NOT EXISTS v2_connection_revisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                connection_id INTEGER NOT NULL REFERENCES v2_connections(id),
                config_json TEXT NOT NULL,
                secret_bindings_json TEXT NOT NULL DEFAULT '[]',
                effective_capabilities_json TEXT NOT NULL DEFAULT '[]',
                lifecycle_features_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS v2_operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                actor_id TEXT NOT NULL,
                actor_kind TEXT NOT NULL,
                client_id TEXT NOT NULL,
                actor_label TEXT,
                origin TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                request_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                callback_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS v2_operations_idempotency_idx
                ON v2_operations(actor_id, client_id, idempotency_key);

            CREATE TABLE IF NOT EXISTS v2_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_id INTEGER NOT NULL REFERENCES v2_operations(id),
                client_run_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                recipe_revision_id INTEGER NOT NULL REFERENCES v2_recipe_revisions(id),
                connection_revision_id INTEGER NOT NULL REFERENCES v2_connection_revisions(id),
                inputs_json TEXT NOT NULL DEFAULT '{}',
                assets_json TEXT NOT NULL DEFAULT '[]',
                status TEXT NOT NULL,
                tracking_state TEXT NOT NULL,
                tracking_deadline TEXT,
                next_poll_at TEXT,
                last_polled_at TEXT,
                error_json TEXT,
                usage_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(operation_id, sequence),
                UNIQUE(operation_id, client_run_id)
            );
            CREATE INDEX IF NOT EXISTS v2_runs_status_idx ON v2_runs(status, created_at);

            CREATE TABLE IF NOT EXISTS v2_run_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES v2_runs(id),
                number INTEGER NOT NULL,
                status TEXT NOT NULL,
                request_fingerprint TEXT NOT NULL,
                fencing_token TEXT NOT NULL,
                remote_handle_json TEXT,
                error_json TEXT,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                UNIQUE(run_id, number)
            );

            CREATE TABLE IF NOT EXISTS v2_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                operation_id INTEGER NOT NULL REFERENCES v2_operations(id),
                run_id INTEGER REFERENCES v2_runs(id),
                actor_json TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                payload_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS v2_events_operation_idx ON v2_events(operation_id, sequence);

            CREATE TABLE IF NOT EXISTS v2_leases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                owner TEXT NOT NULL,
                token TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS v2_assets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                run_id INTEGER,
                filename TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                byte_size INTEGER NOT NULL,
                checksum TEXT NOT NULL,
                storage_key TEXT,
                bytes BLOB,
                url TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS v2_callback_deliveries (
                delivery_id TEXT PRIMARY KEY NOT NULL,
                operation_id INTEGER NOT NULL REFERENCES v2_operations(id),
                event_id INTEGER NOT NULL,
                url TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL,
                next_retry_at TEXT NOT NULL,
                completed INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );

            CREATE TABLE IF NOT EXISTS v2_actor_tokens (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                token_hash TEXT NOT NULL UNIQUE,
                actor_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                client_id TEXT NOT NULL,
                scopes_json TEXT NOT NULL,
                label TEXT,
                expires_at TEXT,
                revoked INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            -- ⚠️ 从这张表开始的六张（`v2_agents` / `v2_deliveries` / `v2_delivery_operations`
            -- / `v2_delivery_watches` / `v2_conversations` / `v2_messages` / `v2_rig_messages`）
            -- **没有任何写入方**：它们服务的那套对话式 Agent 已经不在这个代码库里了。留着是因为
            -- 删表要走一次 schema 迁移，而它们不挡任何事（惰性、不参与任何查询路径）。
            -- **不要**照它们去补代码：新功能请另起表。
            CREATE TABLE IF NOT EXISTS v2_agents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                system_prompt TEXT NOT NULL,
                llm_connection_revision_id INTEGER NOT NULL,
                llm_model TEXT NOT NULL DEFAULT '',
                recipe_allowlist_json TEXT NOT NULL DEFAULT '[]',
                temperature REAL NOT NULL DEFAULT 0.0,
                max_turns INTEGER NOT NULL DEFAULT 4,
                max_wait_seconds INTEGER NOT NULL DEFAULT 60,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                -- 软删标记：只打时间戳、不真删行。NULL = 未删除。
                deleted_at TEXT
            );

            CREATE TABLE IF NOT EXISTS v2_deliveries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                actor_json TEXT NOT NULL,
                executing_agent_id INTEGER NOT NULL REFERENCES v2_agents(id),
                goal TEXT NOT NULL,
                requirements_json TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL,
                pending_question_json TEXT,
                context_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS v2_deliveries_status_idx ON v2_deliveries(status, created_at);

            CREATE TABLE IF NOT EXISTS v2_delivery_operations (
                delivery_id INTEGER NOT NULL REFERENCES v2_deliveries(id),
                operation_id INTEGER NOT NULL REFERENCES v2_operations(id),
                PRIMARY KEY (delivery_id, operation_id)
            );

            -- 「好了叫我」的登记：某个任务到达终态时要叫醒谁。
            -- 行在唤醒后删除，所以这张表同时是「待办」与「去过重」。
            CREATE TABLE IF NOT EXISTS v2_delivery_watches (
                delivery_id INTEGER NOT NULL REFERENCES v2_deliveries(id),
                operation_id INTEGER NOT NULL REFERENCES v2_operations(id),
                created_at TEXT NOT NULL,
                PRIMARY KEY (delivery_id, operation_id)
            );
            CREATE INDEX IF NOT EXISTS v2_delivery_watches_delivery_idx
                ON v2_delivery_watches(delivery_id);

            CREATE TABLE IF NOT EXISTS v2_conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                delivery_id INTEGER NOT NULL REFERENCES v2_deliveries(id),
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS v2_conversations_delivery_idx
                ON v2_conversations(delivery_id);

            CREATE TABLE IF NOT EXISTS v2_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL REFERENCES v2_conversations(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL DEFAULT '',
                tool_calls_json TEXT NOT NULL DEFAULT '[]',
                tool_results_json TEXT NOT NULL DEFAULT '[]',
                operation_ids_json TEXT NOT NULL DEFAULT '[]',
                image_asset_ids_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS v2_messages_conversation_idx
                ON v2_messages(conversation_id, id);

            CREATE TABLE IF NOT EXISTS v2_rig_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL REFERENCES v2_conversations(id),
                message_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS v2_rig_messages_conversation_idx
                ON v2_rig_messages(conversation_id, id);

            CREATE TABLE IF NOT EXISTS v2_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                api_base_url TEXT NOT NULL DEFAULT '',
                llm_base_url TEXT NOT NULL DEFAULT '',
                llm_model TEXT NOT NULL DEFAULT '',
                download_dir TEXT NOT NULL DEFAULT ''
            );
            ",
        )
        .map_err(|error| format!("无法初始化 V2 schema：{error}"))?;
    ensure_column(connection, "v2_assets", "run_id", "INTEGER")?;
    ensure_column(
        connection,
        "v2_recipes",
        "favorite",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(connection, "v2_recipes", "group_name", "TEXT")?;
    ensure_column(connection, "v2_recipes", "note", "TEXT")?;
    ensure_column(connection, "v2_recipes", "last_opened_at", "TEXT")?;
    ensure_column(connection, "v2_runs", "error_json", "TEXT")?;
    // 平台报的用量（`{unit, amount}`）。可空 = "平台没说"，与 0（这次免费）不是一回事。
    ensure_column(connection, "v2_runs", "usage_json", "TEXT")?;
    // 用户消息附带的图片资产 id（Agent 多模态输入的历史回看）。**加法式迁移**：
    // `ensure_column` 先查 PRAGMA table_info，缺列才 ADD COLUMN，且带 DEFAULT '[]'
    // ⇒ 老库的数据、既有消息都不受影响。
    ensure_column(
        connection,
        "v2_messages",
        "image_asset_ids_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    // 产物字节不再进数据库（中转站定位）：v2_assets 增加 url 列，
    // 产物结果存远端 URL 或本地缓存文件；bytes 列仅保留给上传的输入素材。
    ensure_column(connection, "v2_assets", "url", "TEXT")?;
    // 软删标记：老库增量补列（纯加列，向后兼容，不动任何既有数据）。
    ensure_column(connection, "v2_agents", "deleted_at", "TEXT")?;
    // 字段备注（展示元数据）：老库增量补列，默认空对象 ⇒ 既有 Recipe 行为不变。
    ensure_column(
        connection,
        "v2_recipes",
        "field_labels_json",
        "TEXT NOT NULL DEFAULT '{}'",
    )?;
    // **墓碑**：非空 = 已从资源库移除（可恢复）。默认 NULL ⇒ 既有 Recipe 全部"在"，
    // 老库行为不变（这正是加法式迁移的意义：不解释历史数据，只给新列一个安全的默认）。
    ensure_column(connection, "v2_recipes", "removed_at", "TEXT")?;
    Ok(())
}

fn ensure_column(
    connection: &SqliteConnection,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("无法读取 {table} 表结构：{error}"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("无法读取 {table} 表结构：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取 {table} 表结构：{error}"))?
        .iter()
        .any(|name| name == column);
    if !exists {
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {column_type};"
            ))
            .map_err(|error| format!("无法为 {table} 添加列 {column}：{error}"))?;
    }
    Ok(())
}

fn open_in_memory() -> Result<SqliteConnection, String> {
    let connection = SqliteConnection::open_in_memory()
        .map_err(|error| format!("无法打开内存数据库：{error}"))?;
    initialize_schema(&connection)?;
    Ok(connection)
}

pub struct SqliteRepositories {
    connection: Arc<Mutex<SqliteConnection>>,
    /// 产物本地缓存目录（url 为 None 的产物字节存这里，不进数据库）。
    artifact_dir: Option<std::path::PathBuf>,
}

impl SqliteRepositories {
    pub fn in_memory() -> Result<Self, String> {
        Ok(Self {
            connection: Arc::new(Mutex::new(open_in_memory()?)),
            artifact_dir: None,
        })
    }

    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建 V2 数据库目录：{error}"))?;
        }
        let artifact_dir = path.parent().map(|parent| parent.join("artifacts"));
        if let Some(dir) = &artifact_dir {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("无法创建产物缓存目录：{error}"))?;
        }
        let connection =
            SqliteConnection::open(path).map_err(|error| format!("无法打开 V2 数据库：{error}"))?;
        initialize_schema(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            artifact_dir,
        })
    }
}

#[async_trait]
impl WorkspaceRepository for SqliteRepositories {
    fn database_path(&self) -> Option<std::path::PathBuf> {
        // 连接自己知道它开在哪个文件上；内存库给 None。
        self.connection
            .lock()
            .ok()
            .and_then(|connection| connection.path().map(std::path::PathBuf::from))
    }

    fn artifact_dir(&self) -> Option<std::path::PathBuf> {
        self.artifact_dir.clone()
    }

    async fn snapshot_database(&self, target: &std::path::Path) -> DomainResult<()> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SafeError::storage("V2 数据库锁不可用"))?;
        // `VACUUM INTO` 是 SQLite 官方的"给活库做一份一致副本"的做法：它在一个读事务里重新
        // 写出一份完整自洽的库文件——不像拷文件那样可能漏掉 WAL 里还没落盘的事务。
        // 目标文件必须**不存在**（SQLite 自己也会拒绝），这与用例里的"不覆盖已有备份"是同一件事。
        connection
            .execute("VACUUM INTO ?1", params![target.to_string_lossy()])
            .map_err(|error| SafeError::storage(format!("导出数据库失败：{error}")))?;
        Ok(())
    }
}

#[async_trait]
impl SettingsRepository for SqliteRepositories {
    async fn load_settings(&self) -> DomainResult<SettingsRecord> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SafeError::storage("V2 数据库锁不可用"))?;
        let record = connection
            .query_row(
                // llm_base_url / llm_model 列已废弃（LLM 配置不再走设置页），
                // 列保留在 schema 中兼容老库，不再读取。
                "SELECT download_dir FROM v2_settings WHERE id = 1",
                [],
                |row| {
                    Ok(SettingsRecord {
                        download_dir: row.get(0)?,
                    })
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取设置"))?
            .unwrap_or_default();
        Ok(record)
    }

    async fn save_settings(&self, record: &SettingsRecord) -> DomainResult<()> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SafeError::storage("V2 数据库锁不可用"))?;
        connection
            .execute(
                // 废弃列有 DEFAULT ''，INSERT 不再提供。
                "INSERT INTO v2_settings (id, download_dir) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET download_dir = excluded.download_dir",
                params![record.download_dir],
            )
            .map_err(|_| SafeError::storage("无法保存设置"))?;
        Ok(())
    }
}

impl SqliteRepositories {
    fn load_operation(
        transaction: &Transaction,
        id: OperationId,
    ) -> DomainResult<Option<PersistedOperation>> {
        let row = transaction
            .query_row(
                "SELECT actor_id, actor_kind, client_id, actor_label, origin, idempotency_key,
                        request_hash, status, metadata_json, callback_json, created_at, updated_at
                 FROM v2_operations WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 Operation"))?;
        let Some((
            actor_id,
            actor_kind,
            client_id,
            actor_label,
            origin,
            idempotency_key,
            request_hash,
            status,
            metadata_json,
            callback_json,
            created_at,
            updated_at,
        )) = row
        else {
            return Ok(None);
        };
        let actor = ActorRef {
            id: actor_id,
            kind: serde_json::from_str(&actor_kind)
                .unwrap_or(crate::v2::domain::actor::ActorKind::System),
            client_id: crate::v2::domain::ids::ClientId(client_id),
            scopes: vec![],
            label: actor_label,
        };
        let operation = Operation {
            id,
            actor,
            origin,
            idempotency_key: crate::v2::domain::ids::IdempotencyKey(idempotency_key),
            request_hash,
            status: OperationStatus::parse(&status).unwrap_or(OperationStatus::Queued),
            metadata: serde_json::from_str(&metadata_json).unwrap_or_default(),
            callback: callback_json.and_then(|value| serde_json::from_str(&value).ok()),
            created_at: Timestamp(created_at),
            updated_at: Timestamp(updated_at),
        };

        let mut statement = transaction
            .prepare(
                "SELECT id, client_run_id, sequence, recipe_revision_id, connection_revision_id,
                        inputs_json, assets_json, status, tracking_state, tracking_deadline,
                        next_poll_at, last_polled_at, error_json, created_at, updated_at, usage_json
                 FROM v2_runs WHERE operation_id = ?1 ORDER BY sequence ASC",
            )
            .map_err(|_| SafeError::storage("无法读取 V2 Runs"))?;
        let runs = statement
            .query_map(params![id.0], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    // 放在末尾：不动前面 13/14 的下标，少一类手滑。
                    row.get::<_, Option<String>>(15)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 V2 Runs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 V2 Runs"))?;
        let runs = runs
            .into_iter()
            .map(
                |(
                    run_id,
                    client_run_id,
                    sequence,
                    recipe_revision_id,
                    connection_revision_id,
                    inputs_json,
                    assets_json,
                    status,
                    tracking_state,
                    tracking_deadline,
                    next_poll_at,
                    last_polled_at,
                    error_json,
                    created_at,
                    updated_at,
                    usage_json,
                )| {
                    Run {
                        id: RunId(run_id as u64),
                        operation_id: id,
                        client_run_id,
                        recipe_revision_id: RecipeRevisionId(recipe_revision_id as u64),
                        connection_revision_id: ConnectionRevisionId(connection_revision_id as u64),
                        sequence: sequence as u32,
                        inputs: serde_json::from_str(&inputs_json).unwrap_or_default(),
                        assets: serde_json::from_str(&assets_json).unwrap_or_default(),
                        status: RunStatus::parse(&status).unwrap_or(RunStatus::Queued),
                        tracking_state: TrackingState::parse(&tracking_state)
                            .unwrap_or(TrackingState::Inactive),
                        tracking_deadline: tracking_deadline.map(Timestamp),
                        next_poll_at: next_poll_at.map(Timestamp),
                        last_polled_at: last_polled_at.map(Timestamp),
                        error: error_json.and_then(|value| serde_json::from_str(&value).ok()),
                        // 平台报的用量：`None` = 平台没说，与 0（免费）不是一回事。
                        usage: usage_json.and_then(|value| serde_json::from_str(&value).ok()),
                        created_at: Timestamp(created_at),
                        updated_at: Timestamp(updated_at),
                    }
                },
            )
            .collect();
        Ok(Some(PersistedOperation { operation, runs }))
    }
}

#[async_trait]
impl RecipeRepository for SqliteRepositories {
    async fn find_recipe(&self, id: RecipeId) -> DomainResult<Option<Recipe>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, name, description, labels_json, capability, current_revision_id,
                        visibility, favorite, group_name, note, last_opened_at,
                        created_at, updated_at, field_labels_json, removed_at
                 FROM v2_recipes WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        // 新列追加在 SELECT 末尾 ⇒ 索引 13（老库由 ensure_column 补列，值为 '{}'）。
                        row.get::<_, String>(13)?,
                        // 墓碑：索引 14（老库由 ensure_column 补列，值为 NULL）。
                        row.get::<_, Option<String>>(14)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 Recipe"))?;
        Ok(row.map(
            |(
                id,
                name,
                description,
                labels_json,
                capability,
                current_revision_id,
                visibility,
                favorite,
                group_name,
                note,
                last_opened_at,
                created_at,
                updated_at,
                field_labels_json,
                removed_at,
            )| Recipe {
                id: RecipeId(id as u64),
                name,
                description,
                labels: serde_json::from_str(&labels_json).unwrap_or_default(),
                capability,
                current_revision_id: RecipeRevisionId(current_revision_id as u64),
                visibility: serde_json::from_str(&visibility).unwrap_or_default(),
                created_at: Timestamp(created_at),
                updated_at: Timestamp(updated_at),
                favorite: favorite != 0,
                group: group_name,
                note,
                last_opened_at: last_opened_at.map(Timestamp),
                field_labels: serde_json::from_str(&field_labels_json).unwrap_or_default(),
                removed_at: removed_at.map(Timestamp),
            },
        ))
    }

    async fn find_revision(&self, id: RecipeRevisionId) -> DomainResult<Option<RecipeRevision>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
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
            .map_err(|_| SafeError::storage("无法读取 V2 Recipe revision"))?;
        Ok(row.map(
            |(
                id,
                recipe_id,
                revision,
                input_schema_json,
                output_schema_json,
                binding_json,
                defaults_json,
                source,
                created_at,
            )| RecipeRevision {
                id: RecipeRevisionId(id as u64),
                recipe_id: RecipeId(recipe_id as u64),
                revision: revision as u32,
                input_schema: serde_json::from_str(&input_schema_json).unwrap_or(Value::Null),
                output_schema: serde_json::from_str(&output_schema_json).unwrap_or(Value::Null),
                binding: serde_json::from_str(&binding_json).unwrap_or_default(),
                defaults: serde_json::from_str(&defaults_json).unwrap_or_default(),
                source: serde_json::from_str(&source).unwrap_or_default(),
                created_at: Timestamp(created_at),
            },
        ))
    }

    async fn list_recipes(
        &self,
        limit: usize,
        after: Option<RecipeId>,
    ) -> DomainResult<Vec<(Recipe, RecipeBinding)>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT r.id, r.name, r.description, r.labels_json, r.capability,
                        r.current_revision_id, r.visibility, r.favorite, r.group_name,
                        r.note, r.last_opened_at, r.created_at, r.updated_at,
                        rev.binding_json, r.field_labels_json, r.removed_at
                 FROM v2_recipes r
                 LEFT JOIN v2_recipe_revisions rev ON rev.id = r.current_revision_id
                 WHERE r.removed_at IS NULL
                   AND (?1 IS NULL OR r.id > ?1)
                 ORDER BY r.id ASC LIMIT ?2",
            )
            .map_err(|_| SafeError::storage("无法读取 V2 Recipes"))?;
        let rows = statement
            .query_map(params![after.map(|id| id.0 as i64), limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 V2 Recipes"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 V2 Recipes"))?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    name,
                    description,
                    labels_json,
                    capability,
                    current_revision_id,
                    visibility,
                    favorite,
                    group_name,
                    note,
                    last_opened_at,
                    created_at,
                    updated_at,
                    binding_json,
                    field_labels_json,
                    removed_at,
                )| {
                    let recipe = Recipe {
                        id: RecipeId(id as u64),
                        name,
                        description,
                        labels: serde_json::from_str(&labels_json).unwrap_or_default(),
                        capability,
                        current_revision_id: RecipeRevisionId(current_revision_id as u64),
                        visibility: serde_json::from_str(&visibility).unwrap_or_default(),
                        created_at: Timestamp(created_at),
                        updated_at: Timestamp(updated_at),
                        favorite: favorite != 0,
                        group: group_name,
                        note,
                        last_opened_at: last_opened_at.map(Timestamp),
                        field_labels: serde_json::from_str(&field_labels_json).unwrap_or_default(),
                        removed_at: removed_at.map(Timestamp),
                    };
                    let binding = binding_json
                        .and_then(|json| serde_json::from_str(&json).ok())
                        .unwrap_or_default();
                    (recipe, binding)
                },
            )
            .collect())
    }

    async fn create_recipe(
        &self,
        recipe: Recipe,
        revision: RecipeRevision,
    ) -> DomainResult<(Recipe, RecipeRevision)> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        let labels_json = serde_json::to_string(&recipe.labels)
            .map_err(|_| SafeError::storage("无法序列化 labels"))?;
        let visibility = serde_json::to_string(&recipe.visibility)
            .map_err(|_| SafeError::storage("无法序列化 visibility"))?;
        transaction
            .execute(
                "INSERT INTO v2_recipes(
                    name, description, labels_json, capability, current_revision_id,
                    visibility, favorite, group_name, note, last_opened_at,
                    created_at, updated_at, field_labels_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12)",
                params![
                    recipe.name,
                    recipe.description,
                    labels_json,
                    recipe.capability,
                    revision.id.0,
                    visibility,
                    recipe.favorite,
                    recipe.group,
                    recipe.note,
                    recipe.last_opened_at.as_ref().map(|t| t.0.clone()),
                    recipe.created_at.0,
                    serde_json::to_string(&recipe.field_labels)
                        .map_err(|_| SafeError::storage("无法序列化字段备注"))?
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Recipe"))?;
        let recipe_id = RecipeId(transaction.last_insert_rowid() as u64);
        let input_schema = serde_json::to_string(&revision.input_schema)
            .map_err(|_| SafeError::storage("无法序列化 input schema"))?;
        let output_schema = serde_json::to_string(&revision.output_schema)
            .map_err(|_| SafeError::storage("无法序列化 output schema"))?;
        let binding = serde_json::to_string(&revision.binding)
            .map_err(|_| SafeError::storage("无法序列化 binding"))?;
        let defaults = serde_json::to_string(&revision.defaults)
            .map_err(|_| SafeError::storage("无法序列化 defaults"))?;
        let source = serde_json::to_string(&revision.source)
            .map_err(|_| SafeError::storage("无法序列化 source"))?;
        transaction
            .execute(
                "INSERT INTO v2_recipe_revisions(
                    recipe_id, revision, input_schema_json, output_schema_json,
                    binding_json, defaults_json, source, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    recipe_id.0,
                    revision.revision,
                    input_schema,
                    output_schema,
                    binding,
                    defaults,
                    source,
                    revision.created_at.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Recipe revision"))?;
        let revision_id = RecipeRevisionId(transaction.last_insert_rowid() as u64);
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        let mut recipe = recipe;
        recipe.id = recipe_id;
        recipe.current_revision_id = revision_id;
        let mut revision = revision;
        revision.id = revision_id;
        revision.recipe_id = recipe_id;
        Ok((recipe, revision))
    }

    /// 追加一份 revision 并把它设为当前版本（见 trait 上的说明：revision 不可变，
    /// 改了表单语义只能产生新的一份）。
    async fn append_revision(&self, revision: RecipeRevision) -> DomainResult<RecipeRevision> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let input_schema = serde_json::to_string(&revision.input_schema)
            .map_err(|_| SafeError::storage("无法序列化 input schema"))?;
        let output_schema = serde_json::to_string(&revision.output_schema)
            .map_err(|_| SafeError::storage("无法序列化 output schema"))?;
        let binding = serde_json::to_string(&revision.binding)
            .map_err(|_| SafeError::storage("无法序列化 binding"))?;
        let defaults = serde_json::to_string(&revision.defaults)
            .map_err(|_| SafeError::storage("无法序列化 defaults"))?;
        let source = serde_json::to_string(&revision.source)
            .map_err(|_| SafeError::storage("无法序列化 source"))?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开启 V2 事务"))?;
        transaction
            .execute(
                "INSERT INTO v2_recipe_revisions(
                    recipe_id, revision, input_schema_json, output_schema_json,
                    binding_json, defaults_json, source, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    revision.recipe_id.0,
                    revision.revision,
                    input_schema,
                    output_schema,
                    binding,
                    defaults,
                    source,
                    revision.created_at.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 Recipe revision"))?;
        let revision_id = RecipeRevisionId(transaction.last_insert_rowid() as u64);
        // 当前版本指向新的一份；旧的**留着**（历史任务还要指回去）。
        transaction
            .execute(
                "UPDATE v2_recipes SET current_revision_id = ?1, updated_at = ?2 WHERE id = ?3",
                params![revision_id.0, revision.created_at.0, revision.recipe_id.0],
            )
            .map_err(|_| SafeError::storage("无法更新当前 recipe revision"))?;
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事务"))?;
        let mut revision = revision;
        revision.id = revision_id;
        Ok(revision)
    }

    async fn update_recipe(&self, recipe: &Recipe) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let labels_json = serde_json::to_string(&recipe.labels)
            .map_err(|_| SafeError::storage("无法序列化 labels"))?;
        let visibility = serde_json::to_string(&recipe.visibility)
            .map_err(|_| SafeError::storage("无法序列化 visibility"))?;
        let changed = connection
            .execute(
                "UPDATE v2_recipes SET name = ?1, description = ?2, labels_json = ?3,
                        current_revision_id = ?4, visibility = ?5,
                        favorite = ?6, group_name = ?7, note = ?8, last_opened_at = ?9,
                        updated_at = ?10, field_labels_json = ?12 WHERE id = ?11",
                params![
                    recipe.name,
                    recipe.description,
                    labels_json,
                    recipe.current_revision_id.0,
                    visibility,
                    recipe.favorite,
                    recipe.group,
                    recipe.note,
                    recipe.last_opened_at.as_ref().map(|t| t.0.clone()),
                    recipe.updated_at.0,
                    recipe.id.0,
                    serde_json::to_string(&recipe.field_labels)
                        .map_err(|_| SafeError::storage("无法序列化字段备注"))?
                ],
            )
            .map_err(|_| SafeError::storage("无法更新 V2 Recipe"))?;
        if changed == 0 {
            return Err(SafeError::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"));
        }
        Ok(())
    }

    async fn archive_recipe(&self, id: RecipeId) -> DomainResult<bool> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        // **打墓碑，不销毁**。
        //
        // ⚠️ 此前这里是硬删除，且遇到"已被历史任务引用"就返回 `RECIPE_IN_USE`——
        // 于是**只要一个应用跑过一次，它就永远无法从库里移除**。
        // 根因是把两件不同的事塞进了一个动作：**清理**（不想再看到）与**销毁**（抹掉记录）。
        // 清理不需要销毁历史；而"用户自己策展的那一份应用"正是本产品相对平台扁平目录的
        // 价值所在，用户却没法策展它。
        //
        // 现在：`removed_at` 非空即"已移除"，行与 revision 都留着 ⇒
        // * 历史任务仍能解析出"是哪个应用产出的"（可追溯性保住了）；
        // * 资源库列表默认不显示（`list_recipes` 过滤）；
        // * 可恢复（清掉 `removed_at`）。
        //
        // 幂等：已经移除过的不重复打点（保留第一次的时间，那个时间才有意义）。
        let changed = connection
            .execute(
                "UPDATE v2_recipes SET removed_at = ?1
                 WHERE id = ?2 AND removed_at IS NULL",
                params![Timestamp::now().0, id.0],
            )
            .map_err(|_| SafeError::storage("无法移除 V2 Recipe"))?;
        if changed == 0 {
            // 区分"不存在"与"已经移除过"：前者是调用方的错，后者是幂等成功。
            let exists: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM v2_recipes WHERE id = ?1",
                    params![id.0],
                    |row| row.get(0),
                )
                .map_err(|_| SafeError::storage("无法读取 V2 Recipe"))?;
            return Ok(exists > 0);
        }
        Ok(true)
    }

    async fn restore_recipe(&self, id: RecipeId) -> DomainResult<bool> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        // ⚠️ `AND removed_at IS NOT NULL` 不是多余的：没有它，**恢复一个活着的应用也会
        // 返回 `true`**（SQLite 的 changes() 数的是被 UPDATE 命中的行，不是"值真的变了"的行），
        // 于是 HTTP 层的"这个应用不在已移除列表里"永远不会触发——界面会显示恢复成功，
        // 而垃圾桶里本来就没有它。
        //
        // 顺带对齐替身：`fakes::restore_recipe` 返回的是 `removed_at.is_some()`，
        // 也就是"它原本是否在墓碑里"。两边语义必须一致（这个仓库栽过两次）。
        let changed = connection
            .execute(
                "UPDATE v2_recipes SET removed_at = NULL
                 WHERE id = ?1 AND removed_at IS NOT NULL",
                params![id.0],
            )
            .map_err(|_| SafeError::storage("无法恢复 V2 Recipe"))?;
        Ok(changed > 0)
    }

    async fn list_removed_recipes(&self, limit: usize) -> DomainResult<Vec<Recipe>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id, name, description, labels_json, capability, current_revision_id,
                        visibility, favorite, group_name, note, last_opened_at,
                        created_at, updated_at, field_labels_json, removed_at
                 FROM v2_recipes
                 WHERE removed_at IS NOT NULL
                 ORDER BY removed_at DESC LIMIT ?1",
            )
            .map_err(|_| SafeError::storage("无法读取已移除的 Recipes"))?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, Option<String>>(14)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取已移除的 Recipes"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取已移除的 Recipes"))?;
        Ok(rows
            .into_iter()
            .map(|row| Recipe {
                id: RecipeId(row.0 as u64),
                name: row.1,
                description: row.2,
                labels: serde_json::from_str(&row.3).unwrap_or_default(),
                capability: row.4,
                current_revision_id: RecipeRevisionId(row.5 as u64),
                visibility: serde_json::from_str(&row.6).unwrap_or_default(),
                favorite: row.7 != 0,
                group: row.8,
                note: row.9,
                last_opened_at: row.10.map(Timestamp),
                created_at: Timestamp(row.11),
                updated_at: Timestamp(row.12),
                field_labels: serde_json::from_str(&row.13).unwrap_or_default(),
                removed_at: row.14.map(Timestamp),
            })
            .collect())
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_event(
    transaction: &Transaction,
    operation_id: OperationId,
    run_id: Option<RunId>,
    actor: &ActorRef,
    sequence: u64,
    event_type: &str,
    created_at: &Timestamp,
    payload: Value,
) -> DomainResult<()> {
    let actor_json =
        serde_json::to_string(actor).map_err(|_| SafeError::storage("无法序列化事件 Actor"))?;
    let payload_json = serde_json::to_string(&payload)
        .map_err(|_| SafeError::storage("无法序列化事件 payload"))?;
    transaction
        .execute(
            "INSERT INTO v2_events(
                event_type, operation_id, run_id, actor_json, sequence, payload_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event_type,
                operation_id.0,
                run_id.map(|id| id.0),
                actor_json,
                sequence,
                payload_json,
                created_at.0
            ],
        )
        .map_err(|_| SafeError::storage("无法写入 V2 事件"))?;
    Ok(())
}

#[async_trait]
impl EventStore for SqliteRepositories {
    async fn append(&self, events: Vec<EventRecord>) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection
            .unchecked_transaction()
            .map_err(|_| SafeError::storage("无法开始 V2 事务"))?;
        for event in events {
            let actor_json = serde_json::to_string(&event.actor)
                .map_err(|_| SafeError::storage("无法序列化事件 Actor"))?;
            let payload_json = serde_json::to_string(&event.safe_payload)
                .map_err(|_| SafeError::storage("无法序列化事件 payload"))?;
            transaction
                .execute(
                    "INSERT INTO v2_events(
                        event_type, operation_id, run_id, actor_json, sequence, payload_json, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        event.event_type,
                        event.operation_id.0,
                        event.run_id.map(|id| id.0),
                        actor_json,
                        event.sequence,
                        payload_json,
                        event.created_at.0
                    ],
                )
                .map_err(|_| SafeError::storage("无法写入 V2 事件"))?;
        }
        transaction
            .commit()
            .map_err(|_| SafeError::storage("无法提交 V2 事件事务"))?;
        Ok(())
    }

    async fn list_after(
        &self,
        operation_id: OperationId,
        after: u64,
        limit: usize,
    ) -> DomainResult<Vec<EventRecord>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id, event_type, operation_id, run_id, actor_json, sequence, payload_json, created_at
                 FROM v2_events WHERE operation_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
            )
            .map_err(|_| SafeError::storage("无法读取 V2 事件"))?;
        let rows = statement
            .query_map(params![operation_id.0, after, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 V2 事件"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 V2 事件"))?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    event_type,
                    operation_id,
                    run_id,
                    actor_json,
                    sequence,
                    payload_json,
                    created_at,
                )| {
                    EventRecord {
                        id: id as u64,
                        event_type,
                        operation_id: OperationId(operation_id as u64),
                        run_id: run_id.map(|id| RunId(id as u64)),
                        actor: serde_json::from_str(&actor_json).unwrap_or_else(|_| ActorRef {
                            id: String::new(),
                            kind: crate::v2::domain::actor::ActorKind::System,
                            client_id: crate::v2::domain::ids::ClientId(String::new()),
                            scopes: vec![],
                            label: None,
                        }),
                        sequence: sequence as u64,
                        safe_payload: serde_json::from_str(&payload_json).unwrap_or_default(),
                        created_at: Timestamp(created_at),
                    }
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, Scope};
    use crate::v2::domain::ids::{ClientId, ConnectionRevisionId, IdempotencyKey};
    use crate::v2::domain::model::{ConnectionOverride, LifecycleFeatures, RecipeBinding};
    use crate::v2::ports::repositories::{CallbackDeliveryRepository, LeaseStore, NewRun};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn seed_recipe(connection: &SqliteConnection) {
        let now = Timestamp::now().0;
        connection
            .execute(
                "INSERT INTO v2_recipes(
                    id, name, description, labels_json, capability, current_revision_id,
                    visibility, created_at, updated_at
                 ) VALUES (1, 'Image Recipe', 'text to image', '[]', 'text-to-image', 1,
                           'private', ?1, ?1)",
                params![now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO v2_recipe_revisions(
                    id, recipe_id, revision, input_schema_json, output_schema_json,
                    binding_json, defaults_json, source, created_at
                 ) VALUES (1, 1, 1, ?1, '{\"type\":\"object\"}', ?2, '{}', 'local', ?3)",
                params![
                    json!({
                        "type": "object",
                        "required": ["prompt"],
                        "properties": { "prompt": { "type": "string" } }
                    })
                    .to_string(),
                    serde_json::to_string(&RecipeBinding {
                        driver_key: "openai-images".to_string(),
                        provider_config: Default::default(),
                        default_connection_id: ConnectionId(1),
                        allowed_connection_ids: vec![ConnectionId(1)],
                        connection_override: ConnectionOverride::Denied,
                    })
                    .unwrap(),
                    now
                ],
            )
            .unwrap();
    }

    fn seed_connection(connection: &SqliteConnection) {
        let now = Timestamp::now().0;
        connection
            .execute(
                "INSERT INTO v2_connections(
                    id, vendor, protocol, display_name, current_revision_id, status
                 ) VALUES (1, 'openai', 'openai-images', 'OpenAI Images', 1, 'active')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO v2_connection_revisions(
                    id, connection_id, config_json, secret_bindings_json,
                    effective_capabilities_json, lifecycle_features_json, created_at
                 ) VALUES (1, 1, '{}', '[]', '[\"text-to-image\"]', ?1, ?2)",
                params![
                    serde_json::to_string(&LifecycleFeatures {
                        synchronous_result: true,
                        ..Default::default()
                    })
                    .unwrap(),
                    now
                ],
            )
            .unwrap();
    }

    fn actor() -> ActorRef {
        ActorRef {
            id: "actor-1".to_string(),
            kind: ActorKind::Mcp,
            client_id: ClientId("client-1".to_string()),
            scopes: vec![Scope::OperationsExecute],
            label: None,
        }
    }

    fn new_operation(actor: ActorRef) -> NewOperation {
        NewOperation {
            operation: Operation {
                id: OperationId(0),
                actor,
                origin: "mcp".to_string(),
                idempotency_key: IdempotencyKey("key-1".to_string()),
                request_hash: "v1:abcd".to_string(),
                status: OperationStatus::Queued,
                metadata: Default::default(),
                callback: None,
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
            },
            runs: vec![NewRun {
                recipe_revision_id: RecipeRevisionId(1),
                connection_revision_id: ConnectionRevisionId(1),
                client_run_id: "image-1".to_string(),
                inputs: BTreeMap::from([("prompt".to_string(), json!("a red house"))]),
                assets: vec![],
            }],
        }
    }

    #[tokio::test]
    async fn schema_initializes_in_memory() {
        let repositories = SqliteRepositories::in_memory().expect("内存数据库初始化应成功");
        assert!(repositories
            .connection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_autocommit());
    }

    /// 移除 / 恢复的**仓库层契约**（sqlite 侧）。
    ///
    /// ⚠️ 为什么必须在这里、用**真 sqlite** 测：这套语义以前只在替身
    /// （`InMemoryRepositories`）上跑过。替身与 sqlite 一旦不一致，
    /// 就会"测试绿而生产错"——实测 `restore_recipe` 正是如此：
    /// sqlite 版没有 `AND removed_at IS NOT NULL`，于是恢复一个**活着**的应用也返回 `true`
    ///（`changes()` 数的是 UPDATE 命中的行，不是"值真的变了"的行），
    /// 而替身返回的是"它原本在不在墓碑里"。两边语义必须一致。
    ///
    /// 判别点五条：列表过滤、墓碑列表、**幂等不覆盖首次时间**、恢复回列表、
    /// **恢复一个没移除过的必须是 false**（最后这条就是抓上面那个偏差的）。
    #[tokio::test]
    async fn archived_recipe_moves_to_the_removed_list_and_can_come_back() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
        }
        let recipe_id = RecipeId(1);
        assert_eq!(
            repositories
                .list_recipes(50, None)
                .await
                .expect("list")
                .len(),
            1,
            "前置：库里应当有一个应用"
        );

        // 移除 ⇒ 从资源库列表消失、进墓碑列表。
        assert!(
            repositories
                .archive_recipe(recipe_id)
                .await
                .expect("archive"),
            "移除应当成功（不管它有没有历史任务）"
        );
        assert!(
            repositories
                .list_recipes(50, None)
                .await
                .expect("list")
                .is_empty(),
            "移除后不该出现在资源库列表里"
        );
        let removed = repositories
            .list_removed_recipes(10)
            .await
            .expect("removed");
        assert_eq!(removed.len(), 1, "移除后应当出现在墓碑列表里");
        let first_removed_at = removed[0]
            .removed_at
            .clone()
            .expect("墓碑必须有移除时间（界面的「移除于 …」靠它）");

        // 幂等：再移除一次不报错，且**不覆盖**第一次的时间（那个时间才有意义）。
        assert!(
            repositories
                .archive_recipe(recipe_id)
                .await
                .expect("archive twice"),
            "重复移除应当幂等成功"
        );
        assert_eq!(
            repositories
                .list_removed_recipes(10)
                .await
                .expect("removed")[0]
                .removed_at
                .clone()
                .expect("removed_at"),
            first_removed_at,
            "重复移除不该覆盖第一次的移除时间"
        );

        // 恢复 ⇒ 回列表、出墓碑。
        assert!(
            repositories
                .restore_recipe(recipe_id)
                .await
                .expect("restore"),
            "恢复应当成功"
        );
        assert_eq!(
            repositories
                .list_recipes(50, None)
                .await
                .expect("list")
                .len(),
            1,
            "恢复后应当重新出现在资源库列表里"
        );
        assert!(
            repositories
                .list_removed_recipes(10)
                .await
                .expect("removed")
                .is_empty(),
            "恢复后不该还在墓碑列表里"
        );

        // **恢复一个没移除过的必须是 false**：这是替身与 sqlite 曾经分歧的那一点，
        // 也是 HTTP 层 404 `RECIPE_NOT_IN_TRASH` 的依据。
        assert!(
            !repositories
                .restore_recipe(recipe_id)
                .await
                .expect("restore again"),
            "恢复一个活着的应用必须返回 false（否则界面会对没发生的事显示「恢复成功」）"
        );
        assert!(
            !repositories
                .restore_recipe(RecipeId(999))
                .await
                .expect("restore missing"),
            "恢复一个不存在的应用也必须是 false"
        );
    }

    #[tokio::test]
    async fn create_operation_round_trip() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        let persisted = repositories
            .create_operation(new_operation(actor()))
            .await
            .expect("创建应成功");
        assert!(persisted.operation.id.0 > 0);
        assert_eq!(persisted.runs.len(), 1);
        assert!(persisted.runs[0].id.0 > 0);
        assert_eq!(persisted.runs[0].operation_id, persisted.operation.id);
        assert_eq!(persisted.runs[0].recipe_revision_id, RecipeRevisionId(1));
        assert_eq!(
            persisted.runs[0].connection_revision_id,
            ConnectionRevisionId(1)
        );
        assert_eq!(persisted.runs[0].status, RunStatus::Queued);

        let loaded = repositories
            .find_operation(persisted.operation.id)
            .await
            .expect("读取应成功")
            .expect("Operation 应存在");
        assert_eq!(loaded.operation.id, persisted.operation.id);
        assert_eq!(loaded.runs.len(), 1);
        assert_eq!(loaded.runs[0].client_run_id, "image-1");
        assert_eq!(loaded.runs[0].inputs["prompt"], json!("a red house"));
    }

    #[tokio::test]
    async fn create_operation_rejects_duplicate_idempotency() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        repositories
            .create_operation(new_operation(actor()))
            .await
            .expect("第一次创建应成功");
        let error = repositories
            .create_operation(new_operation(actor()))
            .await
            .expect_err("重复 idempotencyKey 应冲突");
        assert_eq!(error.code, "IDEMPOTENCY_CONFLICT");
    }

    #[tokio::test]
    async fn find_by_idempotency_replays_created_operation() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        let created = repositories
            .create_operation(new_operation(actor()))
            .await
            .unwrap();
        let found = repositories
            .find_by_idempotency("actor-1", "client-1", "key-1")
            .await
            .unwrap()
            .expect("幂等记录应存在");
        assert_eq!(found.operation.id, created.operation.id);
        assert_eq!(found.runs[0].id, created.runs[0].id);
    }

    #[tokio::test]
    async fn find_by_idempotency_returns_none_for_unknown_key() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        let found = repositories
            .find_by_idempotency("actor-1", "client-1", "missing")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn events_are_persisted_and_listable() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        let persisted = repositories
            .create_operation(new_operation(actor()))
            .await
            .unwrap();
        let events = repositories
            .list_after(persisted.operation.id, 0, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "run.created");
        assert_eq!(events[0].run_id, Some(persisted.runs[0].id));
        assert_eq!(events[1].event_type, "operation.created");
        assert_eq!(events[1].run_id, None);
    }

    #[tokio::test]
    async fn update_run_persists_status_and_tracking() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        let persisted = repositories
            .create_operation(new_operation(actor()))
            .await
            .unwrap();
        let mut run = persisted.runs[0].clone();
        run.status = RunStatus::Submitting;
        run.tracking_state = TrackingState::Active;
        repositories.update_run(&run).await.unwrap();
        let loaded = repositories
            .find_operation(persisted.operation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.runs[0].status, RunStatus::Submitting);
        assert_eq!(loaded.runs[0].tracking_state, TrackingState::Active);
    }

    /// 摘要列表里的「为什么没在动」：**按严重度取最阻塞的那条 run**，一条相关子查询、不 N+1。
    ///
    /// 为什么要有它：调用方（尤其外界 Agent）是**先 list 再 get** 的。只有 `get` 带 attention
    /// 等于省不了事 —— 它得为列表里每一条各打一次 `get` 才知道该看哪一个，而这次事故的代价
    /// 正是"没人主动去看"。
    ///
    /// 判别点四条：
    /// 1. QUEUED ＋ SUBMITTING ＋ SUBMISSION_UNCERTAIN 三条 run ⇒ 摘要报**等确认**那档，
    ///    并带上那条 run 的死线（不是"第一条"，也不是"随便挑一条"）；
    /// 2. **反向控制**：把等确认那条判终态 ⇒ 摘要必须改口成 SUBMITTING。少了这条，
    ///    一个"永远返回最严重那档"的实现也能过第 1 条；
    /// 3. 没有"在等什么"的 operation（run 正常在跑）⇒ 两个字段都是 `None`（不许编一个原因）；
    /// 4. 与 `operation_attention`（`get` 用的那个判据）**逐字相同** —— 摘要与详情不能各说各话。
    ///
    /// 负对照：让 `list_operations` 直接给出 `attention_reason: None`（编译得过的提前返回）
    /// ⇒ 第 1、2、4 条红。
    #[tokio::test]
    async fn operation_summaries_carry_the_most_blocking_attention() {
        use crate::v2::domain::operation::Transition;

        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        // 三条 run：排队 / 提交中 / 等确认（id 按插入顺序分配）。
        let mut new = new_operation(actor());
        new.runs = ["queued-1", "submitting-1", "uncertain-1"]
            .into_iter()
            .map(|client_run_id| NewRun {
                recipe_revision_id: RecipeRevisionId(1),
                connection_revision_id: ConnectionRevisionId(1),
                client_run_id: client_run_id.to_string(),
                inputs: BTreeMap::from([("prompt".to_string(), json!("a red house"))]),
                assets: vec![],
            })
            .collect();
        let persisted = repositories.create_operation(new).await.unwrap();
        let runs = persisted.runs.clone();
        assert_eq!(runs.len(), 3);

        for transition in [Transition::ToPreparing, Transition::ToSubmitting] {
            repositories
                .transition_run(runs[1].id, transition, Vec::new())
                .await
                .expect("推到提交中");
        }
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToSubmissionUncertain,
        ] {
            repositories
                .transition_run(runs[2].id, transition, Vec::new())
                .await
                .expect("推到等确认");
        }

        let summaries = repositories.list_operations(10, None).await.unwrap();
        let summary = summaries
            .iter()
            .find(|summary| summary.id == persisted.operation.id)
            .expect("刚建的操作应当在列表里");
        assert_eq!(
            summary.attention_reason.as_deref(),
            Some("SUBMISSION_UNCERTAIN"),
            "三条 run 里最该被看见的是「等确认」那条（不是排队那条、也不是第一条）"
        );
        let deadline = summary
            .attention_deadline
            .clone()
            .expect("等确认的 run 必须带死线——没有死线就等于「等多久都行」");
        assert!(
            deadline.millis() > Timestamp::now_millis(),
            "死线应当在未来：{deadline:?}"
        );

        // 4) 与 `get` 的 operation 级 attention 逐字相同。
        let loaded = repositories
            .find_operation(persisted.operation.id)
            .await
            .unwrap()
            .expect("操作应当存在");
        let expected = crate::v2::domain::operation::operation_attention(&loaded.runs)
            .expect("operation 级应当有 attention");
        assert_eq!(
            summary.attention_reason.as_deref(),
            Some(expected.reason.as_str()),
            "摘要与详情的 attention 必须是同一个判据"
        );
        assert_eq!(summary.attention_deadline, expected.deadline);

        // 2) 反向控制：最严重那条退了，摘要要改口。
        repositories
            .transition_run(runs[2].id, Transition::ToCancelled, Vec::new())
            .await
            .expect("取消等确认那条");
        let summaries = repositories.list_operations(10, None).await.unwrap();
        let summary = summaries
            .iter()
            .find(|summary| summary.id == persisted.operation.id)
            .expect("操作仍在列表里");
        assert_eq!(
            summary.attention_reason.as_deref(),
            Some("SUBMITTING"),
            "等确认那条已经终态，最该被看见的变成「提交中」"
        );

        // 3) 没有"在等什么"的 operation：两个字段都必须是 None。
        let mut plain = new_operation(actor());
        plain.operation.idempotency_key = IdempotencyKey("key-plain".to_string());
        let plain = repositories.create_operation(plain).await.unwrap();
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToAccepted,
        ] {
            repositories
                .transition_run(plain.runs[0].id, transition, Vec::new())
                .await
                .expect("推到已受理");
        }
        let summaries = repositories.list_operations(10, None).await.unwrap();
        let summary = summaries
            .iter()
            .find(|summary| summary.id == plain.operation.id)
            .expect("操作在列表里");
        assert_eq!(
            summary.attention_reason, None,
            "正常在跑的 run 没有「为什么没在动」——不许编一个"
        );
        assert_eq!(summary.attention_deadline, None);
    }

    #[tokio::test]
    async fn recipe_and_connection_revision_lookup() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        let recipe = repositories
            .find_recipe(RecipeId(1))
            .await
            .unwrap()
            .expect("Recipe 应存在");
        assert_eq!(recipe.capability, "text-to-image");
        let revision = <SqliteRepositories as RecipeRepository>::find_revision(
            &repositories,
            RecipeRevisionId(1),
        )
        .await
        .unwrap()
        .expect("Revision 应存在");
        assert_eq!(revision.binding.default_connection_id, ConnectionId(1));
        let connection = repositories
            .find_connection(ConnectionId(1))
            .await
            .unwrap()
            .expect("Connection 应存在");
        assert_eq!(connection.vendor, "openai");
        let connection_revision = <SqliteRepositories as ConnectionRepository>::find_revision(
            &repositories,
            ConnectionRevisionId(1),
        )
        .await
        .unwrap()
        .expect("Connection revision 应存在");
        assert_eq!(
            connection_revision.effective_capabilities,
            vec!["text-to-image"]
        );
        assert!(connection_revision.lifecycle_features.synchronous_result);
    }

    #[tokio::test]
    async fn claim_then_recover_after_restart_transitions_to_uncertain() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        repositories
            .create_operation(new_operation(actor()))
            .await
            .expect("创建应成功");
        // 模拟提交前崩溃：claim 成功，run 停在 SUBMITTING
        let claimed = repositories
            .claim_next_run(
                "scheduler-a",
                "fencing-token-1",
                ConnectionRevisionId(1),
                Timestamp::now(),
            )
            .await
            .expect("claim 应成功")
            .expect("应有可领取的 Run");
        assert_eq!(claimed.run.status, RunStatus::Submitting);
        // 模拟重启：将 attempt 的 started_at 改为很久以前
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            connection
                .execute("UPDATE v2_run_attempts SET started_at = '0'", [])
                .expect("更新 started_at 应成功");
        }
        let recovered = repositories
            .recover_expired_submissions()
            .await
            .expect("恢复应成功");
        assert_eq!(recovered, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::SubmissionUncertain);
        let attempt = repositories
            .find_latest_attempt(persisted.runs[0].id)
            .await
            .unwrap()
            .expect("Attempt 应存在");
        assert_eq!(
            attempt.status,
            crate::v2::domain::operation::AttemptStatus::Uncertain
        );
    }

    #[tokio::test]
    async fn lease_is_exclusive_until_expiry() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        repositories
            .acquire("scheduler-a", "token-a", 30)
            .await
            .expect("首次获取应成功");
        let error = repositories
            .acquire("scheduler-b", "token-b", 30)
            .await
            .expect_err("其他所有者应被拒绝");
        assert_eq!(error.code, "LEASE_BUSY");
        // 同一所有者可以重入（token 换新）
        repositories
            .acquire("scheduler-a", "token-a2", 30)
            .await
            .expect("同所有者重入应成功");
        // 过期后可被其他所有者获取
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            connection
                .execute("UPDATE v2_leases SET expires_at = '0'", [])
                .expect("更新租约过期应成功");
        }
        repositories
            .acquire("scheduler-b", "token-b", 30)
            .await
            .expect("过期后其他所有者应可获取");
        // token 不匹配的 release 不生效
        repositories
            .release("scheduler-b", "wrong-token")
            .await
            .expect("release 不应报错");
        let lease = repositories.current().await.expect("读取租约应成功");
        assert_eq!(lease.unwrap().owner, "scheduler-b");
        repositories
            .release("scheduler-b", "token-b")
            .await
            .expect("正确 token release 应成功");
        assert!(repositories.current().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn claim_complete_transition_full_cycle() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        repositories
            .create_operation(new_operation(actor()))
            .await
            .expect("创建应成功");
        let claimed = repositories
            .claim_next_run(
                "scheduler-a",
                "fencing-token-1",
                ConnectionRevisionId(1),
                Timestamp::now(),
            )
            .await
            .expect("claim 应成功")
            .expect("应有可领取的 Run");
        assert_eq!(claimed.run.status, RunStatus::Submitting);
        let _completed = repositories
            .complete_attempt(
                claimed.run.id,
                "fencing-token-1",
                crate::v2::ports::repositories::AttemptResolution::Succeeded {
                    remote_handle: Some(crate::v2::domain::execution::RemoteHandle {
                        provider: "comfyui".to_string(),
                        remote_id: "prompt-1".to_string(),
                    }),
                },
            )
            .await
            .expect("complete 应成功");
        let run = repositories
            .transition_run(
                claimed.run.id,
                crate::v2::domain::operation::Transition::ToAccepted,
                vec![crate::v2::ports::repositories::EventSpec {
                    event_type: "run.accepted".to_string(),
                    run_id: Some(claimed.run.id),
                    safe_payload: serde_json::json!({}),
                }],
            )
            .await
            .expect("transition 应成功");
        assert_eq!(run.status, RunStatus::Accepted);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Accepted);
    }

    /// 不确定的 attempt **必须把「为什么不明」落库**（真库，不是替身）。
    ///
    /// 为什么这条要有：提交失败进不确定态时，这是**唯一**会留下来的证据——事件载荷只有一句
    /// message（不含原因链），daemon 日志那一行要等到死线才写。少了它，「连接被拒」
    /// （`details.nothingSent = true`，字节一定没出去）与「发出去后超时」（`false`，
    /// 可能已经收下）在库里长得一模一样，而这两类能不能重投是**相反**的回答。
    ///
    /// 负对照：把 `complete_attempt` 的 `Uncertain` 分支改回 `None` ⇒ 这条红。
    #[tokio::test]
    async fn an_uncertain_attempt_keeps_the_reason_it_is_uncertain() {
        use crate::v2::ports::repositories::AttemptResolution;

        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        repositories
            .create_operation(new_operation(actor()))
            .await
            .expect("创建应成功");
        let claimed = repositories
            .claim_next_run(
                "scheduler-a",
                "fencing-token-uncertain",
                ConnectionRevisionId(1),
                Timestamp::now(),
            )
            .await
            .expect("claim 应成功")
            .expect("应有可领取的 Run");

        let evidence = SafeError {
            code: "PROVIDER_UNREACHABLE_SUBMIT".to_string(),
            kind: crate::v2::domain::errors::ErrorKind::Network,
            message: "连不上 submit".to_string(),
            retryable: true,
            details: std::collections::BTreeMap::from([("nothingSent".to_string(), json!(true))]),
        };
        repositories
            .complete_attempt(
                claimed.run.id,
                "fencing-token-uncertain",
                AttemptResolution::Uncertain {
                    remote_handle: None,
                    error: Some(evidence.clone()),
                },
            )
            .await
            .expect("complete 应成功");

        let attempt = repositories
            .find_latest_attempt(claimed.run.id)
            .await
            .expect("查 attempt")
            .expect("应当有 attempt");
        assert_eq!(
            attempt.status,
            crate::v2::domain::operation::AttemptStatus::Uncertain
        );
        assert!(attempt.remote_handle.is_none(), "不确定态没有远端句柄");
        let stored = attempt.error.expect("原因必须落库");
        assert_eq!(stored.code, "PROVIDER_UNREACHABLE_SUBMIT");
        assert_eq!(
            stored.details.get("nothingSent"),
            Some(&json!(true)),
            "「字节没出去」这条证据必须一起存下来：{:?}",
            stored.details
        );
    }

    /// 真 HTTP 投递：打到**真实的本地接收端**（不是假投递器），并**按契约验签**。
    ///
    /// 为什么这条必须有：`FakeCallbackDeliverer` 只证明"服务被调用了"，证明不了
    /// "请求真的发出去了、形状是对的、接收端确实收得到"，更证明不了**签名能不能验**。
    /// 这里让"接收端"拿到**实际收到的原始字节**，用它重算一遍 HMAC —— 于是
    /// "签名时序列化的那份 body"与"真正发出去的字节"必须逐字节一致，否则这条就红。
    /// 这正是最容易悄悄坏掉的地方（换个序列化器、中间再加工一次 body 都会中招）。
    ///
    /// 关于代理：一度以为"回环投递必须显式 `no_proxy`，否则会被环境里那个死代理拦掉"，
    /// 于是给投递器加了第二个客户端。**负对照把它否了**——在
    /// `HTTP_PROXY=http://127.0.0.1:1`（没人听）的环境里，强制走"公网客户端"投递到
    /// `http://127.0.0.1:<port>` **照样成功**：reqwest 默认就不把回环交给代理。
    /// 所以那半个客户端被撤掉了（不留没有证据的代码），这条测试留在这里当证据。
    #[tokio::test]
    async fn http_delivery_reaches_a_loopback_receiver_and_the_signature_verifies() {
        use crate::v2::domain::signing;
        use crate::v2::ports::callback::{CallbackDeliverer, DeliveryOutcome, DeliveryRequest};
        use std::io::{Read, Write};

        const KEY: &str = "a-shared-secret-only-the-human-knows";

        /// 起一个只收一次的本地接收端，返回它的端口。
        fn receiver() -> (u16, Arc<Mutex<String>>) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
            let port = listener.local_addr().expect("取端口").port();
            let received = Arc::new(Mutex::new(String::new()));
            let sink = received.clone();
            std::thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                // 读到读不动为止：body 与头可能不在同一个 TCP 段里。
                while let Ok(read) = stream.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") && read < buffer.len()
                    {
                        break;
                    }
                }
                *sink.lock().unwrap_or_else(|e| e.into_inner()) =
                    String::from_utf8_lossy(&request).to_string();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            });
            (port, received)
        }

        let payload = json!({ "operationId": 1, "status": "SUCCEEDED" });
        let body = serde_json::to_string(&payload).expect("序列化");
        let deliverer = super::exec::HttpCallbackDeliverer::new().expect("创建投递器");

        // ① 没配密钥：不带签名头
        let (port, received) = receiver();
        let outcome = deliverer
            .deliver(DeliveryRequest {
                delivery_id: "dlv-plain".to_string(),
                url: format!("http://127.0.0.1:{port}/hook"),
                payload: payload.clone(),
                signature: None,
            })
            .await
            .expect("投递不该返回领域错误");
        assert_eq!(
            outcome,
            DeliveryOutcome::Accepted,
            "本地接收端返回 200，就该算送达"
        );
        let plain = received.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            plain.starts_with("POST /hook"),
            "应当是 POST 到 /hook，实际：{plain}"
        );
        assert!(
            !plain
                .to_ascii_lowercase()
                .contains("x-renderloft-signature"),
            "没配密钥就不该带签名头，实际：{plain}"
        );

        // ② 配了密钥：接收端按契约验签
        let (port, received) = receiver();
        let signature = signing::sign(1_791_609_031_058, &body, KEY.as_bytes());
        let outcome = deliverer
            .deliver(DeliveryRequest {
                delivery_id: "dlv-signed".to_string(),
                url: format!("http://127.0.0.1:{port}/hook"),
                payload: payload.clone(),
                signature: Some(signature.clone()),
            })
            .await
            .expect("投递不该返回领域错误");
        assert_eq!(outcome, DeliveryOutcome::Accepted);

        let signed = received.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let header = signed
            .lines()
            .find(|line| {
                line.to_ascii_lowercase()
                    .starts_with("x-renderloft-signature:")
            })
            .map(|line| line.split_once(':').expect("冒号").1.trim().to_string())
            .expect("配了密钥就必须带签名头");
        assert_eq!(header, signature, "头里的值应当就是算好的那个");

        // 接收端视角：拿**实际收到的原始 body** 重算
        let received_body = signed
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("请求应当有头体分隔");
        let (timestamp, value) = header
            .strip_prefix("t=")
            .and_then(|rest| rest.split_once(",v1="))
            .expect("签名头形状应为 t=…,v1=…");
        assert_eq!(
            signing::hex(&signing::hmac_sha256(
                KEY.as_bytes(),
                format!("{timestamp}.{received_body}").as_bytes()
            )),
            value,
            "按契约重算应当一致——签名用的 body 必须与真正发出去的字节逐字节相同"
        );
    }

    /// 一个"没有密钥"的 SecretStore：签名那条路另有专门用例，这里只关心投递。
    struct NoSecrets;

    #[async_trait]
    impl crate::v2::ports::stores::SecretStore for NoSecrets {
        async fn resolve(
            &self,
            _binding: &crate::v2::domain::model::SecretBinding,
        ) -> DomainResult<Option<crate::v2::ports::stores::SecretValue>> {
            Ok(None)
        }
        async fn set(&self, _slot: &str, _value: &str) -> DomainResult<()> {
            Ok(())
        }
        async fn delete(&self, _slot: &str) -> DomainResult<()> {
            Ok(())
        }
    }

    /// **真路径回归测试**：终态 + 订阅 ⇒ 一轮 `tick` 就该把它投递出去。
    ///
    /// ⚠️ 这条钉的是 2026-10-10 真跑抓到的 bug：投递的触发点**一个都没有**——
    /// `enqueue_terminal` 只在测试里被显式调用过，于是订阅存了、任务跑完了、回调永远不发，
    /// 而所有单测都是绿的（它们等于在测"我调用它它会工作"）。
    ///
    /// 所以这条**不显式调 enqueue**：只调 `tick`（daemon 每 2 秒调的就是它），用**真的
    /// sqlite 仓库**（新加的那条 SQL 也在内），终态由真的 `force_fail_operation` 制造
    /// （卫生清扫走的就是它）。
    ///
    /// 负对照：把 `CallbackService::tick` 里 `enqueue_missing` 那一行删掉 ⇒ 这条红。
    #[tokio::test]
    async fn a_terminal_operation_with_a_subscription_is_delivered_by_one_tick() {
        use crate::v2::application::callbacks::CallbackService;
        use crate::v2::domain::callback::CallbackSubscription;
        use crate::v2::infrastructure::fakes::FakeCallbackDeliverer;
        use crate::v2::ports::repositories::OperationRepository;

        let repositories = Arc::new(SqliteRepositories::in_memory().unwrap());
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        let mut operation = new_operation(actor());
        operation.operation.callback = Some(CallbackSubscription::new(
            "http://127.0.0.1:1/hook",
            vec!["operation.terminal".to_string()],
        ));
        let persisted = repositories
            .create_operation(operation)
            .await
            .expect("创建应成功");
        let operation_id = persisted.operation.id;

        let deliverer = Arc::new(FakeCallbackDeliverer::accepting());
        let service = CallbackService {
            repositories: repositories.clone(),
            deliveries: repositories.clone(),
            deliverer: deliverer.clone(),
            secrets: Arc::new(NoSecrets),
        };

        // 前提：还没进终态时，跑多少轮都不该投递。
        service.tick(10).await.expect("tick 应成功");
        assert_eq!(deliverer.delivered_count(), 0, "还没终态就不该投递");

        // 真的把它推到终态（卫生清扫用的就是这条）。
        repositories
            .force_fail_operation(operation_id, "测试：推到终态")
            .await
            .expect("推终态应成功");
        service.tick(10).await.expect("tick 应成功");
        assert_eq!(
            deliverer.delivered_count(),
            1,
            "终态之后一轮 tick 就该投递出去（这正是此前缺的那一环）"
        );

        // 幂等：再跑几轮不会重复投递。
        service.tick(10).await.expect("tick 应成功");
        service.tick(10).await.expect("tick 应成功");
        assert_eq!(deliverer.delivered_count(), 1, "补扫必须幂等");
    }

    #[tokio::test]
    async fn callback_delivery_persists_and_mark_attempts() {
        let repositories = SqliteRepositories::in_memory().unwrap();
        {
            let connection = repositories
                .connection
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            seed_recipe(&connection);
            seed_connection(&connection);
        }
        repositories
            .create_operation(new_operation(actor()))
            .await
            .expect("创建应成功");
        let delivery = crate::v2::ports::repositories::CallbackDeliveryRecord {
            delivery_id: "dlv-1".to_string(),
            operation_id: OperationId(1),
            event_id: 7,
            url: "https://agent.example/cb".to_string(),
            payload: json!({ "operationId": 1 }),
            attempts: 0,
            max_attempts: 3,
            next_retry_at: Timestamp::now(),
            completed: false,
            last_error: None,
        };
        repositories
            .create_delivery(delivery)
            .await
            .expect("创建投递应成功");
        let due = repositories
            .list_due_deliveries(10)
            .await
            .expect("读取应成功");
        assert_eq!(due.len(), 1);
        repositories
            .mark_delivery_attempt("dlv-1", false, Some("连接失败".to_string()))
            .await
            .expect("标记失败应成功");
        let due = repositories
            .list_due_deliveries(10)
            .await
            .expect("读取应成功");
        assert!(due.is_empty(), "重试时间未到不应可投递");
    }

    /// 老库迁移：`v2_messages` 早于"用户附图"功能创建时没有 `image_asset_ids_json`，
    /// 启动时的 `initialize_schema` 必须**加法式**补列，且不破坏既有消息。
    /// 这条测试存在的理由：真实工作区里有真实数据，迁移出错就是数据事故。
    #[test]
    fn legacy_messages_table_gains_image_asset_ids_column_without_data_loss() {
        // 先造一个**旧结构**的库，并塞一条旧消息。
        let connection = SqliteConnection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE v2_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL DEFAULT '',
                    tool_calls_json TEXT NOT NULL DEFAULT '[]',
                    tool_results_json TEXT NOT NULL DEFAULT '[]',
                    operation_ids_json TEXT NOT NULL DEFAULT '[]',
                    created_at TEXT NOT NULL
                );
                INSERT INTO v2_messages
                    (conversation_id, role, content, created_at)
                    VALUES (1, '\"user\"', 'legacy hello', '1');",
            )
            .unwrap();

        initialize_schema(&connection).expect("迁移应成功");

        let mut statement = connection
            .prepare("PRAGMA table_info(v2_messages)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            columns.iter().any(|name| name == "image_asset_ids_json"),
            "老库应被补上 image_asset_ids_json，实际列：{columns:?}"
        );

        // 旧数据必须还在，且新列取默认值（而不是 NULL 或整表重建）。
        let (content, ids): (String, String) = connection
            .query_row(
                "SELECT content, image_asset_ids_json FROM v2_messages WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, "legacy hello");
        assert_eq!(ids, "[]");

        // 幂等：重复启动不应报错（ensure_column 先查 PRAGMA 再决定是否 ADD COLUMN）。
        initialize_schema(&connection).expect("重复迁移应幂等");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM v2_messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "迁移不该动到行数");
    }
}
