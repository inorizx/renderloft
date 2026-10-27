#![allow(clippy::result_large_err)]
use axum::{
    body::Bytes,
    extract::{Extension, Path, Query, State},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::v2::application::asset::{
    AssetDependencies, CreateAsset, CreateAssetRequest as DomainCreateAssetRequest,
};
use crate::v2::application::connection::{
    ConnectionDependencies, CreateConnection, SecretBindingEntry as DomainSecretBindingEntry,
    UpdateConnection,
};
use crate::v2::application::connection::{
    CreateConnectionRequest as DomainCreateConnectionRequest,
    UpdateConnectionRequest as DomainUpdateConnectionRequest,
};
use crate::v2::application::create_operation::{
    CreateOperationRequest as DomainCreateOperationRequest, RunRequest as DomainRunRequest,
};
use crate::v2::application::recipe::{
    CreateRecipe, CreateRecipeRequest as DomainCreateRecipeRequest, RecipeDependencies,
};
use crate::v2::application::retry_operation::RetryOperation;
use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::callback::{is_acceptable_url, CallbackSubscription};
use crate::v2::domain::ids::{
    AssetId, ConnectionId, OperationId, RecipeId, RecipeRevisionId, RunId,
};
use crate::v2::domain::media::AssetRef;
use crate::v2::domain::model::{Recipe, RecipeBinding};
use crate::v2::ports::repositories::{ActorTokenRecord, ArtifactCursor};
use crate::v2::ports::safety::SafeHttpClient;

use super::dto::{
    CallbackDto, CreateConnectionRequestDto, CreateOperationRequestDto, CreateRecipeRequestDto,
    Envelope, EventResponseDto, IssueTokenRequestDto, OperationResponseDto, Page, PaginationQuery,
    RunAssetsDto, RunRequestDto, SecretBindingEntryDto, TokenIssuedDto, TokenSummaryDto,
    UpdateConnectionRequestDto, UpdateRecipeRequestDto, ValidateOperationRequestDto,
};
use super::error;
// 使用与 axum::Json 同名的包装提取器：请求体解析失败时返回统一信封而非纯文本。
use super::json::Json;
use super::V2HttpState;

type HttpResult = Result<Json<Value>, axum::response::Response>;

fn domain_error(error: crate::v2::domain::errors::SafeError) -> axum::response::Response {
    error::error_response(error)
}

/// 严格解析密钥绑定条目；坏条目显式报错（替代 `.ok()` 静默丢弃）。
fn parse_binding_entries(
    entries: Vec<SecretBindingEntryDto>,
) -> Result<Vec<DomainSecretBindingEntry>, axum::response::Response> {
    entries
        .into_iter()
        .map(|entry| {
            DomainSecretBindingEntry::parse(entry.slot, entry.resolution, entry.value)
                .map_err(domain_error)
        })
        .collect()
}

fn require_scope(actor: &ActorRef, scope: Scope) -> Result<(), axum::response::Response> {
    super::require_scope(actor, scope)
}

fn page_response<T: serde::Serialize>(items: Vec<T>, after: Option<u64>, limit: usize) -> Page<T> {
    let has_more = items.len() > limit;
    let mut visible = items;
    if has_more {
        visible.truncate(limit);
    }
    let next_cursor = after.filter(|_| has_more);
    Page {
        items: visible,
        next_cursor,
        has_more,
    }
}

// ---------- discovery ----------

/// 对外发布的 `openapi.json` 所声明的路由 —— **唯一权威**。
///
/// ⚠️ 手工维护，必须与 `mod.rs::router()` 里注册的路由**完全一致（路径 + 方法）**。
/// axum 的 `Router` 无法自省已注册路由，所以一致性由
/// `transports::http::tests::openapi_table_matches_registered_routes` **扫源码强制**。
/// 实测该表曾只覆盖 28/44 条路径（含漏掉 `/api/v1/settings`），
/// 使对外发布的文档少报 1/3 的 API。
pub const ROUTES: &[(&str, &str)] = &[
    ("/api/v1/health", "get"),
    ("/api/v1/capabilities", "get"),
    ("/api/v1/recipes", "get"),
    ("/api/v1/recipes", "post"),
    ("/api/v1/recipes/import", "post"),
    ("/api/v1/recipes/import-workflow", "post"),
    ("/api/v1/recipes/comfyui-import", "post"),
    ("/api/v1/recipes/comfyui-preview", "post"),
    ("/api/v1/recipes/comfyui-fetch-workflow", "post"),
    ("/api/v1/recipes/{recipe_id}", "get"),
    ("/api/v1/recipes/{recipe_id}", "put"),
    ("/api/v1/recipes/{recipe_id}", "delete"),
    ("/api/v1/recipes/{recipe_id}/schema", "get"),
    ("/api/v1/recipes/{recipe_id}/touch", "post"),
    ("/api/v1/recipes/{recipe_id}/restore", "post"),
    ("/api/v1/operations", "get"),
    ("/api/v1/operations", "post"),
    ("/api/v1/operations/validate", "post"),
    ("/api/v1/operations/{operation_id}", "get"),
    ("/api/v1/operations/{operation_id}", "delete"),
    ("/api/v1/operations/{operation_id}/cancel", "post"),
    ("/api/v1/operations/{operation_id}/retry", "post"),
    (
        "/api/v1/operations/{operation_id}/runs/{run_id}/retry",
        "post",
    ),
    ("/api/v1/operations/{operation_id}/events", "get"),
    ("/api/v1/operations/{operation_id}/manifest", "get"),
    ("/api/v1/runs/{run_id}", "get"),
    ("/api/v1/assets", "post"),
    ("/api/v1/assets/{asset_id}", "get"),
    ("/api/v1/assets/{asset_id}/bytes", "get"),
    ("/api/v1/artifacts", "get"),
    ("/api/v1/artifacts/{artifact_id}", "get"),
    ("/api/v1/artifacts/{artifact_id}/bytes", "get"),
    ("/api/v1/artifacts/{artifact_id}/save", "post"),
    ("/api/v1/connections", "get"),
    ("/api/v1/connections", "post"),
    ("/api/v1/connections/{connection_id}", "get"),
    ("/api/v1/connections/{connection_id}", "put"),
    ("/api/v1/connections/{connection_id}", "delete"),
    ("/api/v1/connections/{connection_id}/check", "get"),
    ("/api/v1/connections/{connection_id}/models", "get"),
    ("/api/v1/actor-tokens", "get"),
    ("/api/v1/actor-tokens", "post"),
    ("/api/v1/actor-tokens/{token_id}", "delete"),
    ("/api/v1/settings", "get"),
    ("/api/v1/settings", "put"),
    ("/api/v1/settings/open-download-dir", "post"),
    ("/api/v1/callback-secret", "get"),
    ("/api/v1/callback-secret", "put"),
    ("/api/v1/callback-secret", "delete"),
    ("/api/v1/diagnostics", "get"),
    ("/api/v1/runninghub/account", "get"),
    ("/api/v1/runninghub/queue", "get"),
    ("/api/v1/workspace/backup", "post"),
    ("/api/v1/openapi.json", "get"),
];

pub async fn openapi() -> HttpResult {
    let mut paths = serde_json::Map::new();
    for (path, method) in ROUTES {
        let entry = paths.entry(path.to_string()).or_insert_with(|| json!({}));
        entry.as_object_mut().unwrap().insert(
            method.to_string(),
            json!({
                "responses": {
                    "200": { "description": "OK" },
                    "400": { "description": "请求无效" },
                    "401": { "description": "未认证" },
                    "403": { "description": "缺少权限" },
                    "404": { "description": "资源不存在" }
                }
            }),
        );
    }
    let document = json!({
        "openapi": "3.0.3",
        "info": {
            "title": "RenderLoft API",
            "version": "1",
            "description": "RenderLoft local AIGC runtime。所有请求（除 health）需 Bearer token。错误使用统一信封。"
        },
        "paths": paths,
        "components": {
            "schemas": {
                "ErrorEnvelope": {
                    "type": "object",
                    "required": ["contractVersion", "ok", "error"],
                    "properties": {
                        "contractVersion": { "type": "string" },
                        "ok": { "type": "boolean" },
                        "error": {
                            "type": "object",
                            "required": ["code", "kind", "message", "retryable"],
                            "properties": {
                                "code": { "type": "string" },
                                "kind": { "type": "string" },
                                "message": { "type": "string" },
                                "retryable": { "type": "boolean" },
                                "details": { "type": "object" }
                            }
                        }
                    }
                },
                "CreateOperation": {
                    "type": "object",
                    "required": ["idempotencyKey", "runs"],
                    "properties": {
                        "idempotencyKey": { "type": "string" },
                        "runs": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/RunRequest" }
                        },
                        "metadata": { "type": "object" }
                    }
                },
                "RunRequest": {
                    "type": "object",
                    "required": ["clientRunId", "recipeRevisionId"],
                    "properties": {
                        "clientRunId": { "type": "string" },
                        "recipeRevisionId": { "type": "integer", "format": "int64" },
                        "connectionId": { "type": "integer", "format": "int64" },
                        "inputs": { "type": "object" },
                        "assets": {
                            "description": "这个 run 的资产。推荐形状是对象：键 = 字段键（inputSchema 里 format 为 file 的键，如 \"475::image\"）。旧的数组形状只对仍使用 asset-N 映射的 revision 有效；新 revision 收到数组会报 POSITIONAL_ASSETS_UNSUPPORTED。",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "additionalProperties": { "$ref": "#/components/schemas/AssetRef" }
                                },
                                {
                                    "type": "array",
                                    "items": { "$ref": "#/components/schemas/AssetRef" }
                                }
                            ]
                        }
                    }
                },
                "AssetRef": {
                    "type": "object",
                    "required": ["assetId", "checksum"],
                    "properties": {
                        "assetId": { "type": "integer", "format": "int64" },
                        "checksum": { "type": "string" }
                    }
                },
                "ValidateOperation": {
                    "type": "object",
                    "required": ["runs"],
                    "properties": {
                        "runs": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/RunRequest" }
                        }
                    }
                }
            }
        }
    });
    Ok(Json(serde_json::to_value(Envelope::new(document)).unwrap()))
}

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn capabilities(State(state): State<Arc<V2HttpState>>) -> Json<Value> {
    let mut protocols = Vec::new();
    for key in state.drivers.keys() {
        if let Ok(driver) = state.drivers.get(key) {
            let descriptor = driver.descriptor();
            protocols.push(json!({
                "vendor": descriptor.vendor_id,
                "protocol": descriptor.protocol_id,
                "displayName": descriptor.display_name,
                "capabilities": descriptor.business_capabilities,
                "connectionSchema": descriptor.connection_schema,
                "bindingSchema": descriptor.binding_schema,
            }));
        }
    }
    Json(
        serde_json::to_value(Envelope::new(json!({
            "contractVersion": super::dto::CONTRACT_VERSION,
            "protocols": protocols,
        })))
        .unwrap(),
    )
}

// ---------- recipes ----------

/// 资源库列表的查询参数。
///
/// 比通用分页多一个 `removed`：`?removed=true` 列的是**已移除（墓碑）**的那一批——
/// 它不是"活着的里面筛一部分"，而是**另一个集合**（墓碑，
/// 界面上的"显示已移除"直接用它）。刻意不复用 `PaginationQuery`：那个结构体被
/// operations / artifacts 共用，在里面加一个只对 recipes 有意义的字段，等于让
/// `?removed=true` 在别的端点上被静默接受然后无视。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecipeListQuery {
    pub limit: Option<usize>,
    pub after: Option<u64>,
    #[serde(default)]
    pub removed: bool,
}

pub async fn list_recipes(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Query(query): Query<RecipeListQuery>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesRead)?;
    let limit = query.limit.unwrap_or(50).min(200);
    if query.removed {
        let removed = state
            .recipes
            .list_removed_recipes(limit)
            .await
            .map_err(domain_error)?;
        // 已移除的这一批**也要带 binding**：界面卡片与活着的那些同形渲染
        //（类型标签、默认连接），否则"最近删除"里的应用会一律显示成「其他」——
        // 那是查询偷懒，不是事实。回收站很小，逐个取 revision 的代价可以接受；
        // 也因此没去改 `list_removed_recipes` 的返回形状（那要同时改 sqlite 与 fakes，
        // 两处都得对齐，而这里不需要）。
        let mut items = Vec::with_capacity(removed.len());
        for recipe in &removed {
            let binding = state
                .recipes
                .find_revision(recipe.current_revision_id)
                .await
                .map_err(domain_error)?
                .map(|revision| revision.binding);
            items.push(recipe_summary(recipe, binding.as_ref()));
        }
        return Ok(Json(
            serde_json::to_value(Envelope::new(json!({
                "items": items,
                // 墓碑列表按移除时间倒序，不分页：它天然是个小集合。
                "nextCursor": Value::Null,
                "hasMore": false,
            })))
            .unwrap(),
        ));
    }
    let recipes = state
        .recipes
        .list_recipes(limit + 1, query.after.map(RecipeId))
        .await
        .map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(page_response(
            recipes
                .iter()
                .map(|(recipe, binding)| recipe_summary(recipe, Some(binding)))
                .collect::<Vec<_>>(),
            query.after,
            limit,
        )))
        .unwrap(),
    ))
}

/// 配方「种类」：**由绑定的形状派生**，不是按驱动猜。
///
/// 为什么单独给一个字段：`driverKey` 只回答"**在哪跑**"，而卡片上紧挨着就有连接名，
/// 于是同一件事说了两遍，真正有用的"**这是什么**"反而没有——用户认自己的东西靠的是后者
///（"我那个 RH 应用" / "我导进来的那个工作流"）。
///
/// ⚠️ 2026-10-11 曾判断"驱动分不出应用与工作流，所以只照实说驱动"。**那是错的**：
/// 绑定里就写着（`webappId` vs `workflowId` vs 内联 `workflow`）。分不出的是"平台上的东西
/// 叫什么名字"，不是"它是哪一类东西"。
///
/// 判据只认**键的存在**，不认值：
/// - `webappId`   → `rh-app`：AI 应用，定义在平台上
/// - 内联 `workflow` → `rh-workflow` / `comfyui-workflow`：图在我们库里
/// - `workflowId` → `rh-workflow`：图在平台上，我们只记 id 与字段映射
/// - OpenAI 图像那条路没有图，只有提示词参数 → `openai-image`
fn recipe_kind(binding: Option<&RecipeBinding>) -> &'static str {
    let Some(binding) = binding else {
        return "other";
    };
    // 驱动键有两种形态（`vendor/protocol` 与历史裸键），统一取末段。
    let protocol = binding
        .driver_key
        .rsplit('/')
        .next()
        .unwrap_or(binding.driver_key.as_str());
    match protocol {
        "runninghub" => {
            if binding.provider_config.contains_key("webappId") {
                "rh-app"
            } else {
                // workflowId 与内联 workflow 都归这里：**它是不是工作流**与"图存在哪"无关，
                // 而"图存在哪"已经由 `RecipeSource` 表达。
                "rh-workflow"
            }
        }
        "comfyui" => "comfyui-workflow",
        "openai-images" => "openai-image",
        _ => "other",
    }
}

fn recipe_summary(recipe: &Recipe, binding: Option<&RecipeBinding>) -> Value {
    json!({
        "recipeId": recipe.id.0,
        "name": recipe.name,
        "description": recipe.description,
        "capability": recipe.capability,
        "labels": recipe.labels,
        "favorite": recipe.favorite,
        "group": recipe.group,
        "note": recipe.note,
        "lastOpenedAt": recipe.last_opened_at.as_ref().map(|timestamp| timestamp.0.clone()),
        "createdAt": recipe.created_at.0.clone(),
        "driverKey": binding.map(|binding| binding.driver_key.clone()),
        // 种类（"这是什么"）与驱动（"在哪跑"）分开：卡片上前者当标签，后者进详情页。
        "kind": recipe_kind(binding),
        "defaultConnectionId": binding.map(|binding| binding.default_connection_id.0),
        "currentRevisionId": recipe.current_revision_id.0,
        "visibility": recipe.visibility,
        // 原始备注（**未经合并**）：界面编辑框要显示"用户自己填的"，
        // 而不是把平台给的 `description`（等于字段名）当成人填的预填进去。
        "fieldLabels": recipe.field_labels,
        // 移除时间（活着的是 null）。界面的"显示已移除"据此显示"移除于 …"，
        // 也让"移除"这件事在数据上**可观察**——否则前端只能靠"它不在列表里"猜。
        "removedAt": recipe.removed_at.as_ref().map(|timestamp| timestamp.0.clone()),
    })
}

pub async fn get_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(recipe_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesRead)?;
    let recipe = state
        .recipes
        .find_recipe(RecipeId(recipe_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"))?;
    let revision = state
        .recipes
        .find_revision(recipe.current_revision_id)
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(json!({
        "recipe": recipe_summary(&recipe, revision.as_ref().map(|r| &r.binding)),
        // 与 `/recipes/{id}/schema` 走**同一个**合并函数。
        // 这里曾漏掉——同一份 schema 有三条读取路径，漏一条就会出现
        // "用户在表单里看到备注、另一个地方看不到"的偏差，而两边都不报错。
        "inputSchema": revision
            .as_ref()
            .map(|r| crate::v2::application::recipe::apply_field_labels(
                &r.input_schema,
                &recipe.field_labels,
            ))
            .unwrap_or(Value::Null),
        "outputSchema": revision.as_ref().map(|r| r.output_schema.clone()).unwrap_or(Value::Null),
    })))
    .unwrap()))
}

pub async fn get_recipe_schema(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(recipe_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesRead)?;
    let recipe = state
        .recipes
        .find_recipe(RecipeId(recipe_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"))?;
    let revision = state
        .recipes
        .find_revision(recipe.current_revision_id)
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("RECIPE_REVISION_NOT_FOUND", "Revision 不存在"))?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "recipeId": recipe.id.0,
            "revision": revision.revision,
            "revisionId": revision.id.0,
            // 与「应用详情」那条路走**同一个**合并函数：备注是给人看的语义，
            // 读 schema 的地方必须看到同一份，否则用户以为改了、另一边却没看到。
            "inputSchema": crate::v2::application::recipe::apply_field_labels(
                &revision.input_schema,
                &recipe.field_labels,
            ),
            "outputSchema": revision.output_schema,
             "defaults": revision.defaults,
            // 这条配方绑定的连接**声明**能跑哪些模型（`config.models`）。
            // 跑表单据此把 model 字段渲染成下拉，而不是让用户手打——打错了要到提交时才知道。
            "modelOptions": model_options(state.as_ref(), &revision.binding).await,
        })))
        .unwrap(),
    ))
}

/// 绑定连接声明的模型清单。拿不到（没绑定/连接没了/没声明）就是空数组 ——
/// **空 = 不过滤**，表单照旧是自由输入：声明是给人方便，不是闸门。
///
/// ⚠️ **默认模型排在最前**：跑表单见到 enum 会预选第一个（`V2Runner` 的既有行为），
/// 而"连接声明的默认模型"正是它该预选的那个。不排前面的话，默认模型会被清单里的第一个
/// 顶掉——用户设了默认、跑出来却是另一个模型，而且看不出为什么。
async fn model_options(
    state: &V2HttpState,
    binding: &crate::v2::domain::model::RecipeBinding,
) -> Vec<String> {
    let Ok(Some(connection)) = state
        .connections
        .find_connection(binding.default_connection_id)
        .await
    else {
        return vec![];
    };
    let Ok(Some(revision)) = state
        .connections
        .find_revision(connection.current_revision_id)
        .await
    else {
        return vec![];
    };
    let mut options: Vec<String> = revision
        .config
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(default_model) = revision
        .config
        .get("defaultModel")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        options.retain(|model| model != default_model);
        options.insert(0, default_model.to_string());
    }
    options
}

pub async fn create_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<CreateRecipeRequestDto>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let default_connection_id =
        ConnectionId(body.default_connection_id.ok_or_else(|| {
            error::validation("CONNECTION_REQUIRED", "defaultConnectionId 不能为空")
        })?);
    let request = DomainCreateRecipeRequest {
        actor,
        name: body
            .name
            .ok_or_else(|| error::validation("NAME_REQUIRED", "Recipe 名称不能为空"))?,
        capability: body
            .capability
            .ok_or_else(|| error::validation("CAPABILITY_REQUIRED", "Recipe 能力不能为空"))?,
        driver_key: body
            .driver_key
            .ok_or_else(|| error::validation("DRIVER_KEY_REQUIRED", "driverKey 不能为空"))?,
        default_connection_id,
        allowed_connection_ids: body
            .allowed_connection_ids
            .unwrap_or_else(|| vec![default_connection_id.0])
            .into_iter()
            .map(ConnectionId)
            .collect(),
        provider_config: body.provider_config.unwrap_or_else(|| json!({})),
        input_schema: body
            .input_schema
            .unwrap_or_else(|| json!({ "type": "object" })),
        output_schema: body
            .output_schema
            .unwrap_or_else(|| json!({ "type": "object" })),
        description: body.description,
        labels: body.labels,
        defaults: body.defaults,
    };
    let use_case = CreateRecipe::new(RecipeDependencies {
        recipes: state.recipes.clone(),
    });
    let recipe = use_case.execute(request).await.map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "recipeId": recipe.id.0,
            "currentRevisionId": recipe.current_revision_id.0,
        })))
        .unwrap(),
    ))
}

pub async fn import_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<crate::v2::application::recipe_import::ImportRequest>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let imported = state
        .import_service
        .import(
            crate::v2::domain::ids::ConnectionId(body.connection_id),
            &body.source,
        )
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(imported)).unwrap()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportWorkflowRequest {
    pub connection_id: u64,
    pub source: String,
    pub name: Option<String>,
    /// 用户在"选择参数"里做的决定（勾哪些、备注名、控件、候选项）。
    ///
    /// 不给 = **机械导入**（除启发式跳过的之外全暴露）。给 = 用户策展过，他说了算 ——
    /// RunningHub 工作流以前**根本没有这一步**，所以"让用户自己选控件"到不了这条路上。
    #[serde(default)]
    pub fields: Option<Vec<crate::v2::application::recipe_import::ComfyFieldSelection>>,
}

/// 从 RunningHub 工作流链接/ID 导入一个原生工作流应用（与 `import_recipe` 对称：
/// 应用存 `webappId`、工作流存 `workflowId`，定义都留在平台上）。
pub async fn import_workflow(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<ImportWorkflowRequest>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let imported = state
        .import_service
        .import_workflow(
            crate::v2::domain::ids::ConnectionId(body.connection_id),
            &body.source,
            body.name,
            body.fields,
        )
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(imported)).unwrap()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportComfyUIRequest {
    pub connection_id: u64,
    pub prompt: Value,
    pub name: Option<String>,
    pub fields: Option<Vec<crate::v2::application::recipe_import::ComfyFieldSelection>>,
}

pub async fn import_comfyui_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<ImportComfyUIRequest>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let imported = state
        .import_service
        .import_comfyui_prompt(
            crate::v2::domain::ids::ConnectionId(body.connection_id),
            body.prompt,
            body.name,
            body.fields,
        )
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(imported)).unwrap()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchComfyUIWorkflowRequest {
    pub connection_id: u64,
    pub source: String,
}

/// 通过 RunningHub Connection 拉取云端 ComfyUI 工作流的 API Prompt JSON。
pub async fn fetch_comfyui_workflow(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<FetchComfyUIWorkflowRequest>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let prompt = state
        .import_service
        .fetch_comfyui_workflow(
            crate::v2::domain::ids::ConnectionId(body.connection_id),
            &body.source,
        )
        .await
        .map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "prompt": prompt }))).unwrap(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewComfyUIRequest {
    pub prompt: Value,
    /// 给哪个 ComfyUI 连接去问节点定义（可选）。
    ///
    /// 给了才能把通用控件升级成真控件（枚举→下拉、数字→带范围、多行→多行框）；
    /// 不给就只按 JSON 值的形状猜——**仍然能用**，只是控件普通，而且响应里的
    /// `enrichment.reason` 会说清原因。
    pub connection_id: Option<u64>,
}

/// 导入前预览：解析 API Prompt 里的全部候选字段（不创建 Recipe），
/// 供用户在导入对话框里勾选与备注。顺带**尽力**问一次引擎的节点定义，
/// 好让界面知道这批字段会拿到真控件还是通用控件。
pub async fn preview_comfyui_fields(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<PreviewComfyUIRequest>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let Some(connection_id) = body.connection_id else {
        // 老调用方（不带连接）照旧能用：字段清单一样给，只是没有富化。
        let fields =
            crate::v2::application::recipe_import::preview_comfyui_fields(&body.prompt, None)
                .map_err(domain_error)?;
        return Ok(Json(
            serde_json::to_value(Envelope::new(json!({
                "fields": fields,
                "enrichment": {
                    "classesUsed": 0,
                    "classesKnown": 0,
                    "reason": "没有指定 ComfyUI 连接，没有去问节点定义（控件用通用类型）",
                },
            })))
            .unwrap(),
        ));
    };
    let preview = state
        .import_service
        .preview_comfyui_prompt(
            crate::v2::domain::ids::ConnectionId(connection_id),
            &body.prompt,
        )
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(preview)).unwrap()))
}

pub async fn update_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(recipe_id): Path<u64>,
    Json(body): Json<UpdateRecipeRequestDto>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    // 控件那部分（若有）先落：它要**追加一份 revision**，而且会失败（键不认识 / 控件名认不出）。
    // 放在前面是为了"要么都成，要么什么都没动"——失败时备注也不会被改掉一半。
    if let Some(changes) = body
        .field_widgets
        .as_deref()
        .filter(|list| !list.is_empty())
    {
        crate::v2::application::recipe::UpdateFieldWidgets::new(
            crate::v2::application::recipe::RecipeDependencies {
                recipes: state.recipes.clone(),
            },
        )
        .execute(&actor, RecipeId(recipe_id), changes)
        .await
        .map_err(domain_error)?;
    }
    let mut recipe = state
        .recipes
        .find_recipe(RecipeId(recipe_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"))?;
    if let Some(name) = body.name {
        recipe.name = name;
    }
    if let Some(description) = body.description {
        recipe.description = description;
    }
    if let Some(labels) = body.labels {
        recipe.labels = labels;
    }
    if let Some(favorite) = body.favorite {
        recipe.favorite = favorite;
    }
    if let Some(group) = body.group {
        recipe.group = Some(group);
    }
    if let Some(note) = body.note {
        recipe.note = Some(note);
    }
    if let Some(field_labels) = body.field_labels {
        recipe.field_labels = field_labels;
    }
    recipe.updated_at = crate::v2::domain::model::Timestamp::now();
    state
        .recipes
        .update_recipe(&recipe)
        .await
        .map_err(domain_error)?;
    let binding = state
        .recipes
        .find_revision(recipe.current_revision_id)
        .await
        .map_err(domain_error)?
        .map(|revision| revision.binding);
    Ok(Json(
        serde_json::to_value(Envelope::new(recipe_summary(&recipe, binding.as_ref()))).unwrap(),
    ))
}

pub async fn touch_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(recipe_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let mut recipe = state
        .recipes
        .find_recipe(RecipeId(recipe_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"))?;
    recipe.last_opened_at = Some(crate::v2::domain::model::Timestamp::now());
    state
        .recipes
        .update_recipe(&recipe)
        .await
        .map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "touched": true,
        })))
        .unwrap(),
    ))
}

pub async fn delete_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(recipe_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let removed = state
        .recipes
        .archive_recipe(RecipeId(recipe_id))
        .await
        .map_err(domain_error)?;
    if !removed {
        return Err(error::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"));
    }
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "deleted": true }))).unwrap(),
    ))
}

/// 从"已移除"里恢复一个应用。
///
/// 恢复后它重新出现在应用库列表里（列表跳过墓碑）。
/// 不在墓碑里的（没移除过、或压根不存在）返回 404 `RECIPE_NOT_IN_TRASH`：
/// "恢复成功"不该对一件没发生的事成立。
pub async fn restore_recipe(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(recipe_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::RecipesWrite)?;
    let restored = state
        .recipes
        .restore_recipe(RecipeId(recipe_id))
        .await
        .map_err(domain_error)?;
    if !restored {
        return Err(error::not_found(
            "RECIPE_NOT_IN_TRASH",
            "这个应用不在已移除列表里",
        ));
    }
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "restored": true }))).unwrap(),
    ))
}

// ---------- operations ----------

/// 回调地址放行规则见 `domain::callback::is_acceptable_url`（**https 或回环 http**）。
///
/// ⚠️ 还没有签名（投递时 `signature: None`）：接收端目前无法验证"这确实来自我的
/// RenderLoft"。所以**别把接收端暴露到公网**，除非先补上 HMAC。
fn callback_from_dto(
    dto: Option<CallbackDto>,
) -> Result<Option<CallbackSubscription>, axum::response::Response> {
    dto.map(|callback| {
        if callback.url.is_empty() || !is_acceptable_url(&callback.url) {
            return Err(error::validation(
                "CALLBACK_URL_INVALID",
                "回调 URL 必须是 HTTPS，或回环地址（http://127.0.0.1 / localhost / [::1]）",
            ));
        }
        Ok(CallbackSubscription {
            url: callback.url,
            events: callback.events,
            secret_ref: callback.secret_ref,
            max_attempts: callback.max_attempts.unwrap_or(10),
            retry_after_seconds: callback.retry_after_seconds.unwrap_or(30),
        })
    })
    .transpose()
}

/// 调用方自报的 `origin`（白名单）。
///
/// ⚠️ 为什么需要它：MCP 与 GUI 走**同一个**端点，origin 都是 `http`——而 daemon 的花钱闸门
/// 要区分"自主调用方"与"人在操作"。所以让客户端自报（`X-RenderLoft-Driver`）。
///
/// ⚠️ 白名单而不是照抄：认不出来的值一律回落到 `http`（= 不设闸的那一类）。
/// 反过来（认不出来就当自主）会让一个拼错的头把正常调用方关进闸门里，
/// 那种故障很难查——而"回落成不设闸"最多是维持现状。
fn declared_origin(headers: &axum::http::HeaderMap) -> String {
    const ALLOWED: [&str; 1] = ["mcp"];
    headers
        .get(crate::v2::clients::client::DRIVER_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| ALLOWED.contains(value))
        .unwrap_or("http")
        .to_string()
}

pub async fn create_operation(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateOperationRequestDto>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsExecute)?;
    let runs = runs_from_dto(body.runs)?;
    let request = DomainCreateOperationRequest {
        actor,
        // 调用方可以自报身份（如 MCP）。**白名单**：认不出来的一律按 `http`，
        // 免得一个拼错的头把调用方变成"未设闸"或"已设闸"里的意外那一类。
        origin: declared_origin(&headers),
        idempotency_key: crate::v2::domain::ids::IdempotencyKey(body.idempotency_key),
        runs,
        metadata: body.metadata,
        callback: callback_from_dto(body.callback)?,
    };
    let persisted = state
        .create_operation
        .execute(request)
        .await
        .map_err(domain_error)?;
    let response = OperationResponseDto {
        operation_id: persisted.operation.id.0,
        status: persisted.operation.status.as_str().to_string(),
        run_ids: persisted.runs.iter().map(|run| run.id.0).collect(),
        status_url: format!("/api/v1/operations/{}", persisted.operation.id.0),
        events_url: format!("/api/v1/operations/{}/events", persisted.operation.id.0),
    };
    Ok(Json(serde_json::to_value(Envelope::new(response)).unwrap()))
}

/// DTO 的 runs → 领域 `RunRequest`。**两种资产形状在这里分叉**，往下走就只有
/// 带字段键的 keyed 资产了（旧数组是 `None` 的 field_key，由 `MediaMapping::bind`
/// 一次性换算）。
fn runs_from_dto(
    runs: Vec<RunRequestDto>,
) -> Result<Vec<DomainRunRequest>, axum::response::Response> {
    let mut converted = Vec::with_capacity(runs.len());
    for run in runs {
        let assets = match run.assets {
            None => Vec::new(),
            Some(RunAssetsDto::Keyed(entries)) => entries
                .into_iter()
                .map(|(field_key, asset)| {
                    AssetRef::new(AssetId(asset.asset_id), asset.checksum).with_field_key(field_key)
                })
                .collect(),
            Some(RunAssetsDto::Positional(entries)) => entries
                .into_iter()
                .map(|asset| AssetRef::new(AssetId(asset.asset_id), asset.checksum))
                .collect(),
        };
        converted.push(DomainRunRequest {
            client_run_id: run.client_run_id,
            recipe_revision_id: RecipeRevisionId(run.recipe_revision_id),
            connection_id: run.connection_id.map(ConnectionId),
            inputs: run.inputs,
            assets,
        });
    }
    Ok(converted)
}

/// `POST /api/v1/operations/validate` —— **只读预检**（dry-run）。
///
/// 与 `create_operation` 走**同一套解析**（同一个 `preflight`），但：
/// * 不落 Operation / Run / event，不调度，不碰 provider；
/// * 不收 `idempotencyKey`（它没有要幂等的东西）；
/// * 成功时回 `{ dryRun: true, valid: true, runs: [...] }` 供调用方在花钱前对形状。
pub async fn validate_operation(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<ValidateOperationRequestDto>,
) -> HttpResult {
    // 与 create 同一道闸：预检的真实用途是"提交前对形状"，能提交的人才能预检。
    // （用例内部 `preflight` 还会再查一次策略——判据只有一份，这里只是传输层的早退。）
    require_scope(&actor, Scope::OperationsExecute)?;
    let runs = runs_from_dto(body.runs)?;
    let resolved = state
        .validate_operation
        .execute(&actor, runs)
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(resolved)).unwrap()))
}

pub async fn get_operation(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(operation_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsRead)?;
    let persisted = state
        .operations
        .find_operation(OperationId(operation_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("OPERATION_NOT_FOUND", "Operation 不存在"))?;
    let operation = &persisted.operation;
    // 用首个 run 的配方修订解析出一个友好的"应用名"，供前端展示（失败后再重试等场景普通用户只看得到机器串）。
    // 顺手把"这个修订属于哪个应用"记进下面的缓存：界面要拿 `recipeId` 才能"用这套参数再跑一次"
    // （跳到那个应用的表单），而它和这里的名字查询是**同一次**读，不该查两遍。
    let mut recipe_of_revision: std::collections::HashMap<u64, Option<u64>> =
        std::collections::HashMap::new();
    // 每条 run 的账号：按 connection revision 缓存（同一批次的 run 通常只涉及一两条）。
    let mut connection_of_revision: std::collections::HashMap<u64, Option<(u64, String)>> =
        std::collections::HashMap::new();
    let recipe_name = if let Some(first) = persisted.runs.first() {
        if let Ok(Some(revision)) = state.recipes.find_revision(first.recipe_revision_id).await {
            recipe_of_revision.insert(first.recipe_revision_id.0, Some(revision.recipe_id.0));
            if let Ok(Some(recipe)) = state.recipes.find_recipe(revision.recipe_id).await {
                Some(recipe.name)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    // 每条 run 的 `recipeId`：**按不同修订缓存**。正常一批只有一个修订（查 1 次）；
    // 混合修订的批量（只有 API 能构造）最多查 N 次，但每条 run 拿到的都是**它自己那个应用**，
    // 而不是"第一个 run 的应用"——后者会让"用这套参数再跑"打开一个 schema 都不一样的表单。
    let mut runs: Vec<Value> = Vec::with_capacity(persisted.runs.len());
    for run in &persisted.runs {
        let revision_id = run.recipe_revision_id.0;
        // 用 `get` 而不是 `contains_key`：后者 + `insert` 是 clippy 的 `map_entry` 告警，
        // 而这里**不能用 entry API** —— 查修订要 `await`，entry 会把借用跨过 await 点。
        let recipe_id = if let Some(cached) = recipe_of_revision.get(&revision_id) {
            *cached
        } else {
            let resolved = match state.recipes.find_revision(run.recipe_revision_id).await {
                Ok(Some(revision)) => Some(revision.recipe_id.0),
                _ => None,
            };
            recipe_of_revision.insert(revision_id, resolved);
            resolved
        };
        let connection = match connection_of_revision.get(&run.connection_revision_id.0) {
            Some(cached) => cached.clone(),
            None => {
                let resolved = resolve_connection_label(&state.connections, run.connection_revision_id).await;
                connection_of_revision.insert(run.connection_revision_id.0, resolved.clone());
                resolved
            }
        };
        runs.push(run_response(run, recipe_id, connection));
    }
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "operationId": operation.id.0,
            "status": operation.status.as_str(),
            "actorId": operation.actor.id,
            "origin": operation.origin,
            "idempotencyKey": operation.idempotency_key.0,
            "requestHash": operation.request_hash,
            "metadata": operation.metadata,
            "createdAt": operation.created_at.0,
            "updatedAt": operation.updated_at.0,
            "recipeName": recipe_name,
            // operation 级：挑"最该被看见"的那条（不确定 > 退避 > 提交中 > 排队）。
            // 调用方不该为了知道"这次在等什么"去遍历 runs —— 那正是这次事故里缺的那一步。
            "attention": crate::v2::domain::operation::operation_attention(&persisted.runs),
            "runs": runs,
        })))
        .unwrap(),
    ))
}

pub async fn list_operations(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Query(query): Query<PaginationQuery>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsRead)?;
    let limit = query.limit.unwrap_or(50).min(200);
    let summaries = state
        .operations
        .list_operations(limit + 1, query.after.map(OperationId))
        .await
        .map_err(domain_error)?;
    let has_more = summaries.len() > limit;
    let visible = summaries.into_iter().take(limit).collect::<Vec<_>>();
    let next_cursor = visible.last().map(|summary| summary.id.0);
    Ok(Json(
        serde_json::to_value(Envelope::new(Page {
            items: visible
                .iter()
                .map(|summary| {
                    json!({
                        "operationId": summary.id.0,
                        "status": summary.status.as_str(),
                        "runCount": summary.run_count,
                        "createdAt": summary.created_at.0,
                        // 「为什么没在动」的摘要版：只有原因码与死线，**没有**那句人话与
                        // nextActions（一页 50 条不该带上 50 份散文）。要读全的按 id 调
                        // `operation get` —— 那里的 `attention` 是同一个判据的完整形态。
                        //
                        // 为什么摘要必须带上它：调用方先 list 再 get。只有 get 有，等于
                        // 逼它为每一条各打一次 get 才知道该看哪一个，而这次事故的代价
                        // 正是"没人主动去看"（见 domain::operation::run_attention）。
                        "attention": summary.attention_reason.as_ref().map(|reason| {
                            json!({
                                "reason": reason,
                                "deadline": summary
                                    .attention_deadline
                                    .as_ref()
                                    .map(|deadline| deadline.0.clone()),
                            })
                        }),
                    })
                })
                .collect::<Vec<_>>(),
            next_cursor,
            has_more,
        }))
        .unwrap(),
    ))
}

/// `GET /api/v1/operations/{id}/manifest` —— 这一批的**交付清单**。
///
/// 为什么单独一条：下游（脚本 / agent）要回答"这批跑出了什么、花在哪、文件叫什么、
/// 从哪取字节"。现在得跨三个接口拼（任务 → run → 产物），每条 run 还得再发一次分页请求。
/// 一份清单一次给全，顺序与提交顺序一致。
///
/// 两条出口都给出，各管一段：
/// * `remoteUrl` —— 平台给的原始地址（RunningHub 那种"只给链接、不给字节"的产物才有）。
///   下游拿去转存或上 CDN 最省事，不必先绕一圈本机；但它**会过期**，是"此刻能取"而不是
///   "永久交付地址"，别把它当长期入口存下来。
/// * `bytesPath`（带 token 取字节，必要时回源并落本地缓存）/ `savePath`（落盘到下载目录）
///   —— 稳定出口：平台那条链接烂了，取过一次的也还能取。
///
/// `byteSize == 0` 表示字节还没落本地（"在平台上"），与 README「产物怎么拿到手」同一口径。
pub async fn operation_manifest(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(operation_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsRead)?;
    let persisted = state
        .operations
        .find_operation(OperationId(operation_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("OPERATION_NOT_FOUND", "Operation 不存在"))?;
    let operation = &persisted.operation;

    let mut connection_of_revision: std::collections::HashMap<u64, Option<(u64, String)>> =
        std::collections::HashMap::new();
    let mut runs = Vec::with_capacity(persisted.runs.len());
    let mut artifact_count = 0usize;
    for run in &persisted.runs {
        let connection = match connection_of_revision.get(&run.connection_revision_id.0) {
            Some(cached) => cached.clone(),
            None => {
                let resolved =
                    resolve_connection_label(&state.connections, run.connection_revision_id).await;
                connection_of_revision.insert(run.connection_revision_id.0, resolved.clone());
                resolved
            }
        };
        let artifacts = state
            .artifacts_repo
            .list_by_run(run.id)
            .await
            .map_err(domain_error)?;
        artifact_count += artifacts.len();
        runs.push(json!({
            "runId": run.id.0,
            // 提交时给的名字（**只有调用方自己起的才算**；自动补的 run-N 不是名字，给 null）。
            // 它是"给人对号"的标签，不是身份——身份是 runId / artifactId。
            "label": run.display_label(),
            "sequence": run.sequence,
            "status": run.status.as_str(),
            "connectionId": connection.as_ref().map(|(id, _)| *id),
            "connectionName": connection.as_ref().map(|(_, name)| name.clone()),
            "usage": run.usage,
            "inputs": run.inputs,
            "error": run.error,
            // 这条 run 落库的时刻（epoch 毫秒）。下游要算"这批跑了多久"，起点在这里。
            // **不给"完成时刻"**：库里没有这个事实（`updatedAt` 会被轮询刷成"最后一次写"），
            // 与其编一个，不如让调用方在收到回调时自己盖个戳——那本来也是它该记的账。
            "createdAt": run.created_at.0,
            "artifacts": artifacts
                .iter()
                .map(|artifact| json!({
                    "artifactId": artifact.id.0,
                    "filename": artifact.filename,
                    "mimeType": artifact.mime_type,
                    "byteSize": artifact.byte_size,
                    "checksum": artifact.checksum,
                    // 平台原始链接：能直接取，但会过期（见上面那段说明）。
                    "remoteUrl": artifact.url,
                    // 这条产物**落库的时刻**（epoch 毫秒）。平台链接的年龄从这里算——
                    // 我们不知道平台的 TTL（实测口径一般是两周左右，但那是经验值，
                    // 不是接口给的），所以给数据不给结论：调用方自己盯剩余寿命。
                    "createdAt": artifact.created_at.0,
                    "bytesPath": format!("/api/v1/artifacts/{}/bytes", artifact.id.0),
                    "savePath": format!("/api/v1/artifacts/{}/save", artifact.id.0),
                }))
                .collect::<Vec<_>>(),
        }));
    }

    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "operationId": operation.id.0,
            "status": operation.status.as_str(),
            "createdAt": operation.created_at.0,
            "runCount": runs.len(),
            "artifactCount": artifact_count,
            "runs": runs,
        })))
        .unwrap(),
    ))
}

/// 这条连接**当前修订**是否被冻结（换过 key 的旧冻结自然不生效）。
fn paused_reason_of(state: &V2HttpState, current_revision_id: u64) -> Option<String> {
    state
        .paused
        .snapshot()
        .into_iter()
        .find(|(revision_id, _)| *revision_id == current_revision_id)
        .map(|(_, reason)| reason)
}

/// connection revision → (connectionId, 账号名)。查不到就是 `None`（不编造名字）。
async fn resolve_connection_label(
    connections: &Arc<dyn crate::v2::ports::repositories::ConnectionRepository>,
    revision_id: crate::v2::domain::ids::ConnectionRevisionId,
) -> Option<(u64, String)> {
    let revision = connections.find_revision(revision_id).await.ok().flatten()?;
    let connection = connections
        .find_connection(revision.connection_id)
        .await
        .ok()
        .flatten()?;
    Some((connection.id.0, connection.display_name))
}

fn run_response(
    run: &crate::v2::domain::operation::Run,
    recipe_id: Option<u64>,
    connection: Option<(u64, String)>,
) -> Value {
    json!({
        "runId": run.id.0,
        "operationId": run.operation_id.0,
        "clientRunId": run.client_run_id,
        "sequence": run.sequence,
        "recipeRevisionId": run.recipe_revision_id.0,
        "connectionRevisionId": run.connection_revision_id.0,
        // 这条 run 落在**哪个账号**上：账号池里同一个批次会分散到多条连接，
        // 没有这两个字段，出了账（钱花在哪）和排障（哪个 key 坏了）都无从下手。
        "connectionId": connection.as_ref().map(|(id, _)| *id),
        "connectionName": connection.as_ref().map(|(_, name)| name.clone()),
        // 这条 run 属于**哪个应用**（由它自己的修订反查，`null` = 应用已被删/查不到）。
        //
        // 为什么要有：界面要"**用这套参数再跑一次**"（跳到那个应用的表单并预填），
        // 而路由是 `/library/:recipeId` —— 只有 `recipeRevisionId` 是跳不过去的。
        // 不在这里让界面自己反查：那要它去比对"哪个应用的当前修订等于这个修订"，
        // 应用一旦重新导入过就**对不上**，而对不上时最坏的结果是**默默用错 schema 提交**。
        "recipeId": recipe_id,
        "status": run.status.as_str(),
        "trackingState": run.tracking_state.as_str(),
        // 提交时用的**输入**（`inputs`）。
        //
        // ⚠️ 这个字段是 2026-10-18 **补上**的，不是一直都有 —— 而在此之前，服务出去的文案已经
        // 承诺过它：`renderloft_get_operation` 的描述写着"每条 run 都带提交时的 `inputs`"
        // （cookbook §11 的恢复建议也建立在它之上：**读 `inputs` 与 `error` 一起读，改对了再重投**）。
        // 也就是说：调用方照文案去做，会**找不到** `inputs`，而它正处在"要不要再花一次钱"的判断里。
        // 这是本项目一直在消灭的那类问题——**接口说了它没做的事**。实测发现（给预览框做
        // "结果按输入分组"时，发现界面拿不到 `inputs`，回头一查是这里从来没发）。
        //
        // 只加在**详情**（与 `GET /api/v1/runs/{id}`）上：列表接口按设计只回摘要
        // （`runCount` + `attention`），**不带** runs、更不带 inputs —— 那条边界没动。
        "inputs": run.inputs,
        // 提交时用的**素材**，形状与 `POST /operations/validate` 的解析结果一致
        // （`[{ fieldKey, assetId, checksum }]`）：同一个概念在两条读路径上只有一种形状。
        //
        // ⚠️ `fieldKey` 为空的那些是**旧的位置编码**（`AssetRef::field_key == None`）：
        // 它们**不能**被当成字段键用（见 `MediaMapping::bind` 的第一性原理：资产的真实身份是
        // 「(revision, 字段键)」，不是数组下标）。界面据此**跳过**它们并让用户重新选择，
        // 而不是猜一个字段键——猜错就是"把钱花在错素材上"。
        "assets": run.assets.iter().map(|asset| json!({
            "fieldKey": asset.field_key,
            "assetId": asset.asset_id.0,
            "checksum": asset.checksum,
        })).collect::<Vec<_>>(),
        // 「它为什么没在动」：非终态里"在等什么"的那些给出原因 + 死线 + 下一动作
        // （见 `domain::operation::run_attention`）。正常在跑的与终态是 null。
        "attention": crate::v2::domain::operation::run_attention(run),
        // 平台报的用量（`null` = 平台没说，不是 0）。
        "usage": run.usage,
        "error": run.error,
        "createdAt": run.created_at.0,
        "updatedAt": run.updated_at.0,
    })
}

pub async fn get_run(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(run_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsRead)?;
    let run_id = RunId(run_id);
    let mut found = None;
    // 通过最新 operation 列表查找 run（Phase 5 增加按 run 索引的查询）
    let mut cursor: Option<u64> = Some(1);
    for _ in 0..1000 {
        let Some(operation_id) = cursor else { break };
        let Some(persisted) = state
            .operations
            .find_operation(OperationId(operation_id))
            .await
            .map_err(domain_error)?
        else {
            break;
        };
        if let Some(run) = persisted.runs.iter().find(|run| run.id == run_id) {
            found = Some(run.clone());
            break;
        }
        cursor = Some(operation_id + 1);
    }
    let run = found.ok_or_else(|| error::not_found("RUN_NOT_FOUND", "Run 不存在"))?;
    // 与详情同一条：这条 run 属于哪个应用（查不到就是 `null`，界面据此**不做**"用这套参数再跑"
    // —— 宁可不给那个按钮，也不要跳到一个 schema 都不一样的表单里去）。
    let recipe_id = match state.recipes.find_revision(run.recipe_revision_id).await {
        Ok(Some(revision)) => Some(revision.recipe_id.0),
        _ => None,
    };
    Ok(Json(
        serde_json::to_value(Envelope::new(run_response(
            &run,
            recipe_id,
            resolve_connection_label(&state.connections, run.connection_revision_id).await,
        )))
        .unwrap(),
    ))
}

pub async fn cancel_operation(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(operation_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsCancel)?;
    let result = state
        .cancel_operation
        .execute(OperationId(operation_id))
        .await
        .map_err(domain_error)?;
    Ok(Json(serde_json::to_value(Envelope::new(json!({
        "operationId": result.operation_id.0,
        "cancelled": result.cancelled.iter().map(|id| id.0).collect::<Vec<_>>(),
        "alreadyTerminal": result.already_terminal.iter().map(|id| id.0).collect::<Vec<_>>(),
        "pending": result.pending.iter().map(|id| id.0).collect::<Vec<_>>(),
        "remoteMayContinue": result.remote_may_continue.iter().map(|id| id.0).collect::<Vec<_>>(),
    })))
    .unwrap()))
}

/// 失败/中断操作一键重试：重建一个新 operation（复用原参数）。
pub async fn retry_operation(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(operation_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsExecute)?;
    retry_operation_impl(state, actor, OperationId(operation_id), None).await
}

pub async fn retry_run(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path((operation_id, run_id)): Path<(u64, u64)>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsExecute)?;
    retry_operation_impl(state, actor, OperationId(operation_id), Some(RunId(run_id))).await
}

async fn retry_operation_impl(
    state: Arc<V2HttpState>,
    actor: ActorRef,
    operation_id: OperationId,
    run_id: Option<RunId>,
) -> HttpResult {
    let service = RetryOperation {
        create: state.create_operation.clone(),
        repositories: state.operations.clone(),
    };
    let new_id = match run_id {
        Some(run_id) => {
            service
                .execute_run(actor, operation_id, run_id, "http")
                .await
        }
        None => service.execute(actor, operation_id, "http").await,
    }
    .map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "operationId": new_id.0 }))).unwrap(),
    ))
}

/// 删除单条操作历史（级联清理其 run/事件/产物）。
pub async fn delete_operation(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(operation_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::OperationsDelete)?;
    let deleted = state
        .operations
        .delete_operations(&[OperationId(operation_id)])
        .await
        .map_err(domain_error)?;
    if deleted == 0 {
        return Err(error::not_found("OPERATION_NOT_FOUND", "Operation 不存在"));
    }
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "deleted": true }))).unwrap(),
    ))
}

// ---------- events ----------

pub async fn operation_events(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(operation_id): Path<u64>,
    Query(query): Query<PaginationQuery>,
) -> axum::response::Response {
    if require_scope(&actor, Scope::OperationsRead).is_err() {
        return error::forbidden("缺少 operations:read 权限");
    }
    let operation_id = OperationId(operation_id);
    // ?stream=1 → SSE 长连接；否则保持光标查询（向后兼容）
    if query.stream.unwrap_or(false) {
        return sse_events(state, operation_id, query.after.unwrap_or(0)).await;
    }
    let events = match state
        .events
        .list_after(
            operation_id,
            query.after.unwrap_or(0),
            query.limit.unwrap_or(200).min(1000),
        )
        .await
    {
        Ok(events) => events,
        Err(error) => return error::error_response(error),
    };
    let dtos: Vec<EventResponseDto> = events
        .into_iter()
        .map(|event| EventResponseDto {
            event_id: event.id,
            event_type: event.event_type,
            operation_id: event.operation_id.0,
            run_id: event.run_id.map(|id| id.0),
            sequence: event.sequence,
            created_at: event.created_at.0,
            payload: event.safe_payload,
        })
        .collect();
    axum::Json(serde_json::to_value(Envelope::new(dtos)).unwrap()).into_response()
}

/// SSE 长连接：按光标轮询事件，空闲发 comment 保活，60 秒后关闭。
async fn sse_events(
    state: Arc<V2HttpState>,
    operation_id: OperationId,
    after: u64,
) -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures::stream;

    let stream = stream::unfold(
        (state, operation_id, after, 0u64),
        |(state, operation_id, mut after, mut idle)| async move {
            if idle >= 30 {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match state.events.list_after(operation_id, after, 50).await {
                Ok(events) if !events.is_empty() => {
                    let last_id = events.last().map(|event| event.id).unwrap_or(after);
                    let mut frame = String::new();
                    for event in events {
                        let dto = EventResponseDto {
                            event_id: event.id,
                            event_type: event.event_type,
                            operation_id: event.operation_id.0,
                            run_id: event.run_id.map(|id| id.0),
                            sequence: event.sequence,
                            created_at: event.created_at.0,
                            payload: event.safe_payload,
                        };
                        frame.push_str(&format!(
                            "event: {}\nid: {}\ndata: {}\n\n",
                            dto.event_type,
                            dto.event_id,
                            serde_json::to_string(&dto).unwrap_or_default()
                        ));
                    }
                    after = last_id;
                    idle = 0;
                    Some((
                        Ok::<_, std::convert::Infallible>(Event::default().data(frame)),
                        (state, operation_id, after, idle),
                    ))
                }
                _ => {
                    idle += 1;
                    Some((
                        Ok::<_, std::convert::Infallible>(Event::default().comment("keep-alive")),
                        (state, operation_id, after, idle),
                    ))
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::new())
        .into_response()
}

// ---------- assets ----------

/// `POST /api/v1/assets` 的 query：文件名与 MIME。
///
/// ⚠️ 它们**不在 body 里**，因为 body 就是文件的原始字节。此前这条路收的是 JSON
/// （`{"filename":…,"bytes":[1,2,3,4]}`），4 个字节的测试全绿，而真实图片会膨胀成十倍的
/// JSON 文本、撞上默认 2 MB 的请求体上限，报出来的是 `INVALID_REQUEST_BODY`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetUploadQuery {
    pub filename: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

pub async fn create_asset(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Query(query): Query<AssetUploadQuery>,
    body: Bytes,
) -> HttpResult {
    require_scope(&actor, Scope::AssetsWrite)?;
    if body.is_empty() {
        return Err(error::validation(
            "BYTES_REQUIRED",
            "请求体不能为空（body 就是文件字节）",
        ));
    }
    let request = DomainCreateAssetRequest {
        actor,
        filename: query
            .filename
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| error::validation("FILENAME_REQUIRED", "filename 不能为空"))?,
        mime_type: query
            .mime_type
            .unwrap_or_else(|| "application/octet-stream".to_string()),
        bytes: body.to_vec(),
    };
    let use_case = CreateAsset::new(AssetDependencies {
        assets: state.assets.clone(),
    });
    let checksum = format!(
        "sha1-{:x}",
        crate::v2::domain::hash::fnv1a_checksum(&request.bytes)
    );
    let id = use_case.execute(request).await.map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "assetId": id.0,
            "checksum": checksum,
        })))
        .unwrap(),
    ))
}

pub async fn get_asset(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(asset_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::AssetsLocalRead)?;
    let asset = state
        .assets
        .find_asset(AssetId(asset_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("ASSET_NOT_FOUND", "Asset 不存在"))?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "assetId": asset_id,
            "filename": asset.filename,
            "mimeType": asset.mime_type,
            "byteSize": asset.byte_size,
            "checksum": asset.checksum,
        })))
        .unwrap(),
    ))
}

/// 取资产（上传素材）的**原始字节**。与 `/artifacts/{id}/bytes` 对称。
///
/// 此前资产只有元数据端点（`GET /assets/{id}` 只回 filename/mimeType/byteSize/checksum），
/// 前端因此**拿不到图片字节**——历史附图就算把 asset id 存下来了也渲染不出来。
/// 组装 `Content-Disposition`：**ASCII 回退 + RFC 5987 的 UTF-8 变体**。
///
/// ⚠️ 只写 `filename="…"` 的后果（2026-10-12 实测：下载了但名字是乱码）：
/// 这个参数按 **ISO-8859-1** 解析，中文名会被逐字节拆开——
/// `Krea2-turbo 工作流-4000.png` 落盘成 `Krea2-turbo å·¥ä½_æµ_-4000.png`。
/// 现在两份都给：现代客户端认 `filename*=UTF-8''…`（RFC 5987 的 attr-char 之外一律百分号编码），
/// 老客户端认前面那份 ASCII 回退（非 ASCII 一律换成 `_`）。
fn content_disposition(filename: &str) -> String {
    let cleaned: String = filename.replace(['"', '\n', '\r'], "");
    let fallback: String = cleaned
        .chars()
        .map(|character| {
            if character.is_ascii() && character != '\\' {
                character
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = cleaned
        .bytes()
        .map(|byte| {
            // RFC 5987 的 attr-char：字母数字与 !#$&+-.^_`|~
            if byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'&'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
            {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

pub async fn get_asset_bytes(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(asset_id): Path<u64>,
) -> axum::response::Response {
    if require_scope(&actor, Scope::AssetsLocalRead).is_err() {
        return error::forbidden("缺少 assets:local-read 权限");
    }
    let id = AssetId(asset_id);
    let asset = match state.assets.find_asset(id).await {
        Ok(Some(asset)) => asset,
        Ok(None) => return error::not_found("ASSET_NOT_FOUND", "Asset 不存在"),
        Err(error) => return error::error_response(error),
    };
    let bytes = match state.assets.read_bytes(id).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return error::not_found("ASSET_BYTES_MISSING", "资产字节不可用"),
        Err(error) => return error::error_response(error),
    };
    // 文件名里的引号/换行会破坏 Content-Disposition 头，与 artifacts 一致的清洗。
    let filename = content_disposition(&asset.filename);
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, asset.mime_type)
        .header(axum::http::header::CONTENT_DISPOSITION, filename)
        .header(axum::http::header::CONTENT_LENGTH, bytes.len().to_string())
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| {
            error::error_response(crate::v2::domain::errors::SafeError::internal(
                "无法生成 Asset 响应",
            ))
        })
}

/// 生成一份**一致的**工作区快照（迁移 / 备份）。逻辑见 `application/workspace_backup.rs`。
///
/// **恢复没有对应的端点**：`daemon stop` → 用备份替换工作区目录 → 任意一条 CLI 命令会把 daemon
/// 拉起来。这里只负责"导出一份干净的"；密钥永远不在这份快照里（它们在系统凭据库）。
pub async fn backup_workspace(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<Value>,
) -> HttpResult {
    require_scope(&actor, Scope::SettingsManage)?;
    let target = body.get("to").and_then(Value::as_str).unwrap_or_default();
    let report = state
        .workspace_backup
        .backup(target)
        .await
        .map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(serde_json::to_value(report).unwrap())).unwrap(),
    ))
}

// ---------- artifacts ----------

pub async fn list_artifacts(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Query(query): Query<PaginationQuery>,
) -> HttpResult {
    require_scope(&actor, Scope::ArtifactsRead)?;
    let limit = query.limit.unwrap_or(50).min(200);
    // 方向与游标必须**配得上**，配不上就明确报错，不许默默挑一个：
    // 挑错了会给出"看起来对、其实是最旧的一批"的结果，而调用方不会发现。
    let newest_first = match query.order.as_deref() {
        Some("desc") => true,
        Some("asc") | None => false,
        Some(other) => {
            return Err(error::validation(
                "ORDER_INVALID",
                format!("`order` 只认 `asc` / `desc`，收到：{other}"),
            ))
        }
    };
    if newest_first && query.after.is_some() {
        return Err(error::validation(
            "CURSOR_CONFLICT",
            "`order=desc`（最新在前）配的是 `before=<id>`（比它更旧）；`after` 是升序方向的游标",
        ));
    }
    if !newest_first && query.before.is_some() {
        return Err(error::validation(
            "CURSOR_CONFLICT",
            "`before` 只在 `order=desc`（最新在前）下有意义；升序方向用 `after`",
        ));
    }
    let cursor = match (newest_first, query.before) {
        (true, before) => ArtifactCursor::Before(before.map(crate::v2::domain::ids::ArtifactId)),
        (false, _) => ArtifactCursor::After(query.after.map(crate::v2::domain::ids::ArtifactId)),
    };
    // ?runId= 过滤（按 Run 查询产物）；否则分页列表（方向见 `cursor`）。
    let summaries = match query.run_id {
        Some(run_id) => state
            .artifacts_repo
            .list_by_run(crate::v2::domain::ids::RunId(run_id))
            .await
            .map_err(domain_error)?,
        None => state
            .artifacts_repo
            .list(limit + 1, cursor)
            .await
            .map_err(domain_error)?,
    };
    let has_more = query.run_id.is_none() && summaries.len() > limit;
    let visible = summaries
        .into_iter()
        .take(if query.run_id.is_none() {
            limit
        } else {
            usize::MAX
        })
        .collect::<Vec<_>>();
    let next_cursor = if query.run_id.is_none() {
        visible.last().map(|summary| summary.id.0)
    } else {
        None
    };
    Ok(Json(
        serde_json::to_value(Envelope::new(Page::<Value> {
            items: visible
                .iter()
                .map(|summary| {
                    json!({
                        "artifactId": summary.id.0,
                        "runId": summary.run_id.0,
                        "filename": summary.filename,
                        "mimeType": summary.mime_type,
                        "byteSize": summary.byte_size,
                        "checksum": summary.checksum,
                        // 平台给的原始地址（可能为 null：本地字节型产物没有它）。
                        // ⚠️ 会过期，别当永久交付地址；稳定出口是 /artifacts/{id}/bytes。
                        "remoteUrl": summary.url,
                        // 落库时刻（epoch 毫秒）：链接年龄的起点，见 manifest 的同名字段。
                        "createdAt": summary.created_at.0,
                    })
                })
                .collect::<Vec<_>>(),
            next_cursor,
            has_more,
        }))
        .unwrap(),
    ))
}

/// 把产物**落盘**到「下载目录」设置里那个目录，返回完整路径。
///
/// 为什么由 daemon 写、而不是让外壳/浏览器去下载（2026-10-12 定：点了下载没反应）：
/// 桌面外壳是 WebView2，它**不弹任何下载提示、落点也不可知**——文件其实进了系统下载目录，
/// 而界面上什么都没发生；我们连"到底存哪了"都说不出来，只能说一句含糊的"系统下载目录"。
/// 本机 daemon 写盘是本地优先应用的主流做法（CLI 的 `artifact download` 一直这么干），
/// 它**知道确切路径**，界面才能如实显示路径并给出"在文件夹中显示"。
///
/// 重名按浏览器口径递增（`名字 (1).png`）——覆盖用户已有的文件是更坏的默认。
pub async fn save_artifact(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(artifact_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::ArtifactsRead)?;
    let id = crate::v2::domain::ids::ArtifactId(artifact_id);
    let artifact = state
        .artifacts
        .load(id)
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("ARTIFACT_NOT_FOUND", "Artifact 不存在"))?;
    // 与 /bytes 共用同一条取字节的路：本地缓存优先，回源命中后就地落盘。
    let bytes = artifact_bytes(&state, &artifact)
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("ARTIFACT_BYTES_MISSING", "产物字节不可用"))?;
    let settings = state.settings.view().await.map_err(domain_error)?;
    let directory = if settings.download_dir.trim().is_empty() {
        crate::v2::application::settings::default_download_dir()
    } else {
        settings.download_dir
    };
    let path = write_download(&directory, &artifact.filename, &bytes).map_err(|message| {
        error::error_response(crate::v2::domain::errors::SafeError::storage(message))
    })?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "path": path.to_string_lossy(),
            "filename": path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default(),
            "directory": directory,
        })))
        .unwrap(),
    ))
}

/// 写文件，重名时按浏览器口径递增（`名字 (1).png`）。
///
/// 单独抽出来是为了**能被单测直接钉住**：这条路径上没有 HTTP，只有一个"别覆盖用户文件"的约定。
fn write_download(
    directory: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf, String> {
    // 文件名里的路径分隔符要清掉：产物名来自平台，别让它跳出下载目录。
    let safe_name = filename
        .replace(['/', '\\'], "_")
        .replace(['"', '\n', '\r'], "");
    let safe_name = if safe_name.trim().is_empty() {
        "artifact".to_string()
    } else {
        safe_name
    };
    let directory = std::path::Path::new(directory);
    std::fs::create_dir_all(directory).map_err(|_| "无法创建下载目录".to_string())?;
    let (stem, extension) = match safe_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            (stem.to_string(), format!(".{extension}"))
        }
        _ => (safe_name.clone(), String::new()),
    };
    let mut candidate = directory.join(&safe_name);
    let mut index = 1;
    while candidate.exists() {
        candidate = directory.join(format!("{stem} ({index}){extension}"));
        index += 1;
        if index > 9999 {
            return Err("下载目录里同名文件太多".to_string());
        }
    }
    std::fs::write(&candidate, bytes).map_err(|_| "无法写入下载目录".to_string())?;
    Ok(candidate)
}

pub async fn get_artifact_bytes(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(artifact_id): Path<u64>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if require_scope(&actor, Scope::ArtifactsRead).is_err() {
        return error::forbidden("缺少 artifacts:read 权限");
    }
    let id = crate::v2::domain::ids::ArtifactId(artifact_id);
    let artifact = match state.artifacts.load(id).await {
        Ok(Some(artifact)) => artifact,
        Ok(None) => return error::not_found("ARTIFACT_NOT_FOUND", "Artifact 不存在"),
        Err(error) => return error::error_response(error),
    };
    let bytes = match artifact_bytes(&state, &artifact).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return error::not_found("ARTIFACT_BYTES_MISSING", "产物字节不可用"),
        Err(error) => return error::error_response(error),
    };
    let etag = artifact_etag(&artifact);
    // 同一个 artifact id 的内容是不变的（名字在落库前就定了，字节也不再变），
    // 所以可以给一个永不过期的强校验器：界面反复画缩略图时 WebView 直接命中本地缓存。
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
    {
        return axum::response::Response::builder()
            .status(axum::http::StatusCode::NOT_MODIFIED)
            .header(axum::http::header::ETAG, &etag)
            .header(axum::http::header::CACHE_CONTROL, ARTIFACT_CACHE_CONTROL)
            .header(axum::http::header::VARY, "Authorization")
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| error::error_response(
                crate::v2::domain::errors::SafeError::internal("无法生成 Artifact 响应"),
            ));
    }
    let filename = content_disposition(&artifact.filename);
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, artifact.mime_type)
        .header(axum::http::header::CONTENT_DISPOSITION, filename)
        .header(axum::http::header::CONTENT_LENGTH, bytes.len().to_string())
        .header(axum::http::header::ETAG, etag)
        .header(axum::http::header::CACHE_CONTROL, ARTIFACT_CACHE_CONTROL)
        .header(axum::http::header::VARY, "Authorization")
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| {
            error::error_response(crate::v2::domain::errors::SafeError::internal(
                "无法生成 Artifact 响应",
            ))
        })
}

/// 产物字节的缓存策略：内容按 id 不可变，所以给一年 + `immutable`。
///
/// `private` 是因为产物只属于这台机器（本机 daemon 的字节，不该被任何共享缓存留存）；
/// 加 `Vary: Authorization` 是因为同一个 URL 在换令牌前后代表同一份内容——不声明的话
/// 中间层可能按"令牌不同"缓存两份。
const ARTIFACT_CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// 强校验器：有 checksum 用它（内容地址），没有就用 id（同一 id 的字节不会变）。
///
/// checksum 来自驱动：正常是十六进制摘要，但它是**自由字符串**——含 `"` 或控制字符时
/// 当 ETag 会生成一个畸形头，所以那种情况退回 id，不冒险拼一个非法头。
pub(crate) fn artifact_etag(artifact: &crate::v2::ports::stores::ArtifactObject) -> String {
    let checksum = artifact.checksum.trim();
    let usable =
        !checksum.is_empty() && !checksum.chars().any(|c| c == '"' || c == '\\' || c.is_control());
    if usable {
        format!("\"{checksum}\"")
    } else {
        format!("\"artifact-{}\"", artifact.id.0)
    }
}

/// 取一条产物的字节：**本地缓存优先**，没有才回源；回源成功后就地落盘。
///
/// 为什么把顺序反过来（2026-10-26）：`url` 型产物（RunningHub 只给 URL 不给字节）此前
/// 每次请求都回源——界面每画一次缩略图、每次重启都重下一遍，而本地缓存目录一直都在，
/// 只是**从来没有人往里写**。落盘之后 `read_bytes` 直接命中；顺带的好处是平台那条链接
/// 过期之后，已经看过的图也不会忽然打不开。
///
/// 返回 `Ok(None)` 表示"既没有缓存、也没有 URL"——那是 404，不是错误。
async fn artifact_bytes(
    state: &V2HttpState,
    artifact: &crate::v2::ports::stores::ArtifactObject,
) -> Result<Option<Vec<u8>>, crate::v2::domain::errors::SafeError> {
    if let Some(bytes) = state.artifacts.read_bytes(artifact.id).await? {
        return Ok(Some(bytes));
    }
    let Some(url) = artifact.url.as_deref() else {
        return Ok(None);
    };
    let bytes = fetch_artifact_remote(state.safe_http.as_ref(), url).await?;
    // 缓存写失败**不该**让一次预览变成错误：字节已经在手上，用户要的是图。
    // 但要留一行日志——"下次还得再下一遍"这件事，出问题时得能看出来。
    if let Err(error) = state.artifacts.cache_bytes(artifact.id, &bytes).await {
        crate::v2::logging::log_failure(
            "artifact",
            &format!("id={}", artifact.id.0),
            "ARTIFACT_CACHE_WRITE_FAILED",
            &error.message,
        );
    }
    Ok(Some(bytes))
}

/// 按远端结果 URL 拉取产物字节（SSRF 与重定向校验由 `SafeHttpClient` 实现侧负责）。
async fn fetch_artifact_remote(
    client: &dyn SafeHttpClient,
    url: &str,
) -> Result<Vec<u8>, crate::v2::domain::errors::SafeError> {
    client.fetch_limited(url, client.max_result_bytes()).await
}

pub async fn get_artifact(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(artifact_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::ArtifactsRead)?;
    let artifact = state
        .artifacts
        .load(crate::v2::domain::ids::ArtifactId(artifact_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("ARTIFACT_NOT_FOUND", "Artifact 不存在"))?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "artifactId": artifact.id.0,
            "filename": artifact.filename,
            "mimeType": artifact.mime_type,
            "byteSize": artifact.byte_size,
            "checksum": artifact.checksum,
            // 平台原始地址（RunningHub 那种"只给链接"的产物才有）；会过期，见 manifest 的说明。
            "remoteUrl": artifact.url,
        })))
        .unwrap(),
    ))
}

// ---------- connections ----------

fn connection_summary(
    connection: &crate::v2::domain::model::Connection,
    paused_reason: Option<&str>,
) -> Value {
    json!({
        "connectionId": connection.id.0,
        "vendor": connection.vendor,
        "protocol": connection.protocol,
        "displayName": connection.display_name,
        "currentRevisionId": connection.current_revision_id.0,
        "status": connection.status,
        // 被冻结时非空：key 失效 / 余额不足，调度器暂时不让它接新单。
        // 界面必须能看见——否则用户只会看到"任务莫名不动"。
        "pausedReason": paused_reason,
    })
}

pub async fn list_connections(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsRead)?;
    let connections = state
        .connections
        .list_connections()
        .await
        .map_err(domain_error)?;
    // 冻结是按 connection revision 记的：拿当前修订去对，换过 key 的旧冻结自然不生效。
    let paused: std::collections::HashMap<u64, String> =
        state.paused.snapshot().into_iter().collect();
    Ok(Json(
        serde_json::to_value(Envelope::new(
            connections
                .iter()
                .map(|connection| {
                    connection_summary(
                        connection,
                        paused
                            .get(&connection.current_revision_id.0)
                            .map(String::as_str),
                    )
                })
                .collect::<Vec<_>>(),
        ))
        .unwrap(),
    ))
}

pub async fn get_connection(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(connection_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsRead)?;
    let connection = state
        .connections
        .find_connection(ConnectionId(connection_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
    let revision = state
        .connections
        .find_revision(connection.current_revision_id)
        .await
        .map_err(domain_error)?;
    // 密钥的**身份**要给界面，密钥的**值**不给。
    //
    // 用户需要确认"我填的是哪把"（两个站点两把 Key 是常态），但把完整密钥发到前端等于拆掉
    // 「密钥不出凭据库」这条边界——而且每一次回显都是一次泄漏面（截图、录屏、剪贴板、扩展）。
    // 所以这里只回一个**派生**的指纹：尾 4 位（跟 GitHub / 云控制台一个口径，推不出全量）。
    //
    // 凭据引用：指纹取的是**这条连接实际会用的那把**（借来的也算），否则界面会对一条
    // 明明能跑的连接显示"没有密钥"。解析不了（引用的连接被删/停用）就退回 `saved: false`
    // ——这里**不能**报错：用户正是要靠这个抽屉去修那个坏引用，详情打不开就没法修了。
    // "引用坏了"的明确报错由运行路径给（提交/对账/测试）。
    //
    // ⚠️ 因此下面返回的 `secretBindings` 与 `secretHint` 都是**有效**的（自己的或借来的），
    // 而不是"这条连接自己存了哪些槽位"。两者的区别由 `config.sourceConnectionId` 回答。
    // 为什么选"有效"而不是"原始"：所有读它的地方问的都是同一个问题——"这条连接能认证吗"。
    // 回原始的会让前端、CLI 各自再实现一遍引用解析（而它们连被引用连接是谁都要再查一次）。
    let revision = match revision {
        Some(revision) => crate::v2::application::credential_reference::resolve(
            state.connections.as_ref(),
            revision,
        )
        .await
        .ok(),
        other => other,
    };
    let secret_hint = match revision.as_ref().and_then(|revision| {
        revision
            .secret_bindings
            .iter()
            .find(|binding| crate::v2::domain::model::is_api_key_slot(&binding.slot))
    }) {
        Some(binding) => state
            .secrets
            .resolve(binding)
            .await
            .ok()
            .flatten()
            .map(|secret| {
                let value = secret.secret;
                let tail: String = value
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                json!({ "saved": true, "tail": tail, "length": value.chars().count() })
            })
            .unwrap_or_else(|| json!({ "saved": false })),
        None => json!({ "saved": false }),
    };
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "connection": connection_summary(
                &connection,
                paused_reason_of(&state, connection.current_revision_id.0).as_deref(),
            ),
            "config": revision.as_ref().map(|revision| revision.config.clone()).unwrap_or_default(),
            "secretBindings": revision.as_ref().map(|revision| revision.secret_bindings.clone()).unwrap_or_default(),
            "secretHint": secret_hint,
            "lifecycleFeatures": revision.as_ref().map(|revision| revision.lifecycle_features.clone()).unwrap_or_default(),
        })))
        .unwrap(),
    ))
}

pub async fn create_connection(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<CreateConnectionRequestDto>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsManage)?;
    let request = DomainCreateConnectionRequest {
        actor,
        vendor: body
            .vendor
            .ok_or_else(|| error::validation("VENDOR_REQUIRED", "vendor 不能为空"))?,
        protocol: body
            .protocol
            .ok_or_else(|| error::validation("PROTOCOL_REQUIRED", "protocol 不能为空"))?,
        display_name: body.display_name,
        config: body.config,
        capabilities: body.capabilities,
        lifecycle_features: serde_json::from_value(body.lifecycle_features).unwrap_or_default(),
        secret_bindings: parse_binding_entries(body.secret_bindings)?,
    };
    let use_case = CreateConnection::new(ConnectionDependencies {
        connections: state.connections.clone(),
        secrets: state.secrets.clone(),
    });
    let connection = use_case.execute(request).await.map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "connectionId": connection.id.0,
            "currentRevisionId": connection.current_revision_id.0,
        })))
        .unwrap(),
    ))
}

pub async fn update_connection(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(connection_id): Path<u64>,
    Json(body): Json<UpdateConnectionRequestDto>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsManage)?;
    let secret_bindings = match body.secret_bindings {
        Some(entries) => Some(parse_binding_entries(entries)?),
        None => None,
    };
    let lifecycle_features = match body.lifecycle_features {
        Some(value) => Some(
            serde_json::from_value(value)
                .map_err(|_| error::validation("LIFECYCLE_INVALID", "lifecycleFeatures 无效"))?,
        ),
        None => None,
    };
    let request = DomainUpdateConnectionRequest {
        actor,
        connection_id: ConnectionId(connection_id),
        display_name: body.display_name,
        config: body.config,
        capabilities: body.capabilities,
        lifecycle_features,
        secret_bindings,
    };
    let use_case = UpdateConnection::new(ConnectionDependencies {
        connections: state.connections.clone(),
        secrets: state.secrets.clone(),
    });
    let connection = use_case.execute(request).await.map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(connection_summary(
            &connection,
            paused_reason_of(&state, connection.current_revision_id.0).as_deref(),
        )))
        .unwrap(),
    ))
}

pub async fn delete_connection(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(connection_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsManage)?;
    let removed = state
        .connections
        .delete_connection(ConnectionId(connection_id))
        .await
        .map_err(domain_error)?;
    if !removed {
        return Err(error::not_found(
            "CONNECTION_NOT_FOUND",
            "Connection 不存在",
        ));
    }
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "deleted": true }))).unwrap(),
    ))
}

/// 这条连接背后平台声明的可用模型（`GET {baseUrl}/models`）。
///
/// 为什么值得一条端点：模型名此前只能**手打**（连接上的"默认模型"、配方里的 model 字段），
/// 打错了要到提交时才知道。清单是平台自己给的，读它不花钱、也不改任何状态。
pub async fn list_connection_models(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(connection_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsRead)?;
    let connection = state
        .connections
        .find_connection(ConnectionId(connection_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
    let revision = state
        .connections
        .find_revision(connection.current_revision_id)
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("CONNECTION_REVISION_NOT_FOUND", "Revision 不存在"))?;
    // 凭据引用：拉清单多半要密钥（网关按 Key 授权模型），所以走与执行同一条解析。
    let revision =
        crate::v2::application::credential_reference::resolve(state.connections.as_ref(), revision)
            .await
            .map_err(domain_error)?;
    let key = crate::v2::ports::provider::driver_key(&connection.vendor, &connection.protocol);
    let driver = state.drivers.get(&key).map_err(domain_error)?;
    let models = driver.fetch_models(&revision).await.map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "connectionId": connection.id.0,
            "models": models,
        })))
        .unwrap(),
    ))
}

pub async fn check_connection(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(connection_id): Path<u64>,
) -> HttpResult {
    require_scope(&actor, Scope::ConnectionsManage)?;
    let connection = state
        .connections
        .find_connection(ConnectionId(connection_id))
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
    let revision = state
        .connections
        .find_revision(connection.current_revision_id)
        .await
        .map_err(domain_error)?
        .ok_or_else(|| error::not_found("CONNECTION_REVISION_NOT_FOUND", "Revision 不存在"))?;
    // 凭据引用：测试按钮也要那把（借来的）密钥——否则"测试"会在一条其实能跑的连接上
    // 报「缺少 API Key」。引用坏了就**如实**把原因当成检查结果回给界面（徽章行内显示），
    // 而不是 500：用户此刻正需要看到"引用的连接已被删除"才好去修。
    let revision = match crate::v2::application::credential_reference::resolve(
        state.connections.as_ref(),
        revision,
    )
    .await
    {
        Ok(revision) => revision,
        Err(error) => {
            return Ok(Json(
                serde_json::to_value(Envelope::new(json!({
                    "connectionId": connection.id.0,
                    "ok": false,
                    "capabilities": Vec::<String>::new(),
                    "message": error.message,
                })))
                .unwrap(),
            ))
        }
    };
    let key = crate::v2::ports::provider::driver_key(&connection.vendor, &connection.protocol);
    let check = match state.drivers.get(&key) {
        Ok(driver) => match driver.validate_connection(&revision).await {
            Ok(check) => check,
            Err(error) => {
                // 检查**没跑成**（连不上、平台拒绝）也算"出事了"——而且它是最常见的那一类：
                // 用户点了「测试」或打开连接页，行内出现一句红字。以前日志里一个字都没有。
                // ⚠️ 节流：打开连接页会对每条连接各跑一次，常坏的连接会把日志刷满。
                // ⚠️ 只记 code + message，**不记 details**（`details.cause` 里可能有 URL 与密钥）。
                crate::v2::logging::log_failure_throttled(
                    &format!("connection-check-{}", connection.id.0),
                    std::time::Duration::from_secs(60),
                    "connection",
                    &format!("connection={} driver={key}", connection.id.0),
                    &error.code,
                    &error.message,
                );
                return Err(domain_error(error));
            }
        },
        Err(_) => crate::v2::domain::execution::ConnectionCheck {
            ok: false,
            capabilities: vec![],
            message: Some(format!("驱动未注册：{key}")),
        },
    };
    // 驱动如实回报"连上了但检查没过"（缺密钥、平台 401…）：同样值得留一行。
    if !check.ok {
        crate::v2::logging::log_failure_throttled(
            &format!("connection-check-{}", connection.id.0),
            std::time::Duration::from_secs(60),
            "connection",
            &format!("connection={} driver={key}", connection.id.0),
            "CONNECTION_CHECK_FAILED",
            check
                .message
                .as_deref()
                .unwrap_or("连接测试失败（平台没给原因）"),
        );
    }
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({
            "connectionId": connection.id.0,
            "ok": check.ok,
            "capabilities": check.capabilities,
            "message": check.message,
        })))
        .unwrap(),
    ))
}

// ---------- tokens ----------

pub async fn list_tokens(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
) -> HttpResult {
    require_scope(&actor, Scope::ActorTokensManage)?;
    let tokens = state.tokens.list().await.map_err(domain_error)?;
    let summaries: Vec<TokenSummaryDto> = tokens
        .iter()
        .map(|record: &ActorTokenRecord| TokenSummaryDto {
            token_id: record.id,
            actor_id: record.actor_id.clone(),
            kind: format!("{:?}", record.kind).to_lowercase(),
            client_id: record.client_id.clone(),
            scopes: record
                .scopes
                .iter()
                .map(|scope| format!("{scope:?}"))
                .collect(),
            label: record.label.clone(),
            expires_at: record.expires_at.as_ref().map(|t| t.0.clone()),
            revoked: record.revoked,
            created_at: record.created_at.0.clone(),
        })
        .collect();
    Ok(Json(
        serde_json::to_value(Envelope::new(summaries)).unwrap(),
    ))
}

pub async fn issue_token(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Json(body): Json<IssueTokenRequestDto>,
) -> HttpResult {
    require_scope(&actor, Scope::ActorTokensManage)?;
    let scopes: Vec<Scope> = body
        .scopes
        .iter()
        .map(|scope| parse_scope(scope))
        .collect::<Result<Vec<_>, _>>()?;
    let kind = crate::v2::domain::actor::ActorKind::Http;
    let (token, record) = state
        .tokens
        .issue(
            &body.actor_id,
            kind,
            &body.client_id,
            scopes,
            body.label.clone(),
            body.ttl_seconds,
        )
        .await
        .map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(TokenIssuedDto {
            token_id: record.id,
            token,
            actor_id: record.actor_id,
            scopes: record
                .scopes
                .iter()
                .map(|scope| format!("{scope:?}"))
                .collect(),
            expires_at: record.expires_at.as_ref().map(|t| t.0.clone()),
        }))
        .unwrap(),
    ))
}

fn parse_scope(raw: &str) -> Result<Scope, axum::response::Response> {
    Ok(match raw {
        "recipes:read" => Scope::RecipesRead,
        "recipes:write" => Scope::RecipesWrite,
        "operations:read" => Scope::OperationsRead,
        "operations:execute" => Scope::OperationsExecute,
        "operations:cancel" => Scope::OperationsCancel,
        "operations:delete" => Scope::OperationsDelete,
        "operations:select-connection" => Scope::OperationsSelectConnection,
        "assets:local-read" => Scope::AssetsLocalRead,
        "assets:write" => Scope::AssetsWrite,
        "artifacts:read" => Scope::ArtifactsRead,
        "connections:read" => Scope::ConnectionsRead,
        "connections:manage" => Scope::ConnectionsManage,
        "settings:read" => Scope::SettingsRead,
        "settings:manage" => Scope::SettingsManage,
        "diagnostics:read" => Scope::DiagnosticsRead,
        "actor-tokens:manage" => Scope::ActorTokensManage,
        _ => {
            return Err(error::validation(
                "SCOPE_INVALID",
                format!("未知 scope：{raw}"),
            ))
        }
    })
}

pub async fn revoke_token(
    State(state): State<Arc<V2HttpState>>,
    Extension(actor): Extension<ActorRef>,
    Path(token_id): Path<i64>,
) -> HttpResult {
    require_scope(&actor, Scope::ActorTokensManage)?;
    state.tokens.revoke(token_id).await.map_err(domain_error)?;
    Ok(Json(
        serde_json::to_value(Envelope::new(json!({ "revoked": true }))).unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::ids::ConnectionId;

    /// 落盘的重名规则：**不覆盖用户已有的文件**，按浏览器口径递增（`名字 (1).png`）。
    ///
    /// 这条路径上没有 HTTP，只有一个约定，所以直接钉住它——包括文件名里带路径分隔符时
    /// 不许跳出下载目录（产物名来自平台，不可信）。
    #[test]
    fn write_download_never_overwrites_and_stays_inside_the_directory() {
        let directory =
            std::env::temp_dir().join(format!("renderloft-download-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let directory_text = directory.to_string_lossy().to_string();

        let first = write_download(&directory_text, "猫.png", b"one").expect("首次写入应成功");
        assert_eq!(first.file_name().unwrap().to_string_lossy(), "猫.png");
        let second = write_download(&directory_text, "猫.png", b"two").expect("重名应成功");
        assert_eq!(second.file_name().unwrap().to_string_lossy(), "猫 (1).png");
        let third = write_download(&directory_text, "猫.png", b"three").expect("再重名应成功");
        assert_eq!(third.file_name().unwrap().to_string_lossy(), "猫 (2).png");
        assert_eq!(
            std::fs::read(&first).unwrap(),
            b"one",
            "第一个文件不能被覆盖"
        );
        assert_eq!(std::fs::read(&third).unwrap(), b"three");

        let escaped = write_download(&directory_text, "../逃逸.png", b"x").expect("应成功");
        assert_eq!(escaped.parent().unwrap(), directory.as_path());
        assert_eq!(
            escaped.file_name().unwrap().to_string_lossy(),
            ".._逃逸.png"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    fn binding(driver_key: &str, config: &[(&str, Value)]) -> RecipeBinding {
        RecipeBinding {
            driver_key: driver_key.to_string(),
            provider_config: config
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
            default_connection_id: ConnectionId(1),
            allowed_connection_ids: vec![ConnectionId(1)],
            connection_override: Default::default(),
        }
    }

    /// 种类看的是**绑定的形状**，而且两种驱动键形态都要认。
    ///
    /// 这条钉住的是 2026-10-11 那个错误判断的反面：驱动分不出应用与工作流，
    /// 但绑定分得出——`webappId` / `workflowId` / 内联 `workflow` 是三种不同的东西。
    #[test]
    fn recipe_kind_comes_from_the_binding_shape() {
        let cases = [
            (
                "runninghub/runninghub",
                vec![("webappId", json!("1904"))],
                "rh-app",
            ),
            ("runninghub", vec![("webappId", json!("1904"))], "rh-app"),
            (
                "runninghub/runninghub",
                vec![("workflowId", json!("1904"))],
                "rh-workflow",
            ),
            (
                "runninghub",
                vec![("workflow", json!({ "3": { "class_type": "KSampler" } }))],
                "rh-workflow",
            ),
            (
                "comfyui/comfyui",
                vec![("workflow", json!({ "3": {} }))],
                "comfyui-workflow",
            ),
            ("comfyui", vec![], "comfyui-workflow"),
            (
                "openai/openai-images",
                vec![("model", json!("gpt-image-1"))],
                "openai-image",
            ),
            ("async/async-task", vec![], "other"),
        ];
        for (driver_key, config, expected) in cases {
            let binding = binding(driver_key, &config);
            assert_eq!(
                recipe_kind(Some(&binding)),
                expected,
                "driver_key={driver_key} 的形状应当判成 {expected}"
            );
        }
        assert_eq!(recipe_kind(None), "other", "没有绑定就不假装知道");
    }

    /// `webappId` 与 `workflowId` 同时存在时以应用为准（导入路径只写其一，这里是防御）。
    #[test]
    fn webapp_id_wins_when_both_are_present() {
        let binding = binding(
            "runninghub/runninghub",
            &[("webappId", json!("1")), ("workflowId", json!("2"))],
        );
        assert_eq!(recipe_kind(Some(&binding)), "rh-app");
    }
}
