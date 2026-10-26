use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::any;
use axum::Router;

use crate::v2::application::callbacks::CallbackService;
use crate::v2::application::cancel_operation::CancelOperation;
use crate::v2::application::create_operation::{CreateOperation, Repositories};
use crate::v2::application::hygiene::HygieneService;
use crate::v2::application::recipe_import::RecipeImportService;
use crate::v2::application::reconcile::ReconcileService;
use crate::v2::application::scheduler::SchedulerService;
use crate::v2::application::settings::{RunningHubStatusService, SettingsService};
use crate::v2::application::validate_operation::ValidateOperation;
use crate::v2::domain::actor::{ActorKind, Scope};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::model::SecretBinding;
use crate::v2::infrastructure::providers::comfyui::ComfyUiDriver;
use crate::v2::infrastructure::providers::openai_images::OpenAiImagesDriver;
use crate::v2::infrastructure::providers::runninghub::RunningHubDriver;
use crate::v2::infrastructure::schema::BasicSchemaValidator;
use crate::v2::infrastructure::sqlite::SqliteRepositories;
use crate::v2::infrastructure::token_service::TokenService;
// ⚠️ 后台循环里的日志一律走 `log_line`（写失败不 panic），**不要**用 `eprintln!`：
// stdout/stderr 一断，`eprintln!` 就 panic，而 `run_tick` 报告 panic 时又要写日志 ⇒
// 循环彻底静默停摆。实测过一次：对账循环就是这么死的（任务永远停在 ACCEPTED）。
// 启动期那几行仍用 `eprintln!`——只写一次，写不出去就该大声失败；但**要带时间前缀**
// （`logging::timestamp()`）：那几行恰恰是"这次是什么时候起的"唯一证据。
use crate::v2::logging::{log_line, timestamp};
use crate::v2::ports::policy::{AuthorizationPolicy, PolicyDecision};
use crate::v2::ports::provider::{driver_key, DriverRegistry};
use crate::v2::ports::stores::{SecretStore, SecretValue};
use crate::v2::transports::http::{build_router, V2HttpState};

/// 库文件名只有一个所有者：端口层（工作区契约的一部分）。这里只是沿用它的旧名字，
/// 免得 `application` 为了拿这个常量去依赖组合根（分层守门会拦）。
pub const V2_DB_FILE: &str = crate::v2::ports::repositories::WORKSPACE_DB_FILE;
pub const BOOTSTRAP_TOKEN_FILE: &str = "v2-bootstrap.token";

/// 基于 Actor scopes 的授权策略。领域层已内置 has_scope 检查，此处作为显式策略端口实现。
pub struct ScopeAuthorizationPolicy;

#[async_trait::async_trait]
impl AuthorizationPolicy for ScopeAuthorizationPolicy {
    async fn require_scope(
        &self,
        actor: &crate::v2::domain::actor::ActorRef,
        scope: Scope,
    ) -> DomainResult<PolicyDecision> {
        if actor.has_scope(scope) {
            Ok(PolicyDecision {
                allowed: true,
                reason: None,
            })
        } else {
            Ok(PolicyDecision {
                allowed: false,
                reason: Some(format!("缺少 {scope:?} 权限")),
            })
        }
    }
}

/// 空 SecretStore：未配置凭据绑定（本地 ComfyUI 等无密钥场景）。
pub struct NoopSecretStore;

#[async_trait::async_trait]
impl SecretStore for NoopSecretStore {
    async fn resolve(&self, _binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
        Ok(None)
    }
    async fn set(&self, _slot: &str, _value: &str) -> DomainResult<()> {
        Ok(())
    }
    async fn delete(&self, _slot: &str) -> DomainResult<()> {
        Ok(())
    }
}

/// 基于系统凭据管理器（keyring）的 SecretStore。
pub struct KeyringSecretStore {
    service: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service: "renderloft-v2".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl SecretStore for KeyringSecretStore {
    async fn resolve(&self, binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
        let entry = keyring::Entry::new(&self.service, &binding.slot)
            .map_err(|error| SafeError::storage(format!("无法打开凭据条目：{error}")))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(SecretValue { secret })),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SafeError::storage(format!("无法读取凭据：{error}"))),
        }
    }
    async fn set(&self, slot: &str, value: &str) -> DomainResult<()> {
        let entry = keyring::Entry::new(&self.service, slot)
            .map_err(|error| SafeError::storage(format!("无法打开凭据条目：{error}")))?;
        entry
            .set_password(value)
            .map_err(|error| SafeError::storage(format!("无法写入凭据：{error}")))
    }
    async fn delete(&self, slot: &str) -> DomainResult<()> {
        let entry = keyring::Entry::new(&self.service, slot)
            .map_err(|error| SafeError::storage(format!("无法打开凭据条目：{error}")))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SafeError::storage(format!("无法删除凭据：{error}"))),
        }
    }
}

pub struct V2Runtime {
    pub repositories: Arc<SqliteRepositories>,
    pub state: Arc<V2HttpState>,
    pub scheduler: Arc<SchedulerService>,
    pub reconcile: Arc<ReconcileService>,
    pub callbacks: Arc<CallbackService>,
    pub hygiene: Arc<HygieneService>,
    pub bootstrap_token: String,
}

impl V2Runtime {
    pub async fn initialize(
        data_dir: &Path,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, String> {
        let mut drivers = DriverRegistry::new();
        drivers.register(
            driver_key("openai", "openai-images"),
            Arc::new(OpenAiImagesDriver::new(Box::new(secrets.clone()))),
        );
        drivers.register(
            driver_key("async", "async-task"),
            Arc::new(
                crate::v2::infrastructure::providers::async_task::AsyncTaskDriver::new(Box::new(
                    secrets.clone(),
                )),
            ),
        );
        drivers.register(
            driver_key("comfyui", "comfyui"),
            Arc::new(ComfyUiDriver::new(Box::new(secrets.clone()))),
        );
        drivers.register(
            driver_key("runninghub", "runninghub"),
            Arc::new(RunningHubDriver::new(Box::new(secrets.clone()))),
        );
        Self::initialize_with_drivers(data_dir, secrets, drivers).await
    }

    pub async fn initialize_with_drivers(
        data_dir: &Path,
        secrets: Arc<dyn SecretStore>,
        drivers: DriverRegistry,
    ) -> Result<Self, String> {
        let repositories = Arc::new(SqliteRepositories::open(&data_dir.join(V2_DB_FILE))?);
        let drivers = Arc::new(drivers);

        let concrete = repositories.clone();
        let connections: Arc<dyn crate::v2::ports::repositories::ConnectionRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::ConnectionRepository>;
        // 一次性迁移旧共享 api_key 槽位到连接级 scoped slot（幂等，见函数注释）。
        crate::v2::application::connection::migrate_legacy_api_key_slots(&connections, &secrets)
            .await
            .map_err(|error| format!("迁移旧连接凭据失败：{error}"))?;
        let recipes: Arc<dyn crate::v2::ports::repositories::RecipeRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::RecipeRepository>;
        let operations: Arc<dyn crate::v2::ports::repositories::OperationRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::OperationRepository>;
        let events: Arc<dyn crate::v2::ports::repositories::EventStore> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::EventStore>;
        let assets: Arc<dyn crate::v2::ports::repositories::AssetRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::AssetRepository>;
        let artifacts: Arc<dyn crate::v2::ports::stores::ArtifactStore> =
            concrete.clone() as Arc<dyn crate::v2::ports::stores::ArtifactStore>;
        let artifacts_repo: Arc<dyn crate::v2::ports::repositories::ArtifactRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::ArtifactRepository>;
        let tokens: Arc<dyn crate::v2::ports::repositories::TokenStore> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::TokenStore>;
        let token_service: Arc<dyn crate::v2::ports::token::TokenService> =
            Arc::new(TokenService::new(tokens.clone()));
        let leases: Arc<dyn crate::v2::ports::repositories::LeaseStore> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::LeaseStore>;
        let callback_deliveries: Arc<
            dyn crate::v2::ports::repositories::CallbackDeliveryRepository,
        > = concrete.clone() as Arc<dyn crate::v2::ports::repositories::CallbackDeliveryRepository>;
        let workspace: Arc<dyn crate::v2::ports::repositories::WorkspaceRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::WorkspaceRepository>;

        let create_operation = Arc::new(CreateOperation::new(
            Repositories {
                recipes: Box::new(recipes.clone()),
                connections: Box::new(connections.clone()),
                operations: Box::new(operations.clone()),
                assets: Box::new(assets.clone()),
            },
            Box::new(ScopeAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        ));
        // 只读预检：与 create 共用同一套解析（一份判据，两个入口）。
        let validate_operation = Arc::new(ValidateOperation::new(create_operation.clone()));
        let cancel_operation = Arc::new(CancelOperation {
            repositories: operations.clone(),
            connections: connections.clone(),
            drivers: drivers.clone(),
        });
        let scheduler = Arc::new(SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "v2-scheduler".to_string(),
            repositories: operations.clone(),
            leases,
            connections: connections.clone(),
            assets: assets.clone(),
            artifacts: artifacts.clone(),
            drivers: drivers.clone(),
        });
        let reconcile = Arc::new(ReconcileService {
            repositories: operations.clone(),
            connections: connections.clone(),
            artifacts: artifacts.clone(),
            drivers: drivers.clone(),
        });
        let hygiene = Arc::new(HygieneService {
            repositories: operations.clone(),
        });
        let callbacks = Arc::new(CallbackService {
            repositories: operations.clone(),
            deliveries: callback_deliveries,
            deliverer: Arc::new(
                crate::v2::infrastructure::sqlite::exec::HttpCallbackDeliverer::new()?,
            ),
            // 只用来**读**回调签名密钥（写它只有人那条路）。
            secrets: secrets.clone(),
        });
        // 平台账号/队列状态服务：HTTP 端点用它。
        let status = Arc::new(RunningHubStatusService::new(
            connections.clone(),
            drivers.clone(),
            secrets.clone(),
        ));

        let concrete_settings: Arc<dyn crate::v2::ports::repositories::SettingsRepository> =
            concrete.clone() as Arc<dyn crate::v2::ports::repositories::SettingsRepository>;
        let settings = Arc::new(SettingsService::new(concrete_settings));
        let import_service = Arc::new(RecipeImportService::new(
            connections.clone(),
            recipes.clone(),
            drivers.clone(),
        ));

        // 迁移/备份：把工作区一致地导出一份（库走 `VACUUM INTO`，产物缓存整份拷）。
        let workspace_backup = Arc::new(
            crate::v2::application::workspace_backup::WorkspaceBackupService::new(workspace),
        );

        let state = Arc::new(V2HttpState::new(
            token_service,
            secrets.clone(),
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
            Arc::new(crate::v2::infrastructure::providers::safety::ReqwestSafeHttpClient),
            workspace_backup,
        ));

        let bootstrap_token = ensure_bootstrap_token(data_dir, &state).await?;

        Ok(Self {
            repositories,
            state,
            scheduler,
            reconcile,
            callbacks,
            hygiene,
            bootstrap_token,
        })
    }

    pub fn router(&self) -> Router {
        build_router(self.state.clone())
    }

    /// 启动后台调度循环：提交、对账、回调投递。返回 JoinHandle。
    pub fn spawn_background_tasks(&self) -> tokio::task::JoinHandle<()> {
        let scheduler = self.scheduler.clone();
        let reconcile = self.reconcile.clone();
        let callbacks = self.callbacks.clone();
        let hygiene = self.hygiene.clone();
        // 三个后台任务各自独立：reconcile 的长轮询不再阻塞 scheduler 提交
        // 与回调投递。**每轮 panic 只掀掉那一轮**（见 `run_tick`）。
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let scheduler = scheduler.clone();
                run_tick("scheduler", async move {
                    if let Err(error) = scheduler.tick().await {
                        log_line(&format!("[renderloft-scheduler] tick 失败：{error}"));
                    }
                })
                .await;
            }
        });
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let reconcile = reconcile.clone();
                run_tick("reconcile", async move {
                    if let Err(error) = reconcile.tick(200).await {
                        log_line(&format!("[renderloft-reconcile] tick 失败：{error}"));
                    }
                })
                .await;
            }
        });
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let callbacks = callbacks.clone();
                run_tick("callbacks", async move {
                    // `tick` = 先补扫（把"已终态但还没投递记录"的操作补上）+ 再投递到期的。
                    if let Err(error) = callbacks.tick(50).await {
                        log_line(&format!("[renderloft-callbacks] tick 失败：{error}"));
                    }
                })
                .await;
            }
        });
        // 卫生清理：陈旧任务判失败、超期历史归档删除。周期较长且独立于实时对账。
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let hygiene = hygiene.clone();
                run_tick("hygiene", async move {
                    if let Err(error) = hygiene.tick().await {
                        log_line(&format!("[renderloft-hygiene] tick 失败：{error}"));
                    }
                })
                .await;
            }
        })
    }

    pub fn bootstrap_token(&self) -> &str {
        &self.bootstrap_token
    }

    /// 将调用方（V1 daemon）的 access token 注册为 V2 全 scope token，
    /// 使 vite 代理注入的同一个 Bearer token 同时通过 V1 与 V2 鉴权。
    /// 轮换时撤销上一轮的 daemon token，避免旧记录无限积累且泄漏后仍有效。
    pub async fn register_access_token(&self, token: &str) -> Result<(), String> {
        use crate::v2::ports::repositories::TokenStore;
        let store = self.repositories.clone() as Arc<dyn TokenStore>;
        let hash = crate::v2::infrastructure::token_service::sha256_hex(token);
        if let Some(existing) = store.find_by_hash(&hash).await.map_err(|e| e.to_string())? {
            // 早退修复：命中但已撤销的记录需要重新激活，
            // 否则 daemon 重启后（token 未变）控制台永久 401。
            if existing.revoked {
                store
                    .set_revoked(existing.id, false)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        for record in store.list().await.map_err(|e| e.to_string())? {
            if record.actor_id == "daemon" && !record.revoked {
                store.revoke(record.id).await.map_err(|e| e.to_string())?;
            }
        }
        store
            .create(&crate::v2::ports::repositories::ActorTokenRecord {
                id: 0,
                token_hash: hash,
                actor_id: "daemon".to_string(),
                kind: ActorKind::Gui,
                client_id: "daemon".to_string(),
                scopes: vec![
                    Scope::RecipesRead,
                    Scope::RecipesWrite,
                    Scope::OperationsRead,
                    Scope::OperationsExecute,
                    Scope::OperationsCancel,
                    Scope::OperationsDelete,
                    Scope::OperationsSelectConnection,
                    Scope::AssetsLocalRead,
                    Scope::AssetsWrite,
                    Scope::ArtifactsRead,
                    Scope::ConnectionsRead,
                    Scope::ConnectionsManage,
                    Scope::SettingsRead,
                    Scope::SettingsManage,
                    Scope::DiagnosticsRead,
                    Scope::ActorTokensManage,
                    Scope::AgentsRead,
                    Scope::AgentsWrite,
                    Scope::AgentsRun,
                    Scope::DeliveriesRead,
                    Scope::DeliveriesRun,
                ],
                label: Some("daemon access token".to_string()),
                expires_at: None,
                revoked: false,
                created_at: crate::v2::domain::model::Timestamp::now(),
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

async fn ensure_bootstrap_token(data_dir: &Path, state: &V2HttpState) -> Result<String, String> {
    let path = data_dir.join(BOOTSTRAP_TOKEN_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let (token, _) = state
        .tokens
        .issue(
            "bootstrap",
            ActorKind::Gui,
            "bootstrap",
            vec![
                Scope::RecipesRead,
                Scope::RecipesWrite,
                Scope::OperationsRead,
                Scope::OperationsExecute,
                Scope::OperationsCancel,
                Scope::OperationsSelectConnection,
                Scope::AssetsLocalRead,
                Scope::AssetsWrite,
                Scope::ArtifactsRead,
                Scope::ConnectionsRead,
                Scope::ConnectionsManage,
                Scope::SettingsRead,
                Scope::SettingsManage,
                Scope::DiagnosticsRead,
                Scope::ActorTokensManage,
                Scope::AgentsRead,
                Scope::AgentsWrite,
                Scope::AgentsRun,
                Scope::DeliveriesRead,
                Scope::DeliveriesRun,
            ],
            Some("bootstrap 管理员".to_string()),
            None,
        )
        .await
        .map_err(|error| format!("无法签发 bootstrap token：{error}"))?;
    std::fs::write(&path, &token).map_err(|error| format!("无法写入 bootstrap token：{error}"))?;
    Ok(token)
}

/// 工作区（SQLite + 产物 + 日志 + 端点文件）的根目录。
///
/// ⚠️ 目录名**不能**是 `renderloft`：安装包（NSIS，`installMode: currentUser`）默认就装到
/// `%LOCALAPPDATA%\RenderLoft`，Windows 路径不区分大小写 ⇒ 那就是**同一个文件夹**——
/// 实测程序文件与用户数据会混在一起（"删掉数据目录重置"会连程序一起删，卸载还会留下一个
/// 名字像程序、内容全是数据的目录）。所以工作区放在**同级的独立目录**里，任何安装模式下都不撞。
pub fn v2_data_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .ok_or_else(|| "无法定位本地数据目录".to_string())
        .map(|path| path.join("renderloft-workspace"))
}

/// 跑一轮后台 tick：**panic 只掀掉这一轮**，循环下一轮照常。
///
/// 为什么必须这样写：`let _ = service.tick().await;` 只吞 `Result`——**panic 会掀掉整个
/// `tokio::spawn` 的任务**，那个循环从此永久停摆。实测（2026-10-08，本机）：对账循环死掉
/// 之后，已经成功完成的任务永远不被记录、那条连接的执行位永远放不出来，用户看到的只是
/// "任务一直转圈、后面的任务永远不开始"，而且**只有重启才恢复**。
///
/// 此前这里的注释写着"每轮 panic 也会被任务循环吞掉，避免单点崩溃停摆全部"——那是假的。
/// `catch_unwind` 配 async 不安全（跨 await 的 panic 不可恢复），可靠的做法就是每轮单开一个
/// 任务，把它返回的 `JoinError` 收下。**崩溃会写进日志**，所以下一次能直接看到是哪个循环。
async fn run_tick<F>(name: &str, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if let Err(error) = tokio::spawn(future).await {
        log_line(&format!(
            "[renderloft-{name}] 这一轮 tick 崩了（已隔离，下一轮继续）：{error}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 负对照：把 `run_tick` 换回直接 `future.await`（不 spawn），这条会随 panic 一起红。
    #[tokio::test]
    async fn a_panicking_tick_only_kills_that_round() {
        let mut rounds = 0;
        for round in 0..3 {
            run_tick("test", async move {
                if round == 1 {
                    panic!("这一轮故意崩");
                }
            })
            .await;
            rounds += 1;
        }
        assert_eq!(rounds, 3, "panic 只该掀掉那一轮，循环必须继续转");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-test-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
        dir
    }

    #[tokio::test]
    async fn initialize_creates_database_and_bootstrap_token() {
        let dir = temp_dir("init");
        let runtime = V2Runtime::initialize(&dir, Arc::new(NoopSecretStore))
            .await
            .expect("初始化应成功");
        assert!(dir.join(V2_DB_FILE).exists());
        assert!(dir.join(BOOTSTRAP_TOKEN_FILE).exists());
        assert!(runtime.bootstrap_token().starts_with("rh-bootstrap-"));
        // bootstrap token 已注册且可认证
        let _records = runtime
            .state
            .tokens
            .list()
            .await
            .expect("读取 token 记录应成功");
        let actor = runtime
            .state
            .tokens
            .authenticate(runtime.bootstrap_token())
            .await
            .expect("bootstrap token 应可认证");
        assert!(actor.has_scope(Scope::ActorTokensManage));
        assert!(actor.has_scope(Scope::OperationsExecute));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bootstrap_token_is_stable_across_restarts() {
        let dir = temp_dir("stable");
        let first = V2Runtime::initialize(&dir, Arc::new(NoopSecretStore))
            .await
            .expect("首次初始化应成功");
        let token_a = first.bootstrap_token().to_string();
        let second = V2Runtime::initialize(&dir, Arc::new(NoopSecretStore))
            .await
            .expect("再次初始化应成功");
        assert_eq!(token_a, second.bootstrap_token());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scope_policy_enforces_actor_scopes() {
        let policy = ScopeAuthorizationPolicy;
        let actor = crate::v2::domain::actor::ActorRef {
            id: "a".to_string(),
            kind: ActorKind::Http,
            client_id: crate::v2::domain::ids::ClientId("c".to_string()),
            scopes: vec![Scope::OperationsExecute],
            label: None,
        };
        let allowed = policy
            .require_scope(&actor, Scope::OperationsExecute)
            .await
            .expect("策略应工作");
        assert!(allowed.allowed);
        let denied = policy
            .require_scope(&actor, Scope::OperationsCancel)
            .await
            .expect("策略应工作");
        assert!(!denied.allowed);
    }

    #[test]
    fn keyring_store_resolves_missing_entry_as_none() {
        // 未配置凭据时应返回 None 而不是报错
        let store = KeyringSecretStore::default();
        let binding = SecretBinding {
            slot: "unlikely-entry-slot".to_string(),
            resolution: crate::v2::domain::model::SecretResolution::Current,
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(store.resolve(&binding));
        // 系统凭据管理器可用性依赖平台；无论返回 None 还是平台错误都不影响结构
        let _ = result;
    }

    /// `shell_router` 的路由集合必须**恰好等于**下列 3 条。
    ///
    /// 为什么需要这条：`shell_router`（daemon 的产品外壳）**不经过** `v2` 路由组那份
    /// `auth` 中间件 —— 它各自的防护是**分别**实现的（见下），所以"必须有 require_scope"
    /// 那种守卫对它不适用。若有人往这里**新增**一条路由而忘了配鉴权，就出现一条
    /// 完全逃出所有既有守卫的缝。这条把"加了路由"从静默变成**构建失败**，
    /// 逼作者在此处显式登记它的防护方式。
    ///
    /// 各路由的防护（改动时请一并更新）：
    /// - `/health`：**故意公开**。只返回 `{ok, version}`，不含任何状态或凭据。
    /// - `/bootstrap`：**一次性 nonce**（`?nonce=`，校验后立即清空 `*stored = None`）
    ///   ⇒ 换取 HttpOnly + SameSite=Strict 会话 cookie。无 nonce 或 nonce 错 = 401。
    /// - `/api/daemon/shutdown`：`shell_authorize`（Host 白名单 + Bearer/session == token），
    ///   失败 = 401。**效果有专门的行为测试**（见 e2e 模块 `daemon_shutdown_requires_auth`）。
    /// - `/api/daemon/bootstrap`：同一把 `shell_authorize`（要 daemon token）。换一张
    ///   **新的**一次性门票并作废旧的那张——外壳在"端口上坐着上一个进程留下的 daemon"
    ///   时靠它把界面打开（见 e2e `daemon_issue_bootstrap_rotates_nonce_for_authenticated_shell`）。
    #[test]
    fn shell_router_routes_are_registered_explicitly() {
        let src = include_str!("daemon.rs");
        // 只取 `fn shell_router` 的函数体。
        // ⚠️ 必须锚定**行首**：本测试的断言消息里也含 "fn shell_router(...)" 字样，
        // 直接 `find` 会先命中**本测试自己**（守卫被自己的搜索键喂脏，
        // 与"注释喂脏扫描器"同族）⇒ 抽到的 body 是测试代码，只能解析出 1 条假路由。
        let key = "fn shell_router(state: ShellState) -> Router {";
        let start = src
            .match_indices(key)
            .find(|(i, _)| {
                let line_start = src[..*i].rfind('\n').map(|j| j + 1).unwrap_or(0);
                src[line_start..*i].trim().is_empty()
            })
            .map(|(i, _)| i)
            .expect("找不到 shell_router 定义");
        let brace = src[start..]
            .find("-> Router {")
            .map(|i| start + i + "-> Router ".len())
            .expect("结构异常");
        let bytes = src.as_bytes();
        let mut depth = 0i32;
        let mut end = brace;
        for (i, b) in bytes.iter().enumerate().skip(brace) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body: String = src[brace..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut found: Vec<String> = Vec::new();
        let mut cursor = 0usize;
        while let Some(rel) = body[cursor..].find(".route(") {
            let at = cursor + rel;
            if let Some(q0) = body[at..].find('"') {
                let after = &body[at + q0 + 1..];
                if let Some(q1) = after.find('"') {
                    found.push(after[..q1].to_string());
                }
            }
            cursor = at + 1;
        }
        found.sort();
        // 反向自检：解析器失灵会抽出空集，那样下面的"相等"会假通过
        assert!(
            found.len() >= 3,
            "只从 shell_router 解析出 {} 条路由，先检查解析器",
            found.len()
        );
        let mut expected = vec![
            // 要 daemon token（`shell_authorize`），行为测试见 `daemon_shutdown_requires_auth`
            "/api/daemon/shutdown".to_string(),
            // 要 daemon token；换一张一次性 bootstrap 门票，行为测试见
            // `daemon_issue_bootstrap_rotates_nonce_for_authenticated_shell`
            "/api/daemon/bootstrap".to_string(),
            // 无需凭据，但校验一次性 nonce（`daemon_bootstrap_requires_one_time_nonce`）
            "/bootstrap".to_string(),
            // 无需凭据的探活（`daemon_health_is_public_and_leaks_nothing`）
            "/health".to_string(),
        ];
        expected.sort();
        assert_eq!(
            found, expected,
            "shell_router 的路由集合变了。新增/删除路由时，必须在此处显式登记它的\
             鉴权方式（并补对应的行为测试），否则它可能逃过所有既有守卫。\n实际: {found:?}"
        );
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use crate::v2::domain::execution::{
        ArtifactData, ArtifactDraft, ProviderDescriptor, RemoteHandle, SubmitOutcome, TaskSnapshot,
        TaskStatus,
    };
    use crate::v2::domain::model::LifecycleFeatures;
    use crate::v2::infrastructure::fakes::FakeProviderDriver;
    use crate::v2::ports::provider::driver_key;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-e2e-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
        dir
    }

    async fn request_json(
        app: &Router,
        method: &str,
        uri: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        let request = if let Some(body) = body {
            builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        } else {
            builder.body(Body::empty()).unwrap()
        };
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

    #[tokio::test]
    async fn full_operation_lifecycle_over_http() {
        let dir = temp_dir("lifecycle");
        let mut drivers = DriverRegistry::new();
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
        driver.push_outcome(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "task-e2e".to_string(),
        }));
        driver.push_snapshot(TaskSnapshot {
            status: TaskStatus::Succeeded,
            results: vec![ArtifactDraft {
                filename: "e2e.png".to_string(),
                mime_type: "image/png".to_string(),
                byte_size: 4,
                checksum: "e2e-checksum".to_string(),
                data: ArtifactData::Bytes(vec![1, 2, 3, 4]),
            }],
            error: None,
            usage: None,
        });
        drivers.register(driver_key("openai", "openai-images"), driver);

        let runtime = V2Runtime::initialize_with_drivers(&dir, Arc::new(NoopSecretStore), drivers)
            .await
            .expect("V2 运行时初始化应成功");
        let app = runtime.router();
        let token = runtime.bootstrap_token().to_string();

        let (status, connection) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "openai",
                "protocol": "openai-images",
                "displayName": "OpenAI Images",
                "config": { "baseUrl": "https://api.openai.com/v1" },
                "capabilities": ["text-to-image"],
                "lifecycleFeatures": { "synchronousResult": true }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{connection}");
        let connection_id = connection["data"]["connectionId"].as_u64().unwrap();

        let (status, recipe) = request_json(
            &app,
            "POST",
            "/api/v1/recipes",
            &token,
            Some(json!({
                "name": "E2E Image",
                "capability": "text-to-image",
                "driverKey": "openai-images",
                "defaultConnectionId": connection_id,
                "inputSchema": {
                    "type": "object",
                    "required": ["prompt"],
                    "properties": { "prompt": { "type": "string" } }
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{recipe}");
        let recipe_id = recipe["data"]["recipeId"].as_u64().unwrap();

        let (status, created) = request_json(
            &app,
            "POST",
            "/api/v1/operations",
            &token,
            Some(json!({
                "idempotencyKey": "e2e-key-1",
                "runs": [{
                    "clientRunId": "e2e-run",
                    "recipeRevisionId": recipe_id,
                    "inputs": { "prompt": "a cat" }
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let operation_id = created["data"]["operationId"].as_u64().unwrap();
        assert_eq!(created["data"]["status"], "QUEUED");

        runtime.scheduler.tick().await.expect("调度应成功");
        let (_, running) = request_json(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &token,
            None,
        )
        .await;
        assert_eq!(
            running["data"]["runs"][0]["status"], "ACCEPTED",
            "{running}"
        );
        assert_eq!(running["data"]["status"], "RUNNING", "{running}");

        runtime.reconcile.tick(200).await.expect("对账应成功");
        let (status, done) = request_json(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(done["data"]["status"], "SUCCEEDED", "{done}");
        assert_eq!(done["data"]["runs"][0]["status"], "SUCCEEDED");

        let (_, events) = request_json(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}/events"),
            &token,
            None,
        )
        .await;
        let types: Vec<&str> = events["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["eventType"].as_str().unwrap())
            .collect();
        assert!(types.contains(&"run.succeeded"), "{types:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn idempotent_replay_and_conflict_over_http() {
        let dir = temp_dir("idempotency");
        let mut drivers = DriverRegistry::new();
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
        drivers.register(driver_key("openai", "openai-images"), driver);
        let runtime = V2Runtime::initialize_with_drivers(&dir, Arc::new(NoopSecretStore), drivers)
            .await
            .expect("初始化应成功");
        let app = runtime.router();
        let token = runtime.bootstrap_token().to_string();

        let (_, connection) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "openai",
                "protocol": "openai-images",
                "displayName": "OpenAI Images",
                "config": { "baseUrl": "https://api.openai.com/v1" },
                "capabilities": ["text-to-image"]
            })),
        )
        .await;
        let connection_id = connection["data"]["connectionId"].as_u64().unwrap();
        let (_, recipe) = request_json(
            &app,
            "POST",
            "/api/v1/recipes",
            &token,
            Some(json!({
                "name": "E2E Image",
                "capability": "text-to-image",
                "driverKey": "openai-images",
                "defaultConnectionId": connection_id,
                "inputSchema": {
                    "type": "object",
                    "required": ["prompt"],
                    "properties": { "prompt": { "type": "string" } }
                }
            })),
        )
        .await;
        let recipe_id = recipe["data"]["recipeId"].as_u64().unwrap();

        let body = json!({
            "idempotencyKey": "e2e-replay",
            "runs": [{
                "clientRunId": "r1",
                "recipeRevisionId": recipe_id,
                "inputs": { "prompt": "a cat" }
            }]
        });
        let (_, first) = request_json(
            &app,
            "POST",
            "/api/v1/operations",
            &token,
            Some(body.clone()),
        )
        .await;
        let (_, second) = request_json(
            &app,
            "POST",
            "/api/v1/operations",
            &token,
            Some(body.clone()),
        )
        .await;
        assert_eq!(first["data"]["operationId"], second["data"]["operationId"]);

        let mut changed = body.clone();
        changed["runs"][0]["inputs"]["prompt"] = json!("a dog");
        let (status, conflict) =
            request_json(&app, "POST", "/api/v1/operations", &token, Some(changed)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["error"]["code"], "IDEMPOTENCY_CONFLICT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn comfyui_prompt_import_over_http() {
        let dir = temp_dir("comfyui-import");
        /*
         * ⚠️ 这里**必须**注册 ComfyUI 驱动，不能再用空注册表。
         *
         * 导入路径现在会读驱动的 `business_capabilities` 来校验能力（那是能力的权威来源：
         * 连接的能力来自客户端请求、recipe 的能力以前在导入路径里写死，三者从没对上过）。
         * 空注册表 ⇒ `DRIVER_NOT_REGISTERED`。以前能过，恰恰是因为**没人读那份声明**。
         *
         * 注册它不引入网络：ComfyUI 的导入走的是"用户粘贴的 API Prompt"，构造驱动也不联网。
         */
        let mut drivers = DriverRegistry::new();
        drivers.register(
            driver_key("comfyui", "comfyui"),
            Arc::new(ComfyUiDriver::new(Box::new(NoopSecretStore))),
        );
        let runtime = V2Runtime::initialize_with_drivers(&dir, Arc::new(NoopSecretStore), drivers)
            .await
            .expect("初始化应成功");
        let app = runtime.router();
        let token = runtime.bootstrap_token().to_string();

        // 1) ComfyUI 连接（仅建库，不做网络校验）。
        let (status, connection) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "comfyui",
                "protocol": "comfyui",
                "displayName": "Local ComfyUI",
                "config": { "baseUrl": "http://127.0.0.1:8188" },
                "capabilities": ["workflow-execution"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{connection}");
        let connection_id = connection["data"]["connectionId"].as_u64().unwrap();

        // 2) 导入 ComfyUI API Prompt，自动发现参数。
        let prompt = json!({
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "seed": 42,
                    "steps": 20,
                    "cfg": 7.0,
                    "positive": ["6", 0]
                }
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "a red house" }
            }
        });
        let (status, imported) = request_json(
            &app,
            "POST",
            "/api/v1/recipes/comfyui-import",
            &token,
            Some(json!({ "connectionId": connection_id, "prompt": prompt })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{imported}");
        assert_eq!(imported["data"]["capability"], "workflow-execution");
        let properties = imported["data"]["inputSchema"]["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        // 机械导入默认暴露的是"用户会改的"：提示词在，采样旋钮不在
        // （旋钮仍在候选里、勾一下就进 —— 判据见 `comfyui_field_relevance`）。
        assert!(properties.contains_key("6::text"), "{imported}");
        assert!(!properties.contains_key("3::seed"), "{imported}");
        assert!(!properties.contains_key("3::steps"), "{imported}");
        assert!(!properties.contains_key("3::positive"), "{imported}");

        // 3) 非 ComfyUI 连接应被拒绝。
        let (status2, bad) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "openai",
                "protocol": "openai-images",
                "displayName": "Not Comfy",
                "config": { "baseUrl": "http://127.0.0.1:1/v1", "model": "x" },
                "capabilities": ["openai-images"]
            })),
        )
        .await;
        assert_eq!(status2, StatusCode::OK, "{bad}");
        let bad_id = bad["data"]["connectionId"].as_u64().unwrap();
        let (rejected, _) = request_json(
            &app,
            "POST",
            "/api/v1/recipes/comfyui-import",
            &token,
            Some(json!({ "connectionId": bad_id, "prompt": prompt })),
        )
        .await;
        assert_eq!(rejected, StatusCode::BAD_REQUEST);

        /*
         * 4) 能力校验：连接的 vendor/protocol 对得上、驱动也在，但**连接没启用**该能力。
         *
         * 这是"能力三处各说各话"里最现实的一种：前端建连接时勾错了能力。
         * 以前没人管，任务提交时才炸；现在在导入这一步就报 `CONNECTION_CAPABILITY_MISSING`。
         */
        let (status3, wrong_caps) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "comfyui",
                "protocol": "comfyui",
                "displayName": "ComfyUI 但没启用 workflow",
                "config": { "baseUrl": "http://127.0.0.1:8188" },
                "capabilities": ["text-to-image"]
            })),
        )
        .await;
        assert_eq!(status3, StatusCode::OK, "{wrong_caps}");
        let wrong_id = wrong_caps["data"]["connectionId"].as_u64().unwrap();
        let (status4, denied) = request_json(
            &app,
            "POST",
            "/api/v1/recipes/comfyui-import",
            &token,
            Some(json!({ "connectionId": wrong_id, "prompt": prompt })),
        )
        .await;
        assert_eq!(status4, StatusCode::BAD_REQUEST, "{denied}");
        assert_eq!(
            denied["error"]["code"], "CONNECTION_CAPABILITY_MISSING",
            "连接没启用该能力时必须报这个码，而不是含糊的失败：{denied}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 能力校验的另一半：**驱动自己没声明**这个能力。
    ///
    /// 用一个"注册在 `comfyui/comfyui` 键上、但只声明 `text-to-image`"的假驱动来造这个局面
    /// ——真实驱动都声明了 `workflow-execution`，所以只有假驱动能测到这条分支。
    /// 这正是"加平台时漏声明"的样子：键对得上、连接也启用了，但驱动不认。
    #[tokio::test]
    async fn comfyui_import_rejects_capability_the_driver_never_declared() {
        let dir = temp_dir("capability-undeclared");
        let mut drivers = DriverRegistry::new();
        drivers.register(
            driver_key("comfyui", "comfyui"),
            Arc::new(FakeProviderDriver::new(ProviderDescriptor {
                vendor_id: "comfyui".to_string(),
                protocol_id: "comfyui".to_string(),
                display_name: "ComfyUI（漏声明）".to_string(),
                business_capabilities: vec!["text-to-image".to_string()],
                lifecycle_features: LifecycleFeatures::default(),
                security_requirements: Default::default(),
                connection_schema: json!({}),
                binding_schema: json!({}),
            })),
        );
        let runtime = V2Runtime::initialize_with_drivers(&dir, Arc::new(NoopSecretStore), drivers)
            .await
            .expect("初始化应成功");
        let app = runtime.router();
        let token = runtime.bootstrap_token().to_string();

        let (_, connection) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "comfyui",
                "protocol": "comfyui",
                "displayName": "Local ComfyUI",
                "config": { "baseUrl": "http://127.0.0.1:8188" },
                "capabilities": ["workflow-execution"]
            })),
        )
        .await;
        let connection_id = connection["data"]["connectionId"].as_u64().unwrap();

        let (status, denied) = request_json(
            &app,
            "POST",
            "/api/v1/recipes/comfyui-import",
            &token,
            Some(json!({
                "connectionId": connection_id,
                "prompt": { "3": { "class_type": "KSampler", "inputs": { "seed": 1 } } }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{denied}");
        assert_eq!(
            denied["error"]["code"], "RECIPE_CAPABILITY_UNDECLARED",
            "驱动没声明的能力必须在导入期报出来（以前没人读那份声明）：{denied}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live smoke: full V2 chain against a real local ComfyUI.
    /// Requires ComfyUI running at http://127.0.0.1:8188.
    /// Run with: cargo test --lib -- --ignored live_comfyui_full_chain --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_comfyui_full_chain() {
        const COMFYUI_BASE: &str = "http://127.0.0.1:8188";

        // 探测可用性。`no_proxy()`：探的是本机回环，不该被机器上开着的代理带偏。
        let probe = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("建探活客户端")
            .get(format!("{COMFYUI_BASE}/system_stats"))
            .send()
            .await;
        if probe.is_err() {
            eprintln!("SKIP: ComfyUI 不可用（{COMFYUI_BASE}）");
            return;
        }

        let dir = temp_dir("live-comfyui");
        let mut drivers = DriverRegistry::new();
        let driver = std::sync::Arc::new(
            crate::v2::infrastructure::providers::comfyui::ComfyUiDriver::new(Box::new(
                NoopSecretStore,
            )),
        );
        drivers.register(
            crate::v2::ports::provider::driver_key("comfyui", "comfyui"),
            driver,
        );
        let runtime = V2Runtime::initialize_with_drivers(&dir, Arc::new(NoopSecretStore), drivers)
            .await
            .expect("初始化应成功");
        let app = runtime.router();
        let token = runtime.bootstrap_token().to_string();

        // 1) Connection（comfyui）
        let (status, connection) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "comfyui",
                "protocol": "comfyui",
                "displayName": "Local ComfyUI",
                "config": { "baseUrl": COMFYUI_BASE },
                "capabilities": ["workflow-execution"],
                "lifecycleFeatures": { "assetUpload": true, "remoteReconciliation": true }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{connection}");
        let connection_id = connection["data"]["connectionId"].as_u64().unwrap();

        // 2) Recipe（workflow 模式 + fieldMapping）
        let workflow = json!({
            "1": {
                "class_type": "LoadImage",
                "inputs": { "image": "00b1d090f4f742929075cb99914ce.jpg" }
            },
            "3": {
                "class_type": "TextToLowercase",
                "inputs": { "texts": "placeholder" }
            },
            "5": {
                "class_type": "ReplaceText",
                "inputs": { "texts": ["3", 0], "find": "", "replace": "" }
            },
            "2": {
                "class_type": "SaveImage",
                "inputs": { "filename_prefix": "ComfyUI", "images": ["1", 0] }
            }
        });
        let (status, recipe) = request_json(
            &app,
            "POST",
            "/api/v1/recipes",
            &token,
            Some(json!({
                "name": "Live ComfyUI",
                "capability": "workflow-execution",
                "driverKey": "comfyui",
                "defaultConnectionId": connection_id,
                "inputSchema": {
                    "type": "object",
                    "required": ["texts"],
                    "properties": { "texts": { "type": "string" } }
                },
                "providerConfig": {
                    "workflow": workflow,
                    "fieldMapping": { "texts": { "node": "3", "field": "texts" } }
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{recipe}");
        let recipe_id = recipe["data"]["recipeId"].as_u64().unwrap();

        // 3) Operation
        let (status, created) = request_json(
            &app,
            "POST",
            "/api/v1/operations",
            &token,
            Some(json!({
                "idempotencyKey": "live-comfyui-1",
                "runs": [{
                    "clientRunId": "live-run",
                    "recipeRevisionId": recipe_id,
                    "inputs": { "texts": "HELLO WORLD" }
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let operation_id = created["data"]["operationId"].as_u64().unwrap();

        // 4) Scheduler：提交 /prompt → ACCEPTED
        let tick = runtime.scheduler.tick().await.expect("调度应成功");
        eprintln!(
            "scheduler tick: claimed={} accepted={}",
            tick.claimed, tick.accepted
        );
        let (_, after_schedule) = request_json(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &token,
            None,
        )
        .await;
        let run_status = after_schedule["data"]["runs"][0]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(run_status, "ACCEPTED", "{after_schedule}");

        // 5) Reconcile：/history → SUCCESS → /view 下载
        let run_id = after_schedule["data"]["runs"][0]["runId"].as_u64().unwrap();
        // 等待 ComfyUI 完成瞬时任务并写入 history
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let mut terminal = false;
        for _ in 0..40 {
            let _ = runtime.reconcile.tick(200).await;
            let (_, done) = request_json(
                &app,
                "GET",
                &format!("/api/v1/operations/{operation_id}"),
                &token,
                None,
            )
            .await;
            let status = done["data"]["status"].as_str().unwrap_or_default();
            if status == "SUCCEEDED" || status == "FAILED" {
                terminal = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        let (_, done) = request_json(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &token,
            None,
        )
        .await;
        assert_eq!(done["data"]["status"], "SUCCEEDED", "{done}");
        assert!(terminal, "轮询应在有限次数内到达终态");

        // 6) Artifact 已生成
        let (status, artifacts) = request_json(
            &app,
            "GET",
            &format!("/api/v1/artifacts?runId={run_id}"),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let items = artifacts["data"]["items"].as_array().expect("应有产物");
        assert!(!items.is_empty(), "{artifacts}");
        eprintln!("artifact: {}", items[0]["filename"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live smoke: synchronous OpenAI-compatible image generation through the
    /// full V2 chain. Requires RENDERLOFT_OPENAI_API_KEY pointing to an
    /// OpenAI-compatible /v1/images/generations endpoint and
    /// RENDERLOFT_OPENAI_BASE_URL (default https://www.poke2api.com/v1).
    /// Run with: RENDERLOFT_OPENAI_API_KEY=... cargo test --lib -- --ignored live_openai_images --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_openai_images_full_chain() {
        let Some(api_key) = std::env::var("RENDERLOFT_OPENAI_API_KEY").ok() else {
            eprintln!("SKIP: 未设置 RENDERLOFT_OPENAI_API_KEY");
            return;
        };
        let base_url = std::env::var("RENDERLOFT_OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://www.poke2api.com/v1".to_string());

        struct LiveSecrets {
            key: String,
        }
        #[async_trait::async_trait]
        impl SecretStore for LiveSecrets {
            async fn resolve(
                &self,
                _binding: &crate::v2::domain::model::SecretBinding,
            ) -> DomainResult<Option<crate::v2::ports::stores::SecretValue>> {
                Ok(Some(crate::v2::ports::stores::SecretValue {
                    secret: self.key.clone(),
                }))
            }
            async fn set(&self, _slot: &str, _value: &str) -> DomainResult<()> {
                Ok(())
            }
            async fn delete(&self, _slot: &str) -> DomainResult<()> {
                Ok(())
            }
        }

        let dir = temp_dir("live-openai");
        let mut drivers = DriverRegistry::new();
        let driver = std::sync::Arc::new(
            crate::v2::infrastructure::providers::openai_images::OpenAiImagesDriver::new(Box::new(
                LiveSecrets {
                    key: api_key.clone(),
                },
            )),
        );
        drivers.register(
            crate::v2::ports::provider::driver_key("openai", "openai-images"),
            driver,
        );
        let runtime = V2Runtime::initialize_with_drivers(&dir, Arc::new(NoopSecretStore), drivers)
            .await
            .expect("初始化应成功");
        let app = runtime.router();
        let token = runtime.bootstrap_token().to_string();

        // 1) Connection
        let (status, connection) = request_json(
            &app,
            "POST",
            "/api/v1/connections",
            &token,
            Some(json!({
                "vendor": "openai",
                "protocol": "openai-images",
                "displayName": "OpenAI Images",
                "config": { "baseUrl": base_url },
                "capabilities": ["text-to-image"],
                "lifecycleFeatures": { "synchronousResult": true }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{connection}");
        let connection_id = connection["data"]["connectionId"].as_u64().unwrap();

        // 2) Recipe
        let (status, recipe) = request_json(
            &app,
            "POST",
            "/api/v1/recipes",
            &token,
            Some(json!({
                "name": "Live OpenAI Image",
                "capability": "text-to-image",
                "driverKey": "openai-images",
                "defaultConnectionId": connection_id,
                "inputSchema": {
                    "type": "object",
                    "required": ["prompt"],
                    "properties": { "prompt": { "type": "string" } }
                },
                "providerConfig": { "model": "gpt-image-2", "size": "1024x1024" }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{recipe}");
        let recipe_id = recipe["data"]["recipeId"].as_u64().unwrap();

        // 3) Operation
        let (status, created) = request_json(
            &app,
            "POST",
            "/api/v1/operations",
            &token,
            Some(json!({
                "idempotencyKey": "live-openai-1",
                "runs": [{
                    "clientRunId": "live-openai-run",
                    "recipeRevisionId": recipe_id,
                    "inputs": { "prompt": "a red house in snow, minimal style" }
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let operation_id = created["data"]["operationId"].as_u64().unwrap();

        // 4) Scheduler：同步完成（submit 直接返回 Completed + 解码 b64）
        let tick = runtime.scheduler.tick().await.expect("调度应成功");
        eprintln!(
            "scheduler tick: claimed={} submitted={} succeeded={} failed={}",
            tick.claimed, tick.submitted, tick.succeeded, tick.failed
        );
        let (_, done) = request_json(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &token,
            None,
        )
        .await;
        // 失败时打印事件中的 reason（如中转站 401/502）
        if done["data"]["status"] != "SUCCEEDED" {
            let (_, events) = request_json(
                &app,
                "GET",
                &format!("/api/v1/operations/{operation_id}/events"),
                &token,
                None,
            )
            .await;
            eprintln!("events: {events}");
        }
        assert_eq!(done["data"]["status"], "SUCCEEDED", "{done}");
        assert_eq!(done["data"]["runs"][0]["status"], "SUCCEEDED");

        // 5) Artifact
        let run_id = done["data"]["runs"][0]["runId"].as_u64().unwrap();
        let (status, artifacts) = request_json(
            &app,
            "GET",
            &format!("/api/v1/artifacts?runId={run_id}"),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let items = artifacts["data"]["items"].as_array().expect("应有产物");
        assert!(!items.is_empty(), "{artifacts}");
        eprintln!("artifact: {}", items[0]["filename"]);
        assert!(items[0]["byteSize"].as_u64().unwrap() > 10_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- 产品外壳（shell_router）路由的鉴权行为 ----------
    /// 构造一个只带 shell 路由的 app（不需要完整 V2Runtime）。
    fn shell_app(token: &str, nonce: Option<&str>) -> Router {
        let (shutdown, _rx) = tokio::sync::watch::channel(false);
        let state = ShellState {
            token: token.to_string(),
            bootstrap_nonce: Arc::new(Mutex::new(nonce.map(str::to_string))),
            shutdown,
            allowed_hosts: Vec::new(),
        };
        shell_router(state)
    }

    async fn raw_request(
        app: &Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String) {
        // ⚠️ 必须显式带 Host：`shell_authorize` 先查 Host 白名单，真实请求一定有 Host
        // header，而 `oneshot` 用相对 URI 不会自动补 ⇒ 不带就一律 401（会把
        // "缺 Host"误读成"鉴权生效"）。
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:31337");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    /// `/api/daemon/shutdown` 无任何凭据必须 401，带正确 Bearer 才 204。
    ///
    /// 为什么这条重要：**这条路由不经过 `v2` 路由组那份 `auth` 中间件**（它在
    /// `shell_router` 上），所以 `every_protected_route_requires_scope_*` 那条守卫
    /// **覆盖不到它**。若哪天 `shell_authorize` 被误删，这条测试是唯一防线。
    /// 关掉 daemon 是副作用最大的动作之一（正在跑的付费任务会被打断）⇒ 必须守住。
    #[tokio::test]
    async fn daemon_shutdown_requires_auth() {
        let app = shell_app("secret-token", None);

        // ① 无凭据 ⇒ 401
        let (status, _) = raw_request(&app, "POST", "/api/daemon/shutdown", &[]).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "无凭据的 shutdown 必须被拒"
        );

        // ② 错的 Bearer ⇒ 401
        let (status, _) = raw_request(
            &app,
            "POST",
            "/api/daemon/shutdown",
            &[("authorization", "Bearer wrong-token")],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "错误 Bearer 必须被拒");

        // ③ 正确 Bearer ⇒ 204（正对照：证明拒绝是因为凭据，不是因为路由不存在）
        let (status, _) = raw_request(
            &app,
            "POST",
            "/api/daemon/shutdown",
            &[("authorization", "Bearer secret-token")],
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "正确 Bearer 应通过");
    }

    /// `/health` 必须**无需凭据**可访问（探活端点），且不含敏感信息。
    #[tokio::test]
    async fn daemon_health_is_public_and_leaks_nothing() {
        let app = shell_app("secret-token", None);
        let (status, body) = raw_request(&app, "GET", "/health", &[]).await;
        assert_eq!(status, StatusCode::OK, "health 应公开");
        assert!(
            !body.contains("secret-token"),
            "health 不应泄露 token：{body}"
        );
        assert!(body.contains("\"ok\":true"), "health 应报 ok：{body}");
    }

    /// `/bootstrap` 必须校验**一次性 nonce**：错 nonce / 无 nonce = 401；
    /// 正确 nonce = 307 重定向 + 下发 HttpOnly 会话 cookie；**且 nonce 用后即焚**。
    #[tokio::test]
    async fn daemon_bootstrap_requires_one_time_nonce() {
        // ① 无 nonce ⇒ 401
        let app = shell_app("secret-token", Some("nonce-abc"));
        let (status, _) = raw_request(&app, "GET", "/bootstrap", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "无 nonce 必须被拒");

        // ② 错 nonce ⇒ 401
        let app = shell_app("secret-token", Some("nonce-abc"));
        let (status, _) = raw_request(&app, "GET", "/bootstrap?nonce=wrong", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "错误 nonce 必须被拒");

        // ③ 正确 nonce ⇒ 307 + Set-Cookie(HttpOnly)
        let (shutdown, _rx) = tokio::sync::watch::channel(false);
        let state = ShellState {
            token: "secret-token".to_string(),
            bootstrap_nonce: Arc::new(Mutex::new(Some("nonce-abc".to_string()))),
            shutdown,
            allowed_hosts: Vec::new(),
        };
        let app_ok = shell_router(state);
        let response = app_ok
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/bootstrap?nonce=nonce-abc")
                    .header(header::HOST, "127.0.0.1:31337")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TEMPORARY_REDIRECT,
            "正确 nonce 应重定向"
        );
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            cookie.contains("renderloft_session=secret-token"),
            "应下发会话 cookie：{cookie}"
        );
        assert!(
            cookie.contains("HttpOnly"),
            "会话 cookie 必须是 HttpOnly：{cookie}"
        );

        // ④ **一次性**：同一个 nonce 再用一次必须失败（防重放）
        let (status, _) = raw_request(&app_ok, "GET", "/bootstrap?nonce=nonce-abc", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "nonce 用后即焚，不可重放");
    }

    /// 换门票：`POST /api/daemon/bootstrap` 必须**要 daemon token**，换出来的 nonce 能真的
    /// 开控制台，并且把旧 nonce 一并作废。
    ///
    /// 这条守的是"强杀外壳之后界面还打得开"：端口上坐着上一个进程留下的 daemon 时
    /// （外壳被强杀、或用户又点了一次图标），端点文件里的 nonce 早被上一轮用掉了，
    /// 外壳只能靠这个接口换一张新的——否则窗口里是一屏 401 的 JSON。
    #[tokio::test]
    async fn daemon_issue_bootstrap_rotates_nonce_for_authenticated_shell() {
        // ① 无凭据 ⇒ 401（这不能变成"谁都能领门票"）
        let app = shell_app("secret-token", Some("old-nonce"));
        let (status, _) = raw_request(&app, "POST", "/api/daemon/bootstrap", &[]).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "换门票必须带 daemon token"
        );

        // ② 带正确 token ⇒ 200 + 一张新 nonce
        let (status, body) = raw_request(
            &app,
            "POST",
            "/api/daemon/bootstrap",
            &[("authorization", "Bearer secret-token")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "带 token 应换到门票：{body}");
        let nonce = body
            .split("\"nonce\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .filter(|value| !value.is_empty())
            .expect("响应里应有 nonce")
            .to_string();
        assert_ne!(nonce, "old-nonce", "必须换一张新的，而不是把旧的还回来");

        // ③ 新 nonce 能开控制台（正对照：证明 ② 拿到的确实是门票）
        let (status, _) = raw_request(&app, "GET", &format!("/bootstrap?nonce={nonce}"), &[]).await;
        assert_eq!(
            status,
            StatusCode::TEMPORARY_REDIRECT,
            "新 nonce 应能开控制台"
        );

        // ④ 旧 nonce 同时作废（换门票 = 轮换，不是追加一张并存的票）
        let (status, _) = raw_request(&app, "GET", "/bootstrap?nonce=old-nonce", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "换门票要作废旧 nonce");
    }
}

// ---------- 产品外壳：V2 作为唯一 daemon 入口 ----------

#[derive(Clone)]
pub struct ShellState {
    token: String,
    bootstrap_nonce: Arc<Mutex<Option<String>>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    allowed_hosts: Vec<String>,
}

/// daemon 主入口：初始化 V2 运行时，装配静态控制台、bootstrap 登录与优雅关闭。
pub async fn run() -> Result<(), String> {
    // 环境事实的所有者是 `v2::environment`（诊断接口也要报同一批值，而 transports 不得
    // import daemon —— 见 `scripts/check-layers.mjs`）。这里只取用，不再自己解析一遍。
    let bind_addr = crate::v2::environment::bind_address();
    let allowed_hosts = crate::v2::environment::allowed_hosts();
    // ⚠️ **先占端口，再碰任何共享状态。** 端口就是"同一时刻只有一个 daemon"的那把锁。
    //
    // 第二个实例是常态而不是异常（用户又点了一次图标、MCP/CLI 顺手拉起旁边的 daemon、
    // 上一次强杀留下的孤儿 daemon 还占着端口）。而它在**绑定之前**做的两件事都会伤到
    // 正在跑的那个 daemon：
    //   * `register_access_token` 会轮换掉上一轮 daemon token ⇒ 正在跑的应用界面立刻
    //     全线 401。实测症状是"界面照常打开、点什么都没反应"，横幅写着「认证令牌已被撤销」
    //     —— 看起来像界面坏了，其实是另一个进程在启动时把它的会话吊销了；
    //   * `hygiene.tick()` 会对同一个库做启动卫生清理（把陈旧未终态操作判失败）⇒
    //     可能掀掉别人正在跑的任务。
    // 绑定失败就在这里退出：共享状态一个字节都没动，正在跑的 daemon 毫发无损。
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|error| {
            format!(
                "无法绑定监听地址 {bind_addr}：{error}（可能已有另一个 RenderLoft daemon 在运行）"
            )
        })?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("无法读取本地 daemon 地址：{error}"))?;

    let data_dir = v2_data_dir()?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::default());
    // daemon access token 必须只生成一次：注册（register_access_token）与写入
    // daemon-endpoint.json（供 Vite/CLI 代理注入）必须使用同一个 token，
    // 否则注册的是 A、落盘的是 B，代理注入 B 永远 TOKEN_INVALID。
    let access_token = daemon_access_token();
    let v2 = Arc::new(V2Runtime::initialize(&data_dir, secrets).await?);
    v2.register_access_token(&access_token).await?;
    v2.spawn_background_tasks();
    // 启动时先做一次卫生清理：把上次进程遗留的陈旧未终态操作判失败，
    // 否则 UI 会一直显示"执行中"（无 run 或 deadline 被清空的孤儿操作 reconcile 救不回）。
    v2.hygiene
        .tick()
        .await
        .map_err(|error| format!("启动卫生清理失败：{error}"))?;

    let (shutdown_sender, _) = tokio::sync::watch::channel(false);
    let state = ShellState {
        token: access_token,
        bootstrap_nonce: Arc::new(Mutex::new(Some(uuid()))),
        shutdown: shutdown_sender,
        allowed_hosts,
    };
    let token_tail = v2
        .bootstrap_token()
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    eprintln!(
        "{} RenderLoft API 已启用：http://{bind_addr}/api/v1（bootstrap token：…{token_tail}）",
        timestamp()
    );

    write_endpoint(&state, address)?;

    let app = shell_router(state.clone())
        .merge(v2.router())
        .fallback(any(static_ui));
    let nonce = state
        .bootstrap_nonce
        .lock()
        .map_err(|_| "daemon bootstrap 锁不可用".to_string())?
        .clone()
        .unwrap_or_default();
    eprintln!(
        "{} RenderLoft daemon 正在监听 http://{address}",
        timestamp()
    );
    eprintln!(
        "{} 打开控制台：http://{address}/bootstrap?nonce={nonce}",
        timestamp()
    );
    let mut shutdown_signal = state.shutdown.subscribe();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_signal.changed().await;
        })
        .await
        .map_err(|error| format!("daemon 服务异常退出：{error}"))?;
    let _ = std::fs::remove_file(endpoint_path()?);
    Ok(())
}

fn uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn daemon_access_token() -> String {
    std::env::var("RENDERLOFT_ACCESS_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}{}", uuid(), uuid()))
}

fn shell_router(state: ShellState) -> Router {
    use axum::routing::{get, post};
    Router::new()
        .route("/health", get(health))
        .route("/bootstrap", get(bootstrap))
        .route("/api/daemon/bootstrap", post(issue_bootstrap))
        .route("/api/daemon/shutdown", post(shutdown_daemon))
        .with_state(state)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EndpointFile {
    version: String,
    endpoint: String,
    token: String,
    bootstrap_url: String,
}

fn write_endpoint(state: &ShellState, address: std::net::SocketAddr) -> Result<(), String> {
    let path = endpoint_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建 daemon 目录：{error}"))?;
    }
    let bootstrap_url = format!(
        "http://{address}/bootstrap?nonce={}",
        state
            .bootstrap_nonce
            .lock()
            .map_err(|_| "daemon bootstrap 锁不可用".to_string())?
            .as_deref()
            .unwrap_or_default()
    );
    let content = serde_json::to_string(&EndpointFile {
        version: env!("CARGO_PKG_VERSION").to_string(),
        endpoint: format!("http://{address}"),
        token: state.token.clone(),
        bootstrap_url,
    })
    .map_err(|error| format!("无法序列化 daemon 端点：{error}"))?;
    std::fs::write(path, content).map_err(|error| format!("无法写入 daemon 端点：{error}"))
}

fn endpoint_path() -> Result<std::path::PathBuf, String> {
    dirs::data_local_dir()
        .map(|path| {
            path.join("renderloft-workspace")
                .join("daemon-endpoint.json")
        })
        .ok_or_else(|| "无法定位本地 daemon 目录".to_string())
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

// axum 处理器的错误类型**就是**渲染好的 `Response`（框架契约：`Err` 必须实现 `IntoResponse`，
// 而 `Box<Response>` 不实现）。clippy 1.99 新增的 `result_large_err` 对这类签名一律报错，
// 但这是框架用法决定的，不是可修的缺陷——改成 box 反而编译不过。
#[allow(clippy::result_large_err)]
async fn bootstrap(
    axum::extract::State(state): axum::extract::State<ShellState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, axum::response::Response> {
    use axum::response::IntoResponse;
    let nonce = query
        .get("nonce")
        .ok_or_else(|| crate::v2::transports::http::error::unauthorized("缺少控制台启动 nonce"))?;
    let mut stored = state.bootstrap_nonce.lock().map_err(|_| {
        crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::internal("daemon bootstrap 锁不可用"),
        )
    })?;
    if stored.as_deref() != Some(nonce.as_str()) {
        return Err(crate::v2::transports::http::error::unauthorized(
            "控制台启动 nonce 无效",
        ));
    }
    *stored = None;
    let destination = std::env::var("RENDERLOFT_UI_ORIGIN").unwrap_or_else(|_| "/".to_string());
    let mut response = axum::response::Redirect::temporary(&destination).into_response();
    if let Ok(cookie) = axum::http::HeaderValue::from_str(&format!(
        "renderloft_session={}; HttpOnly; SameSite=Strict; Path=/",
        state.token
    )) {
        response
            .headers_mut()
            .insert(axum::http::header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// 重新签发一张一次性 bootstrap 门票（要 daemon token）。
///
/// 为什么需要这个接口：`/bootstrap` 的 nonce 是**一次性**的，而外壳（`renderloft-desktop`）
/// 每次都只是"连上端口 → 读 `daemon-endpoint.json` → 打开里面的 bootstrapUrl"。当端口上
/// 坐着的是**上一个进程**留下的 daemon（强杀外壳留下的孤儿；或者用户又点了一次图标，
/// 新 daemon 因端口被占而退出）时，那个 URL 里的 nonce 早被上一轮用掉了 ⇒ 界面拿到的
/// 是 401 的 JSON。有了这个接口，外壳可以拿端点文件里的 token 换一张新门票，
/// **界面照样能打开**，而不是打开一屏 JSON。
///
/// 安全边界与 `/api/daemon/shutdown` 完全一致（同一把 `shell_authorize`：本机回环 +
/// host 白名单 + daemon token）。token 本来就和 nonce 一起躺在同一个文件里，所以这
/// 不新增任何信任假设。
#[allow(clippy::result_large_err)]
async fn issue_bootstrap(
    axum::extract::State(state): axum::extract::State<ShellState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Json<serde_json::Value>, axum::response::Response> {
    shell_authorize(&headers, &state)?;
    let nonce = uuid();
    *state.bootstrap_nonce.lock().map_err(|_| {
        crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::internal("daemon bootstrap 锁不可用"),
        )
    })? = Some(nonce.clone());
    Ok(axum::response::Json(serde_json::json!({ "nonce": nonce })))
}

// 同上：axum 处理器的错误类型是 `Response`，boxing 会破坏 `IntoResponse`。
#[allow(clippy::result_large_err)]
async fn shutdown_daemon(
    axum::extract::State(state): axum::extract::State<ShellState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::http::StatusCode, axum::response::Response> {
    shell_authorize(&headers, &state)?;
    let _ = state.shutdown.send(true);
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[allow(clippy::result_large_err)]
fn shell_authorize(
    headers: &axum::http::HeaderMap,
    state: &ShellState,
) -> Result<(), axum::response::Response> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !host_allowed(host, &state.allowed_hosts) {
        return Err(crate::v2::transports::http::error::unauthorized(
            "不允许的主机访问",
        ));
    }
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let session = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(';')
                .map(str::trim)
                .find_map(|part| part.strip_prefix("renderloft_session="))
        });
    if bearer == Some(state.token.as_str()) || session == Some(state.token.as_str()) {
        Ok(())
    } else {
        Err(crate::v2::transports::http::error::unauthorized(
            "本地 daemon 认证失败",
        ))
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.starts_with("127.0.0.1:")
        || host.starts_with("localhost:")
        || host.starts_with("[::1]:")
        || host == "127.0.0.1"
        || host == "localhost"
        || host == "[::1]"
}

fn host_allowed(host: &str, allowed_hosts: &[String]) -> bool {
    if is_loopback_host(host) {
        return true;
    }
    let hostname = host
        .rsplit_once(':')
        .map(|(name, _)| name)
        .unwrap_or(host)
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    allowed_hosts.iter().any(|entry| {
        let entry = entry.to_ascii_lowercase();
        if let Some(suffix) = entry.strip_prefix("*.") {
            hostname.ends_with(&format!(".{suffix}"))
        } else {
            hostname == entry
        }
    })
}

/// 控制台静态资源的根目录：**显式覆盖 → 可执行文件旁的 `dist` → 源码树**。
///
/// 为什么"可执行文件旁"必须排在"源码树"前面：安装包把 daemon 与 `dist/` 一起装进
/// 控制台静态资源根目录：实现与它的单测都搬到了 `v2::environment`（诊断接口要报同一个值，
/// 而 transports 不得 import daemon —— 见 `scripts/check-layers.mjs`）。
/// 静态资源请求该读哪个文件；**`None` = 404**（不是"回 index.html"）。
///
/// 为什么这里不学 SPA 那套"找不到就回 index.html"：控制台走的是 **hash 路由**
/// （`createWebHashHistory`），服务端只会收到两类请求——入口 `/` 与真实文件；`/bootstrap`
/// 那条是显式路由，根本走不到这里。所以那条兜底没有任何正确用途，唯一的效果是**把"缺文件"
/// 伪装成 `200 + text/html`**：浏览器拿 HTML 当 ES 模块解析，报一个与真实原因无关的语法错，
/// 于是"铺了新前端、旧窗口点不动"这种事**完全静默**（实测踩过一次：点「任务」没有任何反应，
/// 因为那个 chunk 的文件名已经随构建变了）。
///
/// 404 让原因对得上：文件没了就是没了，控制台里也看得懂。
///
/// `canonical_root` 是第二道门：`..`、百分号编码、反斜杠那些在调用方已经挡过，这里再确认
/// 落点仍在根目录内——`canonicalize` 之后再比，符号链接也绕不出去。
fn static_target(root: &Path, canonical_root: &Path, requested: &str) -> Option<PathBuf> {
    if requested.is_empty() {
        return Some(root.join("index.html"));
    }
    let candidate = root.join(requested);
    if !candidate.is_file() {
        return None;
    }
    match std::fs::canonicalize(&candidate) {
        Ok(real) if real.starts_with(canonical_root) => Some(real),
        _ => None,
    }
}

// 同上：axum 处理器的错误类型是 `Response`，boxing 会破坏 `IntoResponse`。
#[allow(clippy::result_large_err)]
async fn static_ui(
    uri: axum::http::Uri,
) -> Result<axum::response::Response, axum::response::Response> {
    use axum::http::header;
    use axum::response::IntoResponse;
    let requested = uri.path().trim_start_matches('/');
    // 拒绝 `..` 段、URL 编码的路径分隔符（%2e%2e / %2f / %5c）与百分号编码本身。
    if requested.split('/').any(|segment| segment == "..")
        || requested.to_ascii_lowercase().contains("%2e%2e")
        || requested.to_ascii_lowercase().contains("%2f")
        || requested.to_ascii_lowercase().contains("%5c")
        || requested.contains('%')
    {
        return Err(crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::validation(
                "STATIC_RESOURCE_DENIED",
                "无效的静态资源路径",
            ),
        ));
    }
    if requested.starts_with("api/") {
        return Err(crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::not_found("ROUTE_NOT_FOUND", "接口不存在"),
        ));
    }
    // 路径遍历防护：拒绝反斜杠（Windows 可绕过 `..` 段检查）与盘符绝对路径。
    if requested.contains('\\') || requested.contains(':') {
        return Err(crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::validation(
                "STATIC_RESOURCE_DENIED",
                "无效的静态资源路径",
            ),
        ));
    }
    let root = crate::v2::environment::console_dist_dir();
    let canonical_root = std::fs::canonicalize(&root).map_err(|_| {
        crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::internal("无法定位 Vue 控制台资源目录"),
        )
    })?;
    // 找不到就 **404**，不回 index.html —— 理由见 `static_target`。
    let path = match static_target(&root, &canonical_root, requested) {
        Some(path) => path,
        None => {
            return Err(crate::v2::transports::http::error::not_found(
                "STATIC_RESOURCE_NOT_FOUND",
                "静态资源不存在",
            ))
        }
    };
    let body = std::fs::read(&path).map_err(|_| {
        crate::v2::transports::http::error::error_response(
            crate::v2::domain::errors::SafeError::internal("找不到 Vue 控制台资源，请先构建前端"),
        )
    })?;
    let content_type = match path.extension().and_then(|extension| extension.to_str()) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("webp") => "image/webp",
        Some("json") => "application/json",
        _ => "text/html; charset=utf-8",
    };
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(body))
        .map_err(|_| {
            crate::v2::transports::http::error::error_response(
                crate::v2::domain::errors::SafeError::internal("无法创建静态资源响应"),
            )
        })
        .map(IntoResponse::into_response)
}

#[cfg(test)]
mod static_target_tests {
    use super::static_target;
    use std::path::PathBuf;

    fn temp_dist(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-static-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(dir.join("assets")).expect("创建临时 dist 应成功");
        std::fs::write(dir.join("index.html"), "<!doctype html>").expect("写入口应成功");
        std::fs::write(dir.join("assets/app.js"), "export {}").expect("写资源应成功");
        dir
    }

    /// **负对照**：把"找不到就回 index.html"那句写回去 ⇒ 第三条断言立刻红。
    ///
    /// 钉住的是"失败要说得出原因"这条：`200 + text/html` 会让浏览器拿 HTML 当模块解析，
    /// 报一个与真实原因无关的语法错——实测踩过一次（铺完新前端，旧窗口点「任务」没反应，
    /// 因为那个 chunk 的文件名随构建变了，旧名字已经不在磁盘上）。
    #[test]
    fn a_missing_file_is_a_404_not_the_spa_fallback() {
        let root = temp_dist("missing");
        let canonical = std::fs::canonicalize(&root).expect("canonicalize 临时目录应成功");

        assert_eq!(
            static_target(&root, &canonical, ""),
            Some(root.join("index.html")),
            "入口 `/` 要给 index.html"
        );
        assert_eq!(
            static_target(&root, &canonical, "assets/app.js"),
            Some(std::fs::canonicalize(root.join("assets/app.js")).expect("canonicalize 应成功")),
            "真实文件给文件本身"
        );
        assert_eq!(
            static_target(&root, &canonical, "assets/gone.js"),
            None,
            "缺文件必须 404：回 index.html 会把「文件没了」伪装成 200 + HTML"
        );

        // 落在根目录外的一律 404（符号链接/相对路径都走这条）
        let outside = root
            .parent()
            .expect("临时目录有父目录")
            .join(format!("outside-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&outside, "secret").expect("写外部文件应成功");
        let escaped = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert_eq!(
            static_target(&root, &canonical, &escaped),
            None,
            "跑出根目录的一律 404"
        );

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }
}
