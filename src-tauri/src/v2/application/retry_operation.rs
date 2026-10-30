use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::v2::domain::actor::ActorRef;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::IdempotencyKey;
use crate::v2::domain::model::Timestamp;
use crate::v2::domain::operation::RunStatus;
use crate::v2::ports::repositories::OperationRepository;

use super::create_operation::{CreateOperation, CreateOperationRequest, RunRequest};

/// 失败/中断任务一键重试：以原 operation 各 run 的配方修订与输入重建一个新 operation。
/// 连接沿用配方默认连接（不绑定可能已失效的旧 revision）。
pub struct RetryOperation {
    pub create: Arc<CreateOperation>,
    pub repositories: Arc<dyn OperationRepository>,
}

impl RetryOperation {
    pub async fn execute(
        &self,
        actor: ActorRef,
        source_operation_id: crate::v2::domain::ids::OperationId,
        origin: &str,
    ) -> DomainResult<crate::v2::domain::ids::OperationId> {
        self.execute_selected(actor, source_operation_id, None, origin)
            .await
    }

    pub async fn execute_run(
        &self,
        actor: ActorRef,
        source_operation_id: crate::v2::domain::ids::OperationId,
        source_run_id: crate::v2::domain::ids::RunId,
        origin: &str,
    ) -> DomainResult<crate::v2::domain::ids::OperationId> {
        self.execute_selected(actor, source_operation_id, Some(source_run_id), origin)
            .await
    }

    async fn execute_selected(
        &self,
        actor: ActorRef,
        source_operation_id: crate::v2::domain::ids::OperationId,
        source_run_id: Option<crate::v2::domain::ids::RunId>,
        origin: &str,
    ) -> DomainResult<crate::v2::domain::ids::OperationId> {
        let source = self
            .repositories
            .find_operation(source_operation_id)
            .await?
            .ok_or_else(|| SafeError::not_found("OPERATION_NOT_FOUND", "Operation 不存在"))?;
        let selected_runs = source
            .runs
            .iter()
            .filter(|run| source_run_id.is_none_or(|id| run.id == id))
            .collect::<Vec<_>>();
        if selected_runs.is_empty() {
            return Err(SafeError::not_found("RUN_NOT_FOUND", "Run 不存在"));
        }
        // 只有**已经停下**的 run 才谈得上重试：对正在跑的 run 调 retry，等于在平台上
        // 再开一份、再扣一次钱，而原来那份还在跑——这不是"重试"，是"重复提交"。
        //
        // `SUBMISSION_UNCERTAIN` **不在此列**：它没有确定的结果，文档把它作为"放弃重来"
        // 的出口（用户明确要求，才会接受"可能再付一次"）。
        let in_flight = selected_runs
            .iter()
            .filter(|run| is_in_flight(run.status))
            .map(|run| {
                format!(
                    "{}（{}）",
                    run.display_label()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("run-{}", run.id.0)),
                    run.status.as_str()
                )
            })
            .collect::<Vec<_>>();
        if !in_flight.is_empty() {
            return Err(SafeError::conflict(
                "RETRY_SOURCE_IN_FLIGHT",
                format!(
                    "任务还在进行中：{}。等它结束，或先取消再重试——现在重试会在平台上\
                     再开一份、再计费一次。",
                    in_flight.join("、")
                ),
            ));
        }
        let suffix = Timestamp::now_millis();
        let runs = selected_runs
            .iter()
            .map(|run| RunRequest {
                // 只有**调用方起过的名字**才带进重试（`-retry-` 后缀可观察）；没名字的留空，
                // 由创建用例按序号补 `run-N`——那种 id 不进文件名，重试后的产物也不会突然
                // 多出一截 `run-3-retry-…` 的前缀。
                client_run_id: run
                    .display_label()
                    .map(|label| format!("{label}-retry-{suffix}"))
                    .unwrap_or_default(),
                recipe_revision_id: run.recipe_revision_id,
                connection_id: None,
                inputs: run.inputs.clone(),
                assets: run.assets.clone(),
            })
            .collect::<Vec<_>>();
        let key_prefix = source_run_id
            .map(|id| format!("run-{}", id.0))
            .unwrap_or_else(|| "operation".to_string());
        let request = CreateOperationRequest {
            actor,
            origin: origin.to_string(),
            idempotency_key: IdempotencyKey(format!(
                "retry-{}-{}-{suffix}",
                source_operation_id.0, key_prefix
            )),
            runs,
            metadata: BTreeMap::<String, Value>::new(),
            callback: source.operation.callback.clone(),
        };
        let created = self.create.execute(request).await?;
        Ok(created.operation.id)
    }
}

/// 还在本地队列或平台上的 run：重试它就是重复提交。
///
/// `SUBMISSION_UNCERTAIN` 刻意**不**在此列：它的语义是"平台收没收到不知道"，
/// 而不是"确定还在跑"；文档把 `operation retry` 写作这条路的出口（放弃重来）。
fn is_in_flight(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Queued
            | RunStatus::Preparing
            | RunStatus::Submitting
            | RunStatus::Accepted
            | RunStatus::Running
            | RunStatus::RetryWait
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::application::create_operation::{CreateOperation, Repositories};
    use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
    use crate::v2::domain::ids::{
        ClientId, ConnectionId, ConnectionRevisionId, OperationId, RecipeId, RecipeRevisionId,
    };
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility, Timestamp,
    };
    use crate::v2::domain::operation::{Operation, OperationStatus, Transition};
    use crate::v2::infrastructure::fakes::{InMemoryRepositories, PermitAllAuthorizationPolicy};
    use crate::v2::infrastructure::schema::BasicSchemaValidator;
    use crate::v2::ports::repositories::{NewOperation, NewRun, OperationRepository};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

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
            config: BTreeMap::new(),
            secret_bindings: vec![],
            effective_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures {
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
                "properties": { "prompt": { "type": "string", "minLength": 1 } }
            }),
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: "openai-images".to_string(),
                provider_config: BTreeMap::new(),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: ConnectionOverride::Denied,
            },
            defaults: BTreeMap::new(),
            source: RecipeSource::Embedded,
            created_at: Timestamp::now(),
        };
        repositories.seed_recipe(recipe, vec![revision]);
    }

    async fn seed_operation(
        operations: &Arc<dyn OperationRepository>,
        status: OperationStatus,
    ) -> OperationId {
        let now = Timestamp::now();
        let mut inputs = BTreeMap::new();
        inputs.insert("prompt".to_string(), Value::String("hello".to_string()));
        let operation = Operation {
            id: OperationId(0),
            actor: ActorRef {
                id: "test".to_string(),
                kind: ActorKind::Cli,
                client_id: ClientId("test".to_string()),
                scopes: vec![Scope::OperationsExecute],
                label: None,
            },
            origin: "test".to_string(),
            idempotency_key: IdempotencyKey("orig-key".to_string()),
            request_hash: "h".to_string(),
            status,
            metadata: Default::default(),
            callback: None,
            created_at: now.clone(),
            updated_at: now,
        };
        let persisted = operations
            .create_operation(NewOperation {
                operation,
                runs: vec![NewRun {
                    recipe_revision_id: RecipeRevisionId(1),
                    connection_revision_id: ConnectionRevisionId(1),
                    client_run_id: "run-1".to_string(),
                    inputs,
                    assets: vec![],
                }],
            })
            .await
            .expect("seed 应成功");
        persisted.operation.id
    }

    /// 真实的"失败任务"：operation 与 run **都**是终态。
    ///
    /// `NewRun` 默认从 `Queued` 开始，所以只把 operation 标成 FAILED 会造出一个现实里
    /// 不存在的组合（任务失败、run 还在排队）；而"能不能重试"的判据看的是 run，
    /// 那条假数据会让测试比生产宽松。
    async fn seed_failed_operation(operations: &Arc<dyn OperationRepository>) -> OperationId {
        let id = seed_operation(operations, OperationStatus::Failed).await;
        let persisted = operations
            .find_operation(id)
            .await
            .expect("读取应成功")
            .expect("刚建的操作应存在");
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToFailed,
        ] {
            operations
                .transition_run(persisted.runs[0].id, transition, vec![])
                .await
                .expect("seed 应把 run 推到失败");
        }
        id
    }

    #[tokio::test]
    async fn retry_creates_new_operation_with_original_inputs() {
        let repos = InMemoryRepositories::default();
        seed_environment(&repos);
        let operations: Arc<dyn OperationRepository> = Arc::new(repos.clone());
        let source_id = seed_failed_operation(&operations).await;

        let create = Arc::new(CreateOperation::new(
            Repositories {
                recipes: Box::new(repos.clone()),
                connections: Box::new(repos.clone()),
                operations: Box::new(operations.clone()),
                assets: Box::new(repos.clone()),
            },
            Box::new(PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        ));
        let retry = RetryOperation {
            create,
            repositories: operations.clone(),
        };
        let actor = ActorRef {
            id: "test".to_string(),
            kind: ActorKind::Cli,
            client_id: ClientId("test".to_string()),
            scopes: vec![Scope::OperationsExecute],
            label: None,
        };
        let new_id = retry.execute(actor, source_id, "test").await.unwrap();
        assert_ne!(new_id, source_id);
        let created = operations.find_operation(new_id).await.unwrap().unwrap();
        assert_eq!(created.runs.len(), 1);
        assert_eq!(created.runs[0].recipe_revision_id, RecipeRevisionId(1));
        assert_eq!(
            created.runs[0].inputs.get("prompt"),
            Some(&Value::String("hello".to_string()))
        );
    }

    /// 对**还在跑**的 run 调 retry 必须被拒：否则平台上会多一份、钱多扣一次，
    /// 而原来那份还在跑。判据是错误码，不是"不报错"——调用方要能分辨原因。
    #[tokio::test]
    async fn retry_refuses_a_run_that_is_still_in_flight() {
        let repos = InMemoryRepositories::default();
        seed_environment(&repos);
        let operations: Arc<dyn OperationRepository> = Arc::new(repos.clone());
        let source_id = seed_operation(&operations, OperationStatus::Queued).await;
        let created = operations
            .find_operation(source_id)
            .await
            .expect("读取应成功")
            .expect("刚建的操作应存在");
        let run_id = created.runs[0].id;
        // Queued → Preparing → Submitting：模拟 scheduler 已经领走、正在提交
        operations
            .transition_run(run_id, Transition::ToPreparing, vec![])
            .await
            .expect("Queued → Preparing 应合法");
        operations
            .transition_run(run_id, Transition::ToSubmitting, vec![])
            .await
            .expect("Preparing → Submitting 应合法");

        let create = Arc::new(CreateOperation::new(
            Repositories {
                recipes: Box::new(repos.clone()),
                connections: Box::new(repos.clone()),
                operations: Box::new(operations.clone()),
                assets: Box::new(repos.clone()),
            },
            Box::new(PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        ));
        let retry = RetryOperation {
            create,
            repositories: operations.clone(),
        };
        let actor = ActorRef {
            id: "test".to_string(),
            kind: ActorKind::Cli,
            client_id: ClientId("test".to_string()),
            scopes: vec![Scope::OperationsExecute],
            label: None,
        };
        let error = retry
            .execute(actor, source_id, "test")
            .await
            .expect_err("在跑的 run 不许重试");
        assert_eq!(error.code, "RETRY_SOURCE_IN_FLIGHT");
        assert!(
            error.message.contains("SUBMITTING"),
            "错误里要指出是哪条 run、什么状态：{}",
            error.message
        );
    }
}
