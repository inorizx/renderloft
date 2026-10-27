use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::event::EventRecord;
use crate::v2::domain::execution::{
    ArtifactDraft, CancelOutcome, ConnectionCheck, ProviderDescriptor, RemoteHandle, SubmitOutcome,
    TaskSnapshot,
};
use crate::v2::domain::ids::{
    ArtifactId, AssetId, AttemptId, ClientId, ConnectionId, ConnectionRevisionId, OperationId,
    RecipeId, RecipeRevisionId, RunId,
};
use crate::v2::domain::media::{AssetRef, MediaObject};
use crate::v2::domain::model::{
    Connection, ConnectionRevision, Recipe, RecipeBinding, RecipeRevision, Timestamp,
};
use crate::v2::domain::operation::{
    AttemptStatus, OperationStatus, Run, RunAttempt, RunStatus, TrackingState, Transition,
};
use crate::v2::ports::callback::{CallbackDeliverer, DeliveryOutcome, DeliveryRequest};
use crate::v2::ports::policy::{AssetPolicy, AuthorizationPolicy, PolicyDecision};
use crate::v2::ports::provider::{
    CancelRequest, PrepareRequest, PreparedExecution, ProviderDriver, QueueCapacity, ReconcileRequest,
};
use crate::v2::ports::repositories::{
    ActiveRun, ActorTokenRecord, AttemptResolution, CallbackDeliveryRecord,
    CallbackDeliveryRepository, ClaimedRun, ConnectionRepository, EventSpec, EventStore,
    LeaseRecord, LeaseStore, NewOperation, OperationRepository, PersistedOperation,
    RecipeRepository, SettingsRecord, SettingsRepository, TokenStore, WorkspaceRepository,
};
use crate::v2::ports::stores::{ArtifactObject, ArtifactStore};

const SUBMISSION_RECOVERY_WINDOW_MILLIS: u64 = 60_000;
// 「默认跟踪死线」那个数字的所有者是 `domain::connection_options`（原来这里也抄了一份）。

#[derive(Debug, Default)]
struct InMemoryState {
    recipes: HashMap<RecipeId, (Recipe, Vec<RecipeRevision>)>,
    connections: HashMap<ConnectionId, (Connection, Vec<ConnectionRevision>)>,
    operations: Vec<PersistedOperation>,
    idempotency: HashMap<(String, String, String), OperationId>,
    events: Vec<EventRecord>,
    next_operation_id: u64,
    next_run_id: u64,
    next_connection_id: u64,
    next_connection_revision_id: u64,
    next_event_id: u64,
    attempts: Vec<RunAttempt>,
    lease: Option<LeaseRecord>,
    assets: HashMap<AssetId, (MediaObject, Vec<u8>)>,
    artifacts: HashMap<ArtifactId, ArtifactObject>,
    deliveries: Vec<CallbackDeliveryRecord>,
    tokens: Vec<crate::v2::ports::repositories::ActorTokenRecord>,
    settings: SettingsRecord,
    next_asset_id: u64,
    recovery_window_millis: u64,
    delivery_retry_delay_millis: u64,
}

#[derive(Clone)]
pub struct InMemoryRepositories {
    state: Arc<Mutex<InMemoryState>>,
}

impl Default for InMemoryRepositories {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRepositories {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(InMemoryState {
                recovery_window_millis: SUBMISSION_RECOVERY_WINDOW_MILLIS,
                delivery_retry_delay_millis: 30_000,
                ..Default::default()
            })),
        }
    }

    pub fn set_delivery_retry_delay(&self, millis: u64) {
        self.state.lock().unwrap().delivery_retry_delay_millis = millis;
    }

    pub fn seed_recipe(&self, recipe: Recipe, revisions: Vec<RecipeRevision>) {
        let mut state = self.state.lock().unwrap();
        state.recipes.insert(recipe.id, (recipe, revisions));
    }

    pub fn seed_connection(&self, connection: Connection, revisions: Vec<ConnectionRevision>) {
        let mut state = self.state.lock().unwrap();
        state
            .connections
            .insert(connection.id, (connection, revisions));
    }

    pub fn seed_asset(&self, id: AssetId, media: MediaObject, bytes: Vec<u8>) {
        let mut state = self.state.lock().unwrap();
        state.assets.insert(id, (media, bytes));
    }

    pub fn set_recovery_window(&self, millis: u64) {
        self.state.lock().unwrap().recovery_window_millis = millis;
    }

    pub fn event_count(&self) -> usize {
        self.state.lock().unwrap().events.len()
    }

    pub fn deliveries(&self) -> Vec<CallbackDeliveryRecord> {
        self.state.lock().unwrap().deliveries.clone()
    }

    pub fn attempts(&self) -> Vec<RunAttempt> {
        self.state.lock().unwrap().attempts.clone()
    }

    pub fn artifacts(&self) -> Vec<ArtifactObject> {
        self.state
            .lock()
            .unwrap()
            .artifacts
            .values()
            .cloned()
            .collect()
    }

    fn connection_snapshot(
        &self,
        state: &InMemoryState,
        revision_id: ConnectionRevisionId,
    ) -> DomainResult<(String, String, ConnectionRevision)> {
        for (connection, revisions) in state.connections.values() {
            if let Some(revision) = revisions.iter().find(|revision| revision.id == revision_id) {
                return Ok((
                    connection.vendor.clone(),
                    connection.protocol.clone(),
                    revision.clone(),
                ));
            }
        }
        Err(SafeError::not_found(
            "CONNECTION_REVISION_NOT_FOUND",
            "Connection revision 不存在",
        ))
    }
}

#[async_trait]
impl RecipeRepository for InMemoryRepositories {
    async fn find_recipe(&self, id: RecipeId) -> DomainResult<Option<Recipe>> {
        let state = self.state.lock().unwrap();
        Ok(state.recipes.get(&id).map(|(recipe, _)| recipe.clone()))
    }

    async fn find_revision(&self, id: RecipeRevisionId) -> DomainResult<Option<RecipeRevision>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .recipes
            .values()
            .flat_map(|(_, revisions)| revisions)
            .find(|revision| revision.id == id)
            .cloned())
    }

    async fn list_recipes(
        &self,
        limit: usize,
        after: Option<RecipeId>,
    ) -> DomainResult<Vec<(Recipe, RecipeBinding)>> {
        let state = self.state.lock().unwrap();
        let mut recipes: Vec<(Recipe, RecipeBinding)> = state
            .recipes
            .values()
            .map(|(recipe, revisions)| {
                let binding = revisions
                    .iter()
                    .find(|revision| revision.id == recipe.current_revision_id)
                    .map(|revision| revision.binding.clone())
                    .unwrap_or_default();
                (recipe.clone(), binding)
            })
            // Align with sqlite's WHERE r.removed_at IS NULL: removed recipes do not list.
            .filter(|(recipe, _)| recipe.removed_at.is_none())
            .filter(|(recipe, _)| after.is_none_or(|after| recipe.id > after))
            .collect();
        recipes.sort_by_key(|(recipe, _)| recipe.id);
        recipes.truncate(limit);
        Ok(recipes)
    }

    async fn create_recipe(
        &self,
        recipe: Recipe,
        revision: RecipeRevision,
    ) -> DomainResult<(Recipe, RecipeRevision)> {
        let mut state = self.state.lock().unwrap();
        state
            .recipes
            .insert(recipe.id, (recipe.clone(), vec![revision.clone()]));
        Ok((recipe, revision))
    }

    /// 与 sqlite 对齐：追加一份 revision、把当前版本指过去、**旧的一份留着**。
    async fn append_revision(&self, revision: RecipeRevision) -> DomainResult<RecipeRevision> {
        let mut state = self.state.lock().unwrap();
        let Some((recipe, revisions)) = state.recipes.get_mut(&revision.recipe_id) else {
            return Err(SafeError::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"));
        };
        let mut revision = revision;
        revision.id = RecipeRevisionId(revisions.len() as u64 + 1);
        revisions.push(revision.clone());
        recipe.current_revision_id = revision.id;
        recipe.updated_at = revision.created_at.clone();
        Ok(revision)
    }

    async fn update_recipe(&self, recipe: &Recipe) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        let Some((existing, _)) = state.recipes.get_mut(&recipe.id) else {
            return Err(SafeError::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"));
        };
        *existing = recipe.clone();
        Ok(())
    }

    async fn archive_recipe(&self, id: RecipeId) -> DomainResult<bool> {
        let mut state = self.state.lock().unwrap();
        // 与 sqlite 对齐：**打墓碑，不销毁**（revision 留着，历史仍可解析）。
        let Some((recipe, _)) = state.recipes.get_mut(&id) else {
            return Ok(false);
        };
        // 幂等：已移除过的不覆盖第一次的时间。
        if recipe.removed_at.is_none() {
            recipe.removed_at = Some(crate::v2::domain::model::Timestamp::now());
        }
        Ok(true)
    }

    async fn restore_recipe(&self, id: RecipeId) -> DomainResult<bool> {
        let mut state = self.state.lock().unwrap();
        let Some((recipe, _)) = state.recipes.get_mut(&id) else {
            return Ok(false);
        };
        let changed = recipe.removed_at.is_some();
        recipe.removed_at = None;
        Ok(changed)
    }

    async fn list_removed_recipes(
        &self,
        limit: usize,
    ) -> DomainResult<Vec<crate::v2::domain::model::Recipe>> {
        let state = self.state.lock().unwrap();
        let mut removed: Vec<_> = state
            .recipes
            .values()
            .map(|(recipe, _)| recipe.clone())
            .filter(|recipe| recipe.removed_at.is_some())
            .collect();
        // 与 sqlite 的 `ORDER BY removed_at DESC` 对齐——替身偷懒会让用例在替身上过、
        // 在生产上失败，那种偏差正是本模块要避免的。
        removed.sort_by(|a, b| {
            b.removed_at
                .as_ref()
                .map(|timestamp| &timestamp.0)
                .cmp(&a.removed_at.as_ref().map(|timestamp| &timestamp.0))
        });
        removed.truncate(limit);
        Ok(removed)
    }
}

#[async_trait]
impl ConnectionRepository for InMemoryRepositories {
    async fn find_connection(&self, id: ConnectionId) -> DomainResult<Option<Connection>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .connections
            .get(&id)
            .map(|(connection, _)| connection.clone()))
    }

    async fn find_revision(
        &self,
        id: ConnectionRevisionId,
    ) -> DomainResult<Option<ConnectionRevision>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .connections
            .values()
            .flat_map(|(_, revisions)| revisions)
            .find(|revision| revision.id == id)
            .cloned())
    }

    async fn list_revisions(&self) -> DomainResult<Vec<(String, String, ConnectionRevision)>> {
        let state = self.state.lock().unwrap();
        let mut result = Vec::new();
        for (connection, revisions) in state.connections.values() {
            for revision in revisions {
                result.push((
                    connection.vendor.clone(),
                    connection.protocol.clone(),
                    revision.clone(),
                ));
            }
        }
        Ok(result)
    }

    async fn list_connections(&self) -> DomainResult<Vec<Connection>> {
        let state = self.state.lock().unwrap();
        let mut connections: Vec<Connection> = state
            .connections
            .values()
            .map(|(connection, _)| connection.clone())
            .collect();
        connections.sort_by_key(|connection| connection.id);
        Ok(connections)
    }

    async fn create_connection(
        &self,
        connection: Connection,
        revision: ConnectionRevision,
    ) -> DomainResult<(Connection, ConnectionRevision)> {
        let mut state = self.state.lock().unwrap();
        // 与 sqlite 实现对齐：connection 与 revision 使用独立递增序列
        // （sqlite 两张表各自 AUTOINCREMENT）。
        let mut connection = connection;
        let mut revision = revision;
        if connection.id.0 == 0 {
            state.next_connection_id += 1;
            connection.id = ConnectionId(state.next_connection_id);
        }
        if revision.id.0 == 0 {
            state.next_connection_revision_id += 1;
            revision.id = ConnectionRevisionId(state.next_connection_revision_id);
        }
        revision.connection_id = connection.id;
        connection.current_revision_id = revision.id;
        state
            .connections
            .insert(connection.id, (connection.clone(), vec![revision.clone()]));
        Ok((connection, revision))
    }

    async fn update_connection(&self, connection: &Connection) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        let Some((existing, _)) = state.connections.get_mut(&connection.id) else {
            return Err(SafeError::not_found(
                "CONNECTION_NOT_FOUND",
                "Connection 不存在",
            ));
        };
        *existing = connection.clone();
        Ok(())
    }

    async fn delete_connection(&self, id: ConnectionId) -> DomainResult<bool> {
        let mut state = self.state.lock().unwrap();
        // 与 sqlite 对齐：那里还会拦"被历史任务引用"的连接（`v2_runs`）；替身没有建模
        // runs 与 revision 的关联，这一半对齐不了——这是**已知的偏差**，别在这里补一道
        // sqlite 没有的守卫（守着一张空表却对用户喊一句他做不到的指令，比不设防更糟）。
        Ok(state.connections.remove(&id).is_some())
    }

    async fn create_revision(
        &self,
        connection_id: ConnectionId,
        revision: ConnectionRevision,
    ) -> DomainResult<ConnectionRevision> {
        let mut state = self.state.lock().unwrap();
        // 与 sqlite 对齐：revision 使用独立递增序列，且从全局已存在最大 id 后继续，
        // 避免与 seed 直接写入的固定 id 冲突。
        let max_existing = state
            .connections
            .values()
            .flat_map(|(_, revisions)| revisions.iter().map(|revision| revision.id.0))
            .max()
            .unwrap_or(0);
        if state.next_connection_revision_id <= max_existing {
            state.next_connection_revision_id = max_existing;
        }
        state.next_connection_revision_id += 1;
        let revision_id = ConnectionRevisionId(state.next_connection_revision_id);
        let Some((_, revisions)) = state.connections.get_mut(&connection_id) else {
            return Err(SafeError::not_found(
                "CONNECTION_NOT_FOUND",
                "Connection 不存在",
            ));
        };
        let mut revision = revision;
        revision.id = revision_id;
        revision.connection_id = connection_id;
        revisions.push(revision.clone());
        Ok(revision)
    }
}

#[async_trait]
impl OperationRepository for InMemoryRepositories {
    async fn find_by_idempotency(
        &self,
        actor_id: &str,
        client_id: &str,
        idempotency_key: &str,
    ) -> DomainResult<Option<PersistedOperation>> {
        let state = self.state.lock().unwrap();
        let key = (
            actor_id.to_string(),
            client_id.to_string(),
            idempotency_key.to_string(),
        );
        let Some(operation_id) = state.idempotency.get(&key) else {
            return Ok(None);
        };
        Ok(state
            .operations
            .iter()
            .find(|persisted| persisted.operation.id == *operation_id)
            .cloned())
    }

    async fn create_operation(&self, request: NewOperation) -> DomainResult<PersistedOperation> {
        let mut state = self.state.lock().unwrap();
        let idempotency_key = (
            request.operation.actor.id.clone(),
            request.operation.actor.client_id.0.clone(),
            request.operation.idempotency_key.0.clone(),
        );
        if state.idempotency.contains_key(&idempotency_key) {
            return Err(SafeError::conflict(
                "IDEMPOTENCY_CONFLICT",
                "idempotencyKey 已存在",
            ));
        }
        state.next_operation_id += 1;
        let operation_id = OperationId(state.next_operation_id);
        let mut operation = request.operation;
        operation.id = operation_id;
        operation.updated_at = Timestamp::now();

        let mut runs = Vec::with_capacity(request.runs.len());
        for new_run in request.runs {
            state.next_run_id += 1;
            let run = Run {
                // 替身不建模用量（与 `set_run_usage` 的 no-op 一致，见那里的注释）。
                usage: None,
                id: RunId(state.next_run_id),
                operation_id,
                client_run_id: new_run.client_run_id,
                recipe_revision_id: new_run.recipe_revision_id,
                connection_revision_id: new_run.connection_revision_id,
                sequence: runs.len() as u32,
                inputs: new_run.inputs,
                assets: new_run.assets,
                status: RunStatus::Queued,
                tracking_state: TrackingState::Inactive,
                tracking_deadline: None,
                next_poll_at: None,
                last_polled_at: None,
                error: None,
                created_at: operation.created_at.clone(),
                updated_at: operation.created_at.clone(),
            };
            runs.push(run);
        }

        for (index, run) in runs.iter().enumerate() {
            state.next_event_id += 1;
            let event_id = state.next_event_id;
            state.events.push(EventRecord::new(
                event_id,
                index as u64,
                "run.created",
                operation_id,
                Some(run.id),
                operation.actor.clone(),
                operation.created_at.clone(),
                Default::default(),
            ));
        }
        state.next_event_id += 1;
        let event_id = state.next_event_id;
        state.events.push(EventRecord::new(
            event_id,
            runs.len() as u64,
            "operation.created",
            operation_id,
            None,
            operation.actor.clone(),
            operation.created_at.clone(),
            Default::default(),
        ));

        state.idempotency.insert(idempotency_key, operation_id);
        let persisted = PersistedOperation { operation, runs };
        state.operations.push(persisted.clone());
        Ok(persisted)
    }

    async fn find_operation(&self, id: OperationId) -> DomainResult<Option<PersistedOperation>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .operations
            .iter()
            .find(|persisted| persisted.operation.id == id)
            .cloned())
    }

    async fn update_run(&self, run: &Run) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        for persisted in &mut state.operations {
            for existing in &mut persisted.runs {
                if existing.id == run.id {
                    *existing = run.clone();
                    return Ok(());
                }
            }
        }
        Err(SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"))
    }

    async fn append_attempt(&self, attempt: &RunAttempt) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        state.attempts.push(attempt.clone());
        Ok(())
    }

    async fn fence_attempt(&self, run_id: RunId) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        for attempt in state.attempts.iter_mut().rev() {
            if attempt.run_id != run_id {
                continue;
            }
            if matches!(
                attempt.status,
                AttemptStatus::Succeeded
                    | AttemptStatus::Failed
                    | AttemptStatus::Uncertain
                    | AttemptStatus::Cancelled
            ) {
                break;
            }
            attempt.status = AttemptStatus::Cancelled;
            attempt.completed_at = Some(Timestamp::now());
            break;
        }
        Ok(())
    }

    async fn claim_next_run(
        &self,
        owner: &str,
        fencing_token: &str,
        connection_revision_id: ConnectionRevisionId,
        now: Timestamp,
    ) -> DomainResult<Option<ClaimedRun>> {
        let mut state = self.state.lock().unwrap();
        // ⚠️ 认领要**保留 run 上原有的字段**（尤其 `tracking_deadline`）。真实现只改
        // `status` + `updated_at`（见 `sqlite::exec::claim_next_run` 里那条 UPDATE），
        // 而这里此前把 run **整个重建成** `tracking_deadline: None` —— 于是任何"看死线"的判据
        // 在测试里永远看不到死线：**生产会判、测试说没判**。实测踩到过（2026-10-18，
        // 退避重试到期那条判据在替身上静默失效）。所以这里连整个 run 一起带出来。
        let (run_id, operation_id, recipe_revision_id, claimed_run) = {
            let run = state
                .operations
                .iter_mut()
                .flat_map(|persisted| persisted.runs.iter_mut())
                .find(|run| {
                    (run.status == RunStatus::Queued || run.status == RunStatus::RetryWait)
                        && run.connection_revision_id == connection_revision_id
                        && (run.status == RunStatus::Queued
                            || run
                                .next_poll_at
                                .as_ref()
                                .is_none_or(|next| next.millis() <= now.millis()))
                });
            let Some(run) = run else {
                return Ok(None);
            };
            run.status = RunStatus::Submitting;
            run.tracking_state = TrackingState::Inactive;
            run.updated_at = now.clone();
            (
                run.id,
                run.operation_id,
                run.recipe_revision_id,
                run.clone(),
            )
        };

        let number = state
            .attempts
            .iter()
            .filter(|attempt| attempt.run_id == run_id)
            .count() as u32
            + 1;
        let attempt = RunAttempt {
            id: AttemptId(0),
            run_id,
            number,
            status: AttemptStatus::Submitting,
            request_fingerprint: format!("run-{}:attempt-{number}", run_id.0),
            fencing_token: fencing_token.to_string(),
            remote_handle: None,
            error: None,
            started_at: now.clone(),
            completed_at: None,
        };
        state.attempts.push(attempt.clone());

        let recipe_revision = state
            .recipes
            .values()
            .flat_map(|(_, revisions)| revisions)
            .find(|revision| revision.id == recipe_revision_id)
            .cloned()
            .ok_or_else(|| {
                SafeError::not_found("RECIPE_REVISION_NOT_FOUND", "Recipe revision 不存在")
            })?;
        let (_, _, connection_revision) =
            self.connection_snapshot(&state, connection_revision_id)?;
        let claimed = ClaimedRun {
            // 用**认领时那条 run 的原样**（只 status/updated_at 被改过）——见上面那段注释：
            // 重建 run 会把死线、退避时间、上次轮询时间一起丢掉，测试就再也测不到真行为。
            run: claimed_run,
            recipe_name: state
                .recipes
                .get(&recipe_revision.recipe_id)
                .map(|(recipe, _)| recipe.name.clone())
                .unwrap_or_default(),
            recipe_revision,
            connection_revision,
            attempt,
        };
        state.next_event_id += 1;
        let event_id = state.next_event_id;
        let sequence = state.events.len() as u64;
        state.events.push(EventRecord::new(
            event_id,
            sequence,
            "run.claimed",
            operation_id,
            Some(run_id),
            ActorRef {
                id: owner.to_string(),
                kind: ActorKind::System,
                client_id: ClientId("scheduler".to_string()),
                scopes: vec![],
                label: None,
            },
            now,
            Default::default(),
        ));
        Ok(Some(claimed))
    }

    async fn complete_attempt(
        &self,
        run_id: RunId,
        fencing_token: &str,
        resolution: AttemptResolution,
    ) -> DomainResult<Option<Run>> {
        let mut state = self.state.lock().unwrap();
        let Some(attempt) = state
            .attempts
            .iter_mut()
            .filter(|attempt| attempt.run_id == run_id)
            .max_by_key(|attempt| attempt.number)
        else {
            return Ok(None);
        };
        if attempt.fencing_token != fencing_token || attempt.status.is_terminal() {
            return Ok(None);
        }
        match &resolution {
            AttemptResolution::Succeeded { remote_handle } => {
                attempt.status = AttemptStatus::Succeeded;
                attempt.remote_handle = remote_handle.clone();
            }
            AttemptResolution::Failed { error, .. } => {
                attempt.status = AttemptStatus::Failed;
                attempt.error = Some(error.clone());
            }
            AttemptResolution::Uncertain {
                remote_handle,
                error,
            } => {
                attempt.status = AttemptStatus::Uncertain;
                attempt.remote_handle = remote_handle.clone();
                // 与 sqlite 一致：不确定态也要把"为什么不明"留下来（替身不一致 = 测试替生产说谎）。
                attempt.error = error.clone();
            }
        }
        attempt.completed_at = Some(Timestamp::now());
        Ok(state
            .operations
            .iter()
            .flat_map(|persisted| persisted.runs.iter())
            .find(|run| run.id == run_id)
            .cloned())
    }

    async fn transition_run(
        &self,
        run_id: RunId,
        transition: Transition,
        events: Vec<EventSpec>,
    ) -> DomainResult<Run> {
        let mut state = self.state.lock().unwrap();
        let mut operation_id = None;
        let mut updated = None;
        for persisted in &mut state.operations {
            for existing in &mut persisted.runs {
                if existing.id == run_id {
                    operation_id = Some(existing.operation_id);
                    let now = Timestamp::now();
                    existing.apply(transition, now.clone())?;
                    apply_tracking(existing, now);
                    updated = Some(existing.clone());
                }
            }
        }
        let Some(operation_id) = operation_id else {
            return Err(SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"));
        };
        let updated = updated.expect("run 已找到");
        for event in events {
            state.next_event_id += 1;
            let event_id = state.next_event_id;
            let sequence = state.events.len() as u64;
            let actor = state
                .operations
                .iter()
                .find(|persisted| persisted.operation.id == operation_id)
                .map(|persisted| persisted.operation.actor.clone())
                .unwrap_or(system_actor());
            state.events.push(EventRecord::new(
                event_id,
                sequence,
                event.event_type,
                operation_id,
                event.run_id,
                actor,
                updated.updated_at.clone(),
                event.safe_payload,
            ));
        }
        for persisted in &mut state.operations {
            if persisted.operation.id == operation_id {
                persisted.operation.status =
                    crate::v2::domain::operation::project_operation_status(&persisted.runs);
                persisted.operation.updated_at = updated.updated_at.clone();
            }
        }
        Ok(updated)
    }

    /// 替身**不模拟用量**：`Run` 上没有这个字段（用量走单独的一次写入 + 批量读）。
    /// 需要断言用量的测试，先给替身加一个 side map，别在断言里假装它是 0。
    async fn set_run_usage(
        &self,
        _run_id: RunId,
        _usage: Option<serde_json::Value>,
    ) -> DomainResult<()> {
        Ok(())
    }

    async fn set_run_error(&self, run_id: RunId, error: Option<SafeError>) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        for persisted in &mut state.operations {
            for existing in &mut persisted.runs {
                if existing.id == run_id {
                    existing.error = error.clone();
                    existing.updated_at = Timestamp::now();
                    return Ok(());
                }
            }
        }
        Err(SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"))
    }

    async fn count_queued_runs_for_connection(
        &self,
        connection_revision_id: ConnectionRevisionId,
    ) -> DomainResult<usize> {
        let state = self.state.lock().unwrap();
        Ok(state
            .operations
            .iter()
            .flat_map(|persisted| persisted.runs.iter())
            .filter(|run| {
                run.connection_revision_id == connection_revision_id
                    && run.status == RunStatus::Queued
            })
            .count())
    }

    async fn adopt_queued_runs(
        &self,
        connection_revision_id: ConnectionRevisionId,
        sources: &[ConnectionRevisionId],
        max: usize,
    ) -> DomainResult<usize> {
        if max == 0 || sources.is_empty() {
            return Ok(0);
        }
        let mut state = self.state.lock().unwrap();
        // 先收集"这张池子已经用到的配方"，再改 run（借用不能混）。
        let recipes: Vec<u64> = state
            .operations
            .iter()
            .flat_map(|persisted| persisted.runs.iter())
            .filter(|run| run.connection_revision_id == connection_revision_id)
            .map(|run| run.recipe_revision_id.0)
            .collect();
        let attempted: Vec<u64> = state
            .attempts
            .iter()
            .map(|attempt| attempt.run_id.0)
            .collect();
        let mut adopted = 0usize;
        'outer: for persisted in &mut state.operations {
            for run in &mut persisted.runs {
                if adopted == max {
                    break 'outer;
                }
                if run.status == RunStatus::Queued
                    && run.connection_revision_id != connection_revision_id
                    && sources.contains(&run.connection_revision_id)
                    && recipes.contains(&run.recipe_revision_id.0)
                    && !attempted.contains(&run.id.0)
                {
                    run.connection_revision_id = connection_revision_id;
                    run.updated_at = Timestamp::now();
                    adopted += 1;
                }
            }
        }
        Ok(adopted)
    }

    async fn count_active_runs_for_connection(
        &self,
        connection_revision_id: ConnectionRevisionId,
    ) -> DomainResult<usize> {
        let state = self.state.lock().unwrap();
        Ok(state
            .operations
            .iter()
            .flat_map(|persisted| persisted.runs.iter())
            .filter(|run| {
                run.connection_revision_id == connection_revision_id
                    // 与 sqlite 同判据：`SUBMISSION_UNCERTAIN` **不算占位**——它占的是
                    // "我们不知道平台收没收到"这个疑问，不是平台的执行位（见 `sqlite::exec` 里那段）。
                    && matches!(run.status, RunStatus::Accepted | RunStatus::Running)
                    && run.tracking_state == TrackingState::Active
            })
            .count())
    }

    async fn count_active_operations(&self, actor_id: &str) -> DomainResult<usize> {
        let state = self.state.lock().unwrap();
        Ok(state
            .operations
            .iter()
            .filter(|persisted| {
                persisted.operation.actor.id == actor_id
                    && !persisted.operation.status.is_terminal()
            })
            .count())
    }

    async fn list_active_runs(&self, limit: usize) -> DomainResult<Vec<ActiveRun>> {
        let state = self.state.lock().unwrap();
        let mut result = Vec::new();
        for persisted in &state.operations {
            for run in &persisted.runs {
                if !matches!(
                    run.status,
                    RunStatus::Accepted | RunStatus::Running | RunStatus::SubmissionUncertain
                ) || run.tracking_state != TrackingState::Active
                {
                    continue;
                }
                let (vendor, protocol, connection_revision) =
                    self.connection_snapshot(&state, run.connection_revision_id)?;
                let recipe_revision = state
                    .recipes
                    .values()
                    .flat_map(|(_, revisions)| revisions)
                    .find(|revision| revision.id == run.recipe_revision_id)
                    .cloned()
                    .ok_or_else(|| {
                        SafeError::not_found("RECIPE_REVISION_NOT_FOUND", "Recipe revision 不存在")
                    })?;
                let Some(attempt) = state
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.run_id == run.id)
                    .max_by_key(|attempt| attempt.number)
                    .cloned()
                else {
                    continue;
                };
                result.push(ActiveRun {
                    run: run.clone(),
                    vendor,
                    protocol,
                    // 与 sqlite 同语义：取不到就留空，由 `compose_filename` 退回原名。
                    recipe_name: state
                        .recipes
                        .get(&recipe_revision.recipe_id)
                        .map(|(recipe, _)| recipe.name.clone())
                        .unwrap_or_default(),
                    recipe_revision,
                    connection_revision,
                    attempt,
                });
                if result.len() >= limit {
                    break;
                }
            }
        }
        Ok(result)
    }

    async fn recover_expired_submissions(&self) -> DomainResult<usize> {
        let mut state = self.state.lock().unwrap();
        let now = Timestamp::now_millis();
        let window = state.recovery_window_millis;
        let mut recovered = 0;
        let expired: Vec<RunId> = state
            .attempts
            .iter()
            .filter(|attempt| {
                attempt.status == AttemptStatus::Submitting
                    && attempt.started_at.millis() + window <= now
            })
            .map(|attempt| attempt.run_id)
            .collect();
        for run_id in expired {
            for persisted in &mut state.operations {
                for run in &mut persisted.runs {
                    if run.id == run_id && run.status == RunStatus::Submitting {
                        run.status = RunStatus::SubmissionUncertain;
                        run.tracking_state = TrackingState::Active;
                        run.tracking_deadline = Some(Timestamp::from_millis(
                            now + crate::v2::domain::connection_options::DEFAULT_TRACKING_DEADLINE_MILLIS,
                        ));
                        run.updated_at = Timestamp::now();
                        recovered += 1;
                    }
                }
            }
            for attempt in &mut state.attempts {
                if attempt.run_id == run_id && attempt.status == AttemptStatus::Submitting {
                    attempt.status = AttemptStatus::Uncertain;
                    attempt.completed_at = Some(Timestamp::now());
                }
            }
        }
        Ok(recovered)
    }

    async fn set_tracking(
        &self,
        run_id: RunId,
        tracking_state: TrackingState,
        next_poll_at: Option<Timestamp>,
        deadline: Option<Timestamp>,
    ) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        for persisted in &mut state.operations {
            for run in &mut persisted.runs {
                if run.id == run_id {
                    run.tracking_state = tracking_state;
                    if next_poll_at.is_some() {
                        run.next_poll_at = next_poll_at;
                    }
                    if deadline.is_some() {
                        run.tracking_deadline = deadline;
                    }
                    run.updated_at = Timestamp::now();
                    return Ok(());
                }
            }
        }
        Err(SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"))
    }

    async fn append_event(
        &self,
        run_id: RunId,
        event_type: String,
        payload: serde_json::Value,
    ) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        let operation_id = state
            .operations
            .iter()
            .flat_map(|persisted| persisted.runs.iter())
            .find(|run| run.id == run_id)
            .map(|run| run.operation_id)
            .ok_or_else(|| SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"))?;
        state.next_event_id += 1;
        let event_id = state.next_event_id;
        let sequence = state.events.len() as u64;
        state.events.push(EventRecord::new(
            event_id,
            sequence,
            event_type,
            operation_id,
            Some(run_id),
            system_actor(),
            Timestamp::now(),
            payload,
        ));
        Ok(())
    }

    async fn find_latest_attempt(&self, run_id: RunId) -> DomainResult<Option<RunAttempt>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .attempts
            .iter()
            .filter(|attempt| attempt.run_id == run_id)
            .max_by_key(|attempt| attempt.number)
            .cloned())
    }

    async fn list_operations(
        &self,
        limit: usize,
        after: Option<OperationId>,
    ) -> DomainResult<Vec<crate::v2::ports::repositories::OperationSummary>> {
        let state = self.state.lock().unwrap();
        let mut summaries: Vec<_> = state
            .operations
            .iter()
            .filter(|persisted| after.is_none_or(|after| persisted.operation.id < after))
            .map(|persisted| {
                // attention 走**与 sqlite 同一套**判据：挑最阻塞的那条 run
                // （`operation_attention` 按 `ATTENTION_LADDER` 的严重度取最小）。
                // 替身里手写一份"哪个状态更严重"就会与生产悄悄不一致，而这类漂移
                // 在这个仓库里已经咬过（见 domain::operation::apply_tracking 的注释）。
                let attention = crate::v2::domain::operation::operation_attention(&persisted.runs);
                crate::v2::ports::repositories::OperationSummary {
                    id: persisted.operation.id,
                    status: persisted.operation.status,
                    created_at: persisted.operation.created_at.clone(),
                    run_count: persisted.runs.len() as u64,
                    attention_reason: attention.as_ref().map(|attention| attention.reason.clone()),
                    attention_deadline: attention.and_then(|attention| attention.deadline),
                }
            })
            .collect();
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.id));
        summaries.truncate(limit);
        Ok(summaries)
    }

    async fn list_stale_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>> {
        let state = self.state.lock().unwrap();
        let mut ids: Vec<_> = state
            .operations
            .iter()
            .filter(|persisted| {
                !persisted.operation.status.is_terminal()
                    && persisted.operation.updated_at.millis() < cutoff.millis()
            })
            .map(|persisted| persisted.operation.id)
            .collect();
        ids.sort_by_key(|id| *id);
        ids.truncate(limit);
        Ok(ids)
    }

    async fn list_old_terminal_operations(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> DomainResult<Vec<OperationId>> {
        let state = self.state.lock().unwrap();
        let mut ids: Vec<_> = state
            .operations
            .iter()
            .filter(|persisted| {
                persisted.operation.status.is_terminal()
                    && persisted.operation.updated_at.millis() < cutoff.millis()
            })
            .map(|persisted| persisted.operation.id)
            .collect();
        ids.sort_by_key(|id| *id);
        ids.truncate(limit);
        Ok(ids)
    }

    async fn force_fail_operation(
        &self,
        operation_id: OperationId,
        _reason: &str,
    ) -> DomainResult<usize> {
        let mut state = self.state.lock().unwrap();
        let Some(persisted) = state
            .operations
            .iter_mut()
            .find(|persisted| persisted.operation.id == operation_id)
        else {
            return Ok(0);
        };
        let now = Timestamp::now();
        // 先以 owned 方式收集 run id，避免与下方对 runs 的持久可变借用冲突。
        let run_ids: Vec<RunId> = persisted.runs.iter().map(|run| run.id).collect();
        let mut failed = 0usize;
        let mut has_success = false;
        for run in &mut persisted.runs {
            if run.status == RunStatus::Succeeded {
                has_success = true;
                continue;
            }
            if run.status.is_terminal() {
                continue;
            }
            failed += 1;
            run.status = RunStatus::Failed;
            run.tracking_state = TrackingState::Exhausted;
            run.tracking_deadline = None;
            run.next_poll_at = None;
            run.updated_at = now.clone();
        }
        persisted.operation.status = if has_success {
            OperationStatus::Partial
        } else {
            OperationStatus::Failed
        };
        persisted.operation.updated_at = now.clone();
        // persisted 的借用至此结束（NLL），后续访问 state 其它字段互不冲突。
        for attempt in state.attempts.iter_mut() {
            if run_ids.contains(&attempt.run_id) && !attempt.status.is_terminal() {
                attempt.status = AttemptStatus::Failed;
                attempt.completed_at = Some(now.clone());
            }
        }
        let seq = state.next_event_id;
        state.next_event_id += 1;
        state.events.push(EventRecord {
            id: seq,
            event_type: "operation.hygiene_failed".to_string(),
            operation_id,
            run_id: None,
            actor: system_actor(),
            sequence: seq,
            created_at: now,
            safe_payload: serde_json::json!({ "failedRuns": failed }),
        });
        Ok(failed)
    }

    async fn delete_operations(&self, operation_ids: &[OperationId]) -> DomainResult<usize> {
        let mut state = self.state.lock().unwrap();
        let removed_run_ids: Vec<RunId> = state
            .operations
            .iter()
            .filter(|persisted| operation_ids.contains(&persisted.operation.id))
            .flat_map(|persisted| persisted.runs.iter().map(|run| run.id))
            .collect();
        let before = state.operations.len();
        state
            .operations
            .retain(|persisted| !operation_ids.contains(&persisted.operation.id));
        state
            .attempts
            .retain(|attempt| !removed_run_ids.contains(&attempt.run_id));
        state
            .events
            .retain(|event| !operation_ids.contains(&event.operation_id));
        Ok(before - state.operations.len())
    }
}

fn system_actor() -> ActorRef {
    ActorRef {
        id: "system".to_string(),
        kind: ActorKind::System,
        client_id: ClientId("system".to_string()),
        scopes: vec![],
        label: None,
    }
}

fn apply_tracking(run: &mut Run, now: Timestamp) {
    // 规则的所有者是 `Run::apply_tracking`（domain）——替身与生产**必须**走同一条，
    // 各写一份的话只改一处就会悄悄不一致，而"替身不一致 = 测试替生产说谎"。
    run.apply_tracking(now);
}

#[async_trait]
impl EventStore for InMemoryRepositories {
    async fn append(&self, events: Vec<EventRecord>) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        for mut event in events {
            state.next_event_id += 1;
            event.id = state.next_event_id;
            state.events.push(event);
        }
        Ok(())
    }

    async fn list_after(
        &self,
        operation_id: OperationId,
        after: u64,
        limit: usize,
    ) -> DomainResult<Vec<EventRecord>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .events
            .iter()
            .filter(|event| event.operation_id == operation_id && event.id > after)
            .take(limit)
            .cloned()
            .collect())
    }
}

pub struct PermitAllAuthorizationPolicy;

impl PermitAllAuthorizationPolicy {
    pub fn boxed() -> Box<Self> {
        Box::new(Self)
    }
}

#[async_trait]
impl AuthorizationPolicy for PermitAllAuthorizationPolicy {
    async fn require_scope(
        &self,
        _actor: &ActorRef,
        _scope: Scope,
    ) -> DomainResult<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason: None,
        })
    }
}

pub struct PermitAllAssetPolicy;

#[async_trait]
impl AssetPolicy for PermitAllAssetPolicy {
    async fn validate_asset_ref(&self, _asset_ref: &AssetRef) -> DomainResult<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason: None,
        })
    }
}

pub struct FakeProviderDriver {
    descriptor: ProviderDescriptor,
    outcomes: Mutex<Vec<SubmitOutcome>>,
    snapshots: Mutex<Vec<TaskSnapshot>>,
    submit_errors: Mutex<Vec<SafeError>>,
    cancel_calls: std::sync::atomic::AtomicUsize,
    /// 这个 provider 允许几个并发（真平台是 `concurrentLimit`；实测 RunningHub 账号是 1）。
    concurrency_limit: usize,
    /// 当前**占着执行位**的远端任务 id。
    ///
    /// ⚠️ 这个字段是本次改造的核心。旧替身只有 `outcomes: Vec<SubmitOutcome>`
    /// （按顺序吐脚本结果），**没有"在跑几个"这个状态**——于是它在结构上
    /// 表达不出"平台因为忙而拒绝新提交"，而调度器抢执行位的 bug 正是这一句：
    /// 它从 309 个测试里活了下来，最后靠**花钱真跑**才发现
    /// （`RUNNINGHUB_REJECTED / TASK_QUEUE_MAXED`）。
    in_flight: Mutex<std::collections::HashSet<String>>,
    /// 被平台拒绝的提交次数。**测试的判据落在这里**——
    /// 断言"我们一次都没被拒"比断言调度器的内部计数更接近用户看到的事实。
    refusals: std::sync::atomic::AtomicUsize,
    /// "平台侧"那张工作流图：`fetch_workflow_prompt` 返回它。
    ///
    /// 导入工作流那条路（`RecipeImportService::import_workflow`）必须先拉一次 prompt
    /// 才知道有哪些字段，所以这条测试需要驱动能吐一张图；默认 `None` = 驱动不支持
    /// （与端口的默认实现同义）。
    workflow_prompt: Mutex<Option<serde_json::Value>>,
    /// 每次 `prepare` 收到的**密钥槽位**（按调用顺序）。
    ///
    /// 为什么记槽位而不是整个 revision：凭据引用这条链上，唯一需要证明的事就是
    /// "驱动最终拿到的是**借来的**那个槽位"（`connection-1-api_key`），
    /// 而不是引用方自己的空绑定。记槽位让断言直接落在这一句话上。
    prepare_slots: Mutex<Vec<Vec<String>>>,
    /// "平台声明的模型清单"。`None` = 这个驱动没有清单这条路（与端口的默认实现同义）。
    models: Mutex<Option<Vec<String>>>,
    /// 平台声明的并发容量。`None` = 这个替身**不报容量**（默认），调度退回静态槽位——
    /// 这样既有的"每 tick 一条"用例语义不变，要用容量的用例显式声明。
    declared_capacity: Mutex<Option<QueueCapacity>>,
    /// `fetch_queue_status` 被调了几次。用来钉住"容量有缓存，不是每个 tick 都问"。
    queue_status_calls: std::sync::atomic::AtomicUsize,
}

impl FakeProviderDriver {
    pub fn new(descriptor: ProviderDescriptor) -> Self {
        Self {
            descriptor,
            outcomes: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
            submit_errors: Mutex::new(Vec::new()),
            cancel_calls: std::sync::atomic::AtomicUsize::new(0),
            // 默认 1：与实测到的真实账号一致（`concurrentLimit: 1`）。
            concurrency_limit: 1,
            in_flight: Mutex::new(std::collections::HashSet::new()),
            refusals: std::sync::atomic::AtomicUsize::new(0),
            workflow_prompt: Mutex::new(None),
            prepare_slots: Mutex::new(Vec::new()),
            models: Mutex::new(None),
            declared_capacity: Mutex::new(None),
            queue_status_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 让这个替身"平台上有这么一张工作流图"（供导入工作流的用例使用）。
    pub fn with_workflow_prompt(mut self, prompt: serde_json::Value) -> Self {
        self.workflow_prompt = Mutex::new(Some(prompt));
        self
    }

    /// 改并发上限（真平台按账号不同；默认 1）。
    pub fn with_concurrency_limit(mut self, limit: usize) -> Self {
        self.concurrency_limit = limit;
        self
    }

    /// 让平台"报出自己的容量"（`concurrentLimit` / `runningCount`）。
    ///
    /// `running` 是平台侧看到的占用，**可以包含不是我们提交的任务**——这正是它比本地账本
    /// 更准的理由；`occupy_external` 造的就是那种任务。
    pub fn with_queue_capacity(self, limit: usize, running: usize) -> Self {
        *self.declared_capacity.lock().unwrap() = Some(QueueCapacity { limit, running });
        self
    }

    /// 模拟"平台上已经有任务在跑，但不是我们提交的"（网页控制台 / 别的工具）。
    ///
    /// 与 `release` 对称：这个任务会占住执行位，让 `submit` 按并发上限拒绝我们。
    pub fn occupy_external(&self, remote_id: &str) {
        self.in_flight.lock().unwrap().insert(remote_id.to_string());
    }

    pub fn queue_status_calls(&self) -> usize {
        self.queue_status_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 模拟"平台上的这个任务跑完了，执行位释放"。
    pub fn release(&self, remote_id: &str) {
        self.in_flight.lock().unwrap().remove(remote_id);
    }

    /// 被平台拒绝过几次。**零**才是正常——非零说明我们在抢执行位。
    pub fn refusal_count(&self) -> usize {
        self.refusals.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn push_outcome(&self, outcome: SubmitOutcome) {
        self.outcomes.lock().unwrap().push(outcome);
    }

    pub fn push_snapshot(&self, snapshot: TaskSnapshot) {
        self.snapshots.lock().unwrap().push(snapshot);
    }

    pub fn push_submit_error(&self, error: SafeError) {
        self.submit_errors.lock().unwrap().push(error);
    }

    pub fn cancel_call_count(&self) -> usize {
        self.cancel_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 历次 `prepare` 收到的密钥槽位（新的在后）。凭据引用的断言看这个。
    pub fn prepare_slots(&self) -> Vec<Vec<String>> {
        self.prepare_slots.lock().unwrap().clone()
    }

    /// 让这个替身"平台声明了这些模型"（供模型发现的用例使用）。
    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = Mutex::new(Some(models));
        self
    }
}

#[async_trait]
impl ProviderDriver for FakeProviderDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    /// 只有在用例显式给了图（`with_workflow_prompt`）时才回答；否则回落到端口的默认行为
    /// （"该 Provider 不支持拉取工作流"）——**别让替身比真驱动更能干**。
    async fn fetch_workflow_prompt(
        &self,
        _connection: &ConnectionRevision,
        _workflow_id: &str,
    ) -> DomainResult<serde_json::Value> {
        match self.workflow_prompt.lock().unwrap().clone() {
            Some(prompt) => Ok(prompt),
            None => Err(SafeError::validation(
                "IMPORT_NOT_SUPPORTED",
                "该 Provider 不支持拉取工作流",
            )),
        }
    }

    /// 只有用例显式给了模型清单（`with_models`）时才回答；否则回落到端口的默认行为
    /// （"该 Provider 不支持模型清单"）——**别让替身比真驱动更能干**。
    async fn fetch_models(&self, _connection: &ConnectionRevision) -> DomainResult<Vec<String>> {
        match self.models.lock().unwrap().clone() {
            Some(models) => Ok(models),
            None => Err(SafeError::validation(
                "MODELS_NOT_SUPPORTED",
                "该 Provider 不支持模型清单",
            )),
        }
    }

    async fn validate_connection(
        &self,
        connection: &ConnectionRevision,
    ) -> DomainResult<ConnectionCheck> {
        Ok(ConnectionCheck {
            ok: true,
            capabilities: connection.effective_capabilities.clone(),
            message: None,
        })
    }

    async fn fetch_account_status(
        &self,
        _connection: &ConnectionRevision,
    ) -> DomainResult<serde_json::Value> {
        Ok(serde_json::json!({
            "remainCoins": "1000",
            "currentTaskCounts": "2",
            "remainMoney": "5.00",
            "currency": "CNY",
            "apiType": "pro"
        }))
    }

    async fn fetch_queue_status(
        &self,
        _connection: &ConnectionRevision,
    ) -> DomainResult<serde_json::Value> {
        self.queue_status_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let declared = *self.declared_capacity.lock().unwrap();
        Ok(match declared {
            Some(capacity) => serde_json::json!({
                "apiKeyType": "pro",
                "concurrentLimit": capacity.limit,
                "runningCount": capacity.running.to_string(),
                "queuedCount": "0",
                "totalCurrentTasks": capacity.running.to_string()
            }),
            // 没声明容量时仍给一份"能看"的队列状态（设置页的展示用例读它），
            // 但 `queue_capacity` 会返回 None —— 两种语义不能混。
            None => serde_json::json!({
                "apiKeyType": "pro",
                "concurrentLimit": 2,
                "runningCount": "1",
                "queuedCount": "0",
                "totalCurrentTasks": "1"
            }),
        })
    }

    fn queue_capacity(&self, status: &serde_json::Value) -> Option<QueueCapacity> {
        if self.declared_capacity.lock().unwrap().is_none() {
            return None;
        }
        let limit = status.get("concurrentLimit")?.as_u64()? as usize;
        let running = status
            .get("runningCount")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        (limit > 0).then_some(QueueCapacity { limit, running })
    }

    fn is_capacity_rejection(&self, error: &SafeError) -> bool {
        error.code == "RUNNINGHUB_REJECTED"
            && error.message.to_ascii_uppercase().contains("TASK_QUEUE_MAXED")
    }

    async fn prepare(&self, request: PrepareRequest) -> DomainResult<PreparedExecution> {
        self.prepare_slots.lock().unwrap().push(
            request
                .connection_revision
                .secret_bindings
                .iter()
                .map(|binding| binding.slot.clone())
                .collect(),
        );
        Ok(PreparedExecution {
            connection_revision: request.connection_revision,
            provider_request: serde_json::json!({ "fingerprint": request.request_fingerprint }),
            request_id: format!("req-{}", request.request_fingerprint),
        })
    }

    async fn submit(&self, _execution: PreparedExecution) -> DomainResult<SubmitOutcome> {
        // ⚠️ **先判忙**——真平台在并发满时**拒绝**新提交，而不是排队。
        //
        // 实测原文（并发上限为 1 的 RunningHub 账号，同时提交 2 个任务）：
        //     RUNNINGHUB_REJECTED / TASK_QUEUE_MAXED
        // 这条分支就是"用假平台复现真平台"的那一句。放在脚本结果**之前**：
        // 平台拒绝时不会消耗我们脚本好的结果，与真实行为一致。
        if self.in_flight.lock().unwrap().len() >= self.concurrency_limit {
            self.refusals
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // 与真驱动一致：队列满**是可重试的**（`retryable: true`），不是终态失败。
            // 旧的替身漏了这一格，于是"被拒"在调度器眼里成了失败——真平台上是等一会儿。
            return Err(SafeError {
                retryable: true,
                ..SafeError::provider("RUNNINGHUB_REJECTED", "TASK_QUEUE_MAXED")
            });
        }
        if let Some(error) = self.submit_errors.lock().unwrap().pop() {
            return Err(error);
        }
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| SafeError::internal("FakeProviderDriver 没有配置提交结果"))?;
        // 受理了 ⇒ 占住一个执行位，直到测试调用 `release`。
        if let SubmitOutcome::Accepted(handle) = &outcome {
            self.in_flight
                .lock()
                .unwrap()
                .insert(handle.remote_id.clone());
        }
        Ok(outcome)
    }

    async fn reconcile(&self, _request: ReconcileRequest) -> DomainResult<TaskSnapshot> {
        self.snapshots
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| SafeError::internal("FakeProviderDriver 没有配置对账结果"))
    }

    async fn cancel(&self, _request: CancelRequest) -> DomainResult<CancelOutcome> {
        Ok(CancelOutcome::Cancelled)
    }
}

pub fn completed_artifact_draft(filename: &str) -> ArtifactDraft {
    ArtifactDraft {
        filename: filename.to_string(),
        mime_type: "image/png".to_string(),
        byte_size: 4,
        checksum: "checksum-1".to_string(),
        data: crate::v2::domain::execution::ArtifactData::Bytes(vec![1, 2, 3, 4]),
    }
}

pub fn accepted_outcome() -> SubmitOutcome {
    SubmitOutcome::Accepted(RemoteHandle {
        provider: "comfyui".to_string(),
        remote_id: "prompt-1".to_string(),
    })
}

#[async_trait]
impl LeaseStore for InMemoryRepositories {
    async fn acquire(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = &state.lease {
            if existing.owner != owner && !existing.expires_at.is_expired() {
                return Err(SafeError::conflict(
                    "LEASE_BUSY",
                    "调度租约被其他所有者持有",
                ));
            }
        }
        state.lease = Some(LeaseRecord {
            owner: owner.to_string(),
            token: token.to_string(),
            expires_at: Timestamp::from_millis(Timestamp::now_millis() + ttl_seconds * 1000),
        });
        Ok(())
    }

    async fn renew(&self, owner: &str, token: &str, ttl_seconds: u64) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        match &state.lease {
            Some(existing) if existing.owner == owner && existing.token == token => {
                state.lease = Some(LeaseRecord {
                    owner: owner.to_string(),
                    token: token.to_string(),
                    expires_at: Timestamp::from_millis(
                        Timestamp::now_millis() + ttl_seconds * 1000,
                    ),
                });
                Ok(())
            }
            _ => Err(SafeError::conflict("LEASE_LOST", "调度租约已失效")),
        }
    }

    async fn release(&self, owner: &str, token: &str) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = &state.lease {
            if existing.owner == owner && existing.token == token {
                state.lease = None;
            }
        }
        Ok(())
    }

    async fn current(&self) -> DomainResult<Option<LeaseRecord>> {
        Ok(self.state.lock().unwrap().lease.clone())
    }
}

#[async_trait]
impl crate::v2::ports::repositories::AssetRepository for InMemoryRepositories {
    async fn find_asset(&self, id: AssetId) -> DomainResult<Option<MediaObject>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .assets
            .get(&id)
            .map(|(media, _)| media.clone()))
    }

    async fn read_bytes(&self, id: AssetId) -> DomainResult<Option<Vec<u8>>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .assets
            .get(&id)
            .map(|(_, bytes)| bytes.clone()))
    }

    async fn create_asset(&self, media: MediaObject, bytes: Vec<u8>) -> DomainResult<AssetId> {
        let mut state = self.state.lock().unwrap();
        state.next_asset_id += 1;
        let id = AssetId(state.next_asset_id);
        state.assets.insert(id, (media, bytes));
        Ok(id)
    }
}

#[async_trait]
impl WorkspaceRepository for InMemoryRepositories {
    fn database_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    fn artifact_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// 替身只保证"目标位置会出现一个库文件"，**不保证内容**——一致性由 sqlite 那条真测试钉住
    /// （`workspace_backup::tests` 里用真实文件库跑 `VACUUM INTO`，再把备份打开来读）。
    async fn snapshot_database(&self, target: &std::path::Path) -> DomainResult<()> {
        std::fs::write(target, b"")
            .map_err(|error| SafeError::storage(format!("写入替身快照失败：{error}")))
    }
}

#[async_trait]
impl SettingsRepository for InMemoryRepositories {
    async fn load_settings(&self) -> DomainResult<SettingsRecord> {
        let state = self.state.lock().unwrap();
        Ok(state.settings.clone())
    }

    async fn save_settings(&self, record: &SettingsRecord) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        state.settings = record.clone();
        Ok(())
    }
}

#[async_trait]
impl crate::v2::ports::repositories::ArtifactRepository for InMemoryRepositories {
    async fn list_by_run(
        &self,
        run_id: RunId,
    ) -> DomainResult<Vec<crate::v2::ports::repositories::ArtifactSummary>> {
        let state = self.state.lock().unwrap();
        let mut summaries = state
            .artifacts
            .values()
            .filter(|artifact| artifact.run_id == Some(run_id.0))
            .map(|artifact| crate::v2::ports::repositories::ArtifactSummary {
                id: artifact.id,
                run_id,
                filename: artifact.filename.clone(),
                mime_type: artifact.mime_type.clone(),
                byte_size: artifact.byte_size,
                checksum: artifact.checksum.clone(),
                url: artifact.url.clone(),
                created_at: Timestamp::now(),
            })
            .collect::<Vec<_>>();
        // 必须与 sqlite 对齐：那边是 `ORDER BY id ASC`，而这里是 HashMap——不排序就
        // **顺序随机**。它不会让测试红，只会让断言偶尔红（实测：manifest 的两条产物
        // 第一次跑是 90001、第二次是 90002），这类"绿得看运气"最费时间。
        summaries.sort_by_key(|summary| summary.id);
        Ok(summaries)
    }

    async fn list(
        &self,
        limit: usize,
        cursor: crate::v2::ports::repositories::ArtifactCursor,
    ) -> DomainResult<Vec<crate::v2::ports::repositories::ArtifactSummary>> {
        use crate::v2::ports::repositories::ArtifactCursor;
        let state = self.state.lock().unwrap();
        let mut summaries: Vec<_> = state
            .artifacts
            .values()
            .filter(|artifact| match cursor {
                ArtifactCursor::After(after) => after.is_none_or(|after| artifact.id > after),
                ArtifactCursor::Before(before) => before.is_none_or(|before| artifact.id < before),
            })
            .map(|artifact| crate::v2::ports::repositories::ArtifactSummary {
                id: artifact.id,
                run_id: RunId(artifact.run_id.unwrap_or_default()),
                filename: artifact.filename.clone(),
                mime_type: artifact.mime_type.clone(),
                byte_size: artifact.byte_size,
                checksum: artifact.checksum.clone(),
                url: artifact.url.clone(),
                created_at: Timestamp::now(),
            })
            .collect();
        // 方向必须与 sqlite 对齐（替身与真实现不一致 = 测试测的不是真行为）：
        // `After` 升序、`Before` **降序**，两者都取**前 limit 条**。
        match cursor {
            ArtifactCursor::After(_) => summaries.sort_by_key(|summary| summary.id),
            ArtifactCursor::Before(_) => {
                summaries.sort_by_key(|summary| std::cmp::Reverse(summary.id))
            }
        }
        summaries.truncate(limit);
        Ok(summaries)
    }
}

#[async_trait]
impl TokenStore for InMemoryRepositories {
    async fn create(&self, record: &ActorTokenRecord) -> DomainResult<i64> {
        let mut state = self.state.lock().unwrap();
        let id = state.tokens.len() as i64 + 1;
        let mut record = record.clone();
        record.id = id;
        state.tokens.push(record);
        Ok(id)
    }

    async fn find_by_hash(&self, token_hash: &str) -> DomainResult<Option<ActorTokenRecord>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .tokens
            .iter()
            .find(|record| record.token_hash == token_hash)
            .cloned())
    }

    async fn find_by_id(&self, id: i64) -> DomainResult<Option<ActorTokenRecord>> {
        let state = self.state.lock().unwrap();
        Ok(state.tokens.iter().find(|record| record.id == id).cloned())
    }

    async fn list(&self) -> DomainResult<Vec<ActorTokenRecord>> {
        Ok(self.state.lock().unwrap().tokens.clone())
    }

    async fn revoke(&self, id: i64) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(record) = state.tokens.iter_mut().find(|record| record.id == id) {
            record.revoked = true;
        }
        Ok(())
    }

    async fn set_revoked(&self, id: i64, revoked: bool) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(record) = state.tokens.iter_mut().find(|record| record.id == id) {
            record.revoked = revoked;
        }
        Ok(())
    }
}

#[async_trait]
impl CallbackDeliveryRepository for InMemoryRepositories {
    /// 与 sqlite 版同一判据（**必须对齐**，否则测试测的是另一个世界）：
    /// 有订阅 + 终态 + 在时间窗内 + 还没有投递记录。
    async fn list_operations_awaiting_callback(
        &self,
        since_millis: u64,
        limit: usize,
    ) -> DomainResult<Vec<(OperationId, u64)>> {
        let state = self.state.lock().unwrap();
        let mut awaiting = Vec::new();
        for operation in state.operations.iter() {
            if operation.operation.callback.is_none() {
                continue;
            }
            if !operation.operation.status.is_terminal() {
                continue;
            }
            if operation.operation.updated_at.millis() < since_millis {
                continue;
            }
            if state
                .deliveries
                .iter()
                .any(|delivery| delivery.operation_id == operation.operation.id)
            {
                continue;
            }
            awaiting.push((operation.operation.id, 0));
            if awaiting.len() >= limit {
                break;
            }
        }
        Ok(awaiting)
    }

    async fn create_delivery(&self, delivery: CallbackDeliveryRecord) -> DomainResult<()> {
        self.state.lock().unwrap().deliveries.push(delivery);
        Ok(())
    }

    async fn list_due_deliveries(&self, limit: usize) -> DomainResult<Vec<CallbackDeliveryRecord>> {
        let state = self.state.lock().unwrap();
        let now = Timestamp::now_millis();
        Ok(state
            .deliveries
            .iter()
            .filter(|delivery| !delivery.completed && delivery.next_retry_at.millis() <= now)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn mark_delivery_attempt(
        &self,
        delivery_id: &str,
        success: bool,
        error: Option<String>,
    ) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        let delivery_retry_delay = state.delivery_retry_delay_millis;
        let Some(delivery) = state
            .deliveries
            .iter_mut()
            .find(|delivery| delivery.delivery_id == delivery_id)
        else {
            return Ok(());
        };
        delivery.attempts += 1;
        delivery.last_error = error;
        if success || delivery.attempts >= delivery.max_attempts {
            delivery.completed = true;
        } else {
            let delay = delivery.attempts as u64 * delivery_retry_delay;
            delivery.next_retry_at = Timestamp::from_millis(Timestamp::now_millis() + delay);
        }
        Ok(())
    }
}

#[async_trait]
impl ArtifactStore for InMemoryRepositories {
    async fn store(&self, artifact: ArtifactObject) -> DomainResult<()> {
        self.state
            .lock()
            .unwrap()
            .artifacts
            .insert(artifact.id, artifact);
        Ok(())
    }

    async fn load(&self, id: ArtifactId) -> DomainResult<Option<ArtifactObject>> {
        Ok(self.state.lock().unwrap().artifacts.get(&id).cloned())
    }

    async fn read_bytes(&self, id: ArtifactId) -> DomainResult<Option<Vec<u8>>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .artifacts
            .get(&id)
            .and_then(|artifact| {
                let bytes = artifact.bytes.clone();
                (!bytes.is_empty()).then_some(bytes)
            }))
    }

    async fn cache_bytes(&self, id: ArtifactId, bytes: &[u8]) -> DomainResult<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(artifact) = state.artifacts.get_mut(&id) {
            artifact.bytes = bytes.to_vec();
            if artifact.byte_size == 0 {
                artifact.byte_size = bytes.len() as u64;
            }
        }
        Ok(())
    }

    async fn exists(&self, checksum: &str) -> DomainResult<bool> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .artifacts
            .values()
            .any(|artifact| artifact.checksum == checksum))
    }
}

pub struct FakeCallbackDeliverer {
    outcome: Mutex<DeliveryOutcome>,
    pub delivered: Mutex<Vec<DeliveryRequest>>,
}

impl FakeCallbackDeliverer {
    pub fn accepting() -> Self {
        Self {
            outcome: Mutex::new(DeliveryOutcome::Accepted),
            delivered: Mutex::new(Vec::new()),
        }
    }

    pub fn set_outcome(&self, outcome: DeliveryOutcome) {
        *self.outcome.lock().unwrap() = outcome;
    }

    pub fn delivered_count(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }

    /// 已经"投递"出去的请求（含载荷与签名头）——用来断言投递**内容**，而不只是次数。
    pub fn delivered_requests(&self) -> Vec<DeliveryRequest> {
        self.delivered.lock().unwrap().clone()
    }
}

#[async_trait]
impl CallbackDeliverer for FakeCallbackDeliverer {
    async fn deliver(&self, request: DeliveryRequest) -> DomainResult<DeliveryOutcome> {
        self.delivered.lock().unwrap().push(request);
        Ok(self.outcome.lock().unwrap().clone())
    }
}
