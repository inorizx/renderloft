//! 工作区壳接口：设置、下载目录、诊断与 RunningHub 账号/队列状态。
//!
//! RunningHub 账号/队列状态基于 Connection（而非全局设置），由
//! `RunningHubStatusService` 选择 runninghub 连接并交给驱动查询。

use axum::{
    extract::{Query, State},
    response::IntoResponse,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

use crate::v2::application::settings::SettingsInput;
use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::callback::SIGNING_KEY_SLOT;
use crate::v2::domain::ids::ConnectionId;
use crate::v2::domain::model::{SecretBinding, SecretResolution};

use super::dto::{ok_json, CallbackSecretDto};
use super::error;
// 与 axum::Json 同名的包装提取器：请求体解析失败时返回统一信封而非纯文本。
use super::json::Json;
use super::V2HttpState;

type HttpResult = Result<axum::response::Response, axum::response::Response>;

fn require_scope(actor: &ActorRef, scope: Scope) -> Result<(), axum::response::Response> {
    super::require_scope(actor, scope)
}

fn requested_connection(query: &HashMap<String, String>) -> Option<ConnectionId> {
    query
        .get("connectionId")
        .and_then(|value| value.parse::<u64>().ok())
        .map(ConnectionId)
}

// ---------- settings ----------

pub async fn get_settings(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsRead)?;
    let view = state.settings.view().await.map_err(error::error_response)?;
    Ok(Reply::ok(view))
}

pub async fn save_settings(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
    Json(input): Json<SettingsInput>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsManage)?;
    let view = state
        .settings
        .save(input)
        .await
        .map_err(error::error_response)?;
    Ok(Reply::ok(view))
}

/// 打开下载目录（可选：选中其中一个文件）。
#[derive(Debug, serde::Deserialize)]
pub struct OpenDownloadDirQuery {
    /// 要选中的文件路径。**必须落在下载目录内**，否则忽略（见 `open_download_dir`）。
    #[serde(default)]
    pub reveal: Option<String>,
}

pub async fn open_download_dir(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
    Query(query): Query<OpenDownloadDirQuery>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsManage)?;
    let directory = state
        .settings
        .open_download_dir(query.reveal.as_deref())
        .await
        .map_err(error::error_response)?;
    Ok(Reply::ok(json!({ "directory": directory })))
}

pub async fn diagnostics(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
) -> HttpResult {
    require_scope(&actor, Scope::DiagnosticsRead)?;
    let brief = state
        .status
        .runninghub_brief()
        .await
        .map_err(error::error_response)?;
    let service_host = brief
        .as_ref()
        .map(|brief| brief.base_url.clone())
        .unwrap_or_default();
    let api_key_configured = brief
        .as_ref()
        .map(|brief| brief.api_key_configured)
        .unwrap_or(false);
    let payload = json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "systemOs": std::env::consts::OS,
        "systemFamily": std::env::consts::FAMILY,
        "systemArch": std::env::consts::ARCH,
        "serviceRegion": "local",
        "serviceHost": service_host,
        "apiKeyConfigured": api_key_configured,
        // 出网代理：reqwest 默认会用它，而它在界面上是**看不见**的——代理软件关掉之后
        // 应用表现为"连不上平台 / key 好像坏了"。把它写进诊断，这类问题一眼可见。
        "outboundProxy": crate::v2::outbound::configured_proxy(),
        // **当前生效的运行参数**：这些值全都来自环境变量或 exe 位置，因此"出问题时能不能
        // 自己改"完全取决于它们**看不看得见**——只写在 NEXT.md（维护者文件）里等于没有。
        // 判据是"报**服务实际用的那个值**"，所以取的是 `v2::environment`（所有者），
        // 而不是在这里再解析一遍环境变量：诊断说 X、服务用 Y，比不报更糟。
        "runtime": {
            "bind": crate::v2::environment::bind_address(),
            "allowedHosts": crate::v2::environment::allowed_hosts(),
            // 设了它 ⇒ `/bootstrap` 会跳到那个 origin（开发时是 vite）。**"外壳窗口里
            // 为什么是旧界面"的答案就在这一行**：同一个 exe，落点由它决定。
            "uiOrigin": crate::v2::environment::ui_origin(),
            // 静态资源目录：重铺 `dist/` 之后该看的就是它（而不是"我以为它在哪"）。
            "distDir": crate::v2::environment::console_dist_dir().display().to_string(),
        },
        // **日志在哪、还在不在写**：这一块是给"出问题以后把目录交给 agent 去发掘"用的。
        // 只报路径是不够的——daemon 的输出可能是别人重定向的（外壳写文件、CLI/手工可能丢进
        // `NUL`），那时目录里会留着一个**陈旧**的日志，看 `modifiedAt` 才知道它没在写。
        "logs": {
            "directory": crate::v2::environment::log_directory().map(|dir| dir.display().to_string()),
            "files": crate::v2::environment::log_files(),
        },
        // 写死的默认值也报出来：用户能改的是**连接级**的那两个量（见 domain::connection_options），
        // 没在连接上填的时候，生效的就是这两个数。报出来才知道"我没填时会发生什么"。
        //
        // 连**范围**一起报：界面上那两个输入框的提示要写"范围 10–3600"，而那个范围同样是
        // daemon 在夹紧时用的。前端自己抄一份数字，改的时候必然漂（这个仓库的老毛病）。
        "defaults": {
            "requestTimeoutSeconds":
                crate::v2::domain::connection_options::DEFAULT_REQUEST_TIMEOUT_SECS,
            "trackingDeadlineSeconds":
                crate::v2::domain::connection_options::DEFAULT_TRACKING_DEADLINE_SECS,
            "requestTimeoutMin":
                crate::v2::domain::connection_options::MIN_REQUEST_TIMEOUT_SECS,
            "requestTimeoutMax":
                crate::v2::domain::connection_options::MAX_REQUEST_TIMEOUT_SECS,
            "trackingDeadlineMin":
                crate::v2::domain::connection_options::MIN_TRACKING_DEADLINE_SECS,
            "trackingDeadlineMax":
                crate::v2::domain::connection_options::MAX_TRACKING_DEADLINE_SECS,
        }
    });
    Ok(Reply::ok(payload))
}

// ---------- RunningHub status ----------

pub async fn runninghub_account(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
    Query(query): Query<HashMap<String, String>>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsRead)?;
    let status = state
        .status
        .account_status(requested_connection(&query))
        .await
        .map_err(error::error_response)?;
    Ok(Reply::ok(status))
}

pub async fn runninghub_queue(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
    Query(query): Query<HashMap<String, String>>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsRead)?;
    let status = state
        .status
        .queue_status(requested_connection(&query))
        .await
        .map_err(error::error_response)?;
    Ok(Reply::ok(status))
}

// ---------- 回调签名密钥 ----------
//
// ⚠️ 这个密钥**只进不出**：写它只有这一条路（经认证的本地请求），读回来永远只有
// "配没配"。它给的是"回调确实来自我的 RenderLoft"这一条保证，所以：
// - MCP 那侧没有对应工具（`verbs.rs` 里 `mcp_name: None`）：工具参数是模型生成的，
//   密钥进去就等于进了模型上下文与对话记录；
// - 也不做"显示密钥"——接收端要用的那一串由**人**自己生成，两边填同一个值即可。

/// 设置回调签名密钥。
pub async fn set_callback_secret(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
    Json(input): Json<CallbackSecretDto>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsManage)?;
    let value = input.value.trim();
    if value.is_empty() {
        return Err(error::validation(
            "CALLBACK_SECRET_EMPTY",
            "回调签名密钥不能为空",
        ));
    }
    state
        .secrets
        .set(SIGNING_KEY_SLOT, value)
        .await
        .map_err(error::error_response)?;
    Ok(Reply::ok(json!({ "configured": true })))
}

/// 清掉回调签名密钥（**关掉签名**）。
///
/// 为什么必须有这条路：没有它，密钥一旦设了就只能在系统凭据管理器里手删——
/// 而"想关掉签名"是完全正当的需求（换接收端、临时排查）。清掉之后投递不带签名头。
pub async fn clear_callback_secret(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsManage)?;
    state
        .secrets
        .delete(SIGNING_KEY_SLOT)
        .await
        .map_err(error::error_response)?;
    Ok(Reply::ok(json!({ "configured": false })))
}

/// 回调签名密钥配没配（**永远不回值**）。
pub async fn callback_secret_status(
    State(state): State<Arc<V2HttpState>>,
    axum::Extension(actor): axum::Extension<ActorRef>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsRead)?;
    Ok(Reply::ok(
        json!({ "configured": callback_secret_configured(&state).await? }),
    ))
}

/// 只读"在不在"，不读值——值一旦读出来就有泄漏面（日志、错误、诊断都可能带出去）。
async fn callback_secret_configured(state: &V2HttpState) -> Result<bool, axum::response::Response> {
    let binding = SecretBinding {
        slot: SIGNING_KEY_SLOT.to_string(),
        resolution: SecretResolution::Current,
    };
    state
        .secrets
        .resolve(&binding)
        .await
        .map(|secret| secret.is_some())
        .map_err(error::error_response)
}

// ---------- reply helper ----------

struct Reply;

impl Reply {
    fn ok<T: serde::Serialize>(data: T) -> axum::response::Response {
        ok_json(data).into_response()
    }
}
