use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::callback::CallbackSubscription;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::hash::canonical_json;
use crate::v2::domain::ids::{ConnectionId, IdempotencyKey, OperationId, RecipeRevisionId};
use crate::v2::domain::media::AssetRef;
use crate::v2::domain::model::Timestamp;
use crate::v2::domain::operation::{Operation, OperationStatus};
use crate::v2::ports::policy::AuthorizationPolicy;
use crate::v2::ports::repositories::{
    ConnectionRepository, NewOperation, NewRun, OperationRepository, PersistedOperation,
    RecipeRepository,
};
use crate::v2::ports::schema::SchemaValidator;

/// 自主调用方**同时进行中**的任务上限（按 actor 计数）。
///
/// 只对**没有交付可挂**的自主调用方生效（见 `is_unguarded_autonomous`）：4 这个数字就是
/// "一次别铺开超过 4 个付费任务"。
pub const MAX_AUTONOMOUS_CONCURRENT_OPERATIONS: usize = 4;

/// 单个 Operation 最多多少个 Run。**这是一条物理边界，必须同时出现在协议地板上。**
///
/// 与并发上限不同，这个上限**对所有通道一视同仁**（一次调用里铺开 100 个 Run 是规模问题，
/// 不是渠道问题）。MCP 那一侧的 `runs` 参数由 `clients/verbs.rs` 表里的 `max` 渲染成
/// `maxItems`，并有测试钉住"表里的数字 == 这里的数字"。
///
/// ⚠️ 为什么要钉：两边不一致的后果是**调用方按清单规划、却被代码拒绝**（或反过来，
/// 清单允许一个必然失败的规模）。清单与代码没有编译期联系，所以只能靠判据测试。
pub const MAX_RUNS_PER_OPERATION: usize = 100;

/// 这个 `origin` 是不是"自主、且没有别的闸门"的调用方。
///
/// ⚠️ 这个判断的**边界就是安全边界**，所以要写清楚为什么是这几个、不是什么：
/// * `mcp` —— 外界 AI 通过我们的 MCP server 调用，而且**自动批准**：它可以无限提交，
///   **必须设闸**。
/// * `cli` —— 脚本/CI。**不设闸**：写脚本的人是自己决定要跑多少的（跟点界面同类），
///   而且批量脚本是文档化的正常用法。
/// * `gui` / `http` —— 人在操作，不设闸。
///
/// ⚠️ 自报家门而不是靠猜：这个判断依赖调用方**如实声明** origin。我们不防伪造——
/// 持有合法 token 的人本来就能直接写脚本花自己的钱，闸门防的是**意外**（模型跑飞），
/// 不是防对手。所以这里用自报，不做安全剧场。
pub fn is_unguarded_autonomous(origin: &str) -> bool {
    origin == "mcp"
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub client_run_id: String,
    pub recipe_revision_id: RecipeRevisionId,
    pub connection_id: Option<ConnectionId>,
    pub inputs: BTreeMap<String, Value>,
    pub assets: Vec<AssetRef>,
}

#[derive(Debug, Clone)]
pub struct CreateOperationRequest {
    pub actor: ActorRef,
    pub origin: String,
    pub idempotency_key: IdempotencyKey,
    pub runs: Vec<RunRequest>,
    pub metadata: BTreeMap<String, Value>,
    pub callback: Option<CallbackSubscription>,
}

pub struct Repositories {
    pub recipes: Box<dyn RecipeRepository>,
    pub connections: Box<dyn ConnectionRepository>,
    pub operations: Box<dyn OperationRepository>,
}

pub struct CreateOperation {
    pub repositories: Repositories,
    pub authorization: Box<dyn AuthorizationPolicy>,
    pub schema_validator: Box<dyn SchemaValidator>,
}

impl CreateOperation {
    pub fn new(
        repositories: Repositories,
        authorization: Box<dyn AuthorizationPolicy>,
        schema_validator: Box<dyn SchemaValidator>,
    ) -> Self {
        Self {
            repositories,
            authorization,
            schema_validator,
        }
    }

    pub async fn execute(
        &self,
        request: CreateOperationRequest,
    ) -> DomainResult<PersistedOperation> {
        if !request.actor.has_scope(Scope::OperationsExecute) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "缺少 operations:execute 权限",
            ));
        }
        let decision = self
            .authorization
            .require_scope(&request.actor, Scope::OperationsExecute)
            .await?;
        if !decision.allowed {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                decision
                    .reason
                    .unwrap_or_else(|| "缺少 operations:execute 权限".to_string()),
            ));
        }
        if request.runs.is_empty() {
            return Err(SafeError::validation(
                "RUNS_REQUIRED",
                "Operation 至少需要一个 Run",
            ));
        }
        // ⚠️ **花钱的闸门放在这里**——副作用真正发生的地方，而不是某个客户端的包装层。
        //
        // 为什么必须有：MCP 那条路是**自动批准**的——一个跑偏的模型能在一条对话里无限提交
        // 付费任务，每次都是钱。而它**没有交付可挂**，只能按 actor 计数。
        //
        // ⚠️ 为什么只对 `origin == "mcp"` 生效，而不是所有调用方：
        // * 人点的（`gui` / `http`）：人看得见自己在花什么，闸门只会碍事。
        //   界面的批量运行是**一个 Operation 带 N 个 Run**，所以按 Operation 计数不会误伤批量。
        // * `cli`：脚本自己决定要跑多少——批量脚本是文档化的正常用法。
        if is_unguarded_autonomous(&request.origin) {
            let active = self
                .repositories
                .operations
                .count_active_operations(&request.actor.id)
                .await?;
            if active >= MAX_AUTONOMOUS_CONCURRENT_OPERATIONS {
                return Err(SafeError::conflict(
                    "AUTONOMOUS_CONCURRENCY_LIMIT",
                    format!(
                        "自主调用方同时进行中的任务已达上限 {MAX_AUTONOMOUS_CONCURRENT_OPERATIONS} 个\
                         （当前 {active} 个）。等已有任务结束后再提交，不要并发铺开付费任务。"
                    ),
                ));
            }
        }
        if request.runs.len() > MAX_RUNS_PER_OPERATION {
            return Err(SafeError::validation(
                "RUN_LIMIT_EXCEEDED",
                format!("单个 Operation 最多 {MAX_RUNS_PER_OPERATION} 个 Run"),
            ));
        }
        let mut seen_run_ids = std::collections::HashSet::new();
        for run in &request.runs {
            if run.client_run_id.trim().is_empty() {
                return Err(SafeError::validation(
                    "CLIENT_RUN_ID_REQUIRED",
                    "每个 Run 必须包含 clientRunId",
                ));
            }
            if !seen_run_ids.insert(run.client_run_id.clone()) {
                return Err(SafeError::validation(
                    "DUPLICATE_CLIENT_RUN_ID",
                    format!("clientRunId 重复：{}", run.client_run_id),
                ));
            }
        }

        let mut resolved_runs = Vec::with_capacity(request.runs.len());
        for (sequence, run_request) in request.runs.iter().enumerate() {
            resolved_runs.push(
                self.resolve_run(&request.actor, run_request, sequence as u32)
                    .await?,
            );
        }

        let request_hash = Self::canonical_hash(&request, &resolved_runs);
        let existing = self
            .repositories
            .operations
            .find_by_idempotency(
                &request.actor.id,
                &request.actor.client_id.0,
                &request.idempotency_key.0,
            )
            .await?;
        if let Some(operation) = existing {
            if operation.operation.request_hash == request_hash {
                return Ok(operation);
            }
            return Err(SafeError::conflict(
                "IDEMPOTENCY_CONFLICT",
                "相同 idempotencyKey 提交了不同请求",
            ));
        }

        let now = Timestamp::now();
        let operation = Operation {
            id: OperationId(0),
            actor: request.actor.clone(),
            origin: request.origin.clone(),
            idempotency_key: request.idempotency_key.clone(),
            request_hash,
            status: OperationStatus::Queued,
            metadata: request.metadata.clone(),
            callback: request.callback.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
        };

        let new_runs = resolved_runs
            .into_iter()
            .map(|run| NewRun {
                recipe_revision_id: run.recipe_revision_id,
                connection_revision_id: run.connection_revision_id,
                client_run_id: run.client_run_id,
                inputs: run.inputs,
                assets: run.assets,
            })
            .collect();

        self.repositories
            .operations
            .create_operation(NewOperation {
                operation,
                runs: new_runs,
            })
            .await
    }

    async fn resolve_run(
        &self,
        actor: &ActorRef,
        request: &RunRequest,
        sequence: u32,
    ) -> DomainResult<ResolvedRun> {
        let revision = self
            .repositories
            .recipes
            .find_revision(request.recipe_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found("RECIPE_REVISION_NOT_FOUND", "Recipe revision 不存在")
            })?;
        let recipe = self
            .repositories
            .recipes
            .find_recipe(revision.recipe_id)
            .await?
            .ok_or_else(|| SafeError::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"))?;

        let violations = self
            .schema_validator
            .validate(
                &revision.input_schema,
                &Value::Object(request.inputs.clone().into_iter().collect()),
            )
            .await?;
        if !violations.is_empty() {
            let message = violations
                .first()
                .map(|violation| format!("{}: {}", violation.path, violation.reason))
                .unwrap_or_else(|| "inputs 不符合 Recipe schema".to_string());
            return Err(SafeError::validation("INPUT_SCHEMA_INVALID", message));
        }

        let connection_id = revision.binding.connection_allowed(request.connection_id)?;
        let override_requested = request.connection_id.is_some()
            && request.connection_id != Some(revision.binding.default_connection_id);
        if override_requested && !actor.has_scope(Scope::OperationsSelectConnection) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "覆盖默认 Connection 需要 operations:select-connection 权限",
            ));
        }

        let connection = self
            .repositories
            .connections
            .find_connection(connection_id)
            .await?
            .ok_or_else(|| SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
        let connection_revision = self
            .repositories
            .connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })?;

        let supports_capability = connection_revision
            .effective_capabilities
            .iter()
            .any(|capability| capability == &recipe.capability);
        if !supports_capability {
            return Err(SafeError::validation(
                "CAPABILITY_MISMATCH",
                format!("Connection 不支持 Recipe 所需能力：{}", recipe.capability),
            ));
        }

        Ok(ResolvedRun {
            recipe_revision_id: revision.id,
            connection_revision_id: connection_revision.id,
            client_run_id: request.client_run_id.clone(),
            inputs: request.inputs.clone(),
            assets: request.assets.clone(),
            sequence,
        })
    }

    fn canonical_hash(request: &CreateOperationRequest, runs: &[ResolvedRun]) -> String {
        let run_payloads: Vec<Value> = runs
            .iter()
            .map(|run| {
                json!({
                    "recipeRevisionId": run.recipe_revision_id.0,
                    "connectionRevisionId": run.connection_revision_id.0,
                    "clientRunId": run.client_run_id,
                    "sequence": run.sequence,
                    "inputs": run.inputs,
                    "assets": run.assets.iter().map(|asset| json!({
                        "assetId": asset.asset_id.0,
                        "checksum": asset.checksum,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let payload = json!({
            "runs": run_payloads,
            "metadata": request.metadata,
            "callback": request.callback.as_ref().map(|callback| json!({
                "url": callback.url,
                "events": callback.events,
                "maxAttempts": callback.max_attempts,
                "retryAfterSeconds": callback.retry_after_seconds,
            })),
        });
        canonical_json(&payload)
    }
}

#[derive(Debug, Clone)]
struct ResolvedRun {
    recipe_revision_id: RecipeRevisionId,
    connection_revision_id: crate::v2::domain::ids::ConnectionRevisionId,
    client_run_id: String,
    inputs: BTreeMap<String, Value>,
    assets: Vec<AssetRef>,
    sequence: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, Scope};
    use crate::v2::domain::ids::{
        ClientId, ConnectionId, ConnectionRevisionId, RecipeId, RecipeRevisionId,
    };
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility,
    };
    use crate::v2::infrastructure::fakes::{InMemoryRepositories, PermitAllAuthorizationPolicy};
    use crate::v2::infrastructure::schema::BasicSchemaValidator;
    use serde_json::json;

    fn actor(scopes: Vec<Scope>) -> ActorRef {
        ActorRef {
            id: "actor-1".to_string(),
            kind: ActorKind::Mcp,
            client_id: ClientId("client-1".to_string()),
            scopes,
            label: None,
        }
    }

    fn seed_environment(repositories: &InMemoryRepositories) -> (RecipeRevisionId, ConnectionId) {
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
        (RecipeRevisionId(1), ConnectionId(1))
    }

    fn use_case(repositories: InMemoryRepositories) -> CreateOperation {
        CreateOperation::new(
            Repositories {
                recipes: Box::new(repositories.clone()),
                connections: Box::new(repositories.clone()),
                operations: Box::new(repositories),
            },
            Box::new(PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        )
    }

    fn request(actor: ActorRef) -> CreateOperationRequest {
        CreateOperationRequest {
            actor,
            origin: "mcp".to_string(),
            idempotency_key: IdempotencyKey("key-1".to_string()),
            runs: vec![RunRequest {
                client_run_id: "image-1".to_string(),
                recipe_revision_id: RecipeRevisionId(1),
                connection_id: None,
                inputs: BTreeMap::from([("prompt".to_string(), json!("a red house"))]),
                assets: vec![],
            }],
            metadata: BTreeMap::new(),
            callback: None,
        }
    }

    /// 自主调用方（MCP）**必须**有并发闸门：它自动批准，又没有交付可挂，只能按 actor 计数。
    ///
    /// 判别点两条，缺一条都不算做对：
    /// 1. `mcp` 到上限后被拒；
    /// 2. **同样次数的 `http`（人在操作）不被拒**——反向控制。少了这条，
    ///    一个"对所有调用方一刀切"的实现也能让第 1 条通过，而那会误伤正常使用。
    #[tokio::test]
    async fn autonomous_concurrency_is_capped_per_origin() {
        async fn submit(origin: &str, times: usize) -> Vec<String> {
            let repositories = InMemoryRepositories::new();
            seed_environment(&repositories);
            let use_case = use_case(repositories);
            let mut errors = Vec::new();
            for index in 0..times {
                let mut request = request(actor(vec![Scope::OperationsExecute]));
                request.origin = origin.to_string();
                // 幂等键必须各不相同，否则第 2 次起会被当成重放而不是新提交。
                request.idempotency_key = IdempotencyKey(format!("{origin}-key-{index}"));
                if let Err(error) = use_case.execute(request).await {
                    errors.push(error.code);
                }
            }
            errors
        }

        // 1) MCP：第 5 个（超过上限 4）必须被拒。
        let errors = submit("mcp", MAX_AUTONOMOUS_CONCURRENT_OPERATIONS + 1).await;
        assert_eq!(
            errors,
            vec!["AUTONOMOUS_CONCURRENCY_LIMIT".to_string()],
            "MCP 超过并发上限必须被拒，且只拒超出的那一个"
        );

        // 2) 反向控制：人在操作（`http`）提交同样次数，**一个都不该被拒**。
        let errors = submit("http", MAX_AUTONOMOUS_CONCURRENT_OPERATIONS + 3).await;
        assert!(
            errors.is_empty(),
            "人在操作的调用方不该被这一层拦（界面上连点几次单跑是正常用法）：{errors:?}"
        );
    }

    #[tokio::test]
    async fn creates_queued_operation_with_one_run() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let persisted = use_case
            .execute(request(actor(vec![Scope::OperationsExecute])))
            .await
            .expect("创建 Operation 应成功");
        assert_eq!(
            persisted.operation.status,
            crate::v2::domain::operation::OperationStatus::Queued
        );
        assert_eq!(persisted.runs.len(), 1);
        assert_eq!(persisted.runs[0].client_run_id, "image-1");
        assert_eq!(
            persisted.runs[0].status,
            crate::v2::domain::operation::RunStatus::Queued
        );
        assert_eq!(
            persisted.runs[0].connection_revision_id,
            ConnectionRevisionId(1)
        );
    }

    #[tokio::test]
    async fn replays_same_request_under_same_idempotency_key() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let first = use_case
            .execute(request(actor(vec![Scope::OperationsExecute])))
            .await
            .expect("第一次创建应成功");
        let second = use_case
            .execute(request(actor(vec![Scope::OperationsExecute])))
            .await
            .expect("幂等重放应返回原 Operation");
        assert_eq!(first.operation.id, second.operation.id);
        assert_eq!(first.runs.len(), second.runs.len());
        assert_eq!(first.runs[0].id, second.runs[0].id);
    }

    #[tokio::test]
    async fn conflicts_on_different_request_under_same_key() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        use_case
            .execute(request(actor(vec![Scope::OperationsExecute])))
            .await
            .expect("第一次创建应成功");
        let mut changed = request(actor(vec![Scope::OperationsExecute]));
        changed.runs[0]
            .inputs
            .insert("prompt".to_string(), json!("a blue house"));
        let error = use_case
            .execute(changed)
            .await
            .expect_err("相同 key 不同请求应冲突");
        assert_eq!(error.code, "IDEMPOTENCY_CONFLICT");
    }

    #[tokio::test]
    async fn rejects_actor_without_execute_scope() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let error = use_case
            .execute(request(actor(vec![Scope::OperationsRead])))
            .await
            .expect_err("缺少执行权限应被拒绝");
        assert_eq!(error.code, "SCOPE_REQUIRED");
    }

    #[tokio::test]
    async fn rejects_inputs_against_recipe_schema() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let mut bad = request(actor(vec![Scope::OperationsExecute]));
        bad.runs[0].inputs = BTreeMap::new();
        let error = use_case
            .execute(bad)
            .await
            .expect_err("缺失必填字段应校验失败");
        assert_eq!(error.code, "INPUT_SCHEMA_INVALID");
    }

    #[tokio::test]
    async fn rejects_connection_override_without_scope() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let mut bad = request(actor(vec![Scope::OperationsExecute]));
        bad.runs[0].connection_id = Some(ConnectionId(2));
        let error = use_case
            .execute(bad)
            .await
            .expect_err("Denied override 应被拒绝");
        assert_eq!(error.code, "CONNECTION_OVERRIDE_DENIED");
    }

    #[tokio::test]
    async fn rejects_missing_recipe_revision() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let mut bad = request(actor(vec![Scope::OperationsExecute]));
        bad.runs[0].recipe_revision_id = RecipeRevisionId(999);
        let error = use_case
            .execute(bad)
            .await
            .expect_err("缺失 revision 应报错");
        assert_eq!(error.code, "RECIPE_REVISION_NOT_FOUND");
    }

    #[tokio::test]
    async fn supports_batch_runs() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let mut batch = request(actor(vec![Scope::OperationsExecute]));
        batch.runs.push(RunRequest {
            client_run_id: "image-2".to_string(),
            recipe_revision_id: RecipeRevisionId(1),
            connection_id: None,
            inputs: BTreeMap::from([("prompt".to_string(), json!("a green house"))]),
            assets: vec![],
        });
        let persisted = use_case.execute(batch).await.expect("批量创建应成功");
        assert_eq!(persisted.runs.len(), 2);
        assert_eq!(persisted.runs[0].client_run_id, "image-1");
        assert_eq!(persisted.runs[1].client_run_id, "image-2");
    }

    #[tokio::test]
    async fn rejects_duplicate_client_run_id() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let use_case = use_case(repositories);
        let mut batch = request(actor(vec![Scope::OperationsExecute]));
        batch.runs.push(RunRequest {
            client_run_id: "image-1".to_string(),
            recipe_revision_id: RecipeRevisionId(1),
            connection_id: None,
            inputs: BTreeMap::from([("prompt".to_string(), json!("a green house"))]),
            assets: vec![],
        });
        let error = use_case
            .execute(batch)
            .await
            .expect_err("重复 clientRunId 应报错");
        assert_eq!(error.code, "DUPLICATE_CLIENT_RUN_ID");
    }
}
