use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::event::EventRecord;
use crate::v2::domain::execution::RemoteHandle;
use crate::v2::domain::ids::{
    AssetId, ConnectionId, ConnectionRevisionId, OperationId, RecipeId, RecipeRevisionId, RunId,
};
use crate::v2::domain::media::{AssetRef, MediaObject};
use crate::v2::domain::model::{
    Connection, ConnectionRevision, Recipe, RecipeBinding, RecipeRevision, Timestamp,
};
use crate::v2::domain::operation::{Operation, Run, RunAttempt, Transition};

#[derive(Debug, Clone)]
pub struct NewRun {
    pub recipe_revision_id: RecipeRevisionId,
    pub connection_revision_id: ConnectionRevisionId,
    pub client_run_id: String,
    pub inputs: BTreeMap<String, Value>,
    pub assets: Vec<AssetRef>,
}

#[derive(Debug, Clone)]
pub struct NewOperation {
    pub operation: Operation,
    pub runs: Vec<NewRun>,
}

#[derive(Debug, Clone)]
pub struct PersistedOperation {
    pub operation: Operation,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone)]
pub struct ClaimedRun {
    pub run: Run,
    /// 应用名——用途与 `ActiveRun::recipe_name` 相同：**在产物落库那一刻用它起名**。
    /// 两条落库路径（scheduler 认领、reconcile 对账）各从一个装载点拿，所以两处都要有。
    pub recipe_name: String,
    pub recipe_revision: RecipeRevision,
    pub connection_revision: ConnectionRevision,
    pub attempt: RunAttempt,
}

#[derive(Debug, Clone)]
pub enum AttemptResolution {
    Succeeded {
        remote_handle: Option<RemoteHandle>,
    },
    Failed {
        error: SafeError,
        retryable: bool,
    },
    /// 提交结果不明。`error` = **我们手上关于"为什么不明"的全部证据**。
    ///
    /// 为什么把它带进来（实测缺口，2026-10-17）：此前这里只有 `remote_handle`，于是
    /// 「连接被拒」（字节一定没出去）与「发出去后超时」（字节可能出去了）在库里长得**一模一样**
    /// —— 两个字段都是 NULL，事件里只有一句不含原因链的话，日志那一行要等到死线才写。
    /// 而"能不能证明没出去"正是**能不能安全重投**的前提（见
    /// `infrastructure::providers::safety::nothing_sent_evidence`）。
    Uncertain {
        remote_handle: Option<RemoteHandle>,
        error: Option<SafeError>,
    },
}

#[derive(Debug, Clone)]
pub struct EventSpec {
    pub event_type: String,
    pub run_id: Option<RunId>,
    pub safe_payload: Value,
}

#[derive(Debug, Clone)]
pub struct OperationSummary {
    pub id: OperationId,
    pub status: crate::v2::domain::operation::OperationStatus,
    pub created_at: Timestamp,
    pub run_count: u64,
    /// 这个 operation 里**最阻塞**的那条 run 的「为什么没在动」原因码
    /// （严重度与取值见 `domain::operation::ATTENTION_LADDER`）；没有就是 `None`。
    ///
    /// 为什么摘要里也要有：调用方（尤其外界 Agent）是**先 list 再 get** 的。只有 `get` 带
    /// `attention` 等于省不了事 —— 它得为列表里每一条各打一次 `get` 才知道该看哪一个，
    /// 而这次事故的代价正是"没人主动去看"。
    ///
    /// 为什么只给原因码与死线，不给那句人话与 nextActions：摘要要精简（一页 50 条不该带上
    /// 50 份散文）。要读"到点会怎样、现在能做什么"，按 id 调 `get`。
    pub attention_reason: Option<String>,
    /// 上一条 run 的**死线**（到点会判失败）；与 `attention_reason` 同时有或同时无。
    pub attention_deadline: Option<Timestamp>,
}

#[derive(Debug, Clone)]
pub struct ActiveRun {
    pub run: Run,
    pub vendor: String,
    pub protocol: String,
    /// 应用名——**在产物落库那一刻用它起名**（见 `ArtifactDraft::compose_filename`）。
    ///
    /// 为什么挂在这里，而不是让 scheduler / reconcile 各自去查：装配 `ActiveRun` 时本来
    /// 就逐个解析了 recipe revision 与 connection revision，**顺手取名字几乎免费**；
    /// 而事后从 run 反查没有便宜的路（`RecipeRevision` 只有 `recipe_id`，仓库层没有
    /// `find_run`，`/api/v1/runs/{id}` 那条还得线性扫 operations）。
    pub recipe_name: String,
    pub recipe_revision: RecipeRevision,
    pub connection_revision: ConnectionRevision,
    pub attempt: RunAttempt,
}

#[async_trait]
pub trait RecipeRepository: Send + Sync {
    async fn find_recipe(&self, id: RecipeId) -> DomainResult<Option<Recipe>>;
    async fn find_revision(&self, id: RecipeRevisionId) -> DomainResult<Option<RecipeRevision>>;
    async fn list_recipes(
        &self,
        limit: usize,
        after: Option<RecipeId>,
    ) -> DomainResult<Vec<(Recipe, RecipeBinding)>>;
    async fn create_recipe(
        &self,
        recipe: Recipe,
        revision: RecipeRevision,
    ) -> DomainResult<(Recipe, RecipeRevision)>;
    async fn update_recipe(&self, recipe: &Recipe) -> DomainResult<()>;
    /// 给一个**已有的**配方追加一份新 revision，并把它设为当前版本。
    ///
    /// 为什么需要它：revision 是**不可变**的（历史任务按 id 指回去，必须永远重现得出来），
    /// 所以"改了表单语义"只能产生**新的一份**，不能就地改旧的。
    ///
    /// 为什么不是"就地改 current revision"：那会**改写历史** —— 昨天跑过的那个任务，
    /// 今天再去看它的输入，会发现字段定义变了。不可变是这里唯一的防线。
    ///
    /// 调用方负责把 `revision.revision` 填成下一个序号、`recipe_id` 填对；
    /// 实现负责回填自增 id，并把 `v2_recipes.current_revision_id` 指过去。
    async fn append_revision(&self, revision: RecipeRevision) -> DomainResult<RecipeRevision>;
    /// 从资源库**移除**（打墓碑，不销毁）。
    ///
    /// 幂等：已移除过的再调一次返回 `Ok(true)`，且**不覆盖**第一次的时间
    /// （那个时间才有意义）。不存在则返回 `Ok(false)`。
    ///
    /// ⚠️ 与旧行为的区别：此前这里是硬删除，且遇到"已被历史任务引用"就返回
    /// `RECIPE_IN_USE`——于是只要一个应用跑过一次就永远无法移除。
    /// 现在历史照旧可解析（行与 revision 都留着），列表默认不显示，且可恢复。
    async fn archive_recipe(&self, id: RecipeId) -> DomainResult<bool>;
    /// 从墓碑恢复（清掉 `removed_at`）。返回是否真的改动了。
    async fn restore_recipe(&self, id: RecipeId) -> DomainResult<bool>;
    /// 列出已移除的（"最近删除"视图用）。按移除时间倒序。
    async fn list_removed_recipes(&self, limit: usize) -> DomainResult<Vec<Recipe>>;
}

#[async_trait]
pub trait ConnectionRepository: Send + Sync {
    async fn find_connection(&self, id: ConnectionId) -> DomainResult<Option<Connection>>;
    async fn find_revision(
        &self,
        id: ConnectionRevisionId,
    ) -> DomainResult<Option<ConnectionRevision>>;
    async fn list_revisions(&self) -> DomainResult<Vec<(String, String, ConnectionRevision)>>;
    async fn list_connections(&self) -> DomainResult<Vec<Connection>>;
    async fn create_connection(
        &self,
        connection: Connection,
        revision: ConnectionRevision,
    ) -> DomainResult<(Connection, ConnectionRevision)>;
    async fn update_connection(&self, connection: &Connection) -> DomainResult<()>;
    async fn create_revision(
        &self,
        connection_id: ConnectionId,
        revision: ConnectionRevision,
    ) -> DomainResult<ConnectionRevision>;
    async fn delete_connection(&self, id: ConnectionId) -> DomainResult<bool>;
}

#[derive(Debug, Clone)]
pub struct ActorTokenRecord {
    pub id: i64,
    pub token_hash: String,
    pub actor_id: String,
    pub kind: crate::v2::domain::actor::ActorKind,
    pub client_id: String,
    pub scopes: Vec<crate::v2::domain::actor::Scope>,
    pub label: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub revoked: bool,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct ArtifactSummary {
    pub id: crate::v2::domain::ids::ArtifactId,
    pub run_id: RunId,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub created_at: Timestamp,
}

#[async_trait]
pub trait ArtifactRepository: Send + Sync {
    async fn list_by_run(&self, run_id: RunId) -> DomainResult<Vec<ArtifactSummary>>;
    async fn list(
        &self,
        limit: usize,
        cursor: ArtifactCursor,
    ) -> DomainResult<Vec<ArtifactSummary>>;
}

/// 产物列表**往哪个方向翻**。
///
/// 为什么要有两个方向（2026-10-18 加）：两个消费者要的是**相反**的顺序，而它们都对 ——
/// * 脚本 / MCP 那侧按 id 递增翻（"从最早开始，一页一页往后"），既有语义，`After` 保持不动；
/// * 界面上的「结果」视角要**最新在前**（"我刚生成的那张在哪"），而 `After` 的第一页给的是
///   **最旧**的 N 条 —— 那个视角根本拿不到最近的产物。让它把整个列表翻完再倒序，是拿流量换顺序，
///   产物一多就废。
///
/// 所以方向是**查询的一部分**，不是调用方的后处理：`Before` 一次查询就给出"比这个 id 更旧的
/// 那些，按 id 降序"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCursor {
    /// 只要 id **大于**它的（`None` = 从头），按 id **升序**返回。
    After(Option<crate::v2::domain::ids::ArtifactId>),
    /// 只要 id **小于**它的（`None` = 从最新开始），按 id **降序**返回。
    Before(Option<crate::v2::domain::ids::ArtifactId>),
}

#[derive(Debug, Clone, Default)]
pub struct SettingsRecord {
    pub download_dir: String,
}

#[async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn load_settings(&self) -> DomainResult<SettingsRecord>;
    async fn save_settings(&self, record: &SettingsRecord) -> DomainResult<()>;
}

/// 工作区库文件名。
///
/// 它是**工作区契约**的一部分（备份/迁移要按这个名字放库），所以住在端口层：`daemon` 与
/// `application` 都从这里取，谁都不必去依赖组合根。
pub const WORKSPACE_DB_FILE: &str = "v2-workspace.db";

/// 工作区**整体**的三件事：库在哪、产物缓存在哪、把库一致地导出一份。
///
/// 为什么值得单独一个端口：迁移/备份要的不是"某张表的数据"，而是整个工作区；而"一致的快照"
/// 只有**握着数据库连接**的人做得到（`VACUUM INTO`，见 sqlite 实现）——直接拷文件不行。
#[async_trait]
pub trait WorkspaceRepository: Send + Sync {
    /// 库文件路径（工作区目录就是它的父目录）。内存库返回 `None`。
    fn database_path(&self) -> Option<std::path::PathBuf>;
    /// 产物本地缓存目录。内存库返回 `None`。
    fn artifact_dir(&self) -> Option<std::path::PathBuf>;
    /// 把库一致地导出到 `target`（对活库用 `VACUUM INTO`，不是拷文件）。
    async fn snapshot_database(&self, target: &std::path::Path) -> DomainResult<()>;
}

#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn create(&self, record: &ActorTokenRecord) -> DomainResult<i64>;
    async fn find_by_hash(&self, token_hash: &str) -> DomainResult<Option<ActorTokenRecord>>;
    async fn find_by_id(&self, id: i64) -> DomainResult<Option<ActorTokenRecord>>;
    async fn list(&self) -> DomainResult<Vec<ActorTokenRecord>>;
    async fn revoke(&self, id: i64) -> DomainResult<()>;
    async fn set_revoked(&self, id: i64, revoked: bool) -> DomainResult<()>;
}

#[async_trait]
pub trait OperationRepository: Send + Sync {
    async fn find_by_idempotency(
        &self,
        actor_id: &str,
        client_id: &str,
        idempotency_key: &str,
    ) -> DomainResult<Option<PersistedOperation>>;

    async fn create_operation(&self, request: NewOperation) -> DomainResult<PersistedOperation>;

    async fn find_operation(&self, id: OperationId) -> DomainResult<Option<PersistedOperation>>;

    async fn update_run(&self, run: &Run) -> DomainResult<()>;

    async fn append_attempt(&self, attempt: &RunAttempt) -> DomainResult<()>;

    /// 取消竞态防护：把 run 的最新非终态 attempt 标记为 CANCELLED，
    /// 使在途 scheduler 的 complete_attempt 被终态检查短路，不再把
    /// 已取消的任务标为 SUCCEEDED（付费任务已提交则仅本地标记）。
    async fn fence_attempt(&self, run_id: RunId) -> DomainResult<()>;

    async fn claim_next_run(
        &self,
        owner: &str,
        fencing_token: &str,
        connection_revision_id: ConnectionRevisionId,
        now: Timestamp,
    ) -> DomainResult<Option<ClaimedRun>>;

    async fn complete_attempt(
        &self,
        run_id: RunId,
        fencing_token: &str,
        resolution: AttemptResolution,
    ) -> DomainResult<Option<Run>>;

    async fn transition_run(
        &self,
        run_id: RunId,
        transition: Transition,
        events: Vec<EventSpec>,
    ) -> DomainResult<Run>;

    /// 持久化 run 的终态错误（结构化 SafeError），供详情接口直接返回。
    async fn set_run_error(&self, run_id: RunId, error: Option<SafeError>) -> DomainResult<()>;

    /// 持久化**平台报的用量**（`TaskSnapshot.usage` 原样转述，形如 `{unit, amount}`）。
    ///
    /// 可空是刻意的：`None` = "平台没说"，`0` = "这次免费"（本地自建 ComfyUI）——
    /// 两者混了就是撒谎。**产品不推算成本**：只存平台自己报过的那句话。
    async fn set_run_usage(
        &self,
        run_id: RunId,
        usage: Option<serde_json::Value>,
    ) -> DomainResult<()>;

    async fn list_active_runs(&self, limit: usize) -> DomainResult<Vec<ActiveRun>>;

    /// 数这个 actor 名下**尚未到终态**的 Operation 数量。
    ///
    /// 给"自主调用方"的并发闸门用（见 `create_operation`）：MCP / CLI 这类调用方
    /// **没有交付**可挂，所以只能按 actor 计数。刻意数 Operation 而不是 Run——
    /// 界面的批量运行是**一个 Operation 带 N 个 Run**，数 Run 会把正常批量当成刷。
    async fn count_active_operations(&self, actor_id: &str) -> DomainResult<usize>;

    /// 数这个连接版本上**正在占用 provider 执行位**的 Run 数量。
    ///
    /// 给调度器用：`ProviderDriver::available_slots()` 是"这个 provider 允许几个并发"，
    /// 而它是**静态值**——调度器此前只把它当"每个 tick 最多领几个"，
    /// 于是 tick 2 会在 tick 1 的任务还在跑时继续提交，被平台拒。
    /// 判据与 `list_active_runs` 一致（`ACCEPTED`/`RUNNING`/`SUBMISSION_UNCERTAIN` 且 tracking 活跃）。
    async fn count_active_runs_for_connection(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
    ) -> DomainResult<usize>;

    /// 把**从未尝试过**的 `QUEUED` run 改派到 `connection_revision_id`，最多 `max` 条。
    ///
    /// 为什么允许改派：这些 run 还没提交过——钱没花、平台没见过它。挪动它不改变任何
    /// 已经发生的事实，只是把排队的人叫到另一个空窗口。
    ///
    /// 三条边界，缺一条就会出错：
    /// * **只认 `QUEUED`**：`RETRY_WAIT` 有 attempt，改派等于让重试换账号（可能双扣）；
    /// * **必须有 attempt 记录就不碰**：状态可能被别处改回 QUEUED，attempt 表才是硬证据；
    /// * **只认同一张配方**：池成员是配方声明的，不能把别的配方的 run 挪过来。
    async fn adopt_queued_runs(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
        sources: &[crate::v2::domain::ids::ConnectionRevisionId],
        max: usize,
    ) -> DomainResult<usize>;

    async fn recover_expired_submissions(&self) -> DomainResult<usize>;

    async fn set_tracking(
        &self,
        run_id: RunId,
        tracking_state: crate::v2::domain::operation::TrackingState,
        next_poll_at: Option<Timestamp>,
        deadline: Option<Timestamp>,
    ) -> DomainResult<()>;

    async fn append_event(
        &self,
        run_id: RunId,
        event_type: String,
        payload: Value,
    ) -> DomainResult<()>;

    async fn find_latest_attempt(&self, run_id: RunId) -> DomainResult<Option<RunAttempt>>;

    async fn list_operations(
        &self,
        limit: usize,
        after: Option<OperationId>,
    ) -> DomainResult<Vec<OperationSummary>>;

    /// 列出陈旧未终态（QUEUED/RUNNING）且 updated_at 早于 cuttoff 的操作 id。
    async fn list_stale_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>>;

    /// 列出已终态且 updated_at 早于 cuttoff 的操作 id（供保留策略清理）。
    async fn list_old_terminal_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>>;

    /// 把陈旧未终态操作整体置为失败（含其仍在途的 run/attempt）。
    /// 返回被置为失败的 run 数量。
    async fn force_fail_operation(
        &self,
        operation_id: OperationId,
        reason: &str,
    ) -> DomainResult<usize>;

    /// 级联删除给定操作及其 run/attempt/event/回调/资产元数据。返回删除的操作数。
    async fn delete_operations(&self, operation_ids: &[OperationId]) -> DomainResult<usize>;
}

#[derive(Debug, Clone)]
pub struct LeaseRecord {
    pub owner: String,
    pub token: String,
    pub expires_at: Timestamp,
}

#[async_trait]
pub trait LeaseStore: Send + Sync {
    async fn acquire(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()>;
    async fn renew(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()>;
    async fn release(&self, owner: &str, token: &str) -> DomainResult<()>;
    async fn current(&self) -> DomainResult<Option<LeaseRecord>>;
}

#[async_trait]
pub trait AssetRepository: Send + Sync {
    async fn find_asset(&self, id: AssetId) -> DomainResult<Option<MediaObject>>;
    async fn read_bytes(&self, id: AssetId) -> DomainResult<Option<Vec<u8>>>;
    async fn create_asset(&self, media: MediaObject, bytes: Vec<u8>) -> DomainResult<AssetId>;
}

#[derive(Debug, Clone)]
pub struct CallbackDeliveryRecord {
    pub delivery_id: String,
    pub operation_id: OperationId,
    pub event_id: u64,
    pub url: String,
    pub payload: Value,
    pub attempts: u32,
    pub max_attempts: u32,
    pub next_retry_at: Timestamp,
    pub completed: bool,
    pub last_error: Option<String>,
}

#[async_trait]
pub trait CallbackDeliveryRepository: Send + Sync {
    async fn create_delivery(&self, delivery: CallbackDeliveryRecord) -> DomainResult<()>;
    async fn list_due_deliveries(&self, limit: usize) -> DomainResult<Vec<CallbackDeliveryRecord>>;
    async fn mark_delivery_attempt(
        &self,
        delivery_id: &str,
        success: bool,
        error: Option<String>,
    ) -> DomainResult<()>;

    /// 找出"已经进终态、带回调订阅、但还没有投递记录"的操作（返回操作 id 与最新事件 id）。
    ///
    /// ⚠️ 为什么需要这条（真跑换来的教训）：投递的触发点曾经**一个都没有**——
    /// `enqueue_terminal` 只在测试里被调用过。于是订阅存了、任务跑完了、回调永远不发，
    /// 而所有单测都是绿的（它们自己显式调 enqueue，等于在测"我调用它它会工作"）。
    /// 改成**按状态补扫**之后：谁把任务推到终态的都不重要（reconcile / scheduler / cancel /
    /// hygiene 全覆盖），而且 **daemon 当时没在跑也补得回来**。
    ///
    /// `since_millis`：只看这之后更新过的操作——不给很久以前的旧任务突然补发通知。
    async fn list_operations_awaiting_callback(
        &self,
        since_millis: u64,
        limit: usize,
    ) -> DomainResult<Vec<(OperationId, u64)>>;
}

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, events: Vec<EventRecord>) -> DomainResult<()>;

    async fn list_after(
        &self,
        operation_id: OperationId,
        after: u64,
        limit: usize,
    ) -> DomainResult<Vec<EventRecord>>;
}

// ---------- Arc forwarding impls ----------
// 允许 `Arc<dyn T>` 作为 `dyn T` 复用（如 `Box::new(arc) -> Box<dyn T>` 与共享引用）。

#[async_trait]
impl RecipeRepository for Arc<dyn RecipeRepository> {
    async fn find_recipe(&self, id: RecipeId) -> DomainResult<Option<Recipe>> {
        self.as_ref().find_recipe(id).await
    }
    async fn find_revision(&self, id: RecipeRevisionId) -> DomainResult<Option<RecipeRevision>> {
        self.as_ref().find_revision(id).await
    }
    async fn list_recipes(
        &self,
        limit: usize,
        after: Option<RecipeId>,
    ) -> DomainResult<Vec<(Recipe, RecipeBinding)>> {
        self.as_ref().list_recipes(limit, after).await
    }
    async fn create_recipe(
        &self,
        recipe: Recipe,
        revision: RecipeRevision,
    ) -> DomainResult<(Recipe, RecipeRevision)> {
        self.as_ref().create_recipe(recipe, revision).await
    }
    async fn update_recipe(&self, recipe: &Recipe) -> DomainResult<()> {
        self.as_ref().update_recipe(recipe).await
    }
    async fn archive_recipe(&self, id: RecipeId) -> DomainResult<bool> {
        self.as_ref().archive_recipe(id).await
    }
    async fn append_revision(&self, revision: RecipeRevision) -> DomainResult<RecipeRevision> {
        self.as_ref().append_revision(revision).await
    }
    async fn restore_recipe(&self, id: RecipeId) -> DomainResult<bool> {
        self.as_ref().restore_recipe(id).await
    }
    async fn list_removed_recipes(&self, limit: usize) -> DomainResult<Vec<Recipe>> {
        self.as_ref().list_removed_recipes(limit).await
    }
}

#[async_trait]
impl ConnectionRepository for Arc<dyn ConnectionRepository> {
    async fn find_connection(&self, id: ConnectionId) -> DomainResult<Option<Connection>> {
        self.as_ref().find_connection(id).await
    }
    async fn find_revision(
        &self,
        id: ConnectionRevisionId,
    ) -> DomainResult<Option<ConnectionRevision>> {
        self.as_ref().find_revision(id).await
    }
    async fn list_revisions(&self) -> DomainResult<Vec<(String, String, ConnectionRevision)>> {
        self.as_ref().list_revisions().await
    }
    async fn list_connections(&self) -> DomainResult<Vec<Connection>> {
        self.as_ref().list_connections().await
    }
    async fn create_connection(
        &self,
        connection: Connection,
        revision: ConnectionRevision,
    ) -> DomainResult<(Connection, ConnectionRevision)> {
        self.as_ref().create_connection(connection, revision).await
    }
    async fn update_connection(&self, connection: &Connection) -> DomainResult<()> {
        self.as_ref().update_connection(connection).await
    }
    async fn create_revision(
        &self,
        connection_id: ConnectionId,
        revision: ConnectionRevision,
    ) -> DomainResult<ConnectionRevision> {
        self.as_ref().create_revision(connection_id, revision).await
    }
    async fn delete_connection(&self, id: ConnectionId) -> DomainResult<bool> {
        self.as_ref().delete_connection(id).await
    }
}

#[async_trait]
impl OperationRepository for Arc<dyn OperationRepository> {
    async fn find_by_idempotency(
        &self,
        actor_id: &str,
        client_id: &str,
        idempotency_key: &str,
    ) -> DomainResult<Option<PersistedOperation>> {
        self.as_ref()
            .find_by_idempotency(actor_id, client_id, idempotency_key)
            .await
    }
    async fn create_operation(&self, request: NewOperation) -> DomainResult<PersistedOperation> {
        self.as_ref().create_operation(request).await
    }
    async fn count_active_operations(&self, actor_id: &str) -> DomainResult<usize> {
        self.as_ref().count_active_operations(actor_id).await
    }
    async fn count_active_runs_for_connection(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
    ) -> DomainResult<usize> {
        self.as_ref()
            .count_active_runs_for_connection(connection_revision_id)
            .await
    }
    async fn adopt_queued_runs(
        &self,
        connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
        sources: &[crate::v2::domain::ids::ConnectionRevisionId],
        max: usize,
    ) -> DomainResult<usize> {
        self.as_ref()
            .adopt_queued_runs(connection_revision_id, sources, max)
            .await
    }
    async fn find_operation(&self, id: OperationId) -> DomainResult<Option<PersistedOperation>> {
        self.as_ref().find_operation(id).await
    }
    async fn update_run(&self, run: &Run) -> DomainResult<()> {
        self.as_ref().update_run(run).await
    }
    async fn fence_attempt(&self, run_id: RunId) -> DomainResult<()> {
        self.as_ref().fence_attempt(run_id).await
    }
    async fn append_attempt(&self, attempt: &RunAttempt) -> DomainResult<()> {
        self.as_ref().append_attempt(attempt).await
    }
    async fn claim_next_run(
        &self,
        owner: &str,
        fencing_token: &str,
        connection_revision_id: ConnectionRevisionId,
        now: Timestamp,
    ) -> DomainResult<Option<ClaimedRun>> {
        self.as_ref()
            .claim_next_run(owner, fencing_token, connection_revision_id, now)
            .await
    }
    async fn complete_attempt(
        &self,
        run_id: RunId,
        fencing_token: &str,
        resolution: AttemptResolution,
    ) -> DomainResult<Option<Run>> {
        self.as_ref()
            .complete_attempt(run_id, fencing_token, resolution)
            .await
    }
    async fn transition_run(
        &self,
        run_id: RunId,
        transition: Transition,
        events: Vec<EventSpec>,
    ) -> DomainResult<Run> {
        self.as_ref()
            .transition_run(run_id, transition, events)
            .await
    }
    async fn set_run_error(&self, run_id: RunId, error: Option<SafeError>) -> DomainResult<()> {
        self.as_ref().set_run_error(run_id, error).await
    }
    async fn set_run_usage(
        &self,
        run_id: RunId,
        usage: Option<serde_json::Value>,
    ) -> DomainResult<()> {
        self.as_ref().set_run_usage(run_id, usage).await
    }
    async fn list_active_runs(&self, limit: usize) -> DomainResult<Vec<ActiveRun>> {
        self.as_ref().list_active_runs(limit).await
    }
    async fn recover_expired_submissions(&self) -> DomainResult<usize> {
        self.as_ref().recover_expired_submissions().await
    }
    async fn set_tracking(
        &self,
        run_id: RunId,
        tracking_state: crate::v2::domain::operation::TrackingState,
        next_poll_at: Option<Timestamp>,
        deadline: Option<Timestamp>,
    ) -> DomainResult<()> {
        self.as_ref()
            .set_tracking(run_id, tracking_state, next_poll_at, deadline)
            .await
    }
    async fn append_event(
        &self,
        run_id: RunId,
        event_type: String,
        payload: Value,
    ) -> DomainResult<()> {
        self.as_ref()
            .append_event(run_id, event_type, payload)
            .await
    }
    async fn find_latest_attempt(&self, run_id: RunId) -> DomainResult<Option<RunAttempt>> {
        self.as_ref().find_latest_attempt(run_id).await
    }
    async fn list_operations(
        &self,
        limit: usize,
        after: Option<OperationId>,
    ) -> DomainResult<Vec<OperationSummary>> {
        self.as_ref().list_operations(limit, after).await
    }
    async fn list_stale_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>> {
        self.as_ref().list_stale_operations(cutoff, limit).await
    }
    async fn list_old_terminal_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>> {
        self.as_ref()
            .list_old_terminal_operations(cutoff, limit)
            .await
    }
    async fn force_fail_operation(
        &self,
        operation_id: OperationId,
        reason: &str,
    ) -> DomainResult<usize> {
        self.as_ref()
            .force_fail_operation(operation_id, reason)
            .await
    }
    async fn delete_operations(&self, operation_ids: &[OperationId]) -> DomainResult<usize> {
        self.as_ref().delete_operations(operation_ids).await
    }
}

#[async_trait]
impl EventStore for Arc<dyn EventStore> {
    async fn append(&self, events: Vec<EventRecord>) -> DomainResult<()> {
        self.as_ref().append(events).await
    }
    async fn list_after(
        &self,
        operation_id: OperationId,
        after: u64,
        limit: usize,
    ) -> DomainResult<Vec<EventRecord>> {
        self.as_ref().list_after(operation_id, after, limit).await
    }
}

#[async_trait]
impl AssetRepository for Arc<dyn AssetRepository> {
    async fn find_asset(&self, id: AssetId) -> DomainResult<Option<MediaObject>> {
        self.as_ref().find_asset(id).await
    }
    async fn read_bytes(&self, id: AssetId) -> DomainResult<Option<Vec<u8>>> {
        self.as_ref().read_bytes(id).await
    }
    async fn create_asset(&self, media: MediaObject, bytes: Vec<u8>) -> DomainResult<AssetId> {
        self.as_ref().create_asset(media, bytes).await
    }
}

#[async_trait]
impl CallbackDeliveryRepository for Arc<dyn CallbackDeliveryRepository> {
    async fn create_delivery(&self, delivery: CallbackDeliveryRecord) -> DomainResult<()> {
        self.as_ref().create_delivery(delivery).await
    }
    async fn list_due_deliveries(&self, limit: usize) -> DomainResult<Vec<CallbackDeliveryRecord>> {
        self.as_ref().list_due_deliveries(limit).await
    }
    async fn mark_delivery_attempt(
        &self,
        delivery_id: &str,
        success: bool,
        error: Option<String>,
    ) -> DomainResult<()> {
        self.as_ref()
            .mark_delivery_attempt(delivery_id, success, error)
            .await
    }
    async fn list_operations_awaiting_callback(
        &self,
        since_millis: u64,
        limit: usize,
    ) -> DomainResult<Vec<(OperationId, u64)>> {
        self.as_ref()
            .list_operations_awaiting_callback(since_millis, limit)
            .await
    }
}

#[async_trait]
impl LeaseStore for Arc<dyn LeaseStore> {
    async fn acquire(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()> {
        self.as_ref().acquire(owner, token, ttl_seconds).await
    }
    async fn renew(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()> {
        self.as_ref().renew(owner, token, ttl_seconds).await
    }
    async fn release(&self, owner: &str, token: &str) -> DomainResult<()> {
        self.as_ref().release(owner, token).await
    }
    async fn current(&self) -> DomainResult<Option<LeaseRecord>> {
        self.as_ref().current().await
    }
}

#[async_trait]
impl ArtifactRepository for Arc<dyn ArtifactRepository> {
    async fn list_by_run(&self, run_id: RunId) -> DomainResult<Vec<ArtifactSummary>> {
        self.as_ref().list_by_run(run_id).await
    }
    async fn list(
        &self,
        limit: usize,
        cursor: ArtifactCursor,
    ) -> DomainResult<Vec<ArtifactSummary>> {
        self.as_ref().list(limit, cursor).await
    }
}

#[async_trait]
impl TokenStore for Arc<dyn TokenStore> {
    async fn create(&self, record: &ActorTokenRecord) -> DomainResult<i64> {
        self.as_ref().create(record).await
    }
    async fn find_by_hash(&self, token_hash: &str) -> DomainResult<Option<ActorTokenRecord>> {
        self.as_ref().find_by_hash(token_hash).await
    }
    async fn find_by_id(&self, id: i64) -> DomainResult<Option<ActorTokenRecord>> {
        self.as_ref().find_by_id(id).await
    }
    async fn list(&self) -> DomainResult<Vec<ActorTokenRecord>> {
        self.as_ref().list().await
    }
    async fn revoke(&self, id: i64) -> DomainResult<()> {
        self.as_ref().revoke(id).await
    }
    async fn set_revoked(&self, id: i64, revoked: bool) -> DomainResult<()> {
        self.as_ref().set_revoked(id, revoked).await
    }
}

#[async_trait]
impl SettingsRepository for Arc<dyn SettingsRepository> {
    async fn load_settings(&self) -> DomainResult<SettingsRecord> {
        self.as_ref().load_settings().await
    }
    async fn save_settings(&self, record: &SettingsRecord) -> DomainResult<()> {
        self.as_ref().save_settings(record).await
    }
}
