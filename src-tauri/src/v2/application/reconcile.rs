use serde_json::json;
use std::sync::Arc;

use crate::v2::application::credential_reference;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::TaskStatus;
use crate::v2::domain::ids::ArtifactId;
use crate::v2::domain::model::Timestamp;
use crate::v2::domain::operation::{RunStatus, TrackingState, Transition};
use crate::v2::ports::provider::{driver_key, DriverRegistry, ReconcileRequest};
use crate::v2::ports::repositories::{
    AttemptResolution, ConnectionRepository, EventSpec, OperationRepository,
};
use crate::v2::ports::stores::ArtifactStore;
// ⚠️ 后台循环里的日志一律走 `log_line`（写失败不 panic），**不要**用 `eprintln!`：
// stdout/stderr 一断，`eprintln!` 就 panic，而 `run_tick` 报告 panic 时又要写日志 ⇒
// 循环彻底静默停摆。实测过一次：对账循环就是这么死的（任务永远停在 ACCEPTED）。
use crate::v2::logging::{log_failure, log_line, log_line_every};
use std::time::Duration;

pub const POLL_INTERVAL_MILLIS: u64 = 5_000;
/// 轮询失败时的重试间隔：比正常轮询慢，避免对不可用的 provider 每 5 秒打一发并刷爆事件流。
pub const POLL_FAILURE_INTERVAL_MILLIS: u64 = 30_000;
// 「默认跟踪死线」那个数字的所有者是 `domain::connection_options`
// （原来这里独立声明了一份，注释自己写着"与 scheduler 同值"——四处各一份的那种）。

#[derive(Debug, Clone, Default)]
pub struct ReconcileResult {
    pub checked: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub updated: usize,
    pub polling_errors: usize,
    pub paused: usize,
}

pub struct ReconcileService {
    pub repositories: Arc<dyn OperationRepository>,
    /// 解析凭据引用要读被引用连接（见 `credential_reference`）。
    pub connections: Arc<dyn ConnectionRepository>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub drivers: Arc<DriverRegistry>,
}

impl ReconcileService {
    pub async fn tick(&self, limit: usize) -> DomainResult<ReconcileResult> {
        let mut result = ReconcileResult::default();
        let active_runs = self.repositories.list_active_runs(limit).await?;
        /*
         * 心跳：**没事时每分钟一行，有事时每拍一行**。
         *
         * 原来这里无条件每 5 秒写一行，实测占了整个日志文件的 **98%**——那个文件的存在意义是
         * "出事以后（用户把目录交给他的 agent）能看出发生了什么"，被心跳淹掉就等于没有。
         * 反过来，有 active run 时"正在跟几个"本身就是要看的信息，所以那时候照旧每拍都写。
         */
        let heartbeat = format!(
            "[renderloft-reconcile] tick active_runs={}",
            active_runs.len()
        );
        if active_runs.is_empty() {
            log_line_every("reconcile-idle", Duration::from_secs(60), &heartbeat);
        } else {
            log_line(&heartbeat);
        }
        for active in active_runs {
            result.checked += 1;
            let run_id = active.run.id;
            // 任何 active run 都必须有对账死线；缺失（历史脏数据/被旧版清空）时当场补设，
            // 否则 deadline_expired 永远为假，run 会永久卡在对账循环里无法被自动终结。
            let deadline = match active.run.tracking_deadline.clone() {
                Some(deadline) => deadline,
                None => {
                    let deadline = Timestamp::from_millis(
                        Timestamp::now_millis()
                            + crate::v2::domain::connection_options::DEFAULT_TRACKING_DEADLINE_MILLIS,
                    );
                    self.repositories
                        .set_tracking(
                            run_id,
                            TrackingState::Active,
                            active.run.next_poll_at.clone(),
                            Some(deadline.clone()),
                        )
                        .await?;
                    deadline
                }
            };
            let deadline_expired = deadline.is_expired();
            let key = driver_key(&active.vendor, &active.protocol);
            let driver = match self.drivers.get(&key) {
                Ok(driver) => driver,
                Err(_) => {
                    result.polling_errors += 1;
                    continue;
                }
            };
            let Some(handle) = active.attempt.remote_handle.clone() else {
                // uncertain 且无对账句柄：无法追踪远端状态。已超对账期限则标记失败
                //（用户可重试），否则续等，避免立即误杀仍在传播中的提交。
                if deadline_expired {
                    let error =
                        SafeError::provider("TRACKING_UNRESOLVED", "提交状态无法确认且无对账句柄");
                    // 出事了：写一行固定形状的（用户在日志里能查到"这条为什么失败"）。
                    log_failure(
                        "run",
                        &format!("run={} op={}", run_id.0, active.run.operation_id.0),
                        &error.code,
                        &error.message,
                    );
                    self.repositories
                        .set_run_error(run_id, Some(error.clone()))
                        .await?;
                    self.repositories
                        .transition_run(
                            run_id,
                            crate::v2::domain::operation::Transition::ToFailed,
                            vec![crate::v2::ports::repositories::EventSpec {
                                event_type: "run.tracking_unresolved".to_string(),
                                run_id: Some(run_id),
                                safe_payload: json!({
                                    "reason": error.message,
                                    "error": error,
                                }),
                            }],
                        )
                        .await?;
                    result.failed += 1;
                    continue;
                }
                let next = Timestamp::from_millis(Timestamp::now_millis() + POLL_INTERVAL_MILLIS);
                self.repositories
                    .set_tracking(
                        run_id,
                        TrackingState::Active,
                        Some(next),
                        Some(deadline.clone()),
                    )
                    .await?;
                continue;
            };
            if deadline_expired {
                // 对账期限耗尽后必须结束本地 Run，否则 Operation 会永久保持 RUNNING，
                // 而 paused tracking 又不会再次进入 active 查询列表。
                let error =
                    SafeError::provider("TRACKING_EXHAUSTED", "远端任务在对账期限内未进入终态");
                log_failure(
                    "run",
                    &format!(
                        "run={} op={} conn={}",
                        run_id.0,
                        active.run.operation_id.0,
                        active.connection_revision.connection_id.0
                    ),
                    &error.code,
                    &error.message,
                );
                self.repositories
                    .set_run_error(run_id, Some(error.clone()))
                    .await?;
                self.repositories
                    .transition_run(
                        run_id,
                        Transition::ToFailed,
                        vec![EventSpec {
                            event_type: "run.tracking_exhausted".to_string(),
                            run_id: Some(run_id),
                            safe_payload: json!({
                                "reason": error.message,
                                "error": error,
                            }),
                        }],
                    )
                    .await?;
                result.failed += 1;
                continue;
            }
            // 凭据引用：对账也要密钥（rh-proxy 的地址里带 key）。坏掉的引用**不可自愈**，
            // 所以当场把 run 判死——否则它会一直 poll_failed 到对账期限耗尽，
            // 用户看到的是"跑了好久然后失败"，而不是"引用的连接已被删除"。
            let connection_revision = match credential_reference::resolve(
                self.connections.as_ref(),
                active.connection_revision.clone(),
            )
            .await
            {
                Ok(revision) => revision,
                Err(error) if credential_reference::is_reference_error(&error) => {
                    self.repositories
                        .set_run_error(run_id, Some(error.clone()))
                        .await?;
                    self.repositories
                        .transition_run(
                            run_id,
                            Transition::ToFailed,
                            vec![EventSpec {
                                event_type: "run.reconcile_unavailable".to_string(),
                                run_id: Some(run_id),
                                safe_payload: json!({
                                    "reason": error.message,
                                    "error": error,
                                }),
                            }],
                        )
                        .await?;
                    result.failed += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let snapshot = driver
                .reconcile(ReconcileRequest {
                    connection_revision,
                    remote_handle: handle.clone(),
                })
                .await;
            log_line(&format!(
                "[renderloft-reconcile] run={} driver.reconcile ok={} remote_id={}",
                run_id.0,
                snapshot.is_ok(),
                handle.remote_id
            ));
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    log_line(&format!(
                        "[renderloft-reconcile] run={} reconcile err code={} msg={}",
                        run_id.0, error.code, error.message
                    ));
                    result.polling_errors += 1;
                    self.repositories
                        .append_event(
                            run_id,
                            "run.poll_failed".to_string(),
                            json!({
                                "code": error.code,
                                "retryable": error.retryable,
                            }),
                        )
                        .await?;
                    let next = Timestamp::from_millis(
                        Timestamp::now_millis() + POLL_FAILURE_INTERVAL_MILLIS,
                    );
                    // 保留对账期限：一旦清成 None，deadline_expired 永远为假，
                    // 该 run 将永久卡 Active 而无法被 reconcile 自动终结。
                    self.repositories
                        .set_tracking(
                            run_id,
                            TrackingState::Active,
                            Some(next),
                            Some(deadline.clone()),
                        )
                        .await?;
                    continue;
                }
            };
            log_line(&format!(
                "[renderloft-reconcile] run={} status={:?}",
                run_id.0, snapshot.status
            ));
            match snapshot.status {
                TaskStatus::Succeeded => {
                    // 与 scheduler 同一道保证（两条落库路径都要有，否则"从哪条路完成"会决定
                    // 库里的名字）。见 `ArtifactDraft::make_filenames_unique`。
                    let mut artifacts = snapshot.results.clone();
                    crate::v2::domain::execution::ArtifactDraft::make_filenames_unique(
                        &mut artifacts,
                    );
                    for (index, artifact) in artifacts.iter().enumerate() {
                        let id = ArtifactId(run_id.0 * 1000 + index as u64);
                        // 与 scheduler 同一条命名规则（两条落库路径都要有，否则"从哪条路完成"
                        // 会决定库里的名字）。见 `ArtifactDraft::compose_filename`。
                        let filename =
                            crate::v2::domain::execution::ArtifactDraft::compose_filename(
                                &active.recipe_name,
                                &active.run.client_run_id,
                                id,
                                &artifact.filename,
                            );
                        let (url, bytes) = match &artifact.data {
                            crate::v2::domain::execution::ArtifactData::Url { url, .. } => {
                                // 中转站定位：有远端 URL 就不下载字节入库，预览时按需拉取。
                                (Some(url.clone()), Vec::new())
                            }
                            crate::v2::domain::execution::ArtifactData::Bytes(bytes) => {
                                // 无远端 URL（ComfyUI / OpenAI）时字节写本地缓存文件。
                                (None, bytes.clone())
                            }
                        };
                        self.artifacts
                            .store(crate::v2::ports::stores::ArtifactObject {
                                id,
                                run_id: Some(run_id.0),
                                filename,
                                mime_type: artifact.mime_type.clone(),
                                byte_size: artifact.byte_size,
                                checksum: artifact.checksum.clone(),
                                storage_key: format!("{id}"),
                                url,
                                bytes,
                            })
                            .await?;
                    }
                    self.repositories
                        .complete_attempt(
                            run_id,
                            &active.attempt.fencing_token,
                            AttemptResolution::Succeeded {
                                remote_handle: Some(handle),
                            },
                        )
                        .await?;
                    self.repositories
                        .transition_run(
                            run_id,
                            Transition::ToSucceeded,
                            vec![EventSpec {
                                event_type: "run.succeeded".to_string(),
                                run_id: Some(run_id),
                                safe_payload: json!({
                                    "artifactCount": snapshot.results.len(),
                                    // 平台报的用量原样带上（`null` = 平台没说）。
                                    "usage": snapshot.usage,
                                }),
                            }],
                        )
                        .await?;
                    // 平台报的用量**原样落库**：`None` = "平台没说"，与 `0`（这次免费）不是一回事。
                    // 写不进去不能反过来毁掉一个已经成功的任务，但**绝不能静默**——日志里留痕。
                    if let Err(error) = self
                        .repositories
                        .set_run_usage(run_id, snapshot.usage.clone())
                        .await
                    {
                        log_line(&format!(
                            "[renderloft-reconcile] run={} 用量落库失败：{error}",
                            run_id.0
                        ));
                    }
                    result.succeeded += 1;
                }
                TaskStatus::Failed => {
                    let error = snapshot.error.unwrap_or_else(|| {
                        SafeError::provider("PROVIDER_TASK_FAILED", "远端任务失败")
                    });
                    self.repositories
                        .complete_attempt(
                            run_id,
                            &active.attempt.fencing_token,
                            AttemptResolution::Failed {
                                error: error.clone(),
                                retryable: false,
                            },
                        )
                        .await?;
                    self.repositories
                        .set_run_error(run_id, Some(error.clone()))
                        .await?;
                    self.repositories
                        .transition_run(
                            run_id,
                            Transition::ToFailed,
                            vec![EventSpec {
                                event_type: "run.failed".to_string(),
                                run_id: Some(run_id),
                                safe_payload: json!({
                                    "reason": error.message,
                                    "error": error,
                                }),
                            }],
                        )
                        .await?;
                    result.failed += 1;
                }
                _ => {
                    if active.run.status == RunStatus::Accepted {
                        self.repositories
                            .transition_run(
                                run_id,
                                Transition::ToRunning,
                                vec![EventSpec {
                                    event_type: "run.running".to_string(),
                                    run_id: Some(run_id),
                                    safe_payload: json!({}),
                                }],
                            )
                            .await?;
                    }
                    let next =
                        Timestamp::from_millis(Timestamp::now_millis() + POLL_INTERVAL_MILLIS);
                    // "仍在运行"也必须保留对账死线：此前这里传 None，第一次成功轮询
                    // 就把死线清空，之后一旦 provider 不可达，run 永久卡在对账循环。
                    self.repositories
                        .set_tracking(run_id, TrackingState::Active, Some(next), Some(deadline))
                        .await?;
                    result.updated += 1;
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
    use crate::v2::domain::execution::ProviderDescriptor;
    use crate::v2::domain::ids::{
        ClientId, ConnectionId, ConnectionRevisionId, OperationId, RecipeId, RecipeRevisionId,
        RunId,
    };
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility,
    };
    use crate::v2::domain::operation::{AttemptStatus, RunStatus};
    use crate::v2::infrastructure::fakes::{
        accepted_outcome, completed_artifact_draft, FakeProviderDriver, InMemoryRepositories,
    };
    use crate::v2::ports::provider::driver_key;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn actor() -> ActorRef {
        ActorRef {
            id: "actor-1".to_string(),
            kind: ActorKind::Mcp,
            client_id: ClientId("client-1".to_string()),
            scopes: vec![Scope::OperationsExecute],
            label: None,
        }
    }

    fn seed_environment(repositories: &InMemoryRepositories) {
        let connection = Connection {
            id: ConnectionId(1),
            vendor: "openai".to_string(),
            protocol: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            current_revision_id: ConnectionRevisionId(1),
            status: crate::v2::domain::model::ConnectionStatus::Active,
        };
        let connection_revision = ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(1),
            config: Default::default(),
            secret_bindings: vec![],
            effective_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures {
                remote_reconciliation: true,
                synchronous_result: true,
                ..Default::default()
            },
            created_at: Timestamp::now(),
        };
        repositories.seed_connection(connection, vec![connection_revision]);
        let recipe = Recipe {
            id: RecipeId(1),
            name: "Image Recipe".to_string(),
            description: "text to image".to_string(),
            labels: vec![],
            capability: "text-to-image".to_string(),
            current_revision_id: RecipeRevisionId(1),
            visibility: RecipeVisibility::Private,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(1),
            recipe_id: RecipeId(1),
            revision: 1,
            input_schema: json!({
                "type": "object",
                "required": ["prompt"],
                "properties": { "prompt": { "type": "string" } }
            }),
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: "openai-images".to_string(),
                provider_config: Default::default(),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: ConnectionOverride::Denied,
            },
            defaults: Default::default(),
            source: RecipeSource::Embedded,
            created_at: Timestamp::now(),
        };
        repositories.seed_recipe(recipe, vec![revision]);
    }

    async fn create_operation(repositories: &InMemoryRepositories) -> (OperationId, RunId) {
        use crate::v2::application::create_operation::{
            CreateOperation, CreateOperationRequest, Repositories, RunRequest,
        };
        use crate::v2::domain::ids::IdempotencyKey;
        use crate::v2::infrastructure::schema::BasicSchemaValidator;
        let use_case = CreateOperation::new(
            Repositories {
                recipes: Box::new(repositories.clone()),
                connections: Box::new(repositories.clone()),
                operations: Box::new(repositories.clone()),
                assets: Box::new(repositories.clone()),
            },
            Box::new(crate::v2::infrastructure::fakes::PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        );
        let persisted = use_case
            .execute(CreateOperationRequest {
                actor: actor(),
                origin: "mcp".to_string(),
                idempotency_key: IdempotencyKey("reconcile-test-key".to_string()),
                runs: vec![RunRequest {
                    client_run_id: "image-1".to_string(),
                    recipe_revision_id: RecipeRevisionId(1),
                    connection_id: None,
                    inputs: BTreeMap::from([("prompt".to_string(), json!("a red house"))]),
                    assets: vec![],
                }],
                metadata: Default::default(),
                callback: None,
            })
            .await
            .expect("创建应成功");
        (persisted.operation.id, persisted.runs[0].id)
    }

    /// 把连接 #1 的 config 指向某条"密钥宿主"连接（或一个不存在的 id，用来造坏引用）。
    fn point_connection_at(
        repositories: &InMemoryRepositories,
        connection_id: u64,
        source_id: u64,
    ) {
        repositories.seed_connection(
            Connection {
                id: ConnectionId(connection_id),
                vendor: "openai".to_string(),
                protocol: "openai-images".to_string(),
                display_name: "OpenAI Images".to_string(),
                current_revision_id: ConnectionRevisionId(connection_id),
                status: crate::v2::domain::model::ConnectionStatus::Active,
            },
            vec![ConnectionRevision {
                id: ConnectionRevisionId(connection_id),
                connection_id: ConnectionId(connection_id),
                config: BTreeMap::from([(
                    crate::v2::application::credential_reference::SOURCE_CONNECTION_ID.to_string(),
                    json!(source_id),
                )]),
                secret_bindings: vec![],
                effective_capabilities: vec!["text-to-image".to_string()],
                lifecycle_features: LifecycleFeatures {
                    remote_reconciliation: true,
                    synchronous_result: true,
                    ..Default::default()
                },
                created_at: Timestamp::now(),
            }],
        );
    }

    fn driver_with_snapshot(
        snapshot: crate::v2::domain::execution::TaskSnapshot,
    ) -> Arc<FakeProviderDriver> {
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_snapshot(snapshot);
        driver
    }

    async fn accepted_run(repositories: &InMemoryRepositories) -> (OperationId, RunId) {
        let (operation_id, run_id) = create_operation(repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_outcome(accepted_outcome());
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        let scheduler = crate::v2::application::scheduler::SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        scheduler.tick().await.expect("调度应成功");
        (operation_id, run_id)
    }

    fn reconcile_service(
        repositories: InMemoryRepositories,
        driver: Arc<FakeProviderDriver>,
    ) -> ReconcileService {
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        ReconcileService {
            repositories: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories),
            drivers: Arc::new(registry),
        }
    }

    /// 引用坏了（被引用的连接被删）⇒ 对账**当场判死**，不退化成"下一轮再试"。
    ///
    /// 负对照：这条断言的是 `result.failed`（不是 `polling_errors`）。把 reconcile 里那段
    /// `is_reference_error` 分支删掉，run 会一直 poll_failed 到对账期限耗尽——
    /// 用户看到的是"跑了好久然后失败"，而不是"引用的连接已被删除"。
    #[tokio::test]
    async fn a_broken_reference_fails_reconciliation_instead_of_polling() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        // 先正常受理（提交时引用还是好的），再删掉被引用的那条连接。
        let (operation_id, run_id) = accepted_run(&repositories).await;
        point_connection_at(&repositories, 1, 99);

        let service = reconcile_service(
            repositories.clone(),
            driver_with_snapshot(crate::v2::domain::execution::TaskSnapshot {
                status: crate::v2::domain::execution::TaskStatus::Running,
                results: vec![],
                error: None,
                usage: None,
            }),
        );
        let result = service.tick(10).await.expect("对账应成功");

        assert_eq!(result.failed, 1, "坏引用应当场判失败");
        assert_eq!(
            result.polling_errors, 0,
            "坏引用不是网络抖动：留着重试只会拖到对账期限耗尽"
        );
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        let run = persisted.runs.iter().find(|run| run.id == run_id).unwrap();
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(
            run.error.as_ref().map(|error| error.code.as_str()),
            Some("CONNECTION_SOURCE_NOT_FOUND")
        );
    }

    #[tokio::test]
    async fn reconcile_succeeded_marks_run_succeeded_with_artifacts() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let (_, run_id) = accepted_run(&repositories).await;
        let driver = driver_with_snapshot(crate::v2::domain::execution::TaskSnapshot {
            status: crate::v2::domain::execution::TaskStatus::Succeeded,
            results: vec![completed_artifact_draft("result.png")],
            error: None,
            usage: None,
        });
        let service = reconcile_service(repositories.clone(), driver);
        let result = service.tick(10).await.expect("对账应成功");
        assert_eq!(result.succeeded, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        let run = persisted.runs.iter().find(|run| run.id == run_id).unwrap();
        assert_eq!(run.status, RunStatus::Succeeded);
        assert_eq!(repositories.artifacts().len(), 1);
        let attempt = repositories.attempts()[0].clone();
        assert_eq!(attempt.status, AttemptStatus::Succeeded);
    }

    /// 产物名在**落库那一刻**就定下来，而且是 `{应用名}-{artifactId}.{ext}`。
    ///
    /// **负对照靶子**：停在 `compose_filename` 上的单测钉不住"落库那条路真的调了它"——
    /// 把这里那行调用删掉，辅助函数的测试照样绿。所以这条断言的是**库里的名字**。
    ///
    /// 背景：RunningHub 按输出节点命名（`870.png`，同一个节点吐多张图就同名），模型 API 是
    /// `output.png`，两者都不可辨认。`artifactId` 天生唯一（`run_id*1000+index`）⇒
    /// 名字里带上它就顺带把撞名从根上消掉了。
    #[tokio::test]
    async fn reconcile_persists_artifact_names_composed_from_the_recipe() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        accepted_run(&repositories).await;
        let driver = driver_with_snapshot(crate::v2::domain::execution::TaskSnapshot {
            status: crate::v2::domain::execution::TaskStatus::Succeeded,
            // 平台侧的形状：同一个输出节点的两张图，名字一模一样。
            results: vec![
                completed_artifact_draft("870.png"),
                completed_artifact_draft("870.png"),
            ],
            error: None,
            usage: None,
        });
        let service = reconcile_service(repositories.clone(), driver);
        service.tick(10).await.expect("对账应成功");

        let mut names: Vec<String> = repositories
            .artifacts()
            .into_iter()
            .map(|artifact| artifact.filename)
            .collect();
        names.sort();
        // 名字带**调用方给的 run 标签**（`image-1`）：这是对账那条路也把标签传下去的证据
        // ——两条落库路径（直接完成 / 事后对账）给出同一个名字，否则"从哪条路完成"会改文件名。
        assert_eq!(
            names,
            ["image-1-Image Recipe-1000.png", "image-1-Image Recipe-1001.png"],
            "名字应当是「run 标签-应用名-artifactId」，既认得出来、也不可能撞"
        );
    }

    #[tokio::test]
    async fn reconcile_failed_marks_run_failed() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        accepted_run(&repositories).await;
        let driver = driver_with_snapshot(crate::v2::domain::execution::TaskSnapshot {
            status: crate::v2::domain::execution::TaskStatus::Failed,
            results: vec![],
            error: Some(crate::v2::domain::errors::SafeError::provider(
                "PROVIDER_TASK_FAILED",
                "生成失败",
            )),
            usage: None,
        });
        let service = reconcile_service(repositories.clone(), driver);
        let result = service.tick(10).await.expect("对账应成功");
        assert_eq!(result.failed, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Failed);
        assert!(persisted.runs[0].error.is_some());
        assert_eq!(
            persisted.runs[0].error.as_ref().unwrap().code,
            "PROVIDER_TASK_FAILED"
        );
    }

    #[tokio::test]
    async fn reconcile_running_preserves_run_and_schedules_next_poll() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        accepted_run(&repositories).await;
        let driver = driver_with_snapshot(crate::v2::domain::execution::TaskSnapshot {
            status: crate::v2::domain::execution::TaskStatus::Running,
            results: vec![],
            error: None,
            usage: None,
        });
        let service = reconcile_service(repositories.clone(), driver);
        let result = service.tick(10).await.expect("对账应成功");
        assert_eq!(result.updated, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Running);
        assert!(persisted.runs[0].next_poll_at.is_some());
    }

    #[tokio::test]
    async fn reconcile_error_is_isolated_and_preserves_run() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        accepted_run(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        // 不配置 snapshot → reconcile 返回 internal 错误
        let service = reconcile_service(repositories.clone(), driver);
        let result = service.tick(10).await.expect("对账应成功");
        assert_eq!(result.polling_errors, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Accepted);
    }

    #[tokio::test]
    async fn expired_deadline_fails_run_and_operation() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let (_, run_id) = accepted_run(&repositories).await;
        repositories
            .set_tracking(
                run_id,
                TrackingState::Active,
                None,
                Some(Timestamp::from_millis(0)),
            )
            .await
            .unwrap();
        let driver = driver_with_snapshot(crate::v2::domain::execution::TaskSnapshot {
            status: crate::v2::domain::execution::TaskStatus::Running,
            results: vec![],
            error: None,
            usage: None,
        });
        let service = reconcile_service(repositories.clone(), driver);
        let result = service.tick(10).await.expect("对账应成功");
        assert_eq!(result.failed, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        let run = persisted.runs.iter().find(|run| run.id == run_id).unwrap();
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.tracking_state, TrackingState::Exhausted);
        assert_eq!(repositories.list_active_runs(10).await.unwrap().len(), 0);
    }
}
