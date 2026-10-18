use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::v2::domain::errors::SafeError;

pub const CONTRACT_VERSION: &str = "1";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<T: Serialize> {
    pub contract_version: &'static str,
    pub ok: bool,
    pub data: T,
}

impl<T: Serialize> Envelope<T> {
    pub fn new(data: T) -> Self {
        Self {
            contract_version: CONTRACT_VERSION,
            ok: true,
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    pub contract_version: &'static str,
    pub ok: bool,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: String,
    pub kind: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl ErrorEnvelope {
    pub fn from_error(error: &SafeError) -> Self {
        Self {
            contract_version: CONTRACT_VERSION,
            ok: false,
            error: ErrorBody {
                code: error.code.clone(),
                kind: format!("{:?}", error.kind).to_lowercase(),
                message: error.message.clone(),
                retryable: error.retryable,
                details: error.details.clone(),
            },
        }
    }
}

pub fn ok_json<T: Serialize>(data: T) -> axum::Json<Value> {
    axum::Json(json!(Envelope::new(data)))
}

pub fn error_json(error: &SafeError) -> axum::Json<Value> {
    axum::Json(serde_json::to_value(ErrorEnvelope::from_error(error)).unwrap_or(Value::Null))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaginationQuery {
    pub limit: Option<usize>,
    /// 游标：只看 id **大于**它的（**升序**，从最早往后翻）。
    pub after: Option<u64>,
    /// 游标（**只在 `order=desc` 下有意义**）：只看 id **小于**它的，即"比这个更旧的"。
    #[serde(default)]
    pub before: Option<u64>,
    /// 方向：`desc` = **最新在前**（界面上的「结果」视角要它）。
    ///
    /// 为什么方向要显式给（2026-10-18 加）：`after` 的第一页是**最旧**的一批，而"我刚生成的那张
    /// 在哪"用它根本拿不到 —— 除非把整个列表翻完再倒序（拿流量换顺序，产物一多就废）。
    /// 而"没有游标、最新在前"这件事**必须能表达**：空值的 `before=` 解析不成数字（400），
    /// 所以方向不能靠"用哪个游标"隐含，得有一个显式的开关。
    ///
    /// 合法的两种组合：`order=desc`（最新在前）与 `order=desc&before=<id>`（比它更旧）。
    /// 其余组合（`desc` 配 `after`、`before` 不给 `desc`）**明确报错**，不默默挑一个 ——
    /// 挑错了会给出"看起来对、其实是最旧的一批"的结果，而调用方不会发现。
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub run_id: Option<u64>,
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T: Serialize> {
    pub items: Vec<T>,
    pub next_cursor: Option<u64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateOperationRequestDto {
    pub idempotency_key: String,
    pub runs: Vec<RunRequestDto>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default)]
    pub callback: Option<CallbackDto>,
}

/// `POST /api/v1/operations/validate` 的请求体：**只有 runs**。
///
/// 刻意不带 `idempotencyKey` / `metadata` / `callback`：预检不落库，没有要幂等的东西；
/// 多发一个它不用的字段应当被 `deny_unknown_fields` 拒绝，而不是被静默忽略。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateOperationRequestDto {
    pub runs: Vec<RunRequestDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRequestDto {
    pub client_run_id: String,
    pub recipe_revision_id: u64,
    #[serde(default)]
    pub connection_id: Option<u64>,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
    #[serde(default)]
    pub assets: Option<RunAssetsDto>,
}

/// 一个 run 的资产引用。**两种形状**（`untagged`：按内容自己认，不靠一个判别字段）：
///
/// * `Keyed`（推荐/规范形状）：`{ "<字段键>": { assetId, checksum } }`——字段键来自该
///   revision 的 inputSchema（`format: "file"` 的那些键，如 `"475::image"`）；
/// * `Positional`（**仅兼容**）：`[ { assetId, checksum } ]`——旧的位置编码，只对仍带
///   旧 `asset-N` 映射的 revision 有效。对新的 keyed revision 收到数组会报
///   `POSITIONAL_ASSETS_UNSUPPORTED`（在 `MediaMapping::bind` 里），不许猜。
///
/// ⚠️ 位置不是语义、也从来不是：数组下标与"第几个图槽"的对应关系随上游节点顺序、
/// schema 键序、调用方顺序各自漂移——这正是错位事故的根因。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RunAssetsDto {
    Keyed(BTreeMap<String, AssetRefDto>),
    Positional(Vec<AssetRefDto>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetRefDto {
    pub asset_id: u64,
    pub checksum: String,
}

/// 设置回调签名密钥的请求体。
///
/// ⚠️ 这个 DTO **只有人那条路会用到**（CLI `callback set-secret` 从文件读值），MCP 没有
/// 对应工具。**刻意不派生 `Debug`**：`String` 的 Debug 会把密钥原样打出来，
/// 而"顺手 `{:?}` 一下"正是密钥泄漏最常见的入口。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallbackSecretDto {
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallbackDto {
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub secret_ref: Option<String>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResponseDto {
    pub operation_id: u64,
    pub status: String,
    pub run_ids: Vec<u64>,
    pub status_url: String,
    pub events_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResponseDto {
    pub run_id: u64,
    pub operation_id: u64,
    pub client_run_id: String,
    pub sequence: u32,
    pub recipe_revision_id: u64,
    pub connection_revision_id: u64,
    pub status: String,
    pub tracking_state: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventResponseDto {
    pub event_id: u64,
    pub event_type: String,
    pub operation_id: u64,
    pub run_id: Option<u64>,
    pub sequence: u64,
    pub created_at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenIssuedDto {
    pub token_id: i64,
    pub token: String,
    pub actor_id: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSummaryDto {
    pub token_id: i64,
    pub actor_id: String,
    pub kind: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub label: Option<String>,
    pub expires_at: Option<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueTokenRequestDto {
    pub actor_id: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateConnectionRequestDto {
    pub vendor: Option<String>,
    pub protocol: Option<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub lifecycle_features: Value,
    #[serde(default)]
    pub secret_bindings: Vec<SecretBindingEntryDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateConnectionRequestDto {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub config: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub lifecycle_features: Option<Value>,
    #[serde(default)]
    pub secret_bindings: Option<Vec<SecretBindingEntryDto>>,
}

/// 密钥绑定条目：`value` 仅用于写入 keyring，不回显。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretBindingEntryDto {
    pub slot: String,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRecipeRequestDto {
    pub name: Option<String>,
    pub capability: Option<String>,
    pub driver_key: Option<String>,
    pub default_connection_id: Option<u64>,
    #[serde(default)]
    pub allowed_connection_ids: Option<Vec<u64>>,
    #[serde(default)]
    pub provider_config: Option<Value>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub defaults: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAssetRequestDto {
    pub filename: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRecipeRequestDto {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub favorite: Option<bool>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// 字段备注（schema 的 key → 人话名字）。
    ///
    /// ⚠️ **整体替换**语义，不是逐键合并：这样"删掉一条备注"才表达得出来
    /// （合并语义下，省略某个键既可能是"不改"也可能是"删"，分不清）。
    /// 传空对象 = 清空全部备注。
    #[serde(default)]
    pub field_labels: Option<std::collections::BTreeMap<String, String>>,
    /// 用户在编辑里改的**字段控件**（"这个字段该是什么控件、有哪些合法选项"）。
    ///
    /// 与备注不同：备注是**展示语义**（读 schema 时合并），控件会改变**执行语义**
    /// （换成"图片或文件"要走资产上传那条路），所以它写进一份**新 revision**。
    /// 空数组 = 不改控件（不是"清空"：控件的来源可以是我们/引擎，没有"清空"这回事）。
    #[serde(default)]
    pub field_widgets: Option<Vec<crate::v2::application::field_widget::FieldWidgetChange>>,
}
