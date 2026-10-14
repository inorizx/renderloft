use serde_json::{json, Value};
use std::sync::Arc;

use crate::v2::domain::callback::SIGNING_KEY_SLOT;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::OperationId;
use crate::v2::domain::model::{SecretBinding, SecretResolution, Timestamp};
use crate::v2::domain::signing;
use crate::v2::ports::callback::{CallbackDeliverer, DeliveryOutcome, DeliveryRequest};
use crate::v2::ports::repositories::{
    CallbackDeliveryRecord, CallbackDeliveryRepository, OperationRepository,
};
use crate::v2::ports::stores::SecretStore;

pub struct CallbackService {
    pub repositories: Arc<dyn OperationRepository>,
    pub deliveries: Arc<dyn CallbackDeliveryRepository>,
    pub deliverer: Arc<dyn CallbackDeliverer>,
    /// 只用来**读**回调签名密钥（写它只有人那条路，见 `SIGNING_KEY_SLOT`）。
    pub secrets: Arc<dyn SecretStore>,
}

impl CallbackService {
    /// 一轮回调工作：**先补扫漏掉的，再投递到期的**。
    ///
    /// ⚠️ 为什么"补扫"是必须的（2026-10-10 真跑抓到的 bug）：投递的触发点曾经**一个都没有**
    /// ——`enqueue_terminal` 只在测试里被调用过。订阅存了、任务跑完了、回调永远不发，
    /// 而所有单测都是绿的（它们自己显式调 enqueue，等于只测了"我调用它它会工作"）。
    ///
    /// 补扫按**状态**找活干（而不是指望某条路径记得调用）：reconcile / scheduler / cancel /
    /// hygiene 哪条路把任务推到终态都无所谓，**daemon 当时没在跑也补得回来**。
    pub async fn tick(&self, limit: usize) -> DomainResult<DeliveryRunResult> {
        let enqueued = self.enqueue_missing(limit).await?;
        let mut result = self.deliver_due(limit).await?;
        result.enqueued = enqueued;
        Ok(result)
    }

    /// 补扫：给"已经进终态、带订阅、但还没有投递记录"的操作补一条投递。
    ///
    /// 幂等：判据是"还没有投递记录"，跑多少次都只补一条。时间窗见
    /// `CALLBACK_CATCH_UP_WINDOW_MILLIS`。
    pub async fn enqueue_missing(&self, limit: usize) -> DomainResult<usize> {
        let since = Timestamp::now_millis().saturating_sub(CALLBACK_CATCH_UP_WINDOW_MILLIS);
        let awaiting = self
            .deliveries
            .list_operations_awaiting_callback(since, limit)
            .await?;
        let mut enqueued = 0;
        for (operation_id, event_id) in awaiting {
            self.enqueue_terminal(operation_id, event_id).await?;
            enqueued += 1;
        }
        Ok(enqueued)
    }

    pub async fn enqueue_terminal(
        &self,
        operation_id: OperationId,
        terminal_event_id: u64,
    ) -> DomainResult<()> {
        let persisted = self
            .repositories
            .find_operation(operation_id)
            .await?
            .ok_or_else(|| SafeError::not_found("OPERATION_NOT_FOUND", "Operation 不存在"))?;
        let Some(callback) = &persisted.operation.callback else {
            return Ok(());
        };
        if !callback.covers("operation.terminal") {
            return Ok(());
        }
        let payload = json!({
            "operationId": operation_id.0,
            "status": persisted.operation.status,
            "runs": persisted.runs.iter().map(|run| json!({
                "runId": run.id.0,
                "status": run.status,
                "clientRunId": run.client_run_id,
            })).collect::<Vec<_>>(),
        });
        let delivery = CallbackDeliveryRecord {
            delivery_id: format!("dlv-{}", uuid::Uuid::new_v4().simple()),
            operation_id,
            event_id: terminal_event_id,
            url: callback.url.clone(),
            payload,
            attempts: 0,
            max_attempts: callback.max_attempts,
            next_retry_at: Timestamp::now(),
            completed: false,
            last_error: None,
        };
        self.deliveries.create_delivery(delivery).await
    }

    pub async fn deliver_due(&self, limit: usize) -> DomainResult<DeliveryRunResult> {
        let mut result = DeliveryRunResult::default();
        for delivery in self.deliveries.list_due_deliveries(limit).await? {
            // 每次投递都重新取一次密钥：人可能刚设好（或刚删掉），不该等到重启才生效。
            let signature = self.signature_for(&delivery.payload).await;
            let outcome = self
                .deliverer
                .deliver(DeliveryRequest {
                    delivery_id: delivery.delivery_id.clone(),
                    url: delivery.url.clone(),
                    payload: delivery.payload.clone(),
                    signature,
                })
                .await
                .unwrap_or(DeliveryOutcome::NetworkError {
                    message: "deliverer 内部错误".to_string(),
                    retryable: true,
                });
            match outcome {
                DeliveryOutcome::Accepted => {
                    self.deliveries
                        .mark_delivery_attempt(&delivery.delivery_id, true, None)
                        .await?;
                    result.delivered += 1;
                }
                DeliveryOutcome::Rejected { message } => {
                    self.deliveries
                        .mark_delivery_attempt(&delivery.delivery_id, false, Some(message))
                        .await?;
                    result.rejected += 1;
                }
                DeliveryOutcome::NetworkError { message, retryable } => {
                    if !retryable {
                        self.deliveries
                            .mark_delivery_attempt(&delivery.delivery_id, false, Some(message))
                            .await?;
                        result.rejected += 1;
                    } else {
                        self.deliveries
                            .mark_delivery_attempt(&delivery.delivery_id, false, Some(message))
                            .await?;
                        result.retrying += 1;
                    }
                }
            }
        }
        Ok(result)
    }

    /// 给这次投递算签名头；**没配密钥就返回 `None`**（不带那个头，而不是签一个假的）。
    ///
    /// 密钥是**人**放进系统凭据库的（`callback set-secret`，MCP 没有这条路）。
    ///
    /// ⚠️ 被签名的串必须与**实际发出去的字节**一致：deliverer 用 `.json(&payload)` 发，
    /// 它内部就是 `serde_json::to_string`，所以这里用同一个函数序列化同一个 `Value`。
    /// 换序列化器、或在中间再加工一次 body，都会让接收端验不过——而且只在真收时才暴露。
    async fn signature_for(&self, payload: &Value) -> Option<String> {
        let binding = SecretBinding {
            slot: SIGNING_KEY_SLOT.to_string(),
            resolution: SecretResolution::Current,
        };
        let secret = self.secrets.resolve(&binding).await.ok().flatten()?;
        let body = serde_json::to_string(payload).ok()?;
        Some(signing::sign(
            Timestamp::now_millis(),
            &body,
            secret.secret.as_bytes(),
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeliveryRunResult {
    pub delivered: usize,
    pub rejected: usize,
    pub retrying: usize,
    /// 这一轮补扫出多少条（见 `CallbackService::enqueue_missing`）。
    pub enqueued: usize,
}

/// 补扫的时间窗：只给这之内更新过的终态操作补发通知。
///
/// 为什么要有窗：补扫要覆盖"daemon 当时没在跑"的情况（这是它的价值），但如果不设窗，
/// 一个几周前的旧任务突然发通知会很吓人。24 小时足够覆盖重启与断电。
pub const CALLBACK_CATCH_UP_WINDOW_MILLIS: u64 = 24 * 60 * 60 * 1000;

pub fn callback_delivery_snapshot(delivery: &CallbackDeliveryRecord) -> Value {
    json!({
        "deliveryId": delivery.delivery_id,
        "operationId": delivery.operation_id.0,
        "eventId": delivery.event_id,
        "attempts": delivery.attempts,
        "maxAttempts": delivery.max_attempts,
        "nextRetryAt": delivery.next_retry_at.0,
        "completed": delivery.completed,
        "lastError": delivery.last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
    use crate::v2::domain::callback::CallbackSubscription;
    use crate::v2::domain::ids::{
        ClientId, ConnectionId, ConnectionRevisionId, OperationId, RecipeId, RecipeRevisionId,
    };
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility, Timestamp,
    };
    use crate::v2::infrastructure::fakes::{FakeCallbackDeliverer, InMemoryRepositories};
    use crate::v2::ports::callback::DeliveryOutcome;
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

    async fn create_operation_with_callback(repositories: &InMemoryRepositories) -> OperationId {
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
            },
            Box::new(crate::v2::infrastructure::fakes::PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        );
        let mut callback = CallbackSubscription::new("https://agent.example/cb", vec![]);
        callback.max_attempts = 3;
        let persisted = use_case
            .execute(CreateOperationRequest {
                actor: actor(),
                origin: "mcp".to_string(),
                idempotency_key: IdempotencyKey("callback-test-key".to_string()),
                runs: vec![RunRequest {
                    client_run_id: "image-1".to_string(),
                    recipe_revision_id: RecipeRevisionId(1),
                    connection_id: None,
                    inputs: BTreeMap::from([("prompt".to_string(), json!("a red house"))]),
                    assets: vec![],
                }],
                metadata: Default::default(),
                callback: Some(callback),
            })
            .await
            .expect("创建应成功");
        persisted.operation.id
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
            lifecycle_features: LifecycleFeatures::default(),
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

    fn callback_service(
        repositories: InMemoryRepositories,
        deliverer: Arc<FakeCallbackDeliverer>,
    ) -> CallbackService {
        callback_service_with_secrets(
            repositories,
            deliverer,
            Arc::new(InMemorySecrets::default()),
        )
    }

    /// 测试用密钥库：这里只关心"有没有那把签名密钥"。
    #[derive(Default)]
    struct InMemorySecrets(std::sync::Mutex<std::collections::HashMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretStore for InMemorySecrets {
        async fn resolve(
            &self,
            binding: &SecretBinding,
        ) -> DomainResult<Option<crate::v2::ports::stores::SecretValue>> {
            Ok(self.0.lock().unwrap().get(&binding.slot).map(|secret| {
                crate::v2::ports::stores::SecretValue {
                    secret: secret.clone(),
                }
            }))
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

    fn callback_service_with_secrets(
        repositories: InMemoryRepositories,
        deliverer: Arc<FakeCallbackDeliverer>,
        secrets: Arc<dyn SecretStore>,
    ) -> CallbackService {
        CallbackService {
            repositories: Arc::new(repositories.clone()),
            deliveries: Arc::new(repositories),
            deliverer,
            secrets,
        }
    }

    /// 跑一次完整的"入队 → 投递"，返回那次投递的请求（含签名头）。
    async fn deliver_once(secrets: Arc<dyn SecretStore>) -> DeliveryRequest {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let deliverer = Arc::new(FakeCallbackDeliverer::accepting());
        let service =
            callback_service_with_secrets(repositories.clone(), deliverer.clone(), secrets);
        let operation_id = create_operation_with_callback(&repositories).await;
        service
            .enqueue_terminal(operation_id, 1)
            .await
            .expect("入队应成功");
        service.deliver_due(10).await.expect("投递应成功");
        deliverer
            .delivered_requests()
            .into_iter()
            .next()
            .expect("应当有一次投递")
    }

    /// 配了密钥就签名、没配就不带——而且**按契约能验回来**。
    ///
    /// 负对照：把 `signature_for` 改成永远 `Some(...)` ⇒ 第一条断言红；
    /// 把签名对象从"序列化后的 body"改成别的（例如只签 payload 的某个字段）⇒
    /// 下面那次重算对不上 ⇒ 红。
    #[tokio::test]
    async fn deliveries_are_signed_only_when_a_key_is_configured() {
        let unsigned = deliver_once(Arc::new(InMemorySecrets::default())).await;
        assert!(
            unsigned.signature.is_none(),
            "没配密钥就不该带签名头——宁可不签，也不签一个假的"
        );

        let key = "a-shared-secret-only-the-human-knows";
        let secrets = Arc::new(InMemorySecrets::default());
        secrets
            .set(SIGNING_KEY_SLOT, key)
            .await
            .expect("写密钥应成功");
        let signed = deliver_once(secrets).await;
        let signature = signed.signature.expect("配了密钥就该带签名头");

        // ↓ 接收端视角：解析 t 与 v1，用同一个密钥重算
        let body = serde_json::to_string(&signed.payload).expect("载荷应当可序列化");
        let (timestamp, value) = signature
            .strip_prefix("t=")
            .and_then(|rest| rest.split_once(",v1="))
            .expect("签名头形状应为 t=…,v1=…");
        assert_eq!(
            signing::hex(&signing::hmac_sha256(
                key.as_bytes(),
                format!("{timestamp}.{body}").as_bytes()
            )),
            value,
            "接收端按契约重算应当一致（body 必须是实际发出去的那份序列化）"
        );
    }

    #[tokio::test]
    async fn enqueue_terminal_creates_delivery_for_matching_operation() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation_with_callback(&repositories).await;
        let deliverer = Arc::new(FakeCallbackDeliverer::accepting());
        let service = callback_service(repositories.clone(), deliverer.clone());
        service
            .enqueue_terminal(operation_id, 42)
            .await
            .expect("入队应成功");
        let deliveries = repositories.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].operation_id, operation_id);
        assert_eq!(deliveries[0].event_id, 42);
        assert_eq!(deliveries[0].max_attempts, 3);
    }

    #[tokio::test]
    async fn deliver_due_sends_and_marks_completed() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation_with_callback(&repositories).await;
        let deliverer = Arc::new(FakeCallbackDeliverer::accepting());
        let service = callback_service(repositories.clone(), deliverer.clone());
        service
            .enqueue_terminal(operation_id, 42)
            .await
            .expect("入队应成功");
        let result = service.deliver_due(10).await.expect("投递应成功");
        assert_eq!(result.delivered, 1);
        assert_eq!(deliverer.delivered_count(), 1);
        let deliveries = repositories.deliveries();
        assert!(deliveries[0].completed);
        assert_eq!(deliveries[0].attempts, 1);
    }

    #[tokio::test]
    async fn failed_delivery_retries_until_max_attempts() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation_with_callback(&repositories).await;
        let deliverer = Arc::new(FakeCallbackDeliverer::accepting());
        deliverer.set_outcome(DeliveryOutcome::NetworkError {
            message: "连接失败".to_string(),
            retryable: true,
        });
        repositories.set_delivery_retry_delay(0);
        let service = callback_service(repositories.clone(), deliverer.clone());
        service
            .enqueue_terminal(operation_id, 42)
            .await
            .expect("入队应成功");
        for _ in 0..3 {
            let result = service.deliver_due(10).await.expect("投递应成功");
            assert_eq!(result.retrying, 1);
        }
        // 第三次后达到 max_attempts=3 → completed
        let _ = service.deliver_due(10).await;
        let deliveries = repositories.deliveries();
        assert!(deliveries[0].completed);
        assert_eq!(deliveries[0].attempts, 3);
    }

    #[tokio::test]
    async fn operation_without_callback_creates_no_delivery() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let operation_id = create_operation(&repositories).await;
        let deliverer = Arc::new(FakeCallbackDeliverer::accepting());
        let service = callback_service(repositories.clone(), deliverer);
        service
            .enqueue_terminal(operation_id, 1)
            .await
            .expect("入队应成功");
        assert!(repositories.deliveries().is_empty());
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
            },
            Box::new(crate::v2::infrastructure::fakes::PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        );
        let persisted = use_case
            .execute(CreateOperationRequest {
                actor: actor(),
                origin: "mcp".to_string(),
                idempotency_key: IdempotencyKey("callback-none-key".to_string()),
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
}
