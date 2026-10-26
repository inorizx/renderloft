use serde_json::{json, Value};
use std::sync::Arc;

use crate::v2::application::credential_reference;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::CancelOutcome;
use crate::v2::domain::ids::{OperationId, RunId};
use crate::v2::domain::operation::Transition;
use crate::v2::ports::provider::{driver_key, CancelRequest, DriverRegistry};
use crate::v2::ports::repositories::{ConnectionRepository, EventSpec, OperationRepository};

#[derive(Debug, Clone, Default)]
pub struct CancelResult {
    pub operation_id: OperationId,
    pub cancelled: Vec<RunId>,
    pub already_terminal: Vec<RunId>,
    pub pending: Vec<RunId>,
    pub remote_may_continue: Vec<RunId>,
}

pub struct CancelOperation {
    pub repositories: Arc<dyn OperationRepository>,
    /// 解析凭据引用要读被引用连接（见 `credential_reference`）。
    pub connections: Arc<dyn ConnectionRepository>,
    pub drivers: Arc<DriverRegistry>,
}

impl CancelOperation {
    pub async fn execute(&self, operation_id: OperationId) -> DomainResult<CancelResult> {
        let persisted = self
            .repositories
            .find_operation(operation_id)
            .await?
            .ok_or_else(|| SafeError::not_found("OPERATION_NOT_FOUND", "Operation 不存在"))?;
        let mut result = CancelResult {
            operation_id,
            ..Default::default()
        };
        for run in persisted.runs {
            if run.status.is_terminal() {
                result.already_terminal.push(run.id);
                continue;
            }
            let handle = self
                .repositories
                .find_latest_attempt(run.id)
                .await?
                .and_then(|attempt| attempt.remote_handle);
            if let Some(handle) = handle {
                if let CancelOutcome::RemoteMayContinue = self.remote_cancel(&run, &handle).await? {
                    result.remote_may_continue.push(run.id);
                }
            } else if matches!(
                run.status,
                crate::v2::domain::operation::RunStatus::Submitting
                    | crate::v2::domain::operation::RunStatus::SubmissionUncertain
            ) {
                // ⚠️ **没有远端句柄 ≠ 远端没有这个任务**（2026-10-18 修）。
                //
                // 这两个状态的含义正是"提交发出去了但结果不明"（`SUBMITTING` 是租约还在，
                // `SUBMISSION_UNCERTAIN` 是已经确认不了），而句柄只有在**拿到平台回执**时才有
                // ——所以恰恰是这两种情况**没有**句柄。此前它们什么都不报，于是：
                // `cancelled` 里躺着一条，用户读成"平台上一定没有残留"，而平台上那份可能正在跑
                // 并会照常计费。取消不了就如实说取消不了（与"有句柄但远端没确认"同桶）。
                result.remote_may_continue.push(run.id);
            }
            // Fencing：先把非终态 attempt 标记 CANCELLED，scheduler 在途的
            // complete_attempt 会因终态检查短路，不再把已取消任务标为 SUCCEEDED。
            let _ = self.repositories.fence_attempt(run.id).await;
            match self
                .repositories
                .transition_run(
                    run.id,
                    Transition::ToCancelled,
                    vec![EventSpec {
                        event_type: "run.cancelled".to_string(),
                        run_id: Some(run.id),
                        safe_payload: json!({}),
                    }],
                )
                .await
            {
                Ok(_) => result.cancelled.push(run.id),
                Err(_) => result.pending.push(run.id),
            }
        }
        Ok(result)
    }

    async fn remote_cancel(
        &self,
        run: &crate::v2::domain::operation::Run,
        handle: &crate::v2::domain::execution::RemoteHandle,
    ) -> DomainResult<CancelOutcome> {
        let active = self
            .repositories
            .list_active_runs(100)
            .await?
            .into_iter()
            .find(|active| active.run.id == run.id);
        let Some(active) = active else {
            return Ok(CancelOutcome::Cancelled);
        };
        let key = driver_key(&active.vendor, &active.protocol);
        let driver = match self.drivers.get(&key) {
            Ok(driver) => driver,
            Err(_) => return Ok(CancelOutcome::RemoteMayContinue),
        };
        if !active
            .connection_revision
            .lifecycle_features
            .supports_remote_cancel()
        {
            return Ok(CancelOutcome::RemoteMayContinue);
        }
        // 凭据引用：远端取消也要密钥（rh-proxy 的地址里带 key）。引用坏了 ⇒ 够不着远端，
        // 与"驱动没注册"同一个口径：本地取消已经生效，如实报"远端可能还在跑"。
        let Ok(connection_revision) = credential_reference::resolve(
            self.connections.as_ref(),
            active.connection_revision.clone(),
        )
        .await
        else {
            return Ok(CancelOutcome::RemoteMayContinue);
        };
        driver
            .cancel(CancelRequest {
                connection_revision,
                remote_handle: handle.clone(),
            })
            .await
            .map(|outcome| match outcome {
                CancelOutcome::Cancelled => CancelOutcome::Cancelled,
                CancelOutcome::AlreadyTerminal => CancelOutcome::AlreadyTerminal,
                CancelOutcome::RemoteMayContinue => CancelOutcome::RemoteMayContinue,
            })
            .or(Ok(CancelOutcome::RemoteMayContinue))
    }
}

pub fn cancel_response(result: &CancelResult) -> Value {
    json!({
        "operationId": result.operation_id.0,
        "cancelled": result.cancelled.iter().map(|id| id.0).collect::<Vec<_>>(),
        "alreadyTerminal": result.already_terminal.iter().map(|id| id.0).collect::<Vec<_>>(),
        "pending": result.pending.iter().map(|id| id.0).collect::<Vec<_>>(),
        "remoteMayContinue": result.remote_may_continue.iter().map(|id| id.0).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
    use crate::v2::domain::execution::ProviderDescriptor;
    use crate::v2::domain::ids::{
        AttemptId, ClientId, ConnectionId, ConnectionRevisionId, OperationId, RecipeId,
        RecipeRevisionId,
    };
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility, Timestamp,
    };
    use crate::v2::domain::operation::AttemptStatus;
    use crate::v2::domain::operation::RunAttempt;
    use crate::v2::domain::operation::RunStatus;
    use crate::v2::infrastructure::fakes::{
        accepted_outcome, FakeProviderDriver, InMemoryRepositories,
    };
    use crate::v2::ports::provider::driver_key;
    use crate::v2::ports::repositories::AttemptResolution;
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
                remote_cancel: true,
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

    async fn create_operation(repositories: &InMemoryRepositories) -> OperationId {
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
                idempotency_key: IdempotencyKey("cancel-test-key".to_string()),
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
        persisted.operation.id
    }

    fn driver(
        outcomes: Vec<crate::v2::domain::execution::SubmitOutcome>,
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
        for outcome in outcomes {
            driver.push_outcome(outcome);
        }
        driver
    }

    fn cancel_service(
        repositories: InMemoryRepositories,
        driver: Arc<FakeProviderDriver>,
    ) -> CancelOperation {
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        CancelOperation {
            connections: Arc::new(repositories.clone()),
            repositories: Arc::new(repositories),
            drivers: Arc::new(registry),
        }
    }

    #[tokio::test]
    async fn cancels_queued_run_locally() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation(&repositories).await;
        let service = cancel_service(repositories.clone(), driver(vec![]));
        let result = service.execute(operation_id).await.expect("取消应成功");
        assert_eq!(result.cancelled.len(), 1);
        assert!(result.remote_may_continue.is_empty());
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Cancelled);
        assert_eq!(
            persisted.operation.status,
            crate::v2::domain::operation::OperationStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn cancels_accepted_run_with_remote_cancel() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation(&repositories).await;
        // 先提交为 ACCEPTED
        let scheduler_driver = driver(vec![accepted_outcome()]);
        {
            let mut registry = DriverRegistry::new();
            registry.register(driver_key("openai", "openai-images"), scheduler_driver);
            let scheduler = crate::v2::application::scheduler::SchedulerService {
                capacity: Default::default(),
                owner: "test-scheduler".to_string(),
                repositories: Arc::new(repositories.clone()),
                leases: Arc::new(repositories.clone()),
                connections: Arc::new(repositories.clone()),
                assets: Arc::new(repositories.clone()),
                artifacts: Arc::new(repositories.clone()),
                drivers: Arc::new(registry),
            };
            scheduler.tick().await.expect("调度应成功");
        }
        let cancel_driver = driver(vec![]);
        let service = cancel_service(repositories.clone(), cancel_driver);
        let result = service.execute(operation_id).await.expect("取消应成功");
        assert_eq!(result.cancelled.len(), 1);
        assert!(result.remote_may_continue.is_empty());
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancel_fences_attempt_so_scheduler_cannot_succeed_it() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation(&repositories).await;
        // 手动构造 in-flight 状态：run 提交中（SUBMITTING attempt + ACCEPTED run）。
        let run = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap()
            .runs
            .into_iter()
            .next()
            .unwrap();
        for transition in [
            crate::v2::domain::operation::Transition::ToPreparing,
            crate::v2::domain::operation::Transition::ToSubmitting,
        ] {
            repositories
                .transition_run(run.id, transition, Vec::new())
                .await
                .expect("推进 run");
        }
        repositories
            .append_attempt(&RunAttempt {
                id: AttemptId(1),
                run_id: run.id,
                number: 1,
                status: AttemptStatus::Submitting,
                request_fingerprint: "fp".to_string(),
                fencing_token: "in-flight-token".to_string(),
                remote_handle: None,
                error: None,
                started_at: Timestamp::now(),
                completed_at: None,
            })
            .await
            .expect("写入 attempt");
        repositories
            .transition_run(
                run.id,
                crate::v2::domain::operation::Transition::ToAccepted,
                Vec::new(),
            )
            .await
            .expect("提交为 ACCEPTED");

        // 取消交付 → run 与 attempt 均被标记 CANCELLED。
        let cancel_service = cancel_service(repositories.clone(), driver(vec![]));
        cancel_service
            .execute(operation_id)
            .await
            .expect("取消应成功");
        // 模拟 scheduler 的 in-flight complete_attempt 带着旧 fencing token 返回：
        // 终态检查必须短路，返回 None，绝不把已取消任务标为 SUCCEEDED。
        let attempt = repositories
            .find_latest_attempt(run.id)
            .await
            .unwrap()
            .expect("attempt 存在");
        assert_eq!(attempt.status, AttemptStatus::Cancelled);
        let completed = repositories
            .complete_attempt(
                run.id,
                &attempt.fencing_token,
                AttemptResolution::Succeeded {
                    remote_handle: None,
                },
            )
            .await
            .expect("complete_attempt 应成功返回");
        assert!(
            completed.is_none(),
            "被 fencing 的 attempt 不允许被标为 SUCCEEDED"
        );
    }

    #[tokio::test]
    async fn accepted_run_without_cancel_capability_reports_remote_may_continue() {
        let repositories = InMemoryRepositories::new();
        // 使用不支持 remote cancel 的 connection
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
                remote_cancel: false,
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
        let operation_id = create_operation(&repositories).await;
        // 先提交为 ACCEPTED（调度不依赖 remote_cancel 能力）
        let scheduler_driver = driver(vec![accepted_outcome()]);
        {
            let mut registry = DriverRegistry::new();
            registry.register(driver_key("openai", "openai-images"), scheduler_driver);
            let scheduler = crate::v2::application::scheduler::SchedulerService {
                capacity: Default::default(),
                owner: "test-scheduler".to_string(),
                repositories: Arc::new(repositories.clone()),
                leases: Arc::new(repositories.clone()),
                connections: Arc::new(repositories.clone()),
                assets: Arc::new(repositories.clone()),
                artifacts: Arc::new(repositories.clone()),
                drivers: Arc::new(registry),
            };
            scheduler.tick().await.expect("调度应成功");
        }
        let service = cancel_service(repositories.clone(), driver(vec![]));
        let result = service.execute(operation_id).await.expect("取消应成功");
        assert_eq!(result.cancelled.len(), 1);
        assert_eq!(result.remote_may_continue.len(), 1);
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Cancelled);
    }

    #[tokio::test]
    async fn terminal_run_is_reported_as_already_terminal() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation(&repositories).await;
        let service = cancel_service(repositories.clone(), driver(vec![]));
        service
            .execute(operation_id)
            .await
            .expect("第一次取消应成功");
        let result = service.execute(operation_id).await.expect("再次取消应成功");
        assert!(result.cancelled.is_empty());
        assert_eq!(result.already_terminal.len(), 1);
    }

    /// **没有远端句柄的不确定 run 也必须进 `remoteMayContinue`**（2026-10-18 修）。
    ///
    /// 为什么（这是钱）：`SUBMITTING` / `SUBMISSION_UNCERTAIN` 的含义正是"提交发出去了但结果不明"
    /// ——而远端句柄只有**拿到平台回执**时才会有，所以恰恰是这两种情况**没有**句柄。此前它们什么
    /// 都不报，于是 `cancelled` 里躺着一条，用户读成"平台上一定没有残留"，而平台上那份可能正在跑、
    /// 并会照常计费。取消不了就如实说取消不了（与"有句柄但远端没确认"同一个桶）。
    ///
    /// 判别点三条：
    /// 1. 不确定的 run（无句柄）⇒ **同时**出现在 `cancelled`（本机确实结束了）与
    ///    `remoteMayContinue`（远端可能还在跑）——两个桶重叠是有意的，见
    ///    `accepted_run_without_cancel_capability_reports_remote_may_continue` 的先例；
    /// 2. **反向控制**：还没提交的 `QUEUED` run **不该**进 `remoteMayContinue`
    ///    （它一定没到平台，见 `cancels_queued_run_locally`）；
    /// 3. 有句柄的那条路照旧（`accepted_run_without_cancel_capability_...`）。
    ///
    /// 负对照：把那个 `else if matches!(...)` 分支删掉 ⇒ 第 1 条红。
    #[tokio::test]
    async fn uncertain_run_without_a_handle_reports_that_the_remote_may_continue() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation(&repositories).await;
        // 先让它落"等确认"：网络类失败 + **没有**"字节没出去"的证据。
        let scheduler_driver = driver(vec![]);
        scheduler_driver.push_submit_error(SafeError::network("SUBMIT_TIMEOUT", "提交超时"));
        {
            let mut registry = DriverRegistry::new();
            registry.register(driver_key("openai", "openai-images"), scheduler_driver);
            let scheduler = crate::v2::application::scheduler::SchedulerService {
                capacity: Default::default(),
                owner: "test-scheduler".to_string(),
                repositories: Arc::new(repositories.clone()),
                leases: Arc::new(repositories.clone()),
                connections: Arc::new(repositories.clone()),
                assets: Arc::new(repositories.clone()),
                artifacts: Arc::new(repositories.clone()),
                drivers: Arc::new(registry),
            };
            scheduler.tick().await.expect("调度应成功");
        }
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted.runs[0].status,
            RunStatus::SubmissionUncertain,
            "前提：这一条要先落不确定态"
        );

        let service = cancel_service(repositories.clone(), driver(vec![]));
        let result = service.execute(operation_id).await.expect("取消应成功");
        // 1) 本机结束了，但**远端可能还在跑**——必须同时说这两件事。
        assert_eq!(result.cancelled.len(), 1, "本机应当确实取消掉了");
        assert_eq!(
            result.remote_may_continue.len(),
            1,
            "没有句柄的不确定 run 也可能正在平台上跑，不能只报 cancelled：{result:?}"
        );
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Cancelled);
    }
}
