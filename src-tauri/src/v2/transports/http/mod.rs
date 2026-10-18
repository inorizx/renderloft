#![allow(clippy::result_large_err)]
pub mod dto;
pub mod error;
pub mod handlers;
pub mod json;
pub mod shell;

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;

use crate::v2::application::cancel_operation::CancelOperation;
use crate::v2::application::create_operation::CreateOperation;
use crate::v2::application::recipe_import::RecipeImportService;
use crate::v2::application::settings::{RunningHubStatusService, SettingsService};
use crate::v2::application::validate_operation::ValidateOperation;
use crate::v2::application::workspace_backup::WorkspaceBackupService;
use crate::v2::domain::actor::ActorRef;
use crate::v2::ports::provider::DriverRegistry;
use crate::v2::ports::repositories::{
    ArtifactRepository, AssetRepository, ConnectionRepository, EventStore, OperationRepository,
    RecipeRepository,
};
use crate::v2::ports::safety::SafeHttpClient;
use crate::v2::ports::stores::{ArtifactStore, SecretStore};
use crate::v2::ports::token::TokenService;

pub struct V2HttpState {
    pub tokens: Arc<dyn TokenService>,
    pub secrets: Arc<dyn SecretStore>,
    pub recipes: Arc<dyn RecipeRepository>,
    pub connections: Arc<dyn ConnectionRepository>,
    pub operations: Arc<dyn OperationRepository>,
    pub events: Arc<dyn EventStore>,
    pub assets: Arc<dyn AssetRepository>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub artifacts_repo: Arc<dyn ArtifactRepository>,
    pub create_operation: Arc<CreateOperation>,
    /// 只读预检（dry-run）：与 create 共用解析，不落库、不调度、不花钱。
    pub validate_operation: Arc<ValidateOperation>,
    pub cancel_operation: Arc<CancelOperation>,
    pub drivers: Arc<DriverRegistry>,
    pub settings: Arc<SettingsService>,
    pub status: Arc<RunningHubStatusService>,
    pub import_service: Arc<RecipeImportService>,
    /// 受限的远端 HTTP 读取（产物/结果 URL 拉取，内置 SSRF 防护）。
    pub safe_http: Arc<dyn SafeHttpClient>,
    /// 迁移/备份：把工作区一致地导出一份（见 `application/workspace_backup.rs`）。
    pub workspace_backup: Arc<WorkspaceBackupService>,
}

impl V2HttpState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tokens: Arc<dyn TokenService>,
        secrets: Arc<dyn SecretStore>,
        recipes: Arc<dyn RecipeRepository>,
        connections: Arc<dyn ConnectionRepository>,
        operations: Arc<dyn OperationRepository>,
        events: Arc<dyn EventStore>,
        assets: Arc<dyn AssetRepository>,
        artifacts: Arc<dyn ArtifactStore>,
        artifacts_repo: Arc<dyn ArtifactRepository>,
        create_operation: Arc<CreateOperation>,
        validate_operation: Arc<ValidateOperation>,
        cancel_operation: Arc<CancelOperation>,
        drivers: Arc<DriverRegistry>,
        settings: Arc<SettingsService>,
        status: Arc<RunningHubStatusService>,
        import_service: Arc<RecipeImportService>,
        safe_http: Arc<dyn SafeHttpClient>,
        workspace_backup: Arc<WorkspaceBackupService>,
    ) -> Self {
        Self {
            tokens,
            secrets,
            recipes,
            connections,
            operations,
            events,
            assets,
            artifacts,
            artifacts_repo,
            create_operation,
            validate_operation,
            cancel_operation,
            drivers,
            settings,
            status,
            import_service,
            safe_http,
            workspace_backup,
        }
    }
}

/// `POST /api/v1/assets` 的请求体上限。
///
/// 32 MB：够放常见的图（几 MB）与音频参考（十几 MB），又不至于让一次请求把内存吃穿。
/// 平台侧对素材有自己的上限，超了会在提交时被拒——那是另一层的错，这里只管"别在传输层先死"。
const ASSET_UPLOAD_LIMIT_BYTES: usize = 32 * 1024 * 1024;

pub fn build_router(state: Arc<V2HttpState>) -> Router {
    let protected = Router::new()
        .route("/api/v1/capabilities", get(handlers::capabilities))
        .route(
            "/api/v1/recipes",
            get(handlers::list_recipes).post(handlers::create_recipe),
        )
        .route(
            "/api/v1/recipes/{recipe_id}",
            get(handlers::get_recipe)
                .put(handlers::update_recipe)
                .delete(handlers::delete_recipe),
        )
        .route(
            "/api/v1/recipes/{recipe_id}/schema",
            get(handlers::get_recipe_schema),
        )
        .route(
            "/api/v1/recipes/{recipe_id}/touch",
            post(handlers::touch_recipe),
        )
        .route(
            "/api/v1/recipes/{recipe_id}/restore",
            post(handlers::restore_recipe),
        )
        .route("/api/v1/recipes/import", post(handlers::import_recipe))
        .route(
            "/api/v1/recipes/import-workflow",
            post(handlers::import_workflow),
        )
        .route(
            "/api/v1/recipes/comfyui-import",
            post(handlers::import_comfyui_recipe),
        )
        .route(
            "/api/v1/recipes/comfyui-preview",
            post(handlers::preview_comfyui_fields),
        )
        .route(
            "/api/v1/recipes/comfyui-fetch-workflow",
            post(handlers::fetch_comfyui_workflow),
        )
        .route(
            "/api/v1/operations",
            get(handlers::list_operations).post(handlers::create_operation),
        )
        .route(
            // 静态段 `validate` 与动态段 `/operations/{operation_id}` 同层：axum（matchit）
            // 保证静态路径优先，`validate` 不会被当成一个 operation_id。
            // 这条路由是**只读预检**：不落库、不调度、不花额度。
            "/api/v1/operations/validate",
            post(handlers::validate_operation),
        )
        .route(
            "/api/v1/operations/{operation_id}",
            get(handlers::get_operation).delete(handlers::delete_operation),
        )
        .route(
            "/api/v1/operations/{operation_id}/retry",
            post(handlers::retry_operation),
        )
        .route(
            "/api/v1/operations/{operation_id}/runs/{run_id}/retry",
            post(handlers::retry_run),
        )
        .route(
            "/api/v1/operations/{operation_id}/cancel",
            // ⚠️ 必须是 POST：cancel 会改变状态。此前注册为 GET，与同组的
            // retry(POST) / runs/{id}/retry(POST) / delete(DELETE) 不一致，
            // 而 GET 承载变更违反 HTTP 语义（预取 / 缓存 / 代理都可能误触发）。
            // 改方法要**同批**改：OpenAPI 声明表、前端 v2.ts、Rust client.rs、以及相关测试。
            post(handlers::cancel_operation),
        )
        .route(
            "/api/v1/operations/{operation_id}/events",
            get(handlers::operation_events),
        )
        .route("/api/v1/runs/{run_id}", get(handlers::get_run))
        .route(
            "/api/v1/assets",
            // 上传是**唯一**一条 body 是原始二进制的路由，所以要单独放开请求体上限：
            // axum 默认只有 2 MB，而一张图轻松超过它（实测 2.36 MB 就被拒，报的却是
            // `INVALID_REQUEST_BODY`）。其余路由保持默认——别顺手把全局上限抬高。
            post(handlers::create_asset).layer(DefaultBodyLimit::max(ASSET_UPLOAD_LIMIT_BYTES)),
        )
        .route("/api/v1/assets/{asset_id}", get(handlers::get_asset))
        .route(
            "/api/v1/assets/{asset_id}/bytes",
            get(handlers::get_asset_bytes),
        )
        .route("/api/v1/artifacts", get(handlers::list_artifacts))
        .route(
            "/api/v1/artifacts/{artifact_id}",
            get(handlers::get_artifact),
        )
        .route(
            "/api/v1/artifacts/{artifact_id}/bytes",
            get(handlers::get_artifact_bytes),
        )
        .route(
            "/api/v1/artifacts/{artifact_id}/save",
            post(handlers::save_artifact),
        )
        .route(
            "/api/v1/connections",
            get(handlers::list_connections).post(handlers::create_connection),
        )
        .route(
            "/api/v1/connections/{connection_id}",
            get(handlers::get_connection)
                .put(handlers::update_connection)
                .delete(handlers::delete_connection),
        )
        .route(
            "/api/v1/connections/{connection_id}/check",
            get(handlers::check_connection),
        )
        .route(
            "/api/v1/connections/{connection_id}/models",
            get(handlers::list_connection_models),
        )
        .route(
            "/api/v1/actor-tokens",
            get(handlers::list_tokens).post(handlers::issue_token),
        )
        .route(
            "/api/v1/actor-tokens/{token_id}",
            delete(handlers::revoke_token),
        )
        .route(
            "/api/v1/settings",
            get(shell::get_settings).put(shell::save_settings),
        )
        .route(
            "/api/v1/settings/open-download-dir",
            post(shell::open_download_dir),
        )
        .route(
            "/api/v1/callback-secret",
            get(shell::callback_secret_status)
                .put(shell::set_callback_secret)
                .delete(shell::clear_callback_secret),
        )
        .route("/api/v1/diagnostics", get(shell::diagnostics))
        .route("/api/v1/runninghub/account", get(shell::runninghub_account))
        .route("/api/v1/runninghub/queue", get(shell::runninghub_queue))
        .route("/api/v1/workspace/backup", post(handlers::backup_workspace))
        .route("/api/v1/openapi.json", get(handlers::openapi))
        .fallback(api_not_found)
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));

    Router::new()
        .route("/api/v1/health", get(handlers::health))
        .merge(protected)
        .with_state(state)
}

async fn api_not_found(request: Request) -> axum::response::Response {
    let path = request.uri().path();
    if path.starts_with("/api/") {
        error::not_found("ROUTE_NOT_FOUND", "接口不存在")
    } else {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn auth(
    State(state): State<Arc<V2HttpState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            // 生产模式（daemon 直接伺服控制台）走 bootstrap 一次性 URL 设置
            // 的 HttpOnly 会话 cookie；dev 模式由 vite 代理注入 Bearer。
            request
                .headers()
                .get(axum::http::header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| {
                    value
                        .split(';')
                        .map(str::trim)
                        .find_map(|part| part.strip_prefix("renderloft_session="))
                })
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    let Some(token) = token else {
        return error::unauthorized("缺少 Bearer 认证令牌");
    };
    match state.tokens.authenticate(token).await {
        Ok(actor) => {
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Err(error) => error::error_response(error),
    }
}

pub fn actor_from_request(request: &Request) -> Option<ActorRef> {
    request.extensions().get::<ActorRef>().cloned()
}

pub fn require_scope(
    actor: &ActorRef,
    scope: crate::v2::domain::actor::Scope,
) -> Result<(), Response> {
    if actor.has_scope(scope) {
        Ok(())
    } else {
        Err(error::forbidden(format!("缺少 {scope:?} 权限")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::application::cancel_operation::CancelOperation;
    use crate::v2::application::create_operation::{CreateOperation, Repositories};
    use crate::v2::domain::actor::{ActorKind, Scope};
    use crate::v2::domain::execution::ProviderDescriptor;
    use crate::v2::domain::ids::{ConnectionId, ConnectionRevisionId, RecipeId, RecipeRevisionId};
    use crate::v2::domain::media::{MediaKind, MediaObject, MediaObjectId};
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility,
    };
    use crate::v2::infrastructure::fakes::{
        FakeProviderDriver, InMemoryRepositories, PermitAllAuthorizationPolicy,
    };
    use crate::v2::infrastructure::schema::BasicSchemaValidator;
    use crate::v2::ports::provider::{driver_key, DriverRegistry};
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use tower::ServiceExt;

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
            config: BTreeMap::from([("baseUrl".to_string(), json!("https://api.openai.com/v1"))]),
            secret_bindings: vec![],
            effective_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures {
                synchronous_result: true,
                ..Default::default()
            },
            created_at: crate::v2::domain::model::Timestamp::now(),
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
            created_at: crate::v2::domain::model::Timestamp::now(),
            updated_at: crate::v2::domain::model::Timestamp::now(),
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
                // 与生产一致：`RecipeBinding.driver_key` 的权威形态是
                // `v2::ports::provider::driver_key(vendor, protocol)` = "vendor/protocol"
                // （与 `DriverRegistry` 的注册键同形，见下方 `drivers.register(...)`）。
                // 夹具原先写的是裸键 "openai-images"，与生产不符——那正是前端类型标签
                // 一度判错方向的根源之一，故一并纠正。
                driver_key: driver_key("openai", "openai-images"),
                provider_config: Default::default(),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: ConnectionOverride::Denied,
            },
            defaults: Default::default(),
            source: RecipeSource::Embedded,
            created_at: crate::v2::domain::model::Timestamp::now(),
        };
        repositories.seed_recipe(recipe, vec![revision]);
    }

    /// 一条 RunningHub"密钥宿主"连接（凭据引用的被引用方）。
    fn seed_runninghub_connection(repositories: &InMemoryRepositories, id: u64) {
        repositories.seed_connection(
            Connection {
                id: ConnectionId(id),
                vendor: "runninghub".to_string(),
                protocol: "runninghub".to_string(),
                display_name: "RunningHub".to_string(),
                current_revision_id: ConnectionRevisionId(id * 100),
                status: crate::v2::domain::model::ConnectionStatus::Active,
            },
            vec![ConnectionRevision {
                id: ConnectionRevisionId(id * 100),
                connection_id: ConnectionId(id),
                config: BTreeMap::from([(
                    "baseUrl".to_string(),
                    json!("https://www.runninghub.cn"),
                )]),
                secret_bindings: vec![crate::v2::domain::model::SecretBinding {
                    slot: format!("connection-{id}-api_key"),
                    resolution: crate::v2::domain::model::SecretResolution::Current,
                }],
                effective_capabilities: vec!["workflow-execution".to_string()],
                lifecycle_features: LifecycleFeatures::default(),
                created_at: crate::v2::domain::model::Timestamp::now(),
            }],
        );
    }

    /// 默认装配。
    fn test_app() -> (Router, InMemoryRepositories) {
        let (app, repositories, _state) = test_app_with();
        (app, repositories)
    }

    /// 装配 + 一个"平台声明了这些模型"的驱动（模型发现的用例用）。
    fn test_app_with_models(models: Vec<&str>) -> (Router, InMemoryRepositories) {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let mut drivers = DriverRegistry::new();
        let driver = std::sync::Arc::new(
            FakeProviderDriver::new(ProviderDescriptor {
                vendor_id: "openai".to_string(),
                protocol_id: "openai-images".to_string(),
                display_name: "OpenAI Images".to_string(),
                business_capabilities: vec!["text-to-image".to_string()],
                lifecycle_features: LifecycleFeatures::default(),
                security_requirements: Default::default(),
                connection_schema: json!({}),
                binding_schema: json!({}),
            })
            .with_models(models.into_iter().map(str::to_string).collect()),
        );
        drivers.register(driver_key("openai", "openai-images"), driver);
        let state = test_state(repositories.clone(), drivers);
        (build_router(state), repositories)
    }

    /// 默认装配 + 交出 state（需要订阅事件总线、或直接调服务造前置状态时用）。
    fn test_app_with_state() -> (Router, InMemoryRepositories, Arc<V2HttpState>) {
        test_app_with()
    }

    fn test_app_with() -> (Router, InMemoryRepositories, Arc<V2HttpState>) {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let mut drivers = DriverRegistry::new();
        let driver = std::sync::Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        drivers.register(driver_key("openai", "openai-images"), driver);
        let state = test_state(repositories.clone(), drivers);
        (build_router(state.clone()), repositories, state)
    }

    /// 用给定的仓库与驱动注册表装配 state（要换驱动时用它，别复制一整套）。
    fn test_state(repositories: InMemoryRepositories, drivers: DriverRegistry) -> Arc<V2HttpState> {
        let create = std::sync::Arc::new(CreateOperation::new(
            Repositories {
                recipes: Box::new(repositories.clone()),
                connections: Box::new(repositories.clone()),
                operations: Box::new(repositories.clone()),
                assets: Box::new(repositories.clone()),
            },
            Box::new(PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        ));
        let validate = std::sync::Arc::new(ValidateOperation::new(create.clone()));
        let cancel = std::sync::Arc::new(CancelOperation {
            repositories: std::sync::Arc::new(repositories.clone()),
            connections: std::sync::Arc::new(repositories.clone()),
            drivers: std::sync::Arc::new(drivers.clone()),
        });
        let settings = std::sync::Arc::new(crate::v2::application::settings::SettingsService::new(
            std::sync::Arc::new(repositories.clone()),
        ));
        let status = std::sync::Arc::new(
            crate::v2::application::settings::RunningHubStatusService::new(
                std::sync::Arc::new(repositories.clone()),
                std::sync::Arc::new(drivers.clone()),
                std::sync::Arc::new(crate::v2::daemon::NoopSecretStore),
            ),
        );
        let import_service = std::sync::Arc::new(
            crate::v2::application::recipe_import::RecipeImportService::new(
                std::sync::Arc::new(repositories.clone()),
                std::sync::Arc::new(repositories.clone()),
                std::sync::Arc::new(drivers.clone()),
            ),
        );
        // 替身库没有真库文件：它的 `snapshot_database` 只保证"目标位置会出现一个库文件"，
        // 一致性由 `application::workspace_backup` 里那条**真库**测试钉住。
        let workspace_backup = std::sync::Arc::new(
            crate::v2::application::workspace_backup::WorkspaceBackupService::new(
                std::sync::Arc::new(repositories.clone())
                    as std::sync::Arc<dyn crate::v2::ports::repositories::WorkspaceRepository>,
            ),
        );
        std::sync::Arc::new(V2HttpState::new(
            std::sync::Arc::new(crate::v2::infrastructure::token_service::TokenService::new(
                std::sync::Arc::new(repositories.clone()),
            )),
            std::sync::Arc::new(crate::v2::daemon::NoopSecretStore),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            create,
            validate,
            cancel,
            std::sync::Arc::new(drivers),
            settings,
            status,
            import_service,
            std::sync::Arc::new(
                crate::v2::infrastructure::providers::safety::ReqwestSafeHttpClient,
            ),
            workspace_backup,
        ))
    }

    async fn issue_token(repositories: &InMemoryRepositories, scopes: &[&str]) -> String {
        let service = crate::v2::infrastructure::token_service::TokenService::new(
            std::sync::Arc::new(repositories.clone()),
        );
        let (token, _) = service
            .issue(
                "admin",
                ActorKind::Gui,
                "ui",
                scopes
                    .iter()
                    .map(|scope| match *scope {
                        "recipes:read" => Scope::RecipesRead,
                        "recipes:write" => Scope::RecipesWrite,
                        "operations:read" => Scope::OperationsRead,
                        "operations:execute" => Scope::OperationsExecute,
                        "operations:cancel" => Scope::OperationsCancel,
                        "assets:local-read" => Scope::AssetsLocalRead,
                        "assets:write" => Scope::AssetsWrite,
                        "artifacts:read" => Scope::ArtifactsRead,
                        "connections:read" => Scope::ConnectionsRead,
                        "connections:manage" => Scope::ConnectionsManage,
                        "settings:read" => Scope::SettingsRead,
                        "settings:manage" => Scope::SettingsManage,
                        "diagnostics:read" => Scope::DiagnosticsRead,
                        "actor-tokens:manage" => Scope::ActorTokensManage,
                        "agents:read" => Scope::AgentsRead,
                        "agents:write" => Scope::AgentsWrite,
                        "agents:run" => Scope::AgentsRun,
                        "deliveries:read" => Scope::DeliveriesRead,
                        "deliveries:run" => Scope::DeliveriesRun,
                        // 写错名字就**当场炸**：此前这里是 `_ => Scope::SettingsRead`，
                        // 于是一个拼错的 scope 会静默变成"设置只读"，测试拿到 403 却查不出原因
                        //（实测绕了一圈：`assets:read` 应为 `assets:local-read`）。
                        other => panic!("issue_token 不认识的 scope：{other}"),
                    })
                    .collect(),
                None,
                None,
            )
            .await
            .expect("签发应成功");
        token
    }

    async fn get_json(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post_json(app: &Router, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// 上传是**唯一**一条 body 是原始二进制的路由，所以要一个发字节的助手。
    async fn post_bytes(
        app: &Router,
        uri: &str,
        token: &str,
        bytes: Vec<u8>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        // 上限比被测的上限还大：这里读的是**响应**，别让读响应把测试卡死。
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn create_operation_body(key: &str) -> Value {
        json!({
            "idempotencyKey": key,
            "runs": [{
                "clientRunId": "image-1",
                "recipeRevisionId": 1,
                "inputs": { "prompt": "a red house" }
            }]
        })
    }

    #[tokio::test]
    async fn health_is_public_without_token() {
        let (app, _repositories) = test_app();
        let request = Request::builder()
            .uri("/api/v1/health")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_route_requires_token() {
        let (app, _repositories) = test_app();
        let request = Request::builder()
            .uri("/api/v1/capabilities")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_token_returns_json_error() {
        let (app, _repositories) = test_app();
        let request = Request::builder()
            .uri("/api/v1/capabilities")
            .header(header::AUTHORIZATION, "Bearer rh-invalid")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["code"], "TOKEN_INVALID");
    }

    #[tokio::test]
    async fn missing_scope_returns_403() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["recipes:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/operations/1", &token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "SCOPE_REQUIRED");
    }

    #[tokio::test]
    async fn create_operation_via_http() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("http-key-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["status"], "QUEUED");
        let operation_id = body["data"]["operationId"].as_u64().unwrap();
        assert!(operation_id > 0);
        let (status, detail) =
            get_json(&app, &format!("/api/v1/operations/{operation_id}"), &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["data"]["status"], "QUEUED");
        assert_eq!(detail["data"]["runs"][0]["clientRunId"], "image-1");
    }

    /// **详情接口必须真的带上每条 run 的 `inputs`** —— 因为服务出去的文案早就承诺了它。
    ///
    /// `renderloft_get_operation` 的描述写着"每条 run 都带提交时的 `inputs`"（cookbook §11 的恢复
    /// 建议也建立在它之上：读 `inputs` 与 `error` 一起读、改对了再重投），而 `run_response` **从来
    /// 没发过这个字段**（2026-10-18 实测发现：给预览框做"结果按输入分组"时界面拿不到 inputs，
    /// 回头一查是这里漏了）。调用方照文案去做会**找不到** `inputs`，而它正处在"要不要再花一次钱"
    /// 的判断里 —— 这正是本项目一直在消灭的那类问题：**接口说了它没做的事**。
    ///
    /// 判别点两条（一正一反）：
    /// 1. 详情里每条 run 的 `inputs` 与**提交时那份逐字相等**（不是空对象、不是改名）；
    /// 2. **列表接口仍然只回摘要**：列表按设计不带 runs（`runCount` + `attention`），
    ///    所以也不该出现 `inputs` —— 加字段最容易顺手越过的一条边界。
    ///
    /// 负对照：把 `run_response` 里的 `"inputs"` 删掉 ⇒ 第 1 条红。
    #[tokio::test]
    async fn operation_detail_carries_each_runs_submitted_inputs() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("http-key-inputs"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let operation_id = body["data"]["operationId"].as_u64().unwrap();
        let (status, detail) =
            get_json(&app, &format!("/api/v1/operations/{operation_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(
            detail["data"]["runs"][0]["inputs"]["prompt"], "a red house",
            "详情必须带上提交时那份 inputs：{detail}"
        );
        // 单条 run 的端点走同一个 `run_response`，所以也必须有。
        let run_id = detail["data"]["runs"][0]["runId"].as_u64().unwrap();
        let (status, one) = get_json(&app, &format!("/api/v1/runs/{run_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{one}");
        assert_eq!(one["data"]["inputs"]["prompt"], "a red house", "{one}");

        // ② 列表仍然只是摘要：不带 runs、更不带 inputs。
        let (status, list) = get_json(&app, "/api/v1/operations?limit=10", &token).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let item = &list["data"]["items"][0];
        assert!(item["runCount"].is_number(), "{list}");
        assert!(
            item.get("runs").is_none() && item.get("inputs").is_none(),
            "列表按设计只回摘要，不许把 inputs 泄进去：{list}"
        );
    }

    /// **"用这套参数再跑一次"所需的两件事**：每条 run 的 `recipeId` 与它的 `assets`。
    ///
    /// 为什么是这两件（产品愿景「批量是核心能力」里"保存参数、复用历史"那一级的实现前提）：
    /// 界面要跳到 `/library/:recipeId` 并把那次提交的参数**预填**回表单。只有
    /// `recipeRevisionId` 是跳不过去的（路由按应用 id）；而图生图少了 `assets` 就是**静默丢图**
    /// ——用户会以为"用的还是上次那张"，实际提交的是另一次。两者都属于"界面说了它没做"的邻居。
    ///
    /// 判别点四条：
    /// 1. `recipeId` 是**这条 run 自己的**应用（由它自己的修订反查，不是"第一个 run 的"）；
    /// 2. `assets` 的形状与 `POST /operations/validate` 的解析结果**一致**
    ///    （`{fieldKey, assetId, checksum}`）——同一个概念在两条读路径上只有一种形状；
    /// 3. `checksum` 必须在（少了它，重投会被 `create` 的校验挡下："wrong-checksum" 是明确拒绝的）；
    /// 4. 负对照见下：删掉 `assets` 那段 ⇒ 第 2 条红。
    #[tokio::test]
    async fn operation_detail_carries_recipe_id_and_assets_for_reuse() {
        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 7, "sum-person");
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            json!({
                "idempotencyKey": "http-key-reuse",
                "runs": [{
                    "clientRunId": "look-1",
                    "recipeRevisionId": 2,
                    "inputs": { "prompt": "a red house" },
                    "assets": { "475::image": { "assetId": 7, "checksum": "sum-person" } }
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let operation_id = body["data"]["operationId"].as_u64().unwrap();
        let (status, detail) =
            get_json(&app, &format!("/api/v1/operations/{operation_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        let run = &detail["data"]["runs"][0];
        assert_eq!(run["recipeRevisionId"], 2, "{detail}");
        assert_eq!(
            run["recipeId"], 1,
            "必须给出这条 run 自己的应用 id（界面靠它跳到表单）：{detail}"
        );
        assert_eq!(run["assets"][0]["fieldKey"], "475::image", "{detail}");
        assert_eq!(run["assets"][0]["assetId"], 7, "{detail}");
        assert_eq!(
            run["assets"][0]["checksum"], "sum-person",
            "checksum 少了，重投会被 create 的校验挡下：{detail}"
        );
        // 列表仍然只是摘要：没有 runs，也就没有 recipeId/assets 可泄。
        let (status, list) = get_json(&app, "/api/v1/operations?limit=10", &token).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let item = &list["data"]["items"][0];
        assert!(
            item.get("runs").is_none() && item.get("recipeId").is_none(),
            "列表按设计只回摘要：{list}"
        );
    }

    #[tokio::test]
    async fn idempotent_replay_returns_same_operation() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let (_, first) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("http-key-2"),
        )
        .await;
        let (_, second) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("http-key-2"),
        )
        .await;
        assert_eq!(first["data"]["operationId"], second["data"]["operationId"]);
    }

    #[tokio::test]
    async fn idempotency_conflict_returns_409() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let (_, _) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("http-key-3"),
        )
        .await;
        let mut changed = create_operation_body("http-key-3");
        changed["runs"][0]["inputs"]["prompt"] = json!("a blue house");
        let (status, body) = post_json(&app, "/api/v1/operations", &token, changed).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "IDEMPOTENCY_CONFLICT");
    }

    /// 媒体配方（用户场景的 4 槽换装）：把 recipe 1 换成带媒体槽位的 revision 2。
    /// `keyed` 选映射形状：true = 新字段键，false = 旧 asset-N（历史 revision）。
    fn seed_media_recipe(repositories: &InMemoryRepositories, keyed: bool) {
        let mapping = if keyed {
            json!({
                "475::image": { "node": "475", "field": "image" },
                "519::image": { "node": "519", "field": "image" },
                "520::image": { "node": "520", "field": "image" },
                "521::image": { "node": "521", "field": "image" }
            })
        } else {
            json!({
                "asset-0": { "node": "475", "field": "image" },
                "asset-1": { "node": "519", "field": "image" },
                "asset-2": { "node": "520", "field": "image" },
                "asset-3": { "node": "521", "field": "image" }
            })
        };
        let recipe = Recipe {
            id: RecipeId(1),
            name: "Image Recipe".to_string(),
            description: "media recipe".to_string(),
            labels: vec![],
            capability: "text-to-image".to_string(),
            current_revision_id: RecipeRevisionId(2),
            visibility: RecipeVisibility::Private,
            created_at: crate::v2::domain::model::Timestamp::now(),
            updated_at: crate::v2::domain::model::Timestamp::now(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(2),
            recipe_id: RecipeId(1),
            revision: 2,
            input_schema: json!({
                "type": "object",
                "required": [],
                "properties": {
                    "475::image": { "type": "string", "format": "file" },
                    "519::image": { "type": "string", "format": "file" },
                    "520::image": { "type": "string", "format": "file" },
                    "521::image": { "type": "string", "format": "file" },
                    "prompt": { "type": "string" }
                }
            }),
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: driver_key("openai", "openai-images"),
                provider_config: BTreeMap::from([("mediaMapping".to_string(), mapping)]),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: ConnectionOverride::Denied,
            },
            defaults: Default::default(),
            source: RecipeSource::Embedded,
            created_at: crate::v2::domain::model::Timestamp::now(),
        };
        repositories.seed_recipe(recipe, vec![revision]);
    }

    fn seed_media_asset(repositories: &InMemoryRepositories, id: u64, checksum: &str) {
        use crate::v2::domain::ids::AssetId;
        repositories.seed_asset(
            AssetId(id),
            MediaObject {
                id: MediaObjectId::Asset(AssetId(id)),
                kind: MediaKind::Asset,
                filename: format!("asset-{id}.png"),
                mime_type: "image/png".to_string(),
                byte_size: 3,
                checksum: checksum.to_string(),
                storage_key: None,
                provider_refs: vec![],
            },
            vec![1, 2, 3],
        );
    }

    /// 回归（用户场景）：HTTP keyed 形状只落这两个槽位；旧数组在 keyed revision 上被明确拒绝。
    #[tokio::test]
    async fn keyed_assets_over_http_land_on_exactly_those_slots() {
        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 7, "sum-person");
        seed_media_asset(&repositories, 8, "sum-cloth");
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            json!({
                "idempotencyKey": "media-key-1",
                "runs": [{
                    "clientRunId": "look-1",
                    "recipeRevisionId": 2,
                    "inputs": {},
                    "assets": {
                        "475::image": { "assetId": 7, "checksum": "sum-person" },
                        "520::image": { "assetId": 8, "checksum": "sum-cloth" }
                    }
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored = repositories
            .find_operation(crate::v2::domain::ids::OperationId(1))
            .await
            .expect("查询应成功")
            .expect("应已落库");
        assert_eq!(stored.runs[0].assets.len(), 2);
        let keys: Vec<&str> = stored.runs[0]
            .assets
            .iter()
            .map(|asset| asset.field_key.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(keys, vec!["475::image", "520::image"]);

        // keyed revision 收到位置数组 ⇒ 400 POSITIONAL_ASSETS_UNSUPPORTED。
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            json!({
                "idempotencyKey": "media-key-2",
                "runs": [{
                    "clientRunId": "look-2",
                    "recipeRevisionId": 2,
                    "inputs": {},
                    "assets": [{ "assetId": 7, "checksum": "sum-person" }]
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "POSITIONAL_ASSETS_UNSUPPORTED");
    }

    /// 旧 revision + 旧位置数组仍然可用（兼容），且第一个元素落第一个槽位（475）。
    #[tokio::test]
    async fn legacy_positional_array_over_http_still_works() {
        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, false);
        seed_media_asset(&repositories, 7, "sum-person");
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            json!({
                "idempotencyKey": "legacy-key-1",
                "runs": [{
                    "clientRunId": "look-1",
                    "recipeRevisionId": 2,
                    "inputs": {},
                    "assets": [{ "assetId": 7, "checksum": "sum-person" }]
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored = repositories
            .find_operation(crate::v2::domain::ids::OperationId(1))
            .await
            .expect("查询应成功")
            .expect("应已落库");
        assert_eq!(
            stored.runs[0].assets[0].field_key.as_deref(),
            Some("475::image")
        );
    }

    /// `POST /operations/validate`：报出解析结果（字段键→assetId），**不落库**。
    #[tokio::test]
    async fn validate_endpoint_is_read_only_and_reports_resolved_assets() {
        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 7, "sum-person");
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations/validate",
            &token,
            json!({
                "runs": [{
                    "clientRunId": "look-1",
                    "recipeRevisionId": 2,
                    "inputs": {},
                    "assets": { "475::image": { "assetId": 7, "checksum": "sum-person" } }
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["dryRun"], true);
        assert_eq!(body["data"]["valid"], true);
        assert_eq!(body["data"]["runs"][0]["clientRunId"], "look-1");
        assert_eq!(body["data"]["runs"][0]["recipeRevisionId"], 2);
        assert_eq!(
            body["data"]["runs"][0]["assets"][0]["fieldKey"],
            "475::image"
        );
        assert_eq!(body["data"]["runs"][0]["assets"][0]["assetId"], 7);
        // 不落库：列表为空。
        let (status, listed) = get_json(&app, "/api/v1/operations", &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            listed["data"]["items"].as_array().unwrap().len(),
            0,
            "{listed}"
        );
    }

    /// 预检失败时给与提交**同码**的错误（这里：未知字段键）。
    #[tokio::test]
    async fn validate_endpoint_reports_stable_codes() {
        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 7, "sum-person");
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations/validate",
            &token,
            json!({
                "runs": [{
                    "clientRunId": "look-1",
                    "recipeRevisionId": 2,
                    "inputs": {},
                    "assets": { "999::image": { "assetId": 7, "checksum": "sum-person" } }
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "ASSET_FIELD_UNKNOWN");
    }

    /// 预检与提交同一道闸：只有读权限的 token 不能预检。
    #[tokio::test]
    async fn validate_endpoint_requires_execute_scope() {
        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        let token = issue_token(&repositories, &["operations:read"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/operations/validate",
            &token,
            json!({ "runs": [{ "clientRunId": "x", "recipeRevisionId": 2, "inputs": {} }] }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    /// MCP 的 dry-run 端到端：**动词层真的把 dryRun 传到了** `/operations/validate`，
    /// 拿回解析结论，不落库。
    #[tokio::test]
    async fn mcp_dry_run_reaches_validate_and_persists_nothing() {
        use crate::v2::clients::client::V2Client;
        use crate::v2::clients::verbs::{self, Args};

        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 7, "sum-person");
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定端口");
        let address = listener.local_addr().expect("端口地址");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client =
            V2Client::new(format!("http://127.0.0.1:{}", address.port()), token).with_driver("mcp");

        let verb = verbs::find_by_id("operation.run").expect("operation.run");
        let args = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 2,
                "inputs": {},
                "assets": { "475::image": { "assetId": 7, "checksum": "sum-person" } },
                "dryRun": true,
            }),
        )
        .expect("参数应当合法");
        let report = verbs::execute(&client, verb, &args)
            .await
            .expect("dry-run 应当成功");
        assert_eq!(report["dryRun"], true, "{report}");
        assert_eq!(report["valid"], true, "{report}");
        assert!(
            report["runs"][0]["clientRunId"]
                .as_str()
                .unwrap_or_default()
                .starts_with("run-"),
            "{report}"
        );
        assert_eq!(
            report["runs"][0]["assets"][0]["fieldKey"], "475::image",
            "{report}"
        );
        assert_eq!(report["runs"][0]["assets"][0]["assetId"], 7, "{report}");
        assert!(
            crate::v2::ports::repositories::OperationRepository::list_operations(
                &repositories,
                10,
                None
            )
            .await
            .expect("列举")
            .is_empty(),
            "dry-run 不许落库"
        );
    }

    /// 工作区备份：**要 `settings:manage`**；被拒时**不写任何东西**，允许时给出报告。
    ///
    /// 替身库没有真库文件，所以这条只钉"路由 + 权限 + 报告形状"；"备份出来的库真能重新打开"
    /// 由 `application::workspace_backup` 里那条**真库**测试钉住。
    #[tokio::test]
    async fn workspace_backup_requires_settings_manage_and_reports_what_landed() {
        let (app, repositories) = test_app();
        let target =
            std::env::temp_dir().join(format!("renderloft-http-backup-{}", uuid::Uuid::new_v4()));
        let body = json!({ "to": target.to_string_lossy() });

        // 权限不足：403，而且目标目录**一个字都没写**。
        let weak = issue_token(&repositories, &["diagnostics:read"]).await;
        let (status, payload) =
            post_json(&app, "/api/v1/workspace/backup", &weak, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{payload}");
        assert_eq!(payload["error"]["code"], "SCOPE_REQUIRED");
        assert!(!target.exists(), "被拒的请求不该写任何东西");

        // 有权限：报告里要有调用方需要的事实（备份落在哪、里面有多少东西）。
        let token = issue_token(&repositories, &["settings:manage"]).await;
        let (status, payload) = post_json(&app, "/api/v1/workspace/backup", &token, body).await;
        assert_eq!(status, StatusCode::OK, "{payload}");
        assert_eq!(payload["data"]["artifactCount"], 0, "{payload}");
        assert!(
            payload["data"]["path"]
                .as_str()
                .unwrap_or_default()
                .contains("renderloft-http-backup-"),
            "报告要说清备份落在哪：{payload}"
        );
        assert!(
            target.join(crate::v2::daemon::V2_DB_FILE).exists(),
            "替身也会在目标位置放一个库文件"
        );
        let _ = std::fs::remove_dir_all(&target);
    }

    /// 负对照：顶层 `assets` 与 `runs` 同时给 ⇒ **明确报错**，不落库、不上传。
    ///
    /// 两种写法各自都合法，合在一起是歧义。过去批量路径**根本不看**顶层 `assets`：每一行都没图
    /// 却照常提交、照常计费、零提示。工具的立场是：要么照做、要么说清楚，不许静默。
    #[tokio::test]
    async fn mcp_assets_with_runs_is_rejected_instead_of_silently_dropped() {
        use crate::v2::clients::client::V2Client;
        use crate::v2::clients::verbs::{self, Args};

        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 8, "sum-cloth");
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定端口");
        let address = listener.local_addr().expect("端口地址");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client =
            V2Client::new(format!("http://127.0.0.1:{}", address.port()), token).with_driver("mcp");

        let verb = verbs::find_by_id("operation.run").expect("operation.run");
        let args = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 2,
                "assets": { "520::image": { "assetId": 8, "checksum": "sum-cloth" } },
                "runs": [{ "clientRunId": "look-1", "inputs": {} }],
            }),
        )
        .expect("参数形状本身是合法的");
        let error = verbs::execute(&client, verb, &args)
            .await
            .expect_err("歧义请求必须报错，不许静默丢弃");
        assert_eq!(error.code, "ASSETS_NOT_ALLOWED_WITH_RUNS");
        assert!(
            error.message.contains("runs[i].assets"),
            "报错要说清两种正确写法：{}",
            error.message
        );
        assert!(
            crate::v2::ports::repositories::OperationRepository::list_operations(
                &repositories,
                10,
                None
            )
            .await
            .expect("列举")
            .is_empty(),
            "歧义请求不许落库"
        );
    }

    /// MCP 批量：`runs[]` 里每行的 assets **真的被传下去**（过去被静默丢弃）。
    #[tokio::test]
    async fn mcp_batch_run_assets_are_transmitted() {
        use crate::v2::clients::client::V2Client;
        use crate::v2::clients::verbs::{self, Args};

        let (app, repositories) = test_app();
        seed_media_recipe(&repositories, true);
        seed_media_asset(&repositories, 8, "sum-cloth");
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定端口");
        let address = listener.local_addr().expect("端口地址");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client =
            V2Client::new(format!("http://127.0.0.1:{}", address.port()), token).with_driver("mcp");

        let verb = verbs::find_by_id("operation.run").expect("operation.run");
        let args = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 2,
                "runs": [{
                    "clientRunId": "look-1",
                    "inputs": {},
                    "assets": { "520::image": { "assetId": 8, "checksum": "sum-cloth" } }
                }],
            }),
        )
        .expect("参数应当合法");
        verbs::execute(&client, verb, &args)
            .await
            .expect("批量提交应当成功");
        let stored = repositories
            .find_operation(crate::v2::domain::ids::OperationId(1))
            .await
            .expect("查询")
            .expect("应已落库");
        assert_eq!(stored.runs[0].client_run_id, "look-1");
        assert_eq!(
            stored.runs[0].assets[0].field_key.as_deref(),
            Some("520::image"),
            "批量行里的字段键必须落到位：{:?}",
            stored.runs[0].assets
        );
    }

    #[tokio::test]
    async fn unknown_api_path_returns_json_404() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/nonexistent", &token).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "ROUTE_NOT_FOUND");
    }

    #[tokio::test]
    async fn operation_events_listed() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (_, created) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("http-events-1"),
        )
        .await;
        let operation_id = created["data"]["operationId"].as_u64().unwrap();
        let (status, body) = get_json(
            &app,
            &format!("/api/v1/operations/{operation_id}/events"),
            &token,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let events = body["data"].as_array().expect("事件应为数组");
        let types: Vec<&str> = events
            .iter()
            .map(|event| event["eventType"].as_str().expect("eventType 应为字符串"))
            .collect();
        assert!(
            types.contains(&"run.created"),
            "事件类型缺失 run.created，实际：{types:?}"
        );
        assert!(
            types.contains(&"operation.created"),
            "事件类型缺失 operation.created，实际：{types:?}"
        );
    }

    #[tokio::test]
    async fn openapi_lists_core_routes() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/openapi.json", &token).await;
        assert_eq!(status, StatusCode::OK);
        let document = &body["data"];
        assert_eq!(document["openapi"], "3.0.3");
        let paths = document["paths"].as_object().unwrap();
        assert!(paths.contains_key("/api/v1/operations"));
        assert!(paths.contains_key("/api/v1/recipes"));
        assert!(paths.contains_key("/api/v1/actor-tokens"));
        // 此前声明表漏了 16 条（28/44），这几条是当时漏掉里最有代表性的，
        // 钉住它们防止再退回：核心设置路由、写操作方法、以及只能通过客户端二进制到达的端点。
        for path in [
            "/api/v1/settings",
            "/api/v1/settings/open-download-dir",
            "/api/v1/operations/{operation_id}/retry",
            "/api/v1/operations/{operation_id}/runs/{run_id}/retry",
            "/api/v1/recipes/{recipe_id}/touch",
            "/api/v1/diagnostics",
            "/api/v1/runninghub/account",
        ] {
            assert!(paths.contains_key(path), "openapi 缺少声明：{path}");
        }
    }

    #[tokio::test]
    async fn asset_upload_returns_asset_id() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["assets:write"]).await;
        let (status, body) = post_bytes(
            &app,
            "/api/v1/assets?filename=ref.png&mimeType=image%2Fpng",
            &token,
            vec![1, 2, 3, 4],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["data"]["assetId"].as_u64().unwrap() > 0);
    }

    /// **大图必须能传上去**——这条是"2.36 MB 的图传不上去"那个 bug 的回归测试。
    ///
    /// 当时的形状是把字节编码成 JSON 数字数组（`[1,2,3,4,…]`），4 个字节的用例全绿，
    /// 而真实图片会膨胀成约十倍的文本、撞上 axum 默认 **2 MB** 的请求体上限，
    /// daemon 回的是 `INVALID_REQUEST_BODY`（指不到病根）。
    ///
    /// 现在 body 是原始字节，上限单独放开到 `ASSET_UPLOAD_LIMIT_BYTES`。
    /// 负对照：把那条路由上的 `DefaultBodyLimit::max(...)` 去掉 ⇒ 这条红（413）。
    #[tokio::test]
    async fn asset_upload_accepts_bodies_far_over_the_default_limit() {
        let (app, repositories) = test_app();
        // 读回字节要 `assets:local-read`，所以这条测试的 token 两个 scope 都要（只给 write 会 403）。
        let token = issue_token(&repositories, &["assets:write", "assets:local-read"]).await;
        let big = vec![7u8; 3 * 1024 * 1024];
        let (status, body) = post_bytes(
            &app,
            "/api/v1/assets?filename=big.png&mimeType=image%2Fpng",
            &token,
            big.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let asset_id = body["data"]["assetId"].as_u64().unwrap();

        // 字节要**一个不差**地存进去（这条同时钉住"二进制没被当文本糟蹋"）。
        let (status, _) = get_json(&app, &format!("/api/v1/assets/{asset_id}"), &token).await;
        assert_eq!(status, StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/assets/{asset_id}/bytes"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let stored = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(stored.len(), big.len(), "存下来的字节数应当一致");
        assert!(stored.iter().all(|byte| *byte == 7), "内容应当逐字节一致");
    }

    /// 凭据引用的两条 HTTP 契约：**写入口拒绝错的引用**、**运行时把坏引用说清楚**。
    ///
    /// 为什么两条都要：写入口只挡得住"你现在就写错了"（指向非 RunningHub 的连接），
    /// 而"写的时候对、后来被删"只能靠运行路径报——用户就是靠测试按钮的那行原因
    /// 才知道该去修什么。
    #[tokio::test]
    async fn credential_reference_is_validated_on_write_and_reported_on_check() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        seed_runninghub_connection(&repositories, 2);

        // ① 指向一条**非** RunningHub 的连接（#1 是 openai）⇒ 写入口当场拒绝。
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({
                "vendor": "comfyui",
                "protocol": "comfyui",
                "displayName": "云端 ComfyUI（借错人）",
                "config": {
                    "baseUrl": "https://www.runninghub.cn/proxy",
                    "mode": "rh-proxy",
                    "sourceConnectionId": 1
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "CONNECTION_SOURCE_INVALID");

        // ② 指向 RunningHub 连接 #2 ⇒ 放行。
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({
                "vendor": "comfyui",
                "protocol": "comfyui",
                "displayName": "云端 ComfyUI",
                "config": {
                    "baseUrl": "https://www.runninghub.cn/proxy",
                    "mode": "rh-proxy",
                    "sourceConnectionId": 2
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let connection_id = body["data"]["connectionId"].as_u64().unwrap();

        // ②′ 既引用又自己填 Key ⇒ 拒绝。**这条是防静默失效**：解析时引用会覆盖自己的绑定，
        // 那把 Key 永远用不上，而 `connection set-key` 只发 secretBindings（不带 config），
        // 所以它必须也能被拦下——这里用同样的形状（只发 secretBindings）验一遍。
        let (status, body) = put_json(
            &app,
            &format!("/api/v1/connections/{connection_id}"),
            &token,
            json!({
                "secretBindings": [
                    { "slot": "api_key", "resolution": "current", "value": "sk-probe" }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "CONNECTION_SOURCE_TAKES_PRECEDENCE");

        // ③ 被引用的连接被删掉之后，测试按钮要**如实报原因**（不是 500、不是"缺少密钥"）。
        let (status, _) = delete_json(&app, "/api/v1/connections/2", &token).await;
        assert_eq!(status, StatusCode::OK);
        let (status, check) = get_json(
            &app,
            &format!("/api/v1/connections/{connection_id}/check"),
            &token,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "详情/测试这条路不能因为坏引用而打不开"
        );
        assert_eq!(check["data"]["ok"], false);
        let message = check["data"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("#2"), "原因要指到是哪条连接：{message}");
    }

    #[tokio::test]
    async fn connection_check_reports_unregistered_driver() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({
                "vendor": "unknown-vendor",
                "protocol": "unknown-protocol",
                "displayName": "Unknown",
                "config": { "baseUrl": "https://example.com" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let connection_id = body["data"]["connectionId"].as_u64().unwrap();
        let (status, check) = get_json(
            &app,
            &format!("/api/v1/connections/{connection_id}/check"),
            &token,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(check["data"]["ok"], false);
    }

    #[tokio::test]
    async fn issue_and_revoke_token_via_http() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["actor-tokens:manage"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/actor-tokens",
            &token,
            json!({
                "actorId": "agent-1",
                "clientId": "mcp-1",
                "scopes": ["operations:read", "operations:execute"],
                "label": "mcp agent"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let issued = body["data"]["token"].as_str().unwrap();
        assert!(issued.starts_with("rh-"));
        let token_id = body["data"]["tokenId"].as_i64().unwrap();
        let request = Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/actor-tokens/{token_id}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let request = Request::builder()
            .uri("/api/v1/operations")
            .header(header::AUTHORIZATION, format!("Bearer {issued}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// 模型发现的两条契约：**拿得到清单**，以及**没有清单的驱动如实报错**。
    ///
    /// 第二条同样是契约：`MODELS_NOT_SUPPORTED` 与"平台说它一个模型都没有"（空数组）
    /// 是两件事——前者界面该把入口收起来，后者才是"真没有"。
    #[tokio::test]
    async fn connection_models_lists_them_and_says_so_when_unsupported() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:read", "connections:manage"]).await;

        // #1（openai/openai-images）的驱动在这个装配里没有清单 ⇒ 如实报错。
        let (status, body) = get_json(&app, "/api/v1/connections/1/models", &token).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "MODELS_NOT_SUPPORTED");

        // 换上一个"平台声明了模型"的驱动 ⇒ 清单原样回。
        let (app, repositories) = test_app_with_models(vec!["gpt-image-1", "dall-e-3"]);
        let token = issue_token(&repositories, &["connections:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/connections/1/models", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["models"], json!(["gpt-image-1", "dall-e-3"]));
        assert_eq!(body["data"]["connectionId"], json!(1));
    }

    /// 连接声明了模型清单 ⇒ 配方 schema 里带上它（跑表单据此把 model 渲染成下拉）。
    ///
    /// 没声明 ⇒ **空数组**（不过滤，表单照旧自由输入）：声明是给人方便，不是闸门。
    #[tokio::test]
    async fn recipe_schema_carries_the_connection_model_options() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["recipes:read", "connections:manage"]).await;

        let (status, body) = get_json(&app, "/api/v1/recipes/1/schema", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["modelOptions"],
            json!([]),
            "连接没声明清单时必须是空数组（空 = 不过滤）"
        );

        // 在连接上声明两个模型（走真实的 PUT 路径，顺便证明 config.models 存得下）。
        let (status, body) = put_json(
            &app,
            "/api/v1/connections/1",
            &token,
            json!({
                "config": {
                    "baseUrl": "https://api.openai.com/v1",
                    "models": ["gpt-image-1", "dall-e-3"]
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = get_json(&app, "/api/v1/recipes/1/schema", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["modelOptions"],
            json!(["gpt-image-1", "dall-e-3"])
        );

        // 声明了默认模型 ⇒ 它**排最前**（跑表单会预选第一个，那正是它该预选的那个）。
        let (status, body) = put_json(
            &app,
            "/api/v1/connections/1",
            &token,
            json!({
                "config": {
                    "baseUrl": "https://api.openai.com/v1",
                    "models": ["gpt-image-1", "dall-e-3"],
                    "defaultModel": "dall-e-3"
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = get_json(&app, "/api/v1/recipes/1/schema", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["modelOptions"],
            json!(["dall-e-3", "gpt-image-1"]),
            "默认模型排最前，且不重复"
        );
    }

    #[tokio::test]
    async fn operation_list_returns_items_without_after_cursor() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        for index in 0..3 {
            let (_, body) = post_json(
                &app,
                "/api/v1/operations",
                &token,
                json!({
                    "idempotencyKey": format!("page-key-{index}"),
                    "runs": [{
                        "clientRunId": format!("run-{index}"),
                        "recipeRevisionId": 1,
                        "inputs": { "prompt": "a cat" }
                    }]
                }),
            )
            .await;
            assert_eq!(body["ok"], true, "{body}");
        }
        // 不带 after 也应返回列表（修复恒空 bug）
        let (status, body) = get_json(&app, "/api/v1/operations?limit=10", &token).await;
        assert_eq!(status, StatusCode::OK);
        let items = body["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert!(body["data"]["items"][0]["operationId"].as_u64().unwrap() > 0);
    }

    /// 列表摘要也带「为什么没在动」：**只有原因码与死线**，人话与 nextActions 留在 `get`。
    ///
    /// 为什么摘要必须带上它：调用方先 list 再 get。只有 get 有，等于逼它为列表里每一条
    /// 各打一次 get 才知道该看哪一个 —— 而这次事故的代价正是"没人主动去看"。
    ///
    /// 判别点四条：
    /// 1. 一条卡在 `SUBMISSION_UNCERTAIN` 的 run ⇒ 它所在 operation 的列表项里
    ///    `attention.reason` 就是那档，且 `deadline` 非空（死线是这个状态唯一的出路）；
    /// 2. 与同一 operation 的 `get`（operation 级 attention）**原因码逐字相同**；
    /// 3. 没有 attention 的 operation ⇒ `attention` 是 `null`（不是空对象、也不是缺字段：
    ///    "没有"与"有但空"必须能分辨）；
    /// 4. 摘要里**不许**出现 `message` / `nextActions`（一页 50 条不该带上 50 份散文）。
    ///
    /// 负对照：让 `list_operations` 直接给出 `attention_reason: None` ⇒ 第 1、2 条红。
    #[tokio::test]
    async fn operation_list_summaries_carry_attention_without_the_prose() {
        use crate::v2::domain::operation::Transition;
        use crate::v2::ports::repositories::OperationRepository;

        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;

        // 两个任务：
        // * 第一个是**两条 run 的批量** —— 第一条一直排队、第二条被推到"等确认"。
        //   ⚠️ **故意给两条 run**：这一层跑的是内存替身，而"挑哪一条"是替身与 sqlite
        //   各写一份的地方；单 run 时"按严重度挑"与"挑第一条"是同一个答案，这条测试就抓不住
        //   替身挑错了 run（实测：把替身改成"取第一条有 attention 的"，单 run 版照样绿、
        //   多 run 版红）。sqlite 那侧的排序由 `sqlite::tests` 里那条同形测试钉住。
        // * 第二个只有一条 run，推到已受理 ⇒ 没有 attention。
        let bodies = [
            json!({
                "idempotencyKey": "attention-key-1",
                "runs": [
                    { "clientRunId": "queued-1", "recipeRevisionId": 1,
                      "inputs": { "prompt": "a cat" } },
                    { "clientRunId": "uncertain-1", "recipeRevisionId": 1,
                      "inputs": { "prompt": "a dog" } },
                ]
            }),
            json!({
                "idempotencyKey": "attention-key-2",
                "runs": [
                    { "clientRunId": "fine-1", "recipeRevisionId": 1,
                      "inputs": { "prompt": "a bird" } },
                ]
            }),
        ];
        let mut ids = Vec::new();
        for body in bodies {
            let (status, created) = post_json(&app, "/api/v1/operations", &token, body).await;
            assert_eq!(status, StatusCode::OK, "{created}");
            ids.push(created["data"]["operationId"].as_u64().unwrap());
        }

        let (_, stuck) = get_json(&app, &format!("/api/v1/operations/{}", ids[0]), &token).await;
        // 推**第二条** run：第一条一直排队，正是"最严重的那条不是第一条"。
        let stuck_run = stuck["data"]["runs"][1]["runId"].as_u64().unwrap();
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToSubmissionUncertain,
        ] {
            repositories
                .transition_run(
                    crate::v2::domain::ids::RunId(stuck_run),
                    transition,
                    Vec::new(),
                )
                .await
                .expect("推到等确认");
        }
        let (_, fine) = get_json(&app, &format!("/api/v1/operations/{}", ids[1]), &token).await;
        let fine_run = fine["data"]["runs"][0]["runId"].as_u64().unwrap();
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToAccepted,
        ] {
            repositories
                .transition_run(
                    crate::v2::domain::ids::RunId(fine_run),
                    transition,
                    Vec::new(),
                )
                .await
                .expect("推到已受理");
        }

        let (status, body) = get_json(&app, "/api/v1/operations?limit=10", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let items = body["data"]["items"].as_array().unwrap();
        let item = |id: u64| {
            items
                .iter()
                .find(|item| item["operationId"].as_u64() == Some(id))
                .unwrap_or_else(|| panic!("列表里应当有 {id}：{body}"))
        };

        // 1) 卡住的那个：原因码 + 死线。
        let summary = item(ids[0]);
        assert_eq!(
            summary["attention"]["reason"], "SUBMISSION_UNCERTAIN",
            "摘要必须报出「等确认」——否则调用方先看到的是 RUNNING: {body}"
        );
        assert!(
            summary["attention"]["deadline"].as_str().is_some(),
            "等确认必须带死线（它是这个状态唯一的出路）：{body}"
        );
        // 4) 精简：人话与下一动作留在 get。
        assert!(
            summary["attention"].get("message").is_none()
                && summary["attention"].get("nextActions").is_none(),
            "摘要不该带上人话与 nextActions（那是 get 的事）：{body}"
        );
        // 2) 与 get 的原因码一致。
        let (_, detail) = get_json(&app, &format!("/api/v1/operations/{}", ids[0]), &token).await;
        assert_eq!(
            summary["attention"]["reason"], detail["data"]["attention"]["reason"],
            "列表与详情必须是同一个判据：{detail}"
        );
        assert_eq!(
            summary["attention"]["deadline"], detail["data"]["attention"]["deadline"],
            "死线也要一致：{detail}"
        );

        // 3) 正常在跑的那个：null（"没有"与"有但空"要能分辨）。
        assert_eq!(
            item(ids[1])["attention"],
            Value::Null,
            "没有「为什么没在动」就该是 null：{body}"
        );
    }

    #[tokio::test]
    async fn sse_events_returns_event_stream_content_type() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;
        let (_, created) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("sse-key-1"),
        )
        .await;
        let operation_id = created["data"]["operationId"].as_u64().unwrap();
        let request = Request::builder()
            .uri(format!(
                "/api/v1/operations/{operation_id}/events?stream=true"
            ))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.contains("text/event-stream"), "{content_type}");
    }

    // ---------- 请求体解析失败的统一信封 ----------

    #[tokio::test]
    async fn invalid_json_body_returns_structured_envelope() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute"]).await;

        // 缺字段：信封中给出缺失字段名（camelCase）。
        let (status, body) = post_json(&app, "/api/v1/operations", &token, json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["code"], "INVALID_REQUEST_BODY");
        assert_eq!(body["error"]["kind"], "validation");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("idempotencyKey"), "{message}");

        // 类型错误：serde 会把出错的值打印出来，错误消息不得回显请求体内容。
        let (status, body) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            json!({ "idempotencyKey": "k", "runs": "secret-value" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "INVALID_REQUEST_BODY");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("secret-value"),
            "错误消息回显了请求体中的值：{message}"
        );
    }

    // ---------- shell / settings / runninghub status 契约 ----------

    fn shell_state(
        repositories: &InMemoryRepositories,
        drivers: DriverRegistry,
    ) -> Arc<V2HttpState> {
        let create = std::sync::Arc::new(CreateOperation::new(
            Repositories {
                recipes: Box::new(repositories.clone()),
                connections: Box::new(repositories.clone()),
                operations: Box::new(repositories.clone()),
                assets: Box::new(repositories.clone()),
            },
            Box::new(PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        ));
        let validate = std::sync::Arc::new(ValidateOperation::new(create.clone()));
        let cancel = std::sync::Arc::new(CancelOperation {
            repositories: std::sync::Arc::new(repositories.clone()),
            connections: std::sync::Arc::new(repositories.clone()),
            drivers: std::sync::Arc::new(drivers.clone()),
        });
        let settings = std::sync::Arc::new(crate::v2::application::settings::SettingsService::new(
            std::sync::Arc::new(repositories.clone()),
        ));
        let status = std::sync::Arc::new(
            crate::v2::application::settings::RunningHubStatusService::new(
                std::sync::Arc::new(repositories.clone()),
                std::sync::Arc::new(drivers.clone()),
                std::sync::Arc::new(crate::v2::daemon::NoopSecretStore),
            ),
        );
        let import_service = std::sync::Arc::new(
            crate::v2::application::recipe_import::RecipeImportService::new(
                std::sync::Arc::new(repositories.clone()),
                std::sync::Arc::new(repositories.clone()),
                std::sync::Arc::new(drivers.clone()),
            ),
        );
        // 替身库没有真库文件：它的 `snapshot_database` 只保证"目标位置会出现一个库文件"，
        // 一致性由 `application::workspace_backup` 里那条**真库**测试钉住。
        let workspace_backup = std::sync::Arc::new(
            crate::v2::application::workspace_backup::WorkspaceBackupService::new(
                std::sync::Arc::new(repositories.clone())
                    as std::sync::Arc<dyn crate::v2::ports::repositories::WorkspaceRepository>,
            ),
        );
        std::sync::Arc::new(V2HttpState::new(
            std::sync::Arc::new(crate::v2::infrastructure::token_service::TokenService::new(
                std::sync::Arc::new(repositories.clone()),
            )),
            std::sync::Arc::new(crate::v2::daemon::NoopSecretStore),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            std::sync::Arc::new(repositories.clone()),
            create,
            validate,
            cancel,
            std::sync::Arc::new(drivers),
            settings,
            status,
            import_service,
            std::sync::Arc::new(
                crate::v2::infrastructure::providers::safety::ReqwestSafeHttpClient,
            ),
            workspace_backup,
        ))
    }

    fn shell_test_app() -> (Router, InMemoryRepositories) {
        let repositories = InMemoryRepositories::new();
        let revision = ConnectionRevision {
            id: ConnectionRevisionId(11),
            connection_id: ConnectionId(10),
            config: std::collections::BTreeMap::from([
                ("baseUrl".to_string(), json!("https://www.runninghub.cn")),
                ("region".to_string(), json!("cn-south-1")),
                ("account".to_string(), json!("user-1")),
            ]),
            secret_bindings: vec![],
            effective_capabilities: vec!["workflow-execution".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: crate::v2::domain::model::Timestamp::now(),
        };
        let connection = Connection {
            id: ConnectionId(10),
            vendor: "runninghub".to_string(),
            protocol: "runninghub".to_string(),
            display_name: "RunningHub".to_string(),
            current_revision_id: ConnectionRevisionId(11),
            status: crate::v2::domain::model::ConnectionStatus::Active,
        };
        repositories.seed_connection(connection, vec![revision]);
        let mut drivers = DriverRegistry::new();
        let driver = std::sync::Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "runninghub".to_string(),
            protocol_id: "runninghub".to_string(),
            display_name: "RunningHub".to_string(),
            business_capabilities: vec!["workflow-execution".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        drivers.register(driver_key("runninghub", "runninghub"), driver);
        (
            build_router(shell_state(&repositories, drivers)),
            repositories,
        )
    }

    #[tokio::test]
    async fn settings_snapshot_shape() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["settings:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/settings", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let data = &body["data"];
        assert!(data.get("downloadDir").is_some());
        // LLM 配置不在设置里：设置快照不该有任何 LLM 字段。
        assert!(data.get("llmBaseUrl").is_none());
        assert!(data.get("llmModel").is_none());
        assert!(data.get("llmApiKeyConfigured").is_none());
        // RunningHub 凭据已由 Connection 管理：设置快照不再暴露 runninghub key 字段。
        assert!(data.get("apiKeyConfigured").is_none());
        assert!(data.get("apiBaseUrl").is_none());
    }

    #[tokio::test]
    async fn settings_save_and_readback_download_dir() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["settings:manage", "settings:read"]).await;
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/settings")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "downloadDir": "D:/tmp/rh-downloads"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["downloadDir"], "D:/tmp/rh-downloads");
        let (_, readback) = get_json(&app, "/api/v1/settings", &token).await;
        assert_eq!(readback["data"]["downloadDir"], "D:/tmp/rh-downloads");
    }

    #[tokio::test]
    async fn runninghub_status_without_connection_is_clear_error() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["settings:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/runninghub/account", &token).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "RUNNINGHUB_CONNECTION_MISSING");
    }

    #[tokio::test]
    async fn runninghub_status_uses_connection_via_driver() {
        let (app, _repositories) = shell_test_app();
        let token = issue_token(&_repositories, &["settings:read"]).await;
        let (status, account) = get_json(&app, "/api/v1/runninghub/account", &token).await;
        assert_eq!(status, StatusCode::OK, "{account}");
        assert_eq!(account["data"]["remainCoins"], "1000");
        assert_eq!(account["data"]["apiType"], "pro");
        let (status, queue) = get_json(&app, "/api/v1/runninghub/queue", &token).await;
        assert_eq!(status, StatusCode::OK, "{queue}");
        assert_eq!(queue["data"]["concurrentLimit"], 2);
        assert_eq!(queue["data"]["runningCount"], "1");
    }

    #[tokio::test]
    async fn runninghub_status_accepts_connection_param() {
        let (app, _repositories) = shell_test_app();
        let token = issue_token(&_repositories, &["settings:read"]).await;
        let (status, body) =
            get_json(&app, "/api/v1/runninghub/account?connectionId=10", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, missing) =
            get_json(&app, "/api/v1/runninghub/account?connectionId=999", &token).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"]["code"], "CONNECTION_NOT_FOUND");
    }

    async fn delete_json(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("DELETE")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn diagnostics_shape() {
        let (app, _repositories) = shell_test_app();
        let token = issue_token(&_repositories, &["diagnostics:read"]).await;
        let (status, body) = get_json(&app, "/api/v1/diagnostics", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["data"]["appVersion"].as_str().is_some());
        assert!(body["data"]["apiKeyConfigured"].is_boolean());
        assert_eq!(body["data"]["serviceRegion"], "local");
        // 运行参数与默认值：界面拿它们回答"出问题我该改哪儿"，所以形状要钉住
        // （少了字段不会报错，只会让那一格永远空着——静默失效）。
        assert!(body["data"]["runtime"]["bind"].as_str().is_some());
        assert!(body["data"]["runtime"]["distDir"].as_str().is_some());
        assert!(body["data"]["runtime"]["allowedHosts"].is_array());
        assert_eq!(
            body["data"]["defaults"]["requestTimeoutSeconds"],
            crate::v2::domain::connection_options::DEFAULT_REQUEST_TIMEOUT_SECS
        );
        assert_eq!(
            body["data"]["defaults"]["trackingDeadlineSeconds"],
            crate::v2::domain::connection_options::DEFAULT_TRACKING_DEADLINE_SECS
        );
        // 日志在哪、还在不在写：形状也要钉住——字段名漂了不会报错，只会让界面那一格
        // 静默变成"未知"（`modified_at_millis` vs `modifiedAtMillis` 实测踩过一次）。
        assert!(body["data"]["logs"]["directory"].is_string());
        assert!(body["data"]["logs"]["files"].is_array());
        for file in body["data"]["logs"]["files"].as_array().unwrap() {
            assert!(file["name"].is_string(), "{file}");
            assert!(file["bytes"].is_u64(), "{file}");
            assert!(file["modifiedAtMillis"].is_u64(), "{file}");
        }
    }

    // ---------- connection 写端点契约（C3 基线：锁定当前行为） ----------

    async fn put_json(app: &Router, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("PUT")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn create_connection_succeeds_without_secret_echo() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage", "connections:read"]).await;
        let secret_value = "sk-connection-secret-contract";
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({
                "vendor": "openai",
                "protocol": "openai-images",
                "displayName": "contract-conn",
                "config": { "baseUrl": "https://api.openai.com/v1", "model": "gpt-4o-mini" },
                "secretBindings": [{ "slot": "api_key", "value": secret_value }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let connection_id = body["data"]["connectionId"].as_u64().expect("connectionId");
        assert!(connection_id > 0);
        let raw = serde_json::to_string(&body).unwrap();
        assert!(!raw.contains(secret_value), "密钥不得回显: {raw}");
        // 列表端点正常（secret 值只进 keyring，不回显）
        let (list_status, list_body) = get_json(&app, "/api/v1/connections", &token).await;
        assert_eq!(list_status, StatusCode::OK, "{list_body}");
        assert!(!list_body.to_string().contains(secret_value));
    }

    #[tokio::test]
    async fn create_connection_requires_vendor() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({ "protocol": "openai-images" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "VENDOR_REQUIRED");
    }

    #[tokio::test]
    async fn create_connection_requires_protocol() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({ "vendor": "openai" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "PROTOCOL_REQUIRED");
    }

    #[tokio::test]
    async fn update_connection_display_name_keeps_revision() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        let (status, body) = put_json(
            &app,
            "/api/v1/connections/1",
            &token,
            json!({ "displayName": "Renamed" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["currentRevisionId"], 1);
    }

    #[tokio::test]
    async fn update_connection_config_creates_new_revision() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        let (status, body) = put_json(
            &app,
            "/api/v1/connections/1",
            &token,
            json!({ "config": { "baseUrl": "https://example.com/v1", "model": "gpt-4o" } }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let revision_id = body["data"]["currentRevisionId"]
            .as_u64()
            .expect("revisionId");
        assert!(revision_id > 1, "配置变更应创建新 revision: {body}");
    }

    #[tokio::test]
    async fn create_connection_rejects_invalid_secret_binding() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["connections:manage"]).await;
        let (status, body) = post_json(
            &app,
            "/api/v1/connections",
            &token,
            json!({
                "vendor": "openai",
                "protocol": "openai-images",
                "secretBindings": [{ "slot": "api_key", "resolution": "bogus", "value": "sk-x" }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "SECRET_BINDING_INVALID");
    }

    // ---------- 配方端点契约（这组此前**零覆盖**） ----------
    // 为什么要补：`/recipes/{id}`、`/recipes/{id}/schema`、`/recipes/{id}`(PUT)、
    // `/recipes/{id}/touch` 都在界面里被调用，却**没有任何 HTTP 契约测试**。
    // 断言刻意落在**可观察行为**上（如 lastOpenedAt 真的被持久化），而不只看响应里的标记位——
    // 只看 `{"touched": true}` 的话，一个"返回 true 但没写库"的实现照样能骗过测试。

    /// 字段备注：用户补一次，**所有读 schema 的地方必须看到同一份**（界面表单、MCP 的工具描述）。
    ///
    /// 判别点三条：
    /// 1. `PUT /recipes/{id}` 的 `fieldLabels` 真的落库（重读仍在）；
    /// 2. 界面读的 `/recipes/{id}/schema` 带上备注；
    /// 3. 没有备注的字段**原样保留**（平台给的 `description` 就等于字段名，读不到语义就不编）。
    ///
    /// ⚠️ "同一份 schema 不要各写一份合并逻辑"：`apply_field_labels` 是唯一实现，
    /// 新增消费方（比如 MCP 的工具描述）接它，不要另写一遍。
    #[tokio::test]
    async fn field_labels_reach_the_form() {
        let (app, repositories, state) = test_app_with_state();
        let token = issue_token(&repositories, &["recipes:read", "recipes:write"]).await;

        // 先确认种子 schema 的 description 就是字段名（这正是要解决的问题）。
        let (status, before) = get_json(&app, "/api/v1/recipes/1/schema", &token).await;
        assert_eq!(status, StatusCode::OK, "{before}");
        assert!(
            before["data"]["inputSchema"]["properties"]["prompt"]
                .get("description")
                .is_none(),
            "前置：种子 schema 没有可读的描述：{before}"
        );

        // 1) 补一条备注。
        let (status, updated) = put_json(
            &app,
            "/api/v1/recipes/1",
            &token,
            json!({ "fieldLabels": { "prompt": "正向提示词" } }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");

        // 2a) 界面读的端点。
        let (status, schema) = get_json(&app, "/api/v1/recipes/1/schema", &token).await;
        assert_eq!(status, StatusCode::OK, "{schema}");
        assert_eq!(
            schema["data"]["inputSchema"]["properties"]["prompt"]["description"],
            json!("正向提示词"),
            "界面表单必须看到备注：{schema}"
        );

        // 1 的反向：重读 Recipe 确认落库（不是只在响应里回显）。
        let recipe = state
            .recipes
            .find_recipe(crate::v2::domain::ids::RecipeId(1))
            .await
            .expect("find")
            .expect("recipe");
        assert_eq!(
            recipe.field_labels.get("prompt").map(String::as_str),
            Some("正向提示词"),
            "备注必须真的落库，否则重启就没了"
        );
    }

    /// **花钱闸门**在真实路由上的整条链：`X-RenderLoft-Driver: mcp` → `origin=mcp` → 并发闸。
    ///
    /// 判别点三条：
    /// 1. 不带那个头（人操作）连续提交 4 个 ⇒ 都成功——**闸门不拦人**；
    /// 2. 第 5 个带上 `mcp` 头 ⇒ 409 `AUTONOMOUS_CONCURRENCY_LIMIT`；
    /// 3. 第 5 个**不带**头 ⇒ 成功（拦的是 origin，不是"第 5 个"）。
    ///
    /// ⚠️ 为什么需要：`header → origin` 与 `origin → 闸门` 各有单测，但**接缝**没有，
    /// 而那正是原缺口所在——MCP 从前 origin 恒为 `http`，于是闸门对它**永不生效**
    ///（`cea5312` 修的就是这个）。两条单测都绿并不能说明它们接得上。
    #[tokio::test]
    async fn autonomous_origin_is_capped_at_the_http_boundary() {
        let (app, repositories, _state) = test_app_with_state();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;

        let submit = |key: String, driver: Option<&'static str>| {
            let mut builder = Request::builder()
                .method("POST")
                .uri("/api/v1/operations")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json");
            if let Some(driver) = driver {
                builder = builder.header(crate::v2::clients::client::DRIVER_HEADER, driver);
            }
            let body = json!({
                "idempotencyKey": key,
                "runs": [{
                    "clientRunId": "run-1",
                    "recipeRevisionId": 1,
                    "inputs": { "prompt": "a cat" }
                }]
            });
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        builder
                            .body(Body::from(serde_json::to_vec(&body).expect("body")))
                            .expect("request"),
                    )
                    .await
                    .expect("response");
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
                    .await
                    .expect("body");
                (
                    status,
                    serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
                )
            }
        };

        // 1) 人操作的四个：全部放行（闸门只对自主调用方生效）。
        for index in 0..4 {
            let (status, body) = submit(format!("gate-human-{index}"), None).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "第 {index} 个人工提交应当放行：{body}"
            );
        }

        // 2) 自主调用方的第 5 个：撞上限。
        let (status, body) = submit("gate-mcp-1".to_string(), Some("mcp")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            body["error"]["code"], "AUTONOMOUS_CONCURRENCY_LIMIT",
            "自主调用方必须被并发闸拦住：{body}"
        );

        // 3) 同样第 5 个、不带自报头：放行（拦的是 origin，不是"第 5 个"）。
        let (status, body) = submit("gate-human-5".to_string(), None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "人操作的提交不该被自主闸拦住：{body}"
        );
    }

    /// 调用方自报身份的**线协议**：`X-RenderLoft-Driver: mcp` ⇒ 该 Operation 的 origin 是 `mcp`。
    ///
    /// 判别点：
    /// 1. 带合法头 ⇒ origin 变成 `mcp`（否则 MCP 那条路的花钱闸门永远不触发，
    ///    等于白写——**闸门和"能不能自报"必须一起验**）；
    /// 2. 认不出来的值 ⇒ 回落 `http`（不设闸那一类）。反向控制：
    ///    一个"照抄任意头值"的实现会让拼错的头把调用方关进闸门，那种故障很难查。
    #[tokio::test]
    async fn declared_driver_header_becomes_the_operation_origin() {
        let (app, repositories, state) = test_app_with_state();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;

        let body = |key: &str| {
            json!({
                "idempotencyKey": key,
                "runs": [{
                    "clientRunId": "run-1",
                    "recipeRevisionId": 1,
                    "inputs": { "prompt": "a cat" }
                }]
            })
        };

        for (header, expected) in [("mcp", "mcp"), ("something-else", "http"), ("", "http")] {
            // ⚠️ 幂等键必须各不相同：同键会被当成**重放**，返回第一个 Operation，
            // 于是三次都读到同一个 origin，测试会以"第二次就不对"失败（踩过）。
            let payload_body = body(&format!("driver-header-key-{}", header.len()));
            let mut builder = Request::builder()
                .method("POST")
                .uri("/api/v1/operations")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json");
            if !header.is_empty() {
                builder = builder.header(crate::v2::clients::client::DRIVER_HEADER, header);
            }
            let response = app
                .clone()
                .oneshot(
                    builder
                        .body(Body::from(serde_json::to_vec(&payload_body).expect("body")))
                        .expect("request"),
                )
                .await
                .expect("response");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
                .await
                .expect("body");
            let payload: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            assert_eq!(status, StatusCode::OK, "header={header:?} {payload}");
            let operation_id = payload["data"]["operationId"]
                .as_u64()
                .expect("operationId");
            let persisted = state
                .create_operation
                .repositories
                .operations
                .find_operation(crate::v2::domain::ids::OperationId(operation_id))
                .await
                .expect("find")
                .expect("operation");
            assert_eq!(
                persisted.operation.origin, expected,
                "header={header:?} 应当落成 origin={expected}"
            );
        }
    }

    #[tokio::test]
    async fn recipe_detail_returns_summary_with_driver_key_and_schemas() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["recipes:read"]).await;

        let (status, body) = get_json(&app, "/api/v1/recipes/1", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let recipe = &body["data"]["recipe"];
        assert_eq!(recipe["recipeId"], 1);
        assert_eq!(recipe["name"], "Image Recipe");
        // driverKey 是前端 `recipeTypeKey` 的唯一输入（首页/资源库的类型标签）。
        // 它的值域是 v2::ports::provider::driver_key(vendor, protocol) ⇒ "vendor/protocol" 形态。
        assert_eq!(recipe["driverKey"], "openai/openai-images", "{body}");
        assert!(
            recipe["lastOpenedAt"].is_null(),
            "种子配方尚未打开过，应为 null: {body}"
        );
        // Runner 表单要的两个 schema 与摘要同响应返回。
        assert!(body["data"]["inputSchema"].is_object(), "{body}");
        assert!(body["data"]["outputSchema"].is_object(), "{body}");

        let (status, body) = get_json(&app, "/api/v1/recipes/1/schema", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["recipeId"], 1);
        assert_eq!(body["data"]["revision"], 1);
        assert!(body["data"]["revisionId"].is_number(), "{body}");
        assert_eq!(
            body["data"]["inputSchema"]["properties"]["prompt"]["type"], "string",
            "{body}"
        );
    }

    #[tokio::test]
    async fn touch_recipe_persists_last_opened_at() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["recipes:read", "recipes:write"]).await;

        let (status, body) = get_json(&app, "/api/v1/recipes/1", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["data"]["recipe"]["lastOpenedAt"].is_null(), "{body}");

        let (status, body) = post_json(&app, "/api/v1/recipes/1/touch", &token, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["touched"], true, "{body}");

        // 关键断言：**真的写进去了**。界面默认排序是「按最近使用」，读的就是这个字段；
        // 若这里只断言 `touched == true`，一个"返回成功但不落库"的实现会静默破坏排序。
        let (status, body) = get_json(&app, "/api/v1/recipes/1", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body["data"]["recipe"]["lastOpenedAt"].is_string(),
            "touch 后 lastOpenedAt 应被写入: {body}"
        );
    }

    #[tokio::test]
    async fn touch_recipe_requires_recipes_write_scope() {
        let (app, repositories) = test_app();
        // 只有读权限：touch 是写操作，必须被拦住。
        // （界面会话持 recipes:write —— 与 create/update/delete recipe 同守卫，故不会 403。）
        let token = issue_token(&repositories, &["recipes:read"]).await;
        let (status, body) = post_json(&app, "/api/v1/recipes/1/touch", &token, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    /// 界面的"移除 / 显示已移除 / 恢复"这一整条路（HTTP 契约）。
    ///
    /// 判别点六条：
    /// 1. 移除后**不在**资源库列表里（`removedAt` 也不再出现在那一批里）；
    /// 2. `?removed=true` 能列出它，且带 `removedAt`；
    /// 3. 墓碑列表**也带 binding**（`driverKey`）——否则界面上的"最近删除"会一律
    ///    显示成「其他」，那是查询偷懒不是事实；
    /// 4. 恢复后回到资源库列表、出墓碑；
    /// 5. **恢复一个没移除过的** ⇒ 404 `RECIPE_NOT_IN_TRASH`
    ///    （"恢复成功"不该对一件没发生的事成立）；
    /// 6. 移除与恢复都要 `recipes:write`。
    ///
    /// ⚠️ 第 5 条曾经是坏的：sqlite 的 `restore_recipe` 没有 `AND removed_at IS NOT NULL`，
    /// 而替身返回的是"它原本在不在墓碑里"——两边不一致，测试只跑替身所以全绿。
    #[tokio::test]
    async fn removed_recipe_leaves_the_library_and_can_be_restored() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["recipes:read", "recipes:write"]).await;

        let (status, body) = get_json(&app, "/api/v1/recipes", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["items"].as_array().expect("items").len(), 1);
        assert!(
            body["data"]["items"][0]["removedAt"].is_null(),
            "活着的应用不该有移除时间: {body}"
        );

        // 1) 移除。
        let (status, body) = delete_json(&app, "/api/v1/recipes/1", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["deleted"], true, "{body}");
        let (status, body) = get_json(&app, "/api/v1/recipes", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body["data"]["items"].as_array().expect("items").is_empty(),
            "移除后不该出现在资源库列表里: {body}"
        );

        // 2 + 3) 墓碑列表。
        let (status, body) = get_json(&app, "/api/v1/recipes?removed=true", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let removed = body["data"]["items"].as_array().expect("items");
        assert_eq!(removed.len(), 1, "已移除的那一个应当在这里: {body}");
        assert_eq!(removed[0]["name"], "Image Recipe", "{body}");
        assert!(
            removed[0]["removedAt"].is_string(),
            "墓碑必须带移除时间（界面显示「移除于 …」）: {body}"
        );
        assert_eq!(
            removed[0]["driverKey"], "openai/openai-images",
            "墓碑列表也要带 binding，否则界面上的类型标签会一律变成「其他」: {body}"
        );

        // 4) 恢复。
        let (status, body) = post_json(&app, "/api/v1/recipes/1/restore", &token, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["restored"], true, "{body}");
        let (status, body) = get_json(&app, "/api/v1/recipes", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["items"].as_array().expect("items").len(),
            1,
            "恢复后应当回到资源库: {body}"
        );
        let (status, body) = get_json(&app, "/api/v1/recipes?removed=true", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body["data"]["items"].as_array().expect("items").is_empty(),
            "恢复后不该还在墓碑列表里: {body}"
        );

        // 5) 再恢复一次 ⇒ 它并不在墓碑里。
        let (status, body) = post_json(&app, "/api/v1/recipes/1/restore", &token, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "RECIPE_NOT_IN_TRASH", "{body}");

        // 6) 写权限守卫。
        let read_only = issue_token(&repositories, &["recipes:read"]).await;
        let (status, body) = delete_json(&app, "/api/v1/recipes/1", &read_only).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, body) =
            post_json(&app, "/api/v1/recipes/1/restore", &read_only, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn update_recipe_only_changes_provided_fields() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["recipes:read", "recipes:write"]).await;

        let (status, body) = put_json(
            &app,
            "/api/v1/recipes/1",
            &token,
            json!({ "name": "Renamed Recipe" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // PUT 的响应体是**裸的 RecipeSummary**（前端按 `v2.put<RecipeSummary>` 消费）。
        assert_eq!(body["data"]["name"], "Renamed Recipe", "{body}");
        // 未提供的字段不应被清空。
        assert_eq!(body["data"]["description"], "text to image", "{body}");
        assert_eq!(body["data"]["driverKey"], "openai/openai-images", "{body}");
    }

    // ---------- 操作写端点契约（这组此前**零覆盖**） ----------
    // 覆盖 `/operations/{id}/cancel`、`/operations/{id}/retry`、
    // `/operations/{id}/runs/{run_id}/retry`、`/runs/{run_id}`——都在界面里被调用却无契约测试。

    #[tokio::test]
    async fn cancel_operation_over_http_cancels_runs_and_projects_operation_status() {
        let (app, repositories) = test_app();
        let token = issue_token(
            &repositories,
            &["operations:execute", "operations:read", "operations:cancel"],
        )
        .await;

        let (status, created) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("cancel-key-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let operation_id = created["data"]["operationId"].as_u64().unwrap();

        let (status, detail) =
            get_json(&app, &format!("/api/v1/operations/{operation_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        let run_id = detail["data"]["runs"][0]["runId"].as_u64().unwrap();

        // cancel 已由 GET 改为 **POST**（此前用 GET 承载变更，违反 HTTP 语义；
        // 与同组的 retry(POST) / runs/{id}/retry(POST) / delete(DELETE) 现在一致）。
        // 这条测试钉住新方法 —— 若有人改回 GET，这里会以 405 失败。
        let (status, cancelled) = post_json(
            &app,
            &format!("/api/v1/operations/{operation_id}/cancel"),
            &token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{cancelled}");
        assert_eq!(
            cancelled["data"]["cancelled"].as_array().unwrap(),
            &vec![json!(run_id)],
            "该 run 应被取消: {cancelled}"
        );
        assert!(
            cancelled["data"]["alreadyTerminal"]
                .as_array()
                .unwrap()
                .is_empty(),
            "尚未终态，不应进 alreadyTerminal: {cancelled}"
        );

        // 关键断言：**状态真的投影过去了**（operation 状态由 runs 推导）。
        let (status, after) =
            get_json(&app, &format!("/api/v1/operations/{operation_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert_eq!(after["data"]["runs"][0]["status"], "CANCELLED", "{after}");
        assert_eq!(after["data"]["status"], "CANCELLED", "{after}");

        // 顺带覆盖 `/runs/{run_id}`（同为未覆盖路由）。
        let (status, run) = get_json(&app, &format!("/api/v1/runs/{run_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["data"]["status"], "CANCELLED", "{run}");
    }

    #[tokio::test]
    async fn retry_operation_over_http_creates_new_operation_reusing_params() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute", "operations:read"]).await;

        let (status, created) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("retry-key-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let source_id = created["data"]["operationId"].as_u64().unwrap();

        let (status, detail) =
            get_json(&app, &format!("/api/v1/operations/{source_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        let source_revision = detail["data"]["runs"][0]["recipeRevisionId"].clone();

        let (status, retried) = post_json(
            &app,
            &format!("/api/v1/operations/{source_id}/retry"),
            &token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{retried}");
        let new_id = retried["data"]["operationId"].as_u64().unwrap();
        assert_ne!(new_id, source_id, "重试应**新建**操作而不是复用: {retried}");

        // 复用原参数：配方修订一致；clientRunId 带 -retry- 后缀（可观察）。
        let (status, new_detail) =
            get_json(&app, &format!("/api/v1/operations/{new_id}"), &token).await;
        assert_eq!(status, StatusCode::OK, "{new_detail}");
        assert_eq!(
            new_detail["data"]["runs"][0]["recipeRevisionId"], source_revision,
            "应复用原配方修订: {new_detail}"
        );
        let client_run_id = new_detail["data"]["runs"][0]["clientRunId"]
            .as_str()
            .unwrap();
        assert!(
            client_run_id.contains("-retry-"),
            "重试的 clientRunId 应带 -retry- 后缀: {client_run_id}"
        );
    }

    #[tokio::test]
    async fn retry_run_over_http_rejects_unknown_run() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["operations:execute"]).await;
        let (status, created) = post_json(
            &app,
            "/api/v1/operations",
            &token,
            create_operation_body("retry-run-key-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let operation_id = created["data"]["operationId"].as_u64().unwrap();

        // 不存在的 run 要报结构化 404，而不是静默建一个空操作。
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/operations/{operation_id}/runs/999999/retry"),
            &token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "RUN_NOT_FOUND", "{body}");
    }

    // ---------- OpenAPI 声明表 ↔ 注册路由 的一致性守卫 ----------
    /// 从 `mod.rs` 源码里抽出**注册区**的 (路径, 方法)。
    ///
    /// 为什么要扫源码：axum 的 `Router` **无法自省**已注册路由，
    /// 所以"表漏了路由"这件事没有运行时判据，只能对照源码文本。
    /// 测试区也含路径字面量（用例里的请求路径），因此先切掉 `mod tests` 之后的部分。
    fn registered_routes_from_source(src: &str) -> Vec<(String, String)> {
        let body = match src.find("\nmod tests {") {
            Some(i) => &src[..i],
            None => src,
        };
        // 剥掉**整行注释**：注释里出现 `delete(...)` 这类文本会被下面的方法名扫描误当成注册。
        // 实测本文件的一条注释写了 `delete(DELETE)`，立刻造出一条假的 "cancel + delete" 条目。
        // 只剥整行注释（行首为 `//`）而不做通用剥注释：后者要处理字符串里的 `//`（如 https://）
        // 与转义，反而更容易错。
        let body: String = body
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];
        let mut out: Vec<(String, String)> = Vec::new();
        let body = body.as_str();
        let bytes = body.as_bytes();
        let mut cursor = 0usize;
        while let Some(rel) = body[cursor..].find(".route(") {
            let start = cursor + rel + ".route(".len();
            // 求该 `.route(...)` 调用自身的括号范围（深度回到 0 即结束）
            let mut depth = 1usize;
            let mut end = start;
            let mut j = start;
            while j < bytes.len() {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = j;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            let seg = &body[start..end];
            if let Some(p0) = seg.find("\"/api/v1") {
                let after = &seg[p0 + 1..];
                if let Some(p1) = after.find('"') {
                    let path = after[..p1].to_string();
                    // 方法名只能出现在段内、且前面不是标识符字符（排除 `handlers::get_x(` 之类）
                    for m in METHODS {
                        let needle = format!("{m}(");
                        let mut k = 0usize;
                        while let Some(r) = seg[k..].find(&needle) {
                            let at = k + r;
                            let prev_ok = at == 0 || {
                                let prev = seg.as_bytes()[at - 1] as char;
                                !prev.is_alphanumeric() && prev != '_'
                            };
                            if prev_ok {
                                out.push((path.clone(), m.to_string()));
                            }
                            k = at + needle.len();
                        }
                    }
                }
            }
            cursor = if end > start { end } else { start + 1 };
        }
        out.sort();
        out.dedup();
        out
    }

    /// `handlers::ROUTES`（对外发布的 openapi.json 的来源）必须与 `mod.rs` 里
    /// 注册的路由**完全一致**（路径 + 方法）。
    ///
    /// 为什么需要这条：该表手工维护、axum 无法自省 ⇒ 实测它曾只覆盖 28/44 条路径
    /// （含漏掉 `/api/v1/settings`），使对外文档少报 1/3 的 API；
    /// 也曾把 `/operations/{id}/cancel` 写成 `get`（实际已改为 POST）。
    /// 这条把"改路由忘了改表"从人肉检查变成**构建失败**。
    #[test]
    fn openapi_table_matches_registered_routes() {
        let registered = registered_routes_from_source(include_str!("mod.rs"));
        // 反向自检：解析器若失灵会抽出空集，那样下面的比对会"假通过"
        assert!(
            registered.len() >= 40,
            "源码扫描只抽到 {} 条路由，先检查解析器而不是相信结论",
            registered.len()
        );
        let mut documented: Vec<(String, String)> = handlers::ROUTES
            .iter()
            .map(|(p, m)| (p.to_string(), m.to_string()))
            .collect();
        documented.sort();
        documented.dedup();

        let missing: Vec<_> = registered
            .iter()
            .filter(|r| !documented.contains(r))
            .collect();
        let extra: Vec<_> = documented
            .iter()
            .filter(|d| !registered.contains(d))
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "OpenAPI 声明表与注册路由不一致：\n  表里缺（已注册、表没写）: {missing:?}\n  幽灵声明（表写了、未注册）: {extra:?}\n  注册 {} 条 / 表 {} 条",
            registered.len(),
            documented.len()
        );
    }

    // ---------- 产物 / 资产 端点契约（这组此前零覆盖） ----------
    // 覆盖：/artifacts、/artifacts/{id}、/artifacts/{id}/bytes、/assets/{id}、
    //       /deliveries/{id}/complete。
    // 刻意**不**覆盖（并说明理由，免得日后被当成漏项）：
    // - /recipes/import 与 comfyui 三条、/providers/llm/models：需要**外部网络**
    //   （RunningHub / ComfyUI / LLM 端点），要么造假要么跳过，属"要不要做集成测试"的决策；
    // - /settings/open-download-dir：会**真的在用户机器上打开资源管理器**，测试不该有这种副作用。

    #[tokio::test]
    async fn artifact_endpoints_empty_list_and_unknown_id_is_404() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["artifacts:read"]).await;

        // 无 run 时是全局分页列表；种子环境没有产物 ⇒ 空列表 + Page 信封字段齐全。
        let (status, body) = get_json(&app, "/api/v1/artifacts", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["items"].as_array().unwrap().len(), 0, "{body}");
        assert_eq!(body["data"]["hasMore"], false, "{body}");
        assert!(body["data"].get("nextCursor").is_some(), "{body}");

        // 未知 id 要报结构化 404，而不是 500 或空 200。
        let (status, body) = get_json(&app, "/api/v1/artifacts/424242", &token).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "ARTIFACT_NOT_FOUND", "{body}");

        let (status, body) = get_json(&app, "/api/v1/artifacts/424242/bytes", &token).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "ARTIFACT_NOT_FOUND", "{body}");
    }

    #[tokio::test]
    async fn asset_metadata_and_bytes_endpoints_with_unknown_id_404() {
        let (app, repositories) = test_app();
        let token = issue_token(&repositories, &["assets:local-read"]).await;
        repositories.seed_asset(
            crate::v2::domain::ids::AssetId(7),
            MediaObject {
                id: MediaObjectId::Asset(crate::v2::domain::ids::AssetId(7)),
                kind: MediaKind::Asset,
                filename: "ref.png".to_string(),
                mime_type: "image/png".to_string(),
                byte_size: 4,
                checksum: "sum-7".to_string(),
                storage_key: None,
                provider_refs: Vec::new(),
            },
            vec![1, 2, 3, 4],
        );

        let (status, body) = get_json(&app, "/api/v1/assets/7", &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["assetId"], 7, "{body}");
        assert_eq!(body["data"]["filename"], "ref.png", "{body}");
        assert_eq!(body["data"]["mimeType"], "image/png", "{body}");
        assert_eq!(body["data"]["byteSize"], 4, "{body}");
        assert_eq!(body["data"]["checksum"], "sum-7", "{body}");

        let (status, body) = get_json(&app, "/api/v1/assets/999999", &token).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "ASSET_NOT_FOUND", "{body}");

        // 字节端点（与 /artifacts/{id}/bytes 对称）：当初就是缺它，历史附图才渲染不出来。
        let request = Request::builder()
            .uri("/api/v1/assets/7/bytes")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "4");
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), &[1u8, 2, 3, 4], "应回原始字节");

        let (status, body) = get_json(&app, "/api/v1/assets/999999/bytes", &token).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "ASSET_NOT_FOUND", "{body}");
    }

    #[tokio::test]
    async fn every_verb_hits_a_real_route_with_the_right_method() {
        use crate::v2::clients::client::V2Client;
        use crate::v2::clients::verbs::{self, Args};

        let (app, repositories) = test_app();
        let token = issue_token(
            &repositories,
            &[
                "recipes:read",
                "recipes:write",
                "operations:read",
                "operations:execute",
                "operations:cancel",
                "assets:write",
                "artifacts:read",
                "connections:read",
                "agents:read",
                "agents:write",
                "deliveries:read",
                "deliveries:run",
            ],
        )
        .await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定端口");
        let address = listener.local_addr().expect("端口地址");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        // 自报 mcp：与真实 MCP 客户端同一条路（花钱闸门也在那条路上）。
        let client =
            V2Client::new(format!("http://127.0.0.1:{}", address.port()), token).with_driver("mcp");

        // 上传用**真的临时文件**：否则本地读文件就先失败了，`/assets` 这条路径根本没被验证。
        let upload_path = std::env::temp_dir().join("renderloft-route-probe.png");
        std::fs::write(&upload_path, [0x89, b'P', b'N', b'G']).expect("写临时文件");

        for verb in verbs::VERBS {
            // `daemon.*` 打的是**外壳路由**（`/api/daemon/shutdown`），不在这个 v2 路由表里，
            // 而且它俩的行为取决于**本机 daemon 是否在跑**（不在跑时压根不发请求）——
            // 放进这条循环会让测试结果随环境漂移（实测踩到：本机 daemon 一在跑，这条就红）。
            // 它们自己的行为由 `daemon.rs` 的路由测试与 CLI 真跑覆盖。
            if !verb.needs_daemon() {
                continue;
            }
            let arguments = route_probe_arguments(verb, &upload_path);
            let args = Args::from_mcp(verb, &arguments)
                .unwrap_or_else(|error| panic!("{} 的探测参数应当合法：{error}", verb.id));
            if let Err(error) = verbs::execute(&client, verb, &args).await {
                assert_ne!(
                    error.status, 405,
                    "{} 用了真实路由不接受的方法：{}",
                    verb.id, error.message
                );
                assert_ne!(
                    error.code, "ROUTE_NOT_FOUND",
                    "{} 打的路径在真实路由上不存在：{}",
                    verb.id, error.message
                );
            }
        }

        // 提交任务有**两种形状**（`inputs` 单次 / `runs` 批量），上面那轮走的是批量那条。
        // 单次那条也要真的打到 `/operations` 上——两条分支各写各的请求体，只测一条
        // 等于另一半没人管。
        let run_verb = verbs::find_by_id("operation.run").expect("run verb");
        let args = Args::from_mcp(
            run_verb,
            &json!({ "recipeRevisionId": 999_999, "inputs": {} }),
        )
        .expect("单次形状应当合法");
        if let Err(error) = verbs::execute(&client, run_verb, &args).await {
            assert_ne!(error.status, 405, "单次提交的方法不对：{}", error.message);
            assert_ne!(
                error.code, "ROUTE_NOT_FOUND",
                "单次提交的路径不存在：{}",
                error.message
            );
        }
        let _ = std::fs::remove_file(&upload_path);
    }

    /// 路由探测用的参数：**不存在的 id**（避免副作用），文件类的给真路径。
    fn route_probe_arguments(
        verb: &'static crate::v2::clients::verbs::Verb,
        upload_path: &std::path::Path,
    ) -> Value {
        use crate::v2::clients::verbs::{Kind, Param};
        fn probe(param: &Param, upload_path: &std::path::Path) -> Value {
            match param.kind {
                // 一个不存在的 id：所有动词都会以**领域错误**结束（404 / 冲突），
                // 而不是真的建出东西来。
                Kind::Id => json!(999_999),
                Kind::Count => json!(1),
                Kind::Text => match param.name {
                    // RunningHub 应用 ID：能过 `parse_webapp_id`，然后死在"连接不存在"上
                    // ——这样路径被真的打到了，又不会发出任何外部请求。
                    "source" => json!("2084540538883235841"),
                    _ => json!("x"),
                },
                Kind::Flag => json!(true),
                Kind::IdList => json!([999_999]),
                Kind::Json => json!({}),
                // 非空的一条 run：空数组会**在本地就被拒**（RUNS_EMPTY），那样路径没被验证。
                Kind::JsonArray => json!([{ "inputs": {} }]),
                Kind::LocalFile => json!(upload_path.to_string_lossy()),
                // keyed 资产：形状合法、指向不存在的资产——路径要真的打到 daemon
                // （然后死在资产校验上），而不是在本地就被丢掉。
                Kind::KeyedAssets => {
                    json!({ "475::image": { "assetId": 999_999, "checksum": "nope" } })
                }
            }
        }
        let mut arguments = serde_json::Map::new();
        // 可选参数也给：给全了才能保证"每条分支都真的打出去"，而不是在本地就被拒。
        for param in verb.mcp_params() {
            arguments.insert(param.mcp_name().to_string(), probe(param, upload_path));
        }
        Value::Object(arguments)
    }

    // ---------- 鉴权 / 授权覆盖面守卫 ----------
    /// 把源码里 `.route(...)` 的**路由组归属**也解析出来：`protected` 组内的路径集合
    /// 与组外的路径集合。用于回答"有没有路由绕过 auth 中间件"。
    ///
    /// 判据是**结构**而非注释：`build_router` 里 `protected` 组以 `.route_layer(...)`
    /// 收尾，之后到 `merge(protected)` 之前的 `.route(` 全部无 auth。
    fn routes_split_by_auth_layer(src: &str) -> (Vec<String>, Vec<String>) {
        let body = match src.find("\nmod tests {") {
            Some(i) => &src[..i],
            None => src,
        };
        // 只剥整行注释（行首 //）——理由同 `registered_routes_from_source`。
        let body: String = body
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // 找到 `let protected = Router::new()` 与 `route_layer` 的分界
        let prot_start = body
            .find("let protected = Router::new()")
            .expect("找不到 protected 路由组起点");
        let layer_at = body[prot_start..]
            .find(".route_layer(")
            .map(|i| prot_start + i)
            .expect("protected 组缺少 route_layer（auth 中间件），整组将无鉴权");

        let path_of = |seg_zone: &str| -> Vec<String> {
            let mut out = Vec::new();
            let bytes = seg_zone.as_bytes();
            let mut cursor = 0usize;
            while let Some(rel) = seg_zone[cursor..].find(".route(") {
                let start = cursor + rel + ".route(".len();
                let mut depth = 1usize;
                let mut end = start;
                let mut j = start;
                while j < bytes.len() {
                    match bytes[j] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let seg = &seg_zone[start..end];
                if let Some(p0) = seg.find("\"/api/v1") {
                    let after = &seg[p0 + 1..];
                    if let Some(p1) = after.find('"') {
                        out.push(after[..p1].to_string());
                    }
                }
                cursor = if end > start { end } else { start + 1 };
            }
            out.sort();
            out.dedup();
            out
        };

        let protected_zone = &body[prot_start..layer_at];
        let outside_zone = &body[layer_at..];
        (path_of(protected_zone), path_of(outside_zone))
    }

    /// 每条受 `auth` 中间件保护的路由，其 handler **必须**调用 `require_scope`；
    /// 且 `health` 必须是**唯一**不经过 auth 的路由。
    ///
    /// 为什么需要这条：axum 无法自省中间件覆盖范围，两件事都没有运行时判据 ——
    /// ① 新加一条路由忘了写 `require_scope`（任何持 token 的 actor 都能操作）；
    /// ② 新加一条路由写在 `protected` 组之外（完全跳过鉴权）。
    /// 两者都是**真实越权缺口**，且都会静默通过现有测试（现有 403 断言只覆盖了
    /// 少数几个端点，不是全量）。这条把"漏守卫"从人肉巡检变成**构建失败**。
    ///
    /// 例外白名单是**显式且带理由**的（避免日后被当成漏项）：
    /// - `health`：设计上公开，结构上就在 `protected` 之外，不经过 auth。
    /// - `capabilities` / `openapi`：在 `protected` 内（**经过 auth**，只是不要求具体 scope）。
    ///   二者只读常量与 driver 描述符（字段形状声明，无密钥、无写操作），
    ///   属于"任何已认证 actor 都应能读到"的发现类端点。
    #[test]
    fn every_protected_route_requires_scope_and_health_is_the_only_public_route() {
        const SRC: &str = include_str!("mod.rs");

        // ---- ① health 必须是唯一的免鉴权路由 ----
        let (protected_paths, outside_paths) = routes_split_by_auth_layer(SRC);
        // 反向自检：解析器失灵会抽出空集，那样下面的"相等"会假通过
        assert!(
            protected_paths.len() >= 25,
            "protected 组只解析出 {} 条路径，先检查解析器而不是相信结论",
            protected_paths.len()
        );
        assert_eq!(
            outside_paths,
            vec!["/api/v1/health".to_string()],
            "有路由绕过了 auth 中间件（protected 组之外只允许 health）：{outside_paths:?}"
        );

        // ---- ② 每个受保护路由的 handler 必须调用 require_scope ----
        // 免 scope 白名单：只读、无副作用、无密钥的发现类端点。
        const SCOPE_EXEMPT: [&str; 2] = ["capabilities", "openapi"];

        // 收集 protected 组接线到的 handler 名（形如 handlers::xxx / shell::xxx）
        let prot_zone_start = SRC
            .find("let protected = Router::new()")
            .expect("找不到 protected 起点");
        let layer_at = SRC[prot_zone_start..]
            .find(".route_layer(")
            .map(|i| prot_zone_start + i)
            .expect("找不到 route_layer");
        let zone: String = SRC[prot_zone_start..layer_at]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut handler_names: Vec<String> = Vec::new();
        for module in ["handlers", "shell"] {
            let needle = format!("{module}::");
            let mut k = 0usize;
            while let Some(r) = zone[k..].find(&needle) {
                let at = k + r + needle.len();
                let rest = &zone[at..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    handler_names.push(name);
                }
                k = at;
            }
        }
        handler_names.sort();
        handler_names.dedup();
        assert!(
            handler_names.len() >= 30,
            "protected 组只解析出 {} 个 handler，先检查解析器",
            handler_names.len()
        );

        // 两个 handler 文件合起来做函数体定位（配平花括号，不靠缩进）
        let all_src = format!(
            "{}\n{}\n{}",
            SRC,
            include_str!("handlers.rs"),
            include_str!("shell.rs"),
        );
        // 剥整行注释后再判 require_scope —— 否则 `// 说明 require_scope` 会被误判为已校验
        // （本项目真实踩过：注释里出现关键字让扫描器"看起来绿"）。
        let stripped: String = all_src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let extract_body = |fn_name: &str| -> Option<String> {
            let pat = format!("pub async fn {fn_name}(");
            let mut cursor = 0usize;
            while let Some(rel) = stripped[cursor..].find(&pat) {
                let at = cursor + rel;
                // 必须位于行首（可含空白）—— 排除签名出现在其它标识符内部
                let line_start = stripped[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
                if stripped[line_start..at].trim().is_empty() {
                    if let Some(brace) = stripped[at..].find('{') {
                        let abs = at + brace;
                        let bytes = stripped.as_bytes();
                        let mut depth = 0i32;
                        let mut j = abs;
                        while j < bytes.len() {
                            match bytes[j] {
                                b'{' => depth += 1,
                                b'}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        return Some(stripped[at..=j].to_string());
                                    }
                                }
                                _ => {}
                            }
                            j += 1;
                        }
                    }
                    return None;
                }
                cursor = at + pat.len();
            }
            None
        };

        let mut missing: Vec<String> = Vec::new();
        let mut not_found: Vec<String> = Vec::new();
        for h in &handler_names {
            if SCOPE_EXEMPT.contains(&h.as_str()) {
                continue;
            }
            match extract_body(h) {
                Some(body) => {
                    if !body.contains("require_scope") {
                        missing.push(h.clone());
                    }
                }
                None => not_found.push(h.clone()),
            }
        }
        assert!(
            not_found.is_empty(),
            "以下 handler 定位不到函数体，先检查解析器：{not_found:?}"
        );
        assert!(
            missing.is_empty(),
            "以下受保护路由的 handler 没有调用 require_scope（越权风险）：{missing:?}\n\
             若确属只读发现类端点，请把它加进 SCOPE_EXEMPT 并写明理由。"
        );
    }
}
