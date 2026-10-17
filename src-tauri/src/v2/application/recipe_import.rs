//! 从平台导入 Recipe：贴 RunningHub 应用链接或 ID，自动生成输入 Schema
//! 与字段映射绑定，普通用户无需手写 JSON Schema。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::v2::application::field_widget::{apply_widget, Widget};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::{ConnectionId, RecipeId, RecipeRevisionId};
use crate::v2::domain::model::{
    ConnectionOverride, ConnectionStatus, Recipe, RecipeBinding, RecipeRevision, RecipeSource,
    RecipeVisibility, Timestamp,
};
use crate::v2::ports::provider::{
    driver_key, DriverRegistry, NodeInfoCatalog, NodeInputSpec, WebappSchema,
};
use crate::v2::ports::repositories::{ConnectionRepository, RecipeRepository};

/// 本文件导入的三条路（RH 应用 / RH 工作流 / ComfyUI API Prompt）都属于这一个能力。
///
/// ⚠️ 这不是"我们随手挑的名字"，而是**驱动声明过的**：RunningHub 与 ComfyUI 的
/// `ProviderDescriptor.business_capabilities` 里都是它。写成常量而不是散落的字面量，
/// 是为了让 `driver_for` 的校验有意义——校验的就是这个值。
///
/// 顺带印证一件事：RunningHub 的「AI 应用」和「工作流」**同属 `workflow-execution`**
/// （驱动内部只差 `webappId` 与 `workflowId`），所以这里只有一个常量，不是疏忽。
pub const CAPABILITY_WORKFLOW: &str = "workflow-execution";

pub struct RecipeImportService {
    connections: Arc<dyn ConnectionRepository>,
    recipes: Arc<dyn RecipeRepository>,
    drivers: Arc<DriverRegistry>,
}

impl RecipeImportService {
    pub fn new(
        connections: Arc<dyn ConnectionRepository>,
        recipes: Arc<dyn RecipeRepository>,
        drivers: Arc<DriverRegistry>,
    ) -> Self {
        Self {
            connections,
            recipes,
            drivers,
        }
    }

    /// 取连接对应的驱动，并**按驱动自己的声明**校验能力。
    ///
    /// ⚠️ 为什么需要这个函数：`ProviderDescriptor.business_capabilities` 一直是
    /// "声明了但没人读"——注册表只自检它非空，`/capabilities` 只把它转发给客户端，
    /// 而**连接的**能力来自客户端请求、**recipe 的**能力在导入路径里写死，
    /// 三者之间没有任何校验。于是"加平台时漏声明 / 写错能力"不会当场报错，
    /// 只会变成运行期的怪事（比如任务提交了才发现驱动根本不支持）。
    ///
    /// 这里把它变成**导入期的明确报错**，两道：
    ///   1. 驱动必须声明过这个能力（否则是代码问题，漏声明）；
    ///   2. 这条连接必须启用了这个能力（否则是配置问题，连接建错了）。
    ///
    /// 驱动键**从连接推导**（`vendor/protocol`），与调度器同源——此前这里写死
    /// `driver_key("runninghub", "runninghub")`，意味着拿一条 ComfyUI 连接也能走到
    /// RunningHub 驱动上。`import_comfyui_prompt` 早有 `connection.protocol` 校验，
    /// 这两条路只是没跟上。
    fn driver_for(
        &self,
        connection: &crate::v2::domain::model::Connection,
        revision: &crate::v2::domain::model::ConnectionRevision,
        capability: &str,
    ) -> DomainResult<Arc<dyn crate::v2::ports::provider::ProviderDriver>> {
        let key = driver_key(&connection.vendor, &connection.protocol);
        let driver = self.drivers.get(&key)?;
        let declared = &driver.descriptor().business_capabilities;
        if !declared.iter().any(|entry| entry == capability) {
            return Err(SafeError::validation(
                "RECIPE_CAPABILITY_UNDECLARED",
                format!(
                    "驱动 {key} 没有声明「{capability}」能力（它声明的是：{}）",
                    if declared.is_empty() {
                        "无".to_string()
                    } else {
                        declared.join("、")
                    }
                ),
            ));
        }
        if !revision
            .effective_capabilities
            .iter()
            .any(|entry| entry == capability)
        {
            return Err(SafeError::validation(
                "CONNECTION_CAPABILITY_MISSING",
                format!("该连接没有启用「{capability}」，无法导入这类应用"),
            ));
        }
        Ok(driver)
    }

    /// 找出已导入的同一个 webapp：`(recipeId, name)`。
    ///
    /// 判据是 **`(driver_key, provider_config.webappId)`**，不是名字——
    /// 名字用户能改，webappId 不会。
    async fn find_imported_webapp(&self, webapp_id: &str) -> DomainResult<Option<(u64, String)>> {
        self.find_imported_by_binding_key("webappId", webapp_id)
            .await
    }

    /// 同一个工作流不该被导入两次——判据与 `find_imported_webapp` 同款，只是键换成
    /// `provider_config.workflowId`。
    async fn find_imported_workflow(
        &self,
        workflow_id: &str,
    ) -> DomainResult<Option<(u64, String)>> {
        self.find_imported_by_binding_key("workflowId", workflow_id)
            .await
    }

    /// 按 `provider_config[<key>]` 找已经导入过的那一份（返回 id 与名字）。
    async fn find_imported_by_binding_key(
        &self,
        config_key: &str,
        platform_id: &str,
    ) -> DomainResult<Option<(u64, String)>> {
        let expected_driver = driver_key("runninghub", "runninghub");
        // 上限 200 与 `list_recipes` 的常规用法一致；库里不会有几百个应用，
        // 真到了那个量级该做的是给 binding 建索引，而不是在这里翻页。
        for (recipe, binding) in self.recipes.list_recipes(200, None).await? {
            // ⚠️ **必须同时认历史裸键。**
            //
            // 早期导入写的是裸键 `"runninghub"`（见本文件 `binding.driver_key` 处的说明），
            // 而那批老行**没有做数据迁移**。只比新形态（`vendor/protocol`）的话，这个守卫会
            // **恰好对它最该保护的那批数据失效**——实测：库里 webappId `2084540538883235841`
            // 的老 recipe 被跳过，同一个应用又导进来一份，而守卫注释里写的事故应用正是这一个。
            // 当时的单测是绿的，因为它种的是新格式：**为错误的理由通过**。
            let key = binding.driver_key.as_str();
            let is_runninghub = key == expected_driver
                || expected_driver
                    .split_once('/')
                    .is_some_and(|(vendor, _)| key == vendor);
            if !is_runninghub {
                continue;
            }
            let imported = binding
                .provider_config
                .get(config_key)
                .and_then(|value| value.as_str());
            if imported == Some(platform_id) {
                return Ok(Some((recipe.id.0, recipe.name)));
            }
        }
        Ok(None)
    }

    pub async fn import(
        &self,
        connection_id: ConnectionId,
        source: &str,
    ) -> DomainResult<ImportedRecipe> {
        let webapp_id = parse_webapp_id(source)?;
        // ⚠️ **同一个 webapp 不该被导入两次。**
        //
        // 实测过一次：用户的库里同一个 RunningHub 应用（`webappId 2084540538883235841`）
        // 存在两份 Recipe、相隔 13 天导入——**没有任何东西拦它**。危害不是洁癖：
        // * Agent 的应用目录里会出现两个同名应用，它可能挑中那个**没标注字段**的；
        // * 用户"自己策展的那一份"被污染——而那正是本产品相对平台扁平目录的价值。
        //
        // 判据是 **(driver, webappId)** 而不是名字：名字用户能改，webappId 不会。
        // 刻意**不**静默返回已有的那个、也不自动合并——两种正当意图
        //（"我要用它" / "我要刷新它"）都需要先知道"它已经存在"，所以给一个**指名**的错误。
        if let Some((existing_id, existing_name)) = self.find_imported_webapp(&webapp_id).await? {
            return Err(SafeError::conflict(
                "WEBAPP_ALREADY_IMPORTED",
                format!(
                    "这个应用已经导入过了：应用 #{existing_id}「{existing_name}」。\
                     要用它直接打开即可；要按平台最新状态重新导入，请先删除那一份。"
                ),
            ));
        }
        let connection = self
            .connections
            .find_connection(connection_id)
            .await?
            .ok_or_else(|| SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
        if connection.status != ConnectionStatus::Active {
            return Err(SafeError::validation(
                "CONNECTION_NOT_READY",
                "该 Connection 已停用，无法导入应用",
            ));
        }
        let connection_revision = self
            .connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })?;
        let driver = self.driver_for(&connection, &connection_revision, CAPABILITY_WORKFLOW)?;
        let schema = driver
            .fetch_webapp_schema(&connection_revision, &webapp_id)
            .await?;
        if schema.webapp_name.trim().is_empty() && schema.nodes.is_empty() {
            return Err(SafeError::validation(
                "WEBAPP_NOT_FOUND",
                "RunningHub 未返回该应用信息，请检查链接与 API Key",
            ));
        }
        let (input_schema, defaults, fields, field_mapping, media_mapping) =
            build_input_schema(&schema)?;

        let now = Timestamp::now();
        let name = if schema.webapp_name.trim().is_empty() {
            format!("RunningHub {webapp_id}")
        } else {
            schema.webapp_name.trim().to_string()
        };
        let description = if !schema.description.trim().is_empty() {
            schema.description.trim().to_string()
        } else {
            schema.description_en.trim().to_string()
        };
        let mut provider_config = BTreeMap::from([
            ("webappId".to_string(), json!(webapp_id)),
            ("fieldMapping".to_string(), json!(field_mapping)),
        ]);
        if !media_mapping.is_empty() {
            provider_config.insert("mediaMapping".to_string(), json!(media_mapping));
        }
        let recipe = Recipe {
            id: RecipeId(0),
            name: name.clone(),
            description,
            labels: vec!["runninghub".to_string()],
            capability: CAPABILITY_WORKFLOW.to_string(),
            current_revision_id: RecipeRevisionId(0),
            visibility: RecipeVisibility::Private,
            created_at: now.clone(),
            updated_at: now.clone(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            // 导入时先不带备注：备注是**事后**补的展示层——用户给已导入的应用补标注即可，
            // 不必在导入流程里再塞一步。ComfyUI 那条路导入时给的 label 落在 schema 里，
            // 与这里的备注是两层：备注是覆盖层，改它不动 revision。
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: RecipeId(0),
            revision: 1,
            input_schema,
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                // 与同文件的驱动查询（`driver_key("runninghub", "runninghub")`）及
                // `DriverRegistry` 的注册键保持**同一形态**。此前这里写裸键 "runninghub"，
                // 导致库里 `binding.driver_key` 出现两种形态（UI 创建走 vendor/protocol、
                // 导入走裸键），进而让前端按 driverKey 判类型时对一半数据判错。
                driver_key: driver_key("runninghub", "runninghub"),
                provider_config,
                default_connection_id: connection_id,
                allowed_connection_ids: vec![connection_id],
                connection_override: ConnectionOverride::Denied,
            },
            defaults,
            source: RecipeSource::Referenced,
            created_at: now,
        };
        let (recipe, revision) = self.recipes.create_recipe(recipe, revision).await?;
        let mut recipe = recipe;
        recipe.current_revision_id = revision.id;
        self.recipes.update_recipe(&recipe).await?;
        Ok(ImportedRecipe {
            recipe_id: recipe.id.0,
            revision_id: revision.id.0,
            name,
            capability: recipe.capability,
            input_schema: revision.input_schema,
            fields: fields
                .into_iter()
                .map(|field| ImportedField {
                    key: field.key,
                    name: field.field_name,
                    field_type: field.field_type,
                    description: field.description,
                    description_en: field.description_en,
                    required: field.required,
                    default: field.default,
                    constraints: field.constraints,
                    media: field.media,
                })
                .collect(),
        })
    }

    /// 从平台链接/工作流 ID 导入一个**原生 RunningHub 工作流**（这条路此前只有界面够得着）。
    ///
    /// 与 `import()`（AI 应用）对称，差的只是"用哪个 id 指向平台上的东西"：
    /// 应用存 `webappId`，工作流存 `workflowId`，**两者的定义都住在平台上** ⇒ 都是
    /// `RecipeSource::Referenced`。刻意**不落整张 workflow JSON**：平台侧的
    /// `task/openapi/create` 收的就是 `workflowId` + `nodeInfoList`，把图复制进我们的库
    /// 只会让它与平台漂移（而 ComfyUI 那条路存 JSON 是因为图确实要在那个引擎上跑）。
    ///
    /// 这里默认只做**机械的第一步**：拉一次 prompt（为了知道有哪些字段）、字段全暴露、不写备注名。
    /// 勾字段、写中文备注、选控件都是人的事 —— 走 `fields` 传进来就是他的决定（`None` = 他没选，
    /// 那就按启发式给默认；两条口径与 `import()` 一致）。
    pub async fn import_workflow(
        &self,
        connection_id: ConnectionId,
        source: &str,
        name: Option<String>,
        selected_fields: Option<Vec<ComfyFieldSelection>>,
    ) -> DomainResult<ImportedRecipe> {
        let workflow_id = parse_webapp_id(source)?;
        // 判据同样是 **(driver, workflowId)** 而不是名字：名字用户能改，id 不会。
        if let Some((existing_id, existing_name)) =
            self.find_imported_workflow(&workflow_id).await?
        {
            return Err(SafeError::conflict(
                "WORKFLOW_ALREADY_IMPORTED",
                format!(
                    "这个工作流已经导入过了：应用 #{existing_id}「{existing_name}」。\
                     要用它直接打开即可；要按平台最新状态重新导入，请先删除那一份。"
                ),
            ));
        }
        // 这一步顺带把连接校验完（存在、未停用、驱动声明了 workflow 能力）。
        let prompt = self
            .fetch_comfyui_workflow(connection_id, &workflow_id)
            .await?;
        // 这张图住在 RunningHub 云端，但**跑它的引擎**由这条连接决定——所以节点定义
        // 要去连接指向的那个引擎问（本机 ComfyUI 或 RH 云端都行）。
        let (catalog, _enrichment) = self.enrich_for_workflow(connection_id, &prompt).await;
        // 用户在"选择参数"里做过决定 ⇒ **他说了算**（勾哪些、备注名、控件、候选项都在里面）；
        // `None` = 他还没选 ⇒ 除"显然不该暴露"的字段外全部暴露，标签用默认的
        // 「节点标题 · 字段名」（以前是空标签，界面上只能看到 3::texts）。
        let selection = selected_fields.as_ref().map(|fields| {
            fields
                .iter()
                .map(|field| (field.key.clone(), ComfyFieldChoice::from_selection(field)))
                .collect::<BTreeMap<String, ComfyFieldChoice>>()
        });
        let (input_schema, defaults, fields, field_mapping, media_mapping) =
            build_comfyui_schema_with(&prompt, selection.as_ref(), catalog.as_ref())?;

        let now = Timestamp::now();
        let name = name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("RunningHub 工作流 {workflow_id}"));
        let mut provider_config = BTreeMap::from([
            ("workflowId".to_string(), json!(workflow_id)),
            ("fieldMapping".to_string(), json!(field_mapping)),
        ]);
        if !media_mapping.is_empty() {
            provider_config.insert("mediaMapping".to_string(), json!(media_mapping));
        }
        let recipe = Recipe {
            id: RecipeId(0),
            name: name.clone(),
            description: String::new(),
            labels: vec!["runninghub".to_string()],
            capability: CAPABILITY_WORKFLOW.to_string(),
            current_revision_id: RecipeRevisionId(0),
            visibility: RecipeVisibility::Private,
            created_at: now.clone(),
            updated_at: now.clone(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: RecipeId(0),
            revision: 1,
            input_schema,
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: driver_key("runninghub", "runninghub"),
                provider_config,
                default_connection_id: connection_id,
                allowed_connection_ids: vec![connection_id],
                connection_override: ConnectionOverride::Denied,
            },
            defaults,
            source: RecipeSource::Referenced,
            created_at: now,
        };
        let (recipe, revision) = self.recipes.create_recipe(recipe, revision).await?;
        let mut recipe = recipe;
        recipe.current_revision_id = revision.id;
        self.recipes.update_recipe(&recipe).await?;
        Ok(ImportedRecipe {
            recipe_id: recipe.id.0,
            revision_id: revision.id.0,
            name,
            capability: recipe.capability,
            input_schema: revision.input_schema,
            fields: fields
                .into_iter()
                .map(|field| ImportedField {
                    key: field.key,
                    name: field.field_name,
                    field_type: field.field_type,
                    description: field.description,
                    description_en: field.description_en,
                    required: field.required,
                    default: field.default,
                    constraints: field.constraints,
                    media: field.media,
                })
                .collect(),
        })
    }

    /// 从 RunningHub 拉取云端 ComfyUI 工作流的 API Prompt JSON，
    /// 供导入对话框直接进入字段勾选步骤。
    pub async fn fetch_comfyui_workflow(
        &self,
        connection_id: ConnectionId,
        source: &str,
    ) -> DomainResult<Value> {
        let workflow_id = parse_webapp_id(source)?;
        let connection = self
            .connections
            .find_connection(connection_id)
            .await?
            .ok_or_else(|| SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
        if connection.status != ConnectionStatus::Active {
            return Err(SafeError::validation(
                "CONNECTION_NOT_READY",
                "该 Connection 已停用，无法拉取工作流",
            ));
        }
        let connection_revision = self
            .connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })?;
        let driver = self.driver_for(&connection, &connection_revision, CAPABILITY_WORKFLOW)?;
        driver
            .fetch_workflow_prompt(&connection_revision, &workflow_id)
            .await
    }

    /// 按连接 id 富化（给"从平台拉一张图再导入"那条路用：那边手里只有 connection_id）。
    async fn enrich_for_workflow(
        &self,
        connection_id: ConnectionId,
        prompt: &Value,
    ) -> (Option<NodeInfoCatalog>, SchemaEnrichment) {
        let Ok(Some(connection)) = self.connections.find_connection(connection_id).await else {
            return (
                None,
                SchemaEnrichment {
                    reason: Some("Connection 不存在".to_string()),
                    ..SchemaEnrichment::default()
                },
            );
        };
        let Ok(Some(revision)) = self
            .connections
            .find_revision(connection.current_revision_id)
            .await
        else {
            return (
                None,
                SchemaEnrichment {
                    reason: Some("Connection revision 不存在".to_string()),
                    ..SchemaEnrichment::default()
                },
            );
        };
        self.enrich_comfyui(&connection, &revision, prompt).await
    }

    /// **尽力**问一次引擎的节点定义。
    ///
    /// 绝不因为拿不到而失败：今天"手里有一张 API Prompt、离线也能导入"是能用的，
    /// 为了控件好看把这条能力弄丢是本末倒置。拿不到就退回通用控件，并且**把原因带回去**
    /// （`SchemaEnrichment::reason`），让界面能说清"为什么这表单是糙的"。
    async fn enrich_comfyui(
        &self,
        connection: &crate::v2::domain::model::Connection,
        connection_revision: &crate::v2::domain::model::ConnectionRevision,
        prompt: &Value,
    ) -> (Option<NodeInfoCatalog>, SchemaEnrichment) {
        // 只用到的类，按图里第一次出现的顺序去重（顺序稳定，日志与测试好读）。
        let mut classes: Vec<String> = Vec::new();
        if let Some(nodes) = prompt.as_object() {
            for node in nodes.values() {
                let Some(class_type) = node
                    .get("class_type")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                if !classes.iter().any(|existing| existing == class_type) {
                    classes.push(class_type.to_string());
                }
            }
        }
        let mut enrichment = SchemaEnrichment {
            classes_used: classes.len(),
            ..SchemaEnrichment::default()
        };
        if classes.is_empty() {
            enrichment.reason = Some("这张图里没有节点".to_string());
            return (None, enrichment);
        }
        let driver = match self.driver_for(connection, connection_revision, CAPABILITY_WORKFLOW) {
            Ok(driver) => driver,
            Err(error) => {
                enrichment.reason = Some(error.message);
                return (None, enrichment);
            }
        };
        // 凭据引用要先解析：驱动只认**本连接自己的** keyring 槽，rh-proxy 那种"借 RunningHub
        // 的 Key"的连接不解析就拿不到钥匙，驱动会回「需要 API Key」——富化白跑（2026-10-14 实测踩到）。
        // 解析失败不当作致命：把原 revision 交出去，让驱动报它自己的错（那才是真原因）。
        let resolved = crate::v2::application::credential_reference::resolve(
            self.connections.as_ref(),
            connection_revision.clone(),
        )
        .await
        .unwrap_or_else(|_| connection_revision.clone());
        match driver.fetch_node_info(&resolved, &classes).await {
            Ok(catalog) => {
                enrichment.classes_known = catalog.classes.len();
                if catalog.is_empty() {
                    enrichment.reason = Some("引擎没有返回这些节点的定义".to_string());
                    return (None, enrichment);
                }
                (Some(catalog), enrichment)
            }
            Err(error) => {
                enrichment.reason = Some(error.message);
                (None, enrichment)
            }
        }
    }

    /// 预览一份 API Prompt（不建 Recipe）：字段清单 + 富化情况。
    ///
    /// 富化是**尽力而为**的：拿不到引擎定义照样能预览（退回通用控件），
    /// 但界面会拿到 `enrichment.reason` 去解释"为什么控件是普通的"。
    pub async fn preview_comfyui_prompt(
        &self,
        connection_id: ConnectionId,
        prompt: &Value,
    ) -> DomainResult<ComfyPromptPreview> {
        let connection = self
            .connections
            .find_connection(connection_id)
            .await?
            .ok_or_else(|| SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
        let revision = self
            .connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })?;
        // 连不上/不支持都只是"没有富化"，不是"预览失败"：仍然把字段列出来。
        let (catalog, enrichment) = self.enrich_comfyui(&connection, &revision, prompt).await;
        Ok(ComfyPromptPreview {
            fields: preview_comfyui_fields(prompt, catalog.as_ref())?,
            enrichment,
        })
    }

    /// 从 ComfyUI「保存(API 格式)」导出的 workflow API Prompt 导入 Recipe，
    /// 自动发现可参数化的节点输入字段并生成 mapping。
    pub async fn import_comfyui_prompt(
        &self,
        connection_id: ConnectionId,
        prompt: Value,
        name: Option<String>,
        selected_fields: Option<Vec<ComfyFieldSelection>>,
    ) -> DomainResult<ImportedRecipe> {
        let workflow = prompt.as_object().ok_or_else(|| {
            SafeError::validation(
                "COMFYUI_PROMPT_INVALID",
                "ComfyUI API Prompt 必须是节点对象",
            )
        })?;
        if workflow.is_empty() {
            return Err(SafeError::validation(
                "COMFYUI_PROMPT_EMPTY",
                "ComfyUI API Prompt 为空",
            ));
        }
        let connection = self
            .connections
            .find_connection(connection_id)
            .await?
            .ok_or_else(|| SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在"))?;
        if connection.status != ConnectionStatus::Active {
            return Err(SafeError::validation(
                "CONNECTION_NOT_READY",
                "该 Connection 已停用，无法导入应用",
            ));
        }
        // 校验连接确实指向 ComfyUI，避免把 workflow 绑到错误的平台。
        if connection.protocol != "comfyui" {
            return Err(SafeError::validation(
                "CONNECTION_NOT_COMFYUI",
                "请选择 ComfyUI 类型的 Connection 导入 API Prompt",
            ));
        }
        // 这条路不拉平台数据，但仍然过一遍驱动/能力的校验：能力是**驱动声明的**，
        // 不该由"这里恰好写了个 workflow-execution"来决定。
        let connection_revision = self
            .connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })?;
        self.driver_for(&connection, &connection_revision, CAPABILITY_WORKFLOW)?;

        // 尽力问一次引擎的节点定义（拿不到不影响导入——见 `enrich_comfyui`）。
        let (catalog, _enrichment) = self
            .enrich_comfyui(&connection, &connection_revision, &prompt)
            .await;

        let selection = selected_fields.as_ref().map(|list| {
            list.iter()
                .map(|field| (field.key.clone(), ComfyFieldChoice::from_selection(field)))
                .collect::<BTreeMap<String, ComfyFieldChoice>>()
        });
        let (input_schema, defaults, fields, field_mapping, media_mapping) =
            build_comfyui_schema_with(&prompt, selection.as_ref(), catalog.as_ref())?;

        let now = Timestamp::now();
        let name = name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "ComfyUI API Prompt".to_string());
        let mut provider_config = BTreeMap::from([("workflow".to_string(), prompt)]);
        if !field_mapping.is_empty() {
            provider_config.insert("fieldMapping".to_string(), json!(field_mapping));
        }
        if !media_mapping.is_empty() {
            provider_config.insert("mediaMapping".to_string(), json!(media_mapping));
        }
        let recipe = Recipe {
            id: RecipeId(0),
            name: name.clone(),
            description: String::new(),
            labels: vec!["comfyui".to_string()],
            capability: CAPABILITY_WORKFLOW.to_string(),
            current_revision_id: RecipeRevisionId(0),
            visibility: RecipeVisibility::Private,
            created_at: now.clone(),
            updated_at: now.clone(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            // 导入时先不带备注：备注是**事后**补的展示层——用户给已导入的应用补标注即可，
            // 不必在导入流程里再塞一步。ComfyUI 那条路导入时给的 label 落在 schema 里，
            // 与这里的备注是两层：备注是覆盖层，改它不动 revision。
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: RecipeId(0),
            revision: 1,
            input_schema,
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                // 同上：用规范形态而非裸键 "comfyui"。
                driver_key: driver_key("comfyui", "comfyui"),
                provider_config,
                default_connection_id: connection_id,
                allowed_connection_ids: vec![connection_id],
                connection_override: ConnectionOverride::Denied,
            },
            defaults,
            source: RecipeSource::Embedded,
            created_at: now,
        };
        let (recipe, revision) = self.recipes.create_recipe(recipe, revision).await?;
        let mut recipe = recipe;
        recipe.current_revision_id = revision.id;
        self.recipes.update_recipe(&recipe).await?;
        Ok(ImportedRecipe {
            recipe_id: recipe.id.0,
            revision_id: revision.id.0,
            name,
            capability: recipe.capability,
            input_schema: revision.input_schema,
            fields: fields
                .into_iter()
                .map(|field: FieldPlan| ImportedField {
                    key: field.key,
                    name: field.field_name,
                    field_type: field.field_type,
                    description: field.description,
                    description_en: field.description_en,
                    required: field.required,
                    default: field.default,
                    constraints: field.constraints,
                    media: field.media,
                })
                .collect(),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedRecipe {
    pub recipe_id: u64,
    pub revision_id: u64,
    pub name: String,
    pub capability: String,
    pub input_schema: Value,
    pub fields: Vec<ImportedField>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedField {
    pub key: String,
    pub name: String,
    pub field_type: String,
    pub description: String,
    pub description_en: String,
    pub required: bool,
    pub default: Option<Value>,
    pub constraints: Value,
    pub media: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub connection_id: u64,
    pub source: String,
}

#[derive(Debug)]
struct FieldPlan {
    key: String,
    field_name: String,
    field_type: String,
    description: String,
    description_en: String,
    required: bool,
    default: Option<Value>,
    constraints: Value,
    media: bool,
}

type BuiltSchema = (
    Value,
    BTreeMap<String, Value>,
    Vec<FieldPlan>,
    BTreeMap<String, Value>,
    BTreeMap<String, Value>,
);

fn build_input_schema(schema: &WebappSchema) -> DomainResult<BuiltSchema> {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let mut defaults = BTreeMap::new();
    let mut plans = Vec::new();
    let mut field_mapping = BTreeMap::new();
    let mut media_mapping = BTreeMap::new();

    for node in &schema.nodes {
        if node.field_name.is_empty() || node.node_id.is_empty() {
            continue;
        }
        let key = format!("{}::{}", node.node_id, node.field_name);
        let description = if !node.description.trim().is_empty() {
            node.description.trim().to_string()
        } else {
            node.description_en.trim().to_string()
        };
        let (schema_value, constraints, default_value, is_media) = field_schema(node)?;
        let has_default = default_value.is_some();
        properties.insert(key.clone(), schema_value);
        if !has_default {
            required.push(key.clone());
        }
        if let Some(value) = default_value.clone() {
            defaults.insert(key.clone(), value.clone());
        }
        if is_media {
            // ⚠️ 键 = **字段键**（不是 asset-N 位置槽）：资产的语义是"写进哪个节点字段"，
            // 上游节点顺序、schema 键序、调用方顺序谁都不该是语义。旧 revision 的
            // asset-N 形状由 MediaMapping 读取时归一化（见 domain::media）。
            media_mapping.insert(
                key.clone(),
                json!({ "node": node.node_id, "field": node.field_name }),
            );
        } else {
            field_mapping.insert(
                key.clone(),
                json!({ "node": node.node_id, "field": node.field_name }),
            );
        }
        plans.push(FieldPlan {
            key,
            field_name: node.field_name.clone(),
            field_type: node.field_type.clone(),
            description,
            description_en: node.description_en.trim().to_string(),
            required: !has_default,
            default: default_value,
            constraints,
            media: is_media,
        });
    }
    if properties.is_empty() {
        return Err(SafeError::validation(
            "WEBAPP_NO_INPUTS",
            "该应用没有可导入的输入字段",
        ));
    }
    let input_schema = json!({
        "type": "object",
        "required": required,
        "properties": properties,
    });
    Ok((input_schema, defaults, plans, field_mapping, media_mapping))
}

fn field_schema(
    node: &crate::v2::ports::provider::WebappNodeInfo,
) -> DomainResult<(Value, Value, Option<Value>, bool)> {
    let mut schema = serde_json::Map::new();
    let mut constraints = serde_json::Map::new();
    let description = if !node.description.trim().is_empty() {
        node.description.trim().to_string()
    } else {
        node.description_en.trim().to_string()
    };
    if !description.is_empty() {
        schema.insert("description".to_string(), json!(description));
    }
    // fieldData: ["INT", {"max":..., "min":...}] / ["STRING", {"multiline": true}]
    let limits = node.field_data.as_array().and_then(|parts| parts.get(1));

    let default_value = node
        .field_value
        .as_ref()
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());

    let (schema_type, typed_default, is_media) = match node.field_type.as_str() {
        "STRING" | "TEXT" => {
            if let Some(multiline) = limits.and_then(|value| value.get("multiline")) {
                if multiline.is_boolean() {
                    schema.insert("format".to_string(), json!("textarea"));
                }
            }
            let default = default_value.map(str::to_string).map(Value::String);
            ("string".to_string(), default, false)
        }
        "INT" | "INTEGER" => {
            for (raw_key, schema_key) in [("min", "minimum"), ("max", "maximum")] {
                if let Some(value) = limits.and_then(|limits| limits.get(raw_key)) {
                    if let Some(number) = value.as_i64() {
                        schema.insert(schema_key.to_string(), json!(number));
                        constraints.insert(raw_key.to_string(), json!(number));
                    }
                }
            }
            let default = default_value
                .and_then(|value| value.parse::<i64>().ok())
                .map(Value::from);
            ("integer".to_string(), default, false)
        }
        "FLOAT" | "DOUBLE" | "NUMBER" => {
            let default = default_value
                .and_then(|value| value.parse::<f64>().ok())
                .map(Value::from);
            ("number".to_string(), default, false)
        }
        "BOOLEAN" | "BOOL" => {
            let default = default_value.map(|value| Value::Bool(value == "true"));
            ("boolean".to_string(), default, false)
        }
        "IMAGE" | "VIDEO" | "AUDIO" | "FILE" | "UPLOAD" => {
            // 媒体输入：schema 声明为 file 字段（inputs 传文件名），
            // 资产字节经 runs[].assets 上传并由 mediaMapping 映射到节点。
            schema.insert("format".to_string(), json!("file"));
            ("string".to_string(), None, true)
        }
        _other => {
            // 未知类型按字符串处理，保持可导入。
            let default = default_value.map(str::to_string).map(Value::String);
            ("string".to_string(), default, false)
        }
    };
    schema.insert("type".to_string(), json!(schema_type));
    Ok((
        Value::Object(schema),
        Value::Object(constraints),
        typed_default,
        is_media,
    ))
}

/// 判断一个字段**值不值得给用户改**（默认勾选 / 机械导入时暴露）。
///
/// ⚠️ 第一性原理：**这件事必须看字段的值**，不能只看名字。旧版只看名字、而且只看"眼熟不眼熟"，
/// 于是在真实工作流上翻了大车（2026-10-14，RH 工作流 `Qwen Image 2.1`）：
///
/// * `PrimitiveStringMultiline.value` 里躺着**两百字的提示词**——用户唯一一定要改的东西——
///   因为名字叫 `value` 被判成"不暴露"；
///   同图的 `cfg`/`denoise`/`seed`/`steps`/`batch_size` 却因为"名字眼熟"被默认暴露。
///   ⇒ 交给用户的界面是**有旋钮、没有输入框**。
/// * 名字匹配还是**全等**，于是 `negative_prompt`（同样是提示词，只是名字长一点）也漏了。
///
/// 判据分两档，**两档都只是"建议"**（导入对话框会把全部候选列出来让用户自己勾，
/// 所以这里猜偏了只是"多一个/少一个默认勾"，不会挡住任何东西）：
///
/// * **默认勾**——用户一定会有意见的：提示词/描述类文本、**长文本值**（与名字无关，正是它救回了
///   上面那个提示词）、图片/遮罩/音视频、尺寸类（width/height/resolution/aspect/megapixels）、
///   以及 ComfyUI 里**用户自己放上去的** `Primitive*` 节点（放它就是要改它）。
/// * **只列出、不默认勾**——高级旋钮（seed/steps/cfg/denoise/sampler_name/scheduler/strength/
///   batch_size…）与纯管道（`*_name`、`*_prefix`、dtype/device/type）。
///   判据是**用户行为**不是洁癖：这些字段"有时有人改"，但绝大多数人不动；摆在第一屏会把
///   真正要填的那个框淹掉（主人的原话：**"用户不会那么无聊"**）。
fn comfyui_field_relevance(class_type: &str, field: &str, value: &Value) -> Relevance {
    let lower = field.to_ascii_lowercase();
    let class_lower = class_type.to_ascii_lowercase();

    // ① 媒体：图片/遮罩/音视频。不要求类名里带 loadimage —— 自定义节点叫法五花八门。
    if (matches!(lower.as_str(), "image" | "mask" | "video" | "audio")
        || lower.ends_with("_image")
        || lower.ends_with("_mask"))
        && value.is_string()
    {
        return Relevance::Media;
    }

    // ② ComfyUI 的 `Primitive*`：**用户自己放上去的**，含义就是"这里我要改"。
    if class_lower.starts_with("primitive") {
        return Relevance::Input;
    }

    // ③ 提示词族：子串匹配（旧版全等，所以 negative_prompt 漏了）。
    let promptish = lower.contains("prompt")
        || lower.contains("caption")
        || lower.contains("description")
        || lower == "text"
        || lower.starts_with("text_")
        || lower.ends_with("_text");
    if promptish || (is_text_class(&class_lower) && (lower == "text" || lower.contains("prompt"))) {
        return Relevance::Input;
    }

    // ④ **值本身像一句人话 ⇒ 那是提示词**（名字给不出任何线索时的兜底，也是这次的关键一条）。
    //    ⚠️ 必须"像句子"，不能只看长度：实测 `lora_name` 的值是
    //    `qwen_image_2.1_turbo_lora_avg_rank_178_bf16.safetensors`（54 字符）——长，但它是**机器标识符**。
    if is_prose_like(value) {
        return Relevance::Input;
    }

    // ⑤ 旋钮与管道：列出来，但不默认勾（见函数注释里那条用户行为判据）。
    if is_advanced_knob(&lower) || is_plumbing(&lower) {
        return Relevance::Skip;
    }

    // ⑥ 尺寸：用户对画幅有意见。
    if ["width", "height", "resolution", "aspect", "megapixel"]
        .iter()
        .any(|key| lower.contains(key))
        || lower == "size"
    {
        return Relevance::Input;
    }

    Relevance::Skip
}

/// 长到"这一定是人写的文本"的阈值（字符数，不是字节）。
///
/// 定 40：文件名/枚举/提示词前缀都远短于此，而一句提示词通常远超。
const LONG_TEXT_CHARS: usize = 40;

/// 这个值看起来像**人写的一句话**吗（提示词），还是像**机器标识符**（文件名/枚举/路径）？
///
/// 两条判据合起来用（长度不够就免谈）：
/// * 有空白；或
/// * 有非 ASCII 字符（中文提示词就是这种；而文件名/模型名几乎全是 ASCII 加 `_`/`.`）。
///
/// 反例（正是这条判据的由来）：`lora_name` 的值是
/// `qwen_image_2.1_turbo_lora_avg_rank_178_bf16.safetensors` —— 54 个字符，但全是 ASCII、
/// 没有空白 ⇒ 它是标识符，不该因为"长"就被当成提示词暴露给用户。
fn is_prose_like(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if text.chars().count() < LONG_TEXT_CHARS {
        return false;
    }
    text.chars().any(|c| c.is_whitespace() || !c.is_ascii())
}

/// 高级旋钮：有时有人改，但绝大多数用户不动 ⇒ **列出来、不默认勾**。
fn is_advanced_knob(lower: &str) -> bool {
    const KNOBS: [&str; 12] = [
        "seed",
        "steps",
        "cfg",
        "denoise",
        "sampler_name",
        "scheduler",
        "strength",
        "batch_size",
        "guidance",
        "noise",
        "shift",
        "sigma",
    ];
    KNOBS
        .iter()
        .any(|knob| lower == *knob || lower.ends_with(&format!("_{knob}")))
}

/// 纯管道：模型文件名、dtype、设备、保存前缀——改了只会把图跑坏，不该出现在第一屏。
fn is_plumbing(lower: &str) -> bool {
    lower.ends_with("_name")
        || lower.ends_with("_prefix")
        || matches!(
            lower,
            "device" | "type" | "weight_dtype" | "dtype" | "language" | "filename"
        )
}

/// 是否为携带文本内容的节点（文本编码器 / Conditioning / SAM 等）。
fn is_text_class(class_lower: &str) -> bool {
    class_lower.contains("text")
        && (class_lower.contains("encode")
            || class_lower.contains("conditioning")
            || class_lower.contains("sam")
            || class_lower.contains("maskfromtext"))
        || class_lower.contains("openpose") // VHS_Text 等
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relevance {
    Input,
    Media,
    Skip,
}

/// 用户在导入时对一条字段的决定。
///
/// 为什么不放在我们这边定：**"这个字段该是什么控件、有哪些合法选项"有一部分只有用户知道**
/// （他的图他自己清楚：这张图其实只有三种画幅、他常用的采样器就那五个）。
/// 我们给的是**默认值**；他一改，就变成**他的决定**，并且记下来源（见 `x-widget-source`），
/// 以后我们改匹配逻辑**不许覆盖它**。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComfyFieldSelection {
    pub key: String,
    #[serde(default)]
    pub label: String,
    /// 用户显式选的控件；`None`/`"default"` = 不覆盖（让引擎声明与值的形状决定）。
    #[serde(default)]
    pub widget: Option<String>,
    /// 选了"下拉"时的候选项（用户自己填）。
    #[serde(default)]
    pub options: Vec<String>,
}

/// 内部用的"用户决定"（已规范化）。控件那部分与"编辑里改控件"**共用一份实现**
/// （`application::field_widget`）——同一件事只留一处，否则两边会悄悄分叉。
#[derive(Debug, Clone, Default)]
struct ComfyFieldChoice {
    label: String,
    widget: Option<Widget>,
    options: Vec<String>,
}

impl ComfyFieldChoice {
    #[cfg(test)]
    fn label_only(label: &str) -> Self {
        Self {
            label: label.to_string(),
            ..Self::default()
        }
    }

    fn from_selection(selection: &ComfyFieldSelection) -> Self {
        Self {
            label: selection.label.trim().to_string(),
            widget: selection
                .widget
                .as_deref()
                .filter(|raw| !raw.trim().is_empty())
                .and_then(Widget::parse),
            options: selection
                .options
                .iter()
                .map(|option| option.trim().to_string())
                .filter(|option| !option.is_empty())
                .collect(),
        }
    }
}

/// 预览返回的单个候选字段。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComfyFieldPreview {
    pub key: String,
    pub node_id: String,
    pub class_type: String,
    pub field_name: String,
    pub field_type: String,
    pub default: Option<Value>,
    pub media: bool,
    pub suggested: bool,
    /// 不写备注名时它会叫什么（节点标题 · 字段名，或类名 · 字段名）。
    /// 给界面当输入框的占位提示：让用户一眼看到"不写会长这样"。
    pub default_label: String,
}

/// 这次预览/导入**从引擎那儿拿到了多少**（决定控件是真控件还是通用控件）。
///
/// 为什么要报"没拿到"：拿不到时我们会静默退回通用控件（字符串→文本框、数字→没有范围的框），
/// 而用户看到的只是"这表单有点糙"。把原因说出来，他才知道"把 ComfyUI 起起来重新导入"
/// 能变好——不然这就是个看不见的降级。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaEnrichment {
    /// 这张图用到几个节点类。
    pub classes_used: usize,
    /// 其中几个问到了定义。
    pub classes_known: usize,
    /// 一个都没问到时的原因（连不上 / 驱动不支持 / 引擎不认识这些类）。
    pub reason: Option<String>,
}

impl SchemaEnrichment {
    pub fn enriched(&self) -> bool {
        self.classes_known > 0
    }
}

/// 预览的结果：字段清单 + 富化情况。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComfyPromptPreview {
    pub fields: Vec<ComfyFieldPreview>,
    pub enrichment: SchemaEnrichment,
}

/// 标签表：`字段键 -> 给人看的名字`（整张图算一次，**与用户勾了哪些无关**——
/// 这样同一条字段在预览里和导入后叫同一个名字）。
///
/// ⚠️ 两条字段可能**算出同一个名字**：实测那张图上有**两台 `LoadImage`**，标题都是「加载图像」、
/// 字段都叫 `image` ⇒ 两个框一模一样，而"哪个槽放哪张图"直接改变结果
/// （那张图干的就是"让图1的人物穿上图2人物的衣服"）。撞名时补上**节点号**：
/// 那是这条字段在**图里的地址**（数据里的事实），不是我们编的词 ——
/// 有了它，用户能回图里找到那台节点，也就分得清谁是谁。
fn label_map(candidates: &[ComfyCandidate]) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, String> = candidates
        .iter()
        .map(|candidate| (candidate.key.clone(), candidate.default_label()))
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for label in labels.values() {
        *counts.entry(label.clone()).or_default() += 1;
    }
    for candidate in candidates {
        let Some(label) = labels.get_mut(&candidate.key) else {
            continue;
        };
        if counts.get(label.as_str()).copied().unwrap_or(0) > 1 {
            *label = format!("{label}（节点 {}）", candidate.node_id);
        }
    }
    labels
}

/// 从标签表里取这条字段的名字（表里没有就现算一个，与表同一套规则）。
fn default_label_for(labels: &BTreeMap<String, String>, candidate: &ComfyCandidate) -> String {
    labels
        .get(&candidate.key)
        .cloned()
        .unwrap_or_else(|| candidate.default_label())
}

/// 解析 ComfyUI API Prompt，返回全部可参数化字段（不创建 Recipe），
/// 供导入前由用户二次筛选与备注。suggested 标记启发式推荐的字段。
pub fn preview_comfyui_fields(
    prompt: &Value,
    catalog: Option<&NodeInfoCatalog>,
) -> DomainResult<Vec<ComfyFieldPreview>> {
    let candidates = discover_comfyui_fields(prompt)?;
    let labels = label_map(&candidates);
    Ok(candidates
        .into_iter()
        .map(|candidate| {
            // 类型同样按引擎修正：预览里显示的类型要和导入后生成的一致
            // （两处不一致的话，用户在预览里看到的"数字"导入后可能变成文本框）。
            let spec = catalog
                .and_then(|catalog| catalog.input(&candidate.class_type, &candidate.field_name));
            ComfyFieldPreview {
                field_type: match spec {
                    Some(spec) => schema_type_from_engine(spec, &candidate.field_type),
                    None => candidate.field_type.clone(),
                },
                default_label: labels
                    .get(&candidate.key)
                    .cloned()
                    .unwrap_or_else(|| candidate.default_label()),
                key: candidate.key,
                node_id: candidate.node_id,
                class_type: candidate.class_type,
                field_name: candidate.field_name,
                default: candidate.default,
                media: matches!(candidate.relevance, Relevance::Media),
                suggested: !matches!(candidate.relevance, Relevance::Skip),
            }
        })
        .collect())
}

struct ComfyCandidate {
    key: String,
    node_id: String,
    class_type: String,
    field_name: String,
    field_type: String,
    default: Option<Value>,
    relevance: Relevance,
    /// 节点在 ComfyUI 里的标题（`_meta.title`）。
    ///
    /// 用户给节点起过名的话，导出的 API JSON 里每个节点都带着它——**这是白捡的可读标签**，
    /// 以前完全没用（于是界面上只能显示 `3::texts` 这种只有画图的人看得懂的东西）。
    node_title: Option<String>,
}

impl ComfyCandidate {
    /// 没有人工备注时，这条字段叫什么名字：**节点标题 · 字段名**。
    ///
    /// ⚠️ 只用**别人说过的话**（ComfyUI 给节点的标题 / 字段在数据里的原始名字），
    /// **一个字都不自己编**：
    ///
    /// * 标题取自 `_meta.title`（用户在 ComfyUI 里起过名就是那个名字，没起过就是引擎给节点的显示名）；
    /// * 字段名原样保留，所以标签**可追溯**——用户能拿它回到图里找到那个输入。
    ///
    /// 为什么这里不许写词典、不许从图里推断语义（2026-10-14 的教训）：那是**替用户起名字**。
    /// 想要人话名字是**用户的通道**——导入时写备注名、之后在编辑里改，一次就好、永久有效；
    /// 而我们编的词会漂、会盖住原始名字，出问题时用户连"这到底是哪个字段"都看不出来。
    fn default_label(&self) -> String {
        let owner = self
            .node_title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(&self.class_type);
        format!("{owner} · {}", self.field_name)
    }
}

/// 这份 JSON 看起来是 ComfyUI 的**工作流**格式（点「保存」导出的那种），不是 API 格式。
///
/// 两种导出长得完全不一样：
/// * 工作流格式（UI）：`{ "nodes": [...], "links": [...], "last_node_id": … }`
/// * API 格式：`{ "3": { "class_type": "KSampler", "inputs": {…} } }`
///
/// 为什么值得单独判一次：这两个菜单项在 ComfyUI 里**挨着**（「保存」/「保存(API 格式)」），
/// 点错是最高频的失误。而按普通流程走下去，UI 格式会被"每个条目都没有 `class_type`"全部跳过，
/// 最后报出「这份工作流里没有可参数化的输入字段（可能整张图都是连接引用）」——**那句话把人指向
/// 图内部**，而真正该做的是换一种导出方式（2026-10-13 实测：报错就是这么误导的）。
///
/// 判据只认 `nodes` 是数组 **且** 同时有 `links`：api 格式的节点键是数字（`"3"`），
/// 恰好叫 `nodes` 的节点不会同时带 `links`，所以这个组合不会误伤。
fn looks_like_ui_workflow(prompt: &Value) -> bool {
    let Some(object) = prompt.as_object() else {
        return false;
    };
    object.get("nodes").is_some_and(Value::is_array) && object.contains_key("links")
}

/// 遍历 workflow，发现全部可参数化的标量字段（不做相关性过滤）。
fn discover_comfyui_fields(prompt: &Value) -> DomainResult<Vec<ComfyCandidate>> {
    if looks_like_ui_workflow(prompt) {
        return Err(SafeError::validation(
            "COMFYUI_PROMPT_IS_UI_FORMAT",
            "这是 ComfyUI 的「工作流」格式（顶层有 nodes/links），不是「API 格式」——\
             请在 ComfyUI 里改用「保存(API 格式)」（Save (API Format)）重新导出一次",
        ));
    }
    let workflow = prompt.as_object().ok_or_else(|| {
        SafeError::validation(
            "COMFYUI_PROMPT_INVALID",
            "ComfyUI API Prompt 必须是节点对象",
        )
    })?;
    let mut candidates = Vec::new();
    for (node_id, node) in workflow {
        let Some(class_type) = node
            .get("class_type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(inputs) = node.get("inputs").and_then(Value::as_object) else {
            continue;
        };
        // `_meta.title`：ComfyUI 里给节点起过名的话就在这里（导出的 API 格式带着它）。
        let node_title = node
            .get("_meta")
            .and_then(|meta| meta.get("title"))
            .and_then(Value::as_str)
            .map(|title| title.to_string());
        for (field_name, value) in inputs {
            // 连接引用 ["6", 0] 或 [[...]] 嵌套引用：跳过。
            if is_connection_ref(value) {
                continue;
            }
            // list/object 等结构值当前不做参数，只暴露标量。
            if !is_scalar(value) {
                continue;
            }
            let relevance = comfyui_field_relevance(class_type, field_name, value);
            let (field_type, default) = comfyui_scalar_type(value);
            candidates.push(ComfyCandidate {
                key: format!("{node_id}::{field_name}"),
                node_id: node_id.clone(),
                class_type: class_type.to_string(),
                field_name: field_name.clone(),
                field_type,
                default,
                relevance,
                node_title: node_title.clone(),
            });
        }
    }
    if candidates.is_empty() {
        return Err(SafeError::validation(
            // 码从 `WEBAPP_NO_INPUTS` 改过来：那是**应用**那条路的词，出现在 ComfyUI 的
            // prompt 校验里属于命名漂移（同一个码两种含义，用户拿去搜会搜到另一件事）。
            "COMFYUI_PROMPT_NO_FIELDS",
            "该 API Prompt 没有可自动发现的输入字段",
        ));
    }
    Ok(candidates)
}

/// 参数发现：遍历 workflow，生成 schema。
/// selection 为 Some 时只保留用户勾选的字段，label 写入 description；
/// 为 None 时保持旧的启发式行为（跳过 Relevance::Skip）。
type ComfyBuiltSchema = (
    Value,
    BTreeMap<String, Value>,
    Vec<FieldPlan>,
    BTreeMap<String, Value>,
    BTreeMap<String, Value>,
);

/// 用引擎定义**修正**从 JSON 值猜出来的类型。
///
/// 值的形状会骗人：`seed` 写成 `42.0` 就成浮点框、`steps` 写成 `"20"` 就成文本框。
/// 引擎说的是权威（`INT`/`FLOAT`/`STRING`/`BOOLEAN`）；其余类型（`IMAGE`/`MODEL`…）
/// 表示这是个连线输入，此时值多半是标量字符串，保持按值判断。
fn schema_type_from_engine(spec: &NodeInputSpec, value_type: &str) -> String {
    match spec.kind.as_str() {
        "INT" => "integer".to_string(),
        "FLOAT" => "number".to_string(),
        "STRING" => "string".to_string(),
        "BOOLEAN" => "boolean".to_string(),
        "COMBO" if !spec.choices.is_empty() => "string".to_string(),
        _ => value_type.to_string(),
    }
}

/// 生成 schema。`catalog` 是**引擎侧的节点定义**（可选）：有它就能把"通用控件"升级成真控件
/// （枚举→下拉、数字→带范围、多行→多行框）。拿不到时必须退回"按 JSON 值的形状猜"的行为——
/// "手里有一张 JSON、离线也能导入"这个能力不能丢，只是控件会普通一些。
/// （单测走的就是 `None` 那条：见测试模块里的同名包装。）
fn build_comfyui_schema_with(
    prompt: &Value,
    selection: Option<&BTreeMap<String, ComfyFieldChoice>>,
    catalog: Option<&NodeInfoCatalog>,
) -> DomainResult<ComfyBuiltSchema> {
    let candidates = discover_comfyui_fields(prompt)?;
    let labels = label_map(&candidates);
    if let Some(selection) = selection {
        for key in selection.keys() {
            if !candidates.iter().any(|candidate| &candidate.key == key) {
                return Err(SafeError::validation(
                    "COMFYUI_FIELD_UNKNOWN",
                    format!("勾选的字段 {key} 在该 API Prompt 中不存在"),
                ));
            }
        }
    }
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let mut defaults = BTreeMap::new();
    let mut plans = Vec::new();
    let mut field_mapping = BTreeMap::new();
    let mut media_mapping = BTreeMap::new();

    for candidate in candidates {
        // 用户对这条字段的决定（勾没勾、备注名、控件、候选项）——没有勾选这条路时是 None。
        let choice: Option<&ComfyFieldChoice> =
            selection.and_then(|selection| selection.get(&candidate.key));
        let label = match selection {
            Some(_) => match choice {
                Some(choice) if !choice.label.is_empty() => choice.label.clone(),
                // 勾了但没写备注名 ⇒ 用默认标签（节点标题 · 字段名，撞名时带节点号），
                // 而不是甩一个 `3::texts`。
                Some(_) => default_label_for(&labels, &candidate),
                None => continue,
            },
            None => {
                if matches!(candidate.relevance, Relevance::Skip) {
                    continue;
                }
                default_label_for(&labels, &candidate)
            }
        };
        let ComfyCandidate {
            key,
            node_id,
            class_type,
            field_name,
            field_type,
            default,
            relevance,
            node_title: _,
        } = candidate;
        let has_default = default.is_some();
        // 引擎对这个输入怎么说（没有引擎定义时是 None ⇒ 全部退回按值猜）。
        let spec = catalog.and_then(|catalog| catalog.input(&class_type, &field_name));
        let field_type = match spec {
            Some(spec) => schema_type_from_engine(spec, &field_type),
            None => field_type,
        };
        let mut is_media = matches!(relevance, Relevance::Media);
        let mut schema = serde_json::Map::new();
        schema.insert("type".to_string(), json!(field_type));
        if is_media {
            schema.insert("format".to_string(), json!("file"));
        } else if let Some(spec) = spec {
            // 枚举：**这次升级的重点**——`sampler_name` 这类字段本来就该是下拉，
            // 而合法取值只有引擎知道（API prompt 里只有当前选中的那一个）。
            if !spec.choices.is_empty() {
                schema.insert("enum".to_string(), json!(spec.choices));
            }
            // 多行：组件认 `format: "textarea"`（它本来就实现了，只是以前没人喂过）。
            if spec.multiline {
                schema.insert("format".to_string(), json!("textarea"));
            }
            if field_type == "integer" || field_type == "number" {
                if let Some(min) = spec.min {
                    schema.insert("minimum".to_string(), json!(min));
                }
                if let Some(max) = spec.max {
                    schema.insert("maximum".to_string(), json!(max));
                }
                // `multipleOf` 是 JSON Schema 里对应"步长"的关键字。
                if let Some(step) = spec.step.filter(|step| *step > 0.0) {
                    schema.insert("multipleOf".to_string(), json!(step));
                }
            }
        }
        // ── 用户显式选过的控件：**他最大**（见 `ComfyFieldSelection` 的注释）──────────
        //
        // 优先级：**用户选的 > 引擎声明的 > 值的形状**。选了就照做，哪怕引擎说的是别的
        // （他知道自己的图：`aspect_ratio` 其实只有三种，或者他就想自由填）。
        // 来源记进 `x-widget-source`：① 界面可以如实说明"这是引擎给的/我们建议的/你选的"；
        // ② 以后我们改匹配逻辑时，**不许覆盖 source = user 的那些**。
        let source = if let Some(widget) = choice.and_then(|choice| choice.widget) {
            apply_widget(
                widget,
                choice.map(|c| c.options.as_slice()).unwrap_or(&[]),
                &mut schema,
            );
            is_media = widget == Widget::File;
            "user"
        } else if spec.is_some() {
            "engine"
        } else {
            "suggested"
        };
        schema.insert("x-widget-source".to_string(), json!(source));
        if !label.is_empty() {
            schema.insert("description".to_string(), json!(label.clone()));
        }
        if let Some(default_value) = &default {
            schema.insert("default".to_string(), default_value.clone());
        }
        properties.insert(key.clone(), Value::Object(schema));
        if !has_default {
            required.push(key.clone());
        }
        if let Some(value) = default.clone() {
            defaults.insert(key.clone(), value);
        }
        if is_media {
            // 键 = **字段键**（见 `build_input_schema` 的同款说明）。
            media_mapping.insert(
                key.clone(),
                json!({ "node": node_id, "field": field_name }),
            );
        } else {
            field_mapping.insert(key.clone(), json!({ "node": node_id, "field": field_name }));
        }
        plans.push(FieldPlan {
            key,
            field_name,
            field_type,
            description: label,
            description_en: String::new(),
            required: !has_default,
            default,
            constraints: Value::Object(serde_json::Map::new()),
            media: is_media,
        });
    }
    if properties.is_empty() {
        return Err(SafeError::validation(
            // 走到这里说明**发现到了字段、但按勾选一个都没留下** ⇒ 话得冲着勾选说。
            // 原来的文案是"这份工作流里没有可参数化的输入字段（可能整张图都是连接引用）"，
            // 那是**在怪图**——而用户只是什么都没勾（或者勾的都不在）。同一类毛病：
            // 报错把人指向一个他没做错的地方。
            "COMFYUI_SELECTION_EMPTY",
            "按当前勾选没有留下任何字段：至少要勾一个（如果勾选框里没有你想要的字段，\
             说明这份 prompt 里那些值都是连接引用，只有标量才能做成表单）",
        ));
    }
    let input_schema = json!({
        "type": "object",
        "required": required,
        "properties": properties,
    });
    Ok((input_schema, defaults, plans, field_mapping, media_mapping))
}

/// 判断值是否为上游节点连接引用（形如 ["6", 0] 或 [["6", 0]]）。
fn is_connection_ref(value: &Value) -> bool {
    let Value::Array(parts) = value else {
        return false;
    };
    if parts.is_empty() {
        return false;
    }
    let head_is_string = matches!(parts[0], Value::String(_));
    let head_is_nested_array = matches!(parts[0], Value::Array(_));
    // 形如 ["6", 0]：首元素字符串 → 连接引用。
    if head_is_string {
        return true;
    }
    // 形如 [["6", 0]]：嵌套数组引用。
    if head_is_nested_array {
        return is_connection_ref(&parts[0]);
    }
    false
}

/// 判断是否为可直接参数化的标量（string / number / boolean / null）。
fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null
    )
}

/// 从标量值推断字段类型并保留其默认值。
///
/// ⚠️ **猜出来的类型只能放宽，不能收紧**——这是本函数存在的意义，也是它的边界。
/// 所以数字一律给 `number`，**不给 `integer`**：
///
/// * JSON 里写 `1` 还是 `1.0` 取决于**写的人**，不是引擎的声明。ComfyUI 的 `cfg`/`denoise`
///   是 FLOAT，而图上写的就是 `1`；按"看着像整数"判成整数框 ⇒ 用户输入 `7.5` 会被
///   **我们自己的校验**判成"必须是整数"——**我们挡住了引擎接受的值**（2026-10-14 在真实的
///   RH 工作流上就是这么错的：`cfg`/`denoise` 双双被判成整数）。
/// * 反过来宽松的代价只是"整数框多收一个 8.5"，而那个值平台会明确拒掉——
///   **看得见的错，好过我们造出来的错**。
///
/// 真正的 `integer` 只来自**引擎的声明**（`/object_info` 的 `INT`，见 `schema_type_from_engine`）。
fn comfyui_scalar_type(value: &Value) -> (String, Option<Value>) {
    match value {
        Value::String(text) => ("string".to_string(), Some(Value::String(text.clone()))),
        Value::Bool(boolean) => ("boolean".to_string(), Some(Value::Bool(*boolean))),
        Value::Number(number) => ("number".to_string(), Some(Value::Number(number.clone()))),
        Value::Null => ("string".to_string(), None),
        _ => ("string".to_string(), Some(value.clone())),
    }
}

/// 接受纯数字 ID 或 RunningHub 应用链接，提取路径中最后一段数字作为 webappId。
fn parse_webapp_id(source: &str) -> DomainResult<String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(SafeError::validation(
            "SOURCE_REQUIRED",
            "请输入 RunningHub 应用链接或应用 ID",
        ));
    }
    // 去掉查询参数与锚点，再取**最后一个非空**的路径段。
    //
    // ⚠️ "非空"不是洁癖，是实测过的一个坑：取"最后一段"时，`…/post/<id>/`（尾斜杠）会拿到
    // 空串 ⇒ 一条完全合法的链接被判成"无法从链接中识别 RunningHub 应用 ID"（400 SOURCE_INVALID）。
    // 而从浏览器地址栏复制、或从手机分享出来的链接，尾斜杠都很常见——那是**用户的正常输入**，
    // 不该由他来替我们剪掉。判据按"剪贴板里可能是什么"定，不按"我们文档里写了什么形状"定。
    //
    // 顺带这条规则对"分享文案"也成立：整段粘"换装 换衣 https://…/post/<id> 复制此链接"时，
    // 最后一段里除了数字还有中文，滤完只剩 id（下面的 digits 过滤）——不是我们刻意支持的，
    // 但既然免费，就别把它弄坏。
    let path = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let candidate = path
        .rsplit('/')
        .find(|segment| !segment.trim().is_empty())
        .unwrap_or(path);
    let digits: String = candidate
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    if digits.len() < 5 {
        return Err(SafeError::validation(
            "SOURCE_INVALID",
            "无法从链接中识别 RunningHub 应用 ID",
        ));
    }
    Ok(digits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::execution::ProviderDescriptor;
    use crate::v2::domain::ids::ConnectionRevisionId;
    use crate::v2::domain::model::{
        Connection, ConnectionRevision, ConnectionStatus, LifecycleFeatures,
    };
    use crate::v2::infrastructure::fakes::{FakeProviderDriver, InMemoryRepositories};
    use crate::v2::ports::provider::{
        driver_key as provider_driver_key, DriverRegistry, NodeClassInfo,
    };

    /// 测试里的两条入口**默认没有引擎定义**（`catalog = None`）。
    ///
    /// 这是故意的：那条路是"手里有一张 JSON、离线也能导入"的降级路径，
    /// 它必须一直有效——所以绝大多数用例就该跑在它上面。要验富化就显式调 `super::*_with`。
    fn build_comfyui_schema(
        prompt: &Value,
        selection: Option<&BTreeMap<String, ComfyFieldChoice>>,
    ) -> DomainResult<ComfyBuiltSchema> {
        build_comfyui_schema_with(prompt, selection, None)
    }

    /// 测试里造"用户勾选"的最短写法：只给备注名（控件走默认）。
    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, ComfyFieldChoice> {
        pairs
            .iter()
            .map(|(key, label)| (key.to_string(), ComfyFieldChoice::label_only(label)))
            .collect()
    }

    /// 测试里造"用户选了控件"的写法。
    fn widget_choice(key: &str, widget: Widget, options: &[&str]) -> (String, ComfyFieldChoice) {
        (
            key.to_string(),
            ComfyFieldChoice {
                label: String::new(),
                widget: Some(widget),
                options: options.iter().map(|option| option.to_string()).collect(),
            },
        )
    }

    fn preview_comfyui_fields(prompt: &Value) -> DomainResult<Vec<ComfyFieldPreview>> {
        super::preview_comfyui_fields(prompt, None)
    }

    /// 一份"引擎侧"的节点定义，用来验富化。
    ///
    /// 这里**手搓**而不是调 `comfyui.rs` 的解析器：JSON → 规格 那一步是那个模块的职责
    /// （它有自己的单测），应用层只该依赖"规格长什么样"这个契约。
    fn engine_catalog() -> NodeInfoCatalog {
        let spec = |kind: &str,
                    choices: Vec<&str>,
                    min: Option<f64>,
                    max: Option<f64>,
                    step: Option<f64>,
                    multiline: bool| NodeInputSpec {
            kind: kind.to_string(),
            choices: choices
                .into_iter()
                .map(|choice| choice.to_string())
                .collect(),
            default: None,
            min,
            max,
            step,
            multiline,
        };
        let mut catalog = NodeInfoCatalog::default();
        catalog.classes.insert(
            "KSampler".to_string(),
            NodeClassInfo {
                display_name: "KSampler".to_string(),
                inputs: BTreeMap::from([
                    (
                        "seed".to_string(),
                        spec("INT", vec![], Some(0.0), Some(1.8e19), None, false),
                    ),
                    (
                        "steps".to_string(),
                        spec("INT", vec![], Some(1.0), Some(100.0), Some(1.0), false),
                    ),
                    (
                        "cfg".to_string(),
                        spec("FLOAT", vec![], Some(0.0), Some(100.0), Some(0.1), false),
                    ),
                    (
                        "sampler_name".to_string(),
                        spec(
                            "COMBO",
                            vec!["euler", "euler_ancestral", "dpmpp_2m"],
                            None,
                            None,
                            None,
                            false,
                        ),
                    ),
                ]),
            },
        );
        catalog.classes.insert(
            "CLIPTextEncode".to_string(),
            NodeClassInfo {
                display_name: "CLIP Text Encode (Prompt)".to_string(),
                inputs: BTreeMap::from([(
                    "text".to_string(),
                    spec("STRING", vec![], None, None, None, true),
                )]),
            },
        );
        catalog
    }

    /// 一条 RunningHub 连接 + 一个"平台上有这么一张工作流图"的替身驱动。
    fn runninghub_service(
        prompt: Value,
    ) -> (RecipeImportService, Arc<InMemoryRepositories>, ConnectionId) {
        let repositories = Arc::new(InMemoryRepositories::new());
        let connection_id = ConnectionId(1);
        repositories.seed_connection(
            Connection {
                id: connection_id,
                vendor: "runninghub".to_string(),
                protocol: "runninghub".to_string(),
                display_name: "RunningHub".to_string(),
                current_revision_id: ConnectionRevisionId(1),
                status: ConnectionStatus::Active,
            },
            vec![ConnectionRevision {
                id: ConnectionRevisionId(1),
                connection_id,
                config: BTreeMap::from([
                    ("baseUrl".to_string(), json!("https://www.runninghub.cn")),
                    ("region".to_string(), json!("cn")),
                ]),
                secret_bindings: vec![],
                effective_capabilities: vec![CAPABILITY_WORKFLOW.to_string()],
                lifecycle_features: LifecycleFeatures::default(),
                created_at: Timestamp::now(),
            }],
        );
        let mut drivers = DriverRegistry::new();
        drivers.register(
            provider_driver_key("runninghub", "runninghub"),
            Arc::new(
                FakeProviderDriver::new(ProviderDescriptor {
                    vendor_id: "runninghub".to_string(),
                    protocol_id: "runninghub".to_string(),
                    display_name: "RunningHub".to_string(),
                    business_capabilities: vec![CAPABILITY_WORKFLOW.to_string()],
                    lifecycle_features: LifecycleFeatures::default(),
                    security_requirements: Default::default(),
                    connection_schema: json!({}),
                    binding_schema: json!({}),
                })
                .with_workflow_prompt(prompt),
            ),
        );
        let service = RecipeImportService::new(
            repositories.clone(),
            repositories.clone(),
            Arc::new(drivers),
        );
        (service, repositories, connection_id)
    }

    fn sample_prompt() -> Value {
        json!({
            "6": { "class_type": "CLIPTextEncode", "inputs": { "text": "a red house" } },
            "7": { "class_type": "EmptyLatentImage", "inputs": { "width": 1024, "height": 1024 } },
            "8": { "class_type": "KSampler", "inputs": { "steps": 20, "seed": 42 } },
            "9": { "class_type": "SaveImage", "inputs": { "images": ["8", 0] } }
        })
    }

    /// 导入工作流必须落成**原生 RunningHub 配方**：存 `workflowId`、定义留在平台上
    /// （`Referenced`）、字段全暴露且不带备注名。
    ///
    /// 为什么这条重要：这条路此前**只有界面够得着**（而且界面的那条会落成 comfyui 配方），
    /// CLI/MCP 完全拿不到 RH 工作流——"只有 MCP 没有界面时，这个工作流根本进不来"就是它。
    /// 判别点四条：driver 是 runninghub、配置里是 workflowId（不是
    /// webappId、也不是整张图）、source 是 Referenced、字段来自这张图。
    #[tokio::test]
    async fn imports_workflow_as_a_native_runninghub_recipe() {
        let (service, repositories, connection_id) = runninghub_service(sample_prompt());
        let imported = service
            .import_workflow(
                connection_id,
                "https://www.runninghub.cn/post/1904136902449209346",
                Some("我的工作流".to_string()),
                None,
            )
            .await
            .expect("导入应当成功");

        assert_eq!(imported.name, "我的工作流");
        let (recipe, binding) = repositories
            .list_recipes(10, None)
            .await
            .expect("列表")
            .into_iter()
            .find(|(recipe, _)| recipe.id.0 == imported.recipe_id)
            .expect("新导入的配方应当在库里");
        // 显式限定走哪个 trait：`InMemoryRepositories` 同时实现了连接与配方两个仓储，
        // 两边都有 `find_revision`，不限定会被编译器判为歧义。
        let revision =
            RecipeRepository::find_revision(repositories.as_ref(), recipe.current_revision_id)
                .await
                .expect("查 revision")
                .expect("revision 应当在");

        assert_eq!(recipe.capability, CAPABILITY_WORKFLOW);
        assert_eq!(binding.driver_key, "runninghub/runninghub");
        assert_eq!(
            binding.provider_config.get("workflowId"),
            Some(&json!("1904136902449209346")),
            "原生工作流存的是 workflowId"
        );
        assert!(
            !binding.provider_config.contains_key("webappId"),
            "不要把它当成 AI 应用"
        );
        assert!(
            !binding.provider_config.contains_key("workflow"),
            "定义留在平台上：不要把整张图复制进我们的库（会漂移）"
        );
        assert_eq!(revision.source, RecipeSource::Referenced);
        // 字段全暴露（`text` / `width` / `height` 都是可参数化候选），且**没有人工备注名**
        // ——但每一条都有**默认标签**（`类名 · 字段名`）：留空会让界面只能显示 `3::texts`，
        // 那种名字只有画图的人看得懂。
        let properties = revision.input_schema["properties"]
            .as_object()
            .expect("应有字段");
        assert!(
            properties.keys().any(|key| key.ends_with("::text")),
            "文本字段应当暴露出来：{properties:?}"
        );
        assert!(
            properties.values().all(|schema| schema
                .get("description")
                .and_then(|value| value.as_str())
                // 机械导入不写人工备注，但默认标签必须**可追溯**：`节点标题 · 字段名`，
                // 于是它一定含原始字段名（用户要拿它回图里找、也要对着它写备注名）。
                .is_some_and(|label| !label.is_empty() && label.contains(" · "))),
            "机械导入不写人工备注，但默认标签必须可追溯到字段名：{properties:?}"
        );
        // 映射是驱动认的那个形状：{ inputKey: { node, field } }
        let mapping = binding.provider_config["fieldMapping"]
            .as_object()
            .expect("应有 fieldMapping");
        let (_, entry) = mapping.iter().next().expect("至少一条映射");
        assert!(entry.get("node").is_some() && entry.get("field").is_some());
    }

    /// **用户的选择能到达 RunningHub 工作流这条路**（以前到不了：这条没有"选择参数"那一步）。
    ///
    /// 判据是边界那一条：我们只给默认值；用户勾了什么、叫什么、用什么控件，他说了算。
    /// 负对照：把 `import_workflow` 里的 `selection` 换回 `None` ⇒ 这条会红。
    #[tokio::test]
    async fn workflow_import_honours_the_users_field_choices() {
        let (service, repositories, connection_id) = runninghub_service(sample_prompt());
        let imported = service
            .import_workflow(
                connection_id,
                "https://www.runninghub.cn/post/1904136902449209346",
                None,
                Some(vec![
                    // 只勾提示词：其余字段（含启发式本来会暴露的）都不该出现。
                    ComfyFieldSelection {
                        key: "6::text".to_string(),
                        label: "我的提示词".to_string(),
                        widget: None,
                        options: vec![],
                    },
                    // 用户知道 `8::steps` 常用的就三个值 ⇒ 他自己做下拉。
                    ComfyFieldSelection {
                        key: "8::steps".to_string(),
                        label: String::new(),
                        widget: Some("dropdown".to_string()),
                        options: vec!["8".to_string(), "20".to_string(), "30".to_string()],
                    },
                ]),
            )
            .await
            .expect("导入应当成功");
        let (recipe, _) = repositories
            .list_recipes(10, None)
            .await
            .expect("列表")
            .into_iter()
            .find(|(recipe, _)| recipe.id.0 == imported.recipe_id)
            .expect("新导入的配方应当在库里");
        let revision =
            RecipeRepository::find_revision(repositories.as_ref(), recipe.current_revision_id)
                .await
                .expect("查 revision")
                .expect("revision 应当在");
        let properties = revision.input_schema["properties"].as_object().unwrap();
        assert_eq!(properties["6::text"]["description"], "我的提示词");
        assert_eq!(properties["8::steps"]["enum"], json!(["8", "20", "30"]));
        assert_eq!(properties["8::steps"]["x-widget-source"], "user");
        assert_eq!(properties.len(), 2, "勾了两条就只留两条：{properties:?}");
    }

    /// 同一个工作流导入两次必须被拦，且**在网络调用之前**（离线判据）——与 webapp 那条同款。
    #[tokio::test]
    async fn refuses_to_import_the_same_workflow_twice() {
        let (service, repositories, connection_id) = runninghub_service(sample_prompt());
        let source = "https://www.runninghub.cn/post/1904136902449209346";
        service
            .import_workflow(connection_id, source, None, None)
            .await
            .expect("第一次导入应当成功");

        let error = service
            .import_workflow(connection_id, source, None, None)
            .await
            .expect_err("同一个工作流不该被导入两次");
        assert_eq!(error.code, "WORKFLOW_ALREADY_IMPORTED");
        assert!(
            error
                .message
                .contains("RunningHub 工作流 1904136902449209346"),
            "错误里要指名已有的那一份：{}",
            error.message
        );
        assert_eq!(
            repositories
                .list_recipes(10, None)
                .await
                .expect("列表")
                .len(),
            1,
            "拒绝之后不该留下第二份"
        );
    }

    fn node(
        node_id: &str,
        field_name: &str,
        field_type: &str,
        field_value: Option<&str>,
        field_data: Value,
    ) -> crate::v2::ports::provider::WebappNodeInfo {
        crate::v2::ports::provider::WebappNodeInfo {
            node_id: node_id.to_string(),
            field_name: field_name.to_string(),
            field_type: field_type.to_string(),
            field_value: field_value.map(|value| json!(value)),
            description: format!("描述{field_name}"),
            description_en: String::new(),
            field_data,
        }
    }

    fn schema() -> WebappSchema {
        WebappSchema {
            webapp_id: "2063962442769715202".to_string(),
            webapp_name: "文生图示例".to_string(),
            description: "示例应用".to_string(),
            description_en: String::new(),
            nodes: vec![
                node(
                    "136",
                    "prompt",
                    "STRING",
                    None,
                    json!(["STRING", { "multiline": true }]),
                ),
                node(
                    "142",
                    "width",
                    "INT",
                    Some("720"),
                    json!(["INT", { "min": 16, "max": 16384 }]),
                ),
                node("148", "batch", "INT", Some("1"), json!(["INT", {}])),
                node("149", "zoom", "BOOLEAN", None, json!(["BOOLEAN"])),
            ],
        }
    }

    /// 同一个 webapp 导入两次必须被拦，且错误要**指名**已有的那一份。
    ///
    /// 起因是实测：用户库里同一个 RunningHub 应用（`webappId 2084540538883235841`）
    /// 存在两份 Recipe、相隔 13 天导入，**没有任何东西拦它**。
    ///
    /// 判别点三条：
    /// 1. 已存在同一 `(driver, webappId)` ⇒ 拒绝，且**不产生第二份**；
    /// 2. 错误里带上已有那一份的 **id 与名字**——两种正当意图（"用它"/"刷新它"）
    ///    都需要先知道它已经存在，光说"重复了"不够；
    /// 3. **别的 webapp 不受影响**（反向控制）。少了它，一个"永远拒绝导入"的实现也能过前两条。
    ///
    /// 判据用 `(driver, webappId)` 而不是名字：名字用户能改，webappId 不会。
    ///
    /// 这条测试完全**离线**：守卫在网络调用之前触发（这是刻意的——重复导入不该先花一次请求）。
    #[tokio::test]
    async fn refuses_to_import_the_same_webapp_twice() {
        use crate::v2::domain::model::{RecipeBinding, RecipeVisibility};
        use crate::v2::infrastructure::fakes::InMemoryRepositories;
        use crate::v2::ports::provider::DriverRegistry;

        let repositories = InMemoryRepositories::new();
        let already = "2084540538883235841";
        repositories.seed_recipe(
            Recipe {
                id: RecipeId(7),
                name: "Krea2-turbo-文生图".to_string(),
                description: String::new(),
                labels: vec![],
                capability: CAPABILITY_WORKFLOW.to_string(),
                current_revision_id: RecipeRevisionId(7),
                visibility: RecipeVisibility::Private,
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
                favorite: false,
                group: None,
                note: None,
                last_opened_at: None,
                field_labels: Default::default(),
                removed_at: None,
            },
            vec![RecipeRevision {
                id: RecipeRevisionId(7),
                recipe_id: RecipeId(7),
                revision: 1,
                input_schema: json!({ "type": "object", "properties": {} }),
                output_schema: json!({ "type": "object" }),
                binding: RecipeBinding {
                    // ⚠️ 这里刻意用**历史裸键**：真实库里那批老 recipe 就是这个形态
                    //（新导入写 `runninghub/runninghub`）。实测过：守卫只认新形态时，
                    // 它会**恰好对最该保护的老数据失效**，而当时的单测种的是新格式、
                    // 因此是绿的——**为错误的理由通过**。这条现在钉住老格式也必须被认出来。
                    driver_key: "runninghub".to_string(),
                    provider_config: BTreeMap::from([("webappId".to_string(), json!(already))]),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1)],
                    connection_override: Default::default(),
                },
                defaults: Default::default(),
                source: RecipeSource::Embedded,
                created_at: Timestamp::now(),
            }],
        );

        let service = RecipeImportService::new(
            Arc::new(repositories.clone()),
            Arc::new(repositories.clone()),
            Arc::new(DriverRegistry::new()),
        );

        // 1 + 2：同一 webapp ⇒ 拒绝，且指名已有的那一份。
        let error = service
            .import(ConnectionId(1), already)
            .await
            .expect_err("同一个 webapp 不该被导入两次");
        assert_eq!(error.code, "WEBAPP_ALREADY_IMPORTED");
        assert!(
            error.message.contains("#7") && error.message.contains("Krea2-turbo-文生图"),
            "要说清是**哪一份**已经存在，光说「重复」不够：{}",
            error.message
        );

        // 3：反向控制——别的 webapp 不该被这条守卫误拦。
        // 它会走到网络那一步（registry 是空的 ⇒ 报错），但**错误码必须不是重复**。
        let other = service
            .import(ConnectionId(1), "2085996627512352770")
            .await
            .expect_err("没有 driver 时应当失败");
        assert_ne!(
            other.code, "WEBAPP_ALREADY_IMPORTED",
            "另一个 webapp 不该被判成重复：{}",
            other.message
        );
    }

    #[test]
    fn parses_plain_id_and_urls() {
        assert_eq!(
            parse_webapp_id("2063962442769715202").unwrap(),
            "2063962442769715202"
        );
        assert_eq!(
            parse_webapp_id("https://www.runninghub.cn/workflow/detail/2063962442769715202")
                .unwrap(),
            "2063962442769715202"
        );
        assert_eq!(
            parse_webapp_id(
                "https://www.runninghub.cn/workflow/detail/2063962442769715202?tab=info"
            )
            .unwrap(),
            "2063962442769715202"
        );
        // 尾斜杠（地址栏复制、手机分享的常见形态）。第一条在改成"跳过空段"之前**会红**：
        // 那时"最后一段"是空串，一条合法链接被判成无法识别。
        assert_eq!(
            parse_webapp_id("https://www.runninghub.cn/post/2063962442769715202/").unwrap(),
            "2063962442769715202"
        );
        assert_eq!(
            parse_webapp_id("https://www.runninghub.cn/post/2063962442769715202//").unwrap(),
            "2063962442769715202"
        );
        assert_eq!(
            parse_webapp_id("https://www.runninghub.cn/post/2063962442769715202/?tab=info")
                .unwrap(),
            "2063962442769715202"
        );
        // 整段分享文案：数字以外的字被滤掉，所以照样认得出。
        assert_eq!(
            parse_webapp_id(
                "换装 换衣 https://www.runninghub.cn/post/2063962442769715202 复制此链接"
            )
            .unwrap(),
            "2063962442769715202"
        );
        // 负对照：跳过空段 ≠ 放宽判据——只有目录、没有 id 的链接仍然要拒。
        assert!(parse_webapp_id("https://www.runninghub.cn/post/").is_err());
        assert!(parse_webapp_id("https://www.runninghub.cn/post//").is_err());
        assert!(parse_webapp_id("not-a-link").is_err());
        assert!(parse_webapp_id("").is_err());
    }

    #[test]
    fn builds_schema_with_defaults_and_limits() {
        let (input_schema, defaults, fields, field_mapping, media_mapping) =
            build_input_schema(&schema()).unwrap();
        let properties = input_schema["properties"].as_object().unwrap();
        assert_eq!(properties["136::prompt"]["type"], "string");
        assert_eq!(properties["136::prompt"]["format"], "textarea");
        assert_eq!(properties["142::width"]["type"], "integer");
        assert_eq!(properties["142::width"]["minimum"], 16);
        assert_eq!(properties["142::width"]["maximum"], 16384);
        assert_eq!(properties["149::zoom"]["type"], "boolean");
        let required = input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("136::prompt")));
        assert!(required.contains(&json!("149::zoom")));
        assert!(!required.contains(&json!("142::width")));
        assert_eq!(defaults["142::width"], json!(720));
        assert_eq!(defaults["148::batch"], json!(1));
        assert_eq!(fields.len(), 4);
        assert!(field_mapping.contains_key("136::prompt"));
        assert!(field_mapping.contains_key("142::width"));
        assert!(media_mapping.is_empty());
    }

    #[test]
    fn maps_media_inputs_to_file_fields_and_media_mapping() {
        let mut schema = schema();
        schema
            .nodes
            .push(node("17", "image", "IMAGE", None, json!(["IMAGE", {}])));
        let (input_schema, _defaults, fields, field_mapping, media_mapping) =
            build_input_schema(&schema).unwrap();
        let properties = input_schema["properties"].as_object().unwrap();
        assert_eq!(properties["17::image"]["type"], "string");
        assert_eq!(properties["17::image"]["format"], "file");
        assert!(input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("17::image")));
        assert!(!field_mapping.contains_key("17::image"));
        assert_eq!(
            media_mapping["17::image"],
            json!({ "node": "17", "field": "image" })
        );
        assert!(fields
            .iter()
            .any(|field| field.media && field.key == "17::image"));
    }

    /// ⚠️ 回归（用户场景）：媒体映射的键是**字段键**，不是位置槽。此前它按上游节点
    /// 声明顺序生成 `asset-0..N`，而上游顺序 / schema 键序 / 调用方顺序各自漂移，于是
    /// 4 槽换装配方在界面上第二格其实是 519（图4）。现在键就是节点::字段，读的人不需要
    /// 知道任何顺序。
    #[test]
    fn media_mapping_is_keyed_by_field_key_not_by_position() {
        let mut schema = schema();
        // 故意让上游声明顺序与键的字典序**不一致**：槽位语义完全由键决定，顺序不参与。
        schema
            .nodes
            .push(node("475", "image", "IMAGE", None, json!(["IMAGE", {}])));
        schema
            .nodes
            .push(node("519", "image", "IMAGE", None, json!(["IMAGE", {}])));
        schema
            .nodes
            .push(node("90", "video", "VIDEO", None, json!(["VIDEO", {}])));
        let (_input_schema, _defaults, _fields, _field_mapping, media_mapping) =
            build_input_schema(&schema).unwrap();
        assert_eq!(
            media_mapping["475::image"],
            json!({ "node": "475", "field": "image" })
        );
        assert_eq!(
            media_mapping["519::image"],
            json!({ "node": "519", "field": "image" })
        );
        assert_eq!(
            media_mapping["90::video"],
            json!({ "node": "90", "field": "video" })
        );
        assert!(
            !media_mapping.keys().any(|key| key.starts_with("asset-")),
            "新导入不许再写位置槽：{media_mapping:?}"
        );
    }

    // ---------- ComfyUI API Prompt 参数发现 ----------

    fn comfyui_prompt_fixture() -> Value {
        json!({
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "seed": 156680208,
                    "steps": 20,
                    "cfg": 8.0,
                    "positive": ["6", 0],
                    "latent_image": ["5", 0]
                }
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "a red house in snow, minimal style" }
            },
            "17": {
                "class_type": "LoadImage",
                "inputs": { "image": "example.png" }
            },
            "9": {
                "class_type": "SaveImage",
                "inputs": { "filename_prefix": "ComfyUI", "images": ["3", 0] }
            }
        })
    }

    #[test]
    fn comfyui_discovers_sampler_and_text_fields() {
        let (input_schema, defaults, fields, field_mapping, media_mapping) =
            build_comfyui_schema(&comfyui_prompt_fixture(), None).unwrap();
        let properties = input_schema["properties"].as_object().unwrap();
        // 提示词要暴露（`CLIPTextEncode.text`）。
        assert_eq!(properties["6::text"]["type"], "string");
        // ⚠️ 采样旋钮**不默认暴露**（列在导入对话框里，用户想勾再勾）：
        // 判定依据是用户行为——绝大多数人不动 cfg/seed/steps（见 `comfyui_field_relevance`）。
        assert!(!properties.contains_key("3::seed"));
        assert!(!properties.contains_key("3::steps"));
        assert!(!properties.contains_key("3::cfg"));
        // 连接引用（positive/latent_image）不应暴露。
        assert!(!properties.contains_key("3::positive"));
        assert!(!properties.contains_key("3::latent_image"));
        // 非常用参数（SaveImage 的 filename_prefix）不应暴露。
        assert!(!properties.contains_key("9::filename_prefix"));
        // media 字段不应进 fieldMapping，但进 mediaMapping。
        assert!(!field_mapping.contains_key("17::image"));
        assert!(
            !properties.contains_key("17::image") || properties["17::image"]["format"] == "file"
        );
        assert_eq!(
            media_mapping["17::image"],
            json!({ "node": "17", "field": "image" })
        );
        // defaults 只保留**被暴露的**字段的默认值：旋钮没暴露，它们的默认值自然也不在这里
        // （图的默认值仍在配方里，只是不当作表单默认值）。
        assert!(!defaults.contains_key("3::steps"));
        assert!(!defaults.contains_key("3::cfg"));
        assert_eq!(fields.len(), 2); // text + image(media)
                                     // 旋钮要**列在候选里**（`suggested: false`），用户勾了才进表单。
        let candidates = preview_comfyui_fields(&comfyui_prompt_fixture()).unwrap();
        let steps = candidates.iter().find(|f| f.key == "3::steps").unwrap();
        assert!(!steps.suggested);
        // 勾选之后照旧能进表单、能写进 fieldMapping（策展那条路不受影响）。
        let selection = labels(&[("3::steps", "步数")]);
        let (schema, defaults, _, mapping, _) =
            build_comfyui_schema(&comfyui_prompt_fixture(), Some(&selection)).unwrap();
        assert_eq!(schema["properties"]["3::steps"]["description"], "步数");
        assert_eq!(defaults["3::steps"], json!(20));
        assert_eq!(
            mapping["3::steps"],
            json!({ "node": "3", "field": "steps" })
        );
    }

    #[test]
    fn comfyui_rejects_empty_or_non_object_prompt() {
        let err = build_comfyui_schema(&json!([]), None).unwrap_err();
        assert_eq!(err.code, "COMFYUI_PROMPT_INVALID");
        let err = build_comfyui_schema(&json!({}), None).unwrap_err();
        assert_eq!(err.code, "COMFYUI_PROMPT_NO_FIELDS");
    }

    /// 点错菜单项（「保存」而不是「保存(API 格式)」）必须**说清是哪种格式错了**。
    ///
    /// 这条钉的是一句会被误读的报错：UI 格式走普通流程会全部字段被跳过，最后报
    /// "没有可参数化的输入字段（可能整张图都是连接引用）"——用户会去图里找"连接引用"，
    /// 而他要做的是换个导出方式。负对照：去掉 `looks_like_ui_workflow` 那一判 ⇒ 这条会红。
    #[test]
    fn comfyui_names_the_ui_format_mistake_instead_of_blaming_the_graph() {
        let ui_workflow = json!({
            "id": "abc",
            "revision": 0,
            "last_node_id": 9,
            "last_link_id": 9,
            "nodes": [{ "id": 3, "type": "KSampler", "pos": [1, 2], "inputs": [], "outputs": [] }],
            "links": [],
            "groups": [],
            "config": {},
            "extra": {},
            "version": 0.4
        });
        let err = build_comfyui_schema(&ui_workflow, None).unwrap_err();
        assert_eq!(err.code, "COMFYUI_PROMPT_IS_UI_FORMAT");
        assert!(
            err.message.contains("保存(API 格式)"),
            "要把**正确的那个菜单项**说出来：{}",
            err.message
        );
        // 预览那条路走的是同一个判据（两处各判一次迟早漂）。
        let err = preview_comfyui_fields(&ui_workflow).unwrap_err();
        assert_eq!(err.code, "COMFYUI_PROMPT_IS_UI_FORMAT");
    }

    /// 边界：`nodes` 是**节点名**而不是 UI 格式的数组时，不许误判。
    #[test]
    fn comfyui_does_not_mistake_a_node_named_nodes_for_the_ui_format() {
        let prompt = json!({
            "7": { "class_type": "ETN_LoadImageBase64", "inputs": { "nodes": "hello" } }
        });
        let fields = preview_comfyui_fields(&prompt).expect("这是合法的 API 格式，不该被挡");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].key, "7::nodes");
    }

    #[test]
    fn comfyui_preview_lists_all_scalar_fields_with_suggestions() {
        let fields = preview_comfyui_fields(&comfyui_prompt_fixture()).unwrap();
        // 全部标量字段都应出现在预览里，含启发式跳过的 filename_prefix。
        let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
        assert!(keys.contains(&"3::seed"));
        assert!(keys.contains(&"6::text"));
        assert!(keys.contains(&"17::image"));
        assert!(keys.contains(&"9::filename_prefix"));
        let suggested = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .unwrap()
                .suggested
        };
        // 候选**列全**（连旋钮都列），但默认勾的只有"用户会改的"那几个。
        assert!(suggested("6::text"));
        assert!(suggested("17::image"));
        assert!(!suggested("3::seed"), "旋钮不默认勾：{fields:?}");
        assert!(!suggested("3::steps"));
        assert!(!suggested("3::cfg"));
        assert!(!suggested("9::filename_prefix"));
        let image = fields
            .iter()
            .find(|field| field.key == "17::image")
            .unwrap();
        assert!(image.media);
        // 连接引用不进入候选。
        assert!(!keys.contains(&"3::positive"));
    }

    #[test]
    fn comfyui_selection_filters_fields_and_applies_labels() {
        let selection = labels(&[("6::text", "正向提示词"), ("3::seed", "")]);
        let (input_schema, _defaults, fields, field_mapping, media_mapping) =
            build_comfyui_schema(&comfyui_prompt_fixture(), Some(&selection)).unwrap();
        let properties = input_schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 2);
        assert_eq!(properties["6::text"]["description"], "正向提示词");
        // 勾了但没写备注名 ⇒ **给默认标签**（类名 · 字段名），而不是留空让界面显示 `3::seed`。
        // 节点在 ComfyUI 里起过名的话用那个名字（`_meta.title`），见下一条用例。
        assert_eq!(properties["3::seed"]["description"], "KSampler · seed");
        assert!(!properties.contains_key("3::steps"));
        assert!(!properties.contains_key("17::image"));
        assert_eq!(fields.len(), 2);
        assert!(media_mapping.is_empty());
        assert!(field_mapping.contains_key("6::text"));
        // 勾选启发式跳过的字段同样生效。
        let mut with_extra = selection.clone();
        with_extra.insert(
            "9::filename_prefix".to_string(),
            ComfyFieldChoice::label_only("文件名前缀"),
        );
        let (schema, _, _, _, _) =
            build_comfyui_schema(&comfyui_prompt_fixture(), Some(&with_extra)).unwrap();
        assert_eq!(
            schema["properties"]["9::filename_prefix"]["description"],
            "文件名前缀"
        );
    }

    #[test]
    fn comfyui_selection_rejects_unknown_key() {
        let selection = labels(&[("999::nope", "")]);
        let err = build_comfyui_schema(&comfyui_prompt_fixture(), Some(&selection)).unwrap_err();
        assert_eq!(err.code, "COMFYUI_FIELD_UNKNOWN");
        let empty: BTreeMap<String, ComfyFieldChoice> = BTreeMap::new();
        let err = build_comfyui_schema(&comfyui_prompt_fixture(), Some(&empty)).unwrap_err();
        // 一个都没勾 ⇒ 说"你没勾"，不许说"图里没有字段"（后者会把人骗去改图）。
        assert_eq!(err.code, "COMFYUI_SELECTION_EMPTY");
        assert!(
            err.message.contains("至少要勾一个"),
            "话要冲着勾选说：{}",
            err.message
        );
    }

    /// **有引擎定义时**：枚举变下拉、数字带范围与步长、多行文本变多行框、类型按引擎修正。
    ///
    /// 这条钉的是这次升级的核心：`sampler_name` 这类字段本来就该是下拉，而合法取值
    /// 只有引擎知道（API prompt 里只有当前选中的那一个）。
    #[test]
    fn comfyui_uses_engine_definitions_when_available() {
        let catalog = engine_catalog();
        let prompt = json!({
            "3": {
                "class_type": "KSampler",
                "_meta": { "title": "采样器" },
                "inputs": {
                    "seed": 42,
                    "steps": 20,
                    "cfg": 8.0,
                    "sampler_name": "euler",
                    "positive": ["6", 0]
                }
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "a red house" }
            }
        });
        // 用"全勾上、但都不写备注名"的 selection：既覆盖 `sampler_name` 这类**启发式默认跳过**
        // 的字段（不勾就不暴露），又顺带验默认标签。
        let selection = labels(&[
            ("3::seed", ""),
            ("3::steps", ""),
            ("3::cfg", ""),
            ("3::sampler_name", ""),
            ("6::text", ""),
        ]);
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), Some(&catalog)).unwrap();
        let properties = schema["properties"].as_object().unwrap();

        // 枚举 → `enum`（组件见到 enum 就渲染成 select）。
        assert_eq!(
            properties["3::sampler_name"]["enum"],
            json!(["euler", "euler_ancestral", "dpmpp_2m"])
        );
        // 数字 → 范围与步长（`multipleOf` 是 JSON Schema 里的步长）。
        assert_eq!(properties["3::steps"]["minimum"], json!(1.0));
        assert_eq!(properties["3::steps"]["maximum"], json!(100.0));
        assert_eq!(properties["3::steps"]["multipleOf"], json!(1.0));
        assert_eq!(properties["3::cfg"]["minimum"], json!(0.0));
        assert_eq!(properties["3::cfg"]["type"], "number");
        // 多行 → `format: textarea`（组件本来就实现了这条分支，只是以前没人喂过）。
        assert_eq!(properties["6::text"]["format"], "textarea");
        // 标签：`节点标题 · 字段名`——**只用别人说过的话**（ComfyUI 的节点标题 + 字段的原始名字），
        // 我们一个字都不编（想改叫法是用户的通道：备注名）。
        assert_eq!(properties["3::steps"]["description"], "采样器 · steps");
        assert_eq!(
            properties["6::text"]["description"],
            "CLIPTextEncode · text"
        );
        // 连接引用仍然被跳过（不是标量）。
        assert!(!properties.contains_key("3::positive"));
    }

    /// 节点标题（`_meta.title`）是**白捡的可读标签**：ComfyUI 里起过名的话导出的 JSON 就带着它。
    /// 负对照：把 `default_label` 里的 `node_title` 去掉 ⇒ 这条会红。
    #[test]
    fn comfyui_prefers_the_node_title_for_the_default_label() {
        let prompt = json!({
            "6": {
                "class_type": "CLIPTextEncode",
                "_meta": { "title": "正向提示词" },
                "inputs": { "text": "a red house" }
            }
        });
        let fields = preview_comfyui_fields(&prompt).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].default_label, "正向提示词 · text");
    }

    /// 默认暴露必须**围着用户会改的东西**转，而不是围着"名字眼熟的技术旋钮"转。
    ///
    /// 这条用的是真实工作流的形状（RH 的 `Qwen Image 2.1`）：提示词躺在
    /// `PrimitiveStringMultiline.value` 里（名字毫无线索），`negative_prompt` 名字比 `prompt` 长，
    /// 画幅用 `aspect_ratio`/`megapixels`/`resolution` 而不是 `width`/`height`。
    /// 旧规则在**这四个上都判错**：它只认 `text`/`prompt` 全等与 `width`/`height`，
    /// 结果把两百字的提示词藏了、把 cfg/denoise/seed/steps/batch_size 摆上了第一屏。
    ///
    /// 负对照：把 `comfyui_field_relevance` 的"看值"那一条去掉 ⇒ 这条的第一个断言会红。
    #[test]
    fn default_exposure_follows_what_users_actually_change() {
        let prompt = json!({
            "12": { "class_type": "PrimitiveStringMultiline",
                    "inputs": { "value": "真人摄像，人像摄影，性感美丽的中国美女，白色吊带紧身碎花连衣裙，站在海边的礁石上，近景，橘色光" } },
            "9": { "class_type": "TextEncodeQwenImage21",
                   "inputs": { "negative_prompt": "", "resolution": 1024 } },
            // 提示词重写器**消费**了 12 的输出 ⇒ 12 那个框就是提示词（从图里推出来的）。
            "19": { "class_type": "RH_QwenImagePromptEnhancer",
                    "inputs": { "prompt": ["12", 0], "style": "none" } },
            "6": { "class_type": "ResolutionSelector",
                   "inputs": { "aspect_ratio": "9:16 (Portrait Widescreen)", "megapixels": 1, "multiple": 8 } },
            "8": { "class_type": "KSampler",
                   "inputs": { "seed": 42, "steps": 8, "cfg": 1, "denoise": 1,
                               "sampler_name": "euler", "scheduler": "simple" } },
            "2": { "class_type": "EmptyLatentImage", "inputs": { "batch_size": 1 } },
            "11": { "class_type": "UNETLoader", "inputs": { "unet_name": "x.safetensors", "weight_dtype": "default" } },
            "14": { "class_type": "LoraLoaderModelOnly",
                    "inputs": { "lora_name": "qwen_image_2.1_turbo_lora_avg_rank_178_bf16.safetensors", "strength_model": 1 } },
            "13": { "class_type": "SaveImage", "inputs": { "filename_prefix": "ComfyUI" } },
            "17": { "class_type": "LoadImage", "inputs": { "image": "ref.png" } }
        });
        let (schema, _, _, _, media_mapping) = build_comfyui_schema(&prompt, None).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        let keys: Vec<&str> = properties.keys().map(String::as_str).collect();

        // 用户一定会改的：提示词（名字毫无线索、靠**值**认出来）、反向提示词、画幅三件套。
        assert!(keys.contains(&"12::value"), "提示词必须默认暴露：{keys:?}");
        assert!(
            keys.contains(&"9::negative_prompt"),
            "反向提示词也是提示词：{keys:?}"
        );
        assert!(keys.contains(&"6::aspect_ratio"), "{keys:?}");
        assert!(keys.contains(&"6::megapixels"), "{keys:?}");
        assert!(keys.contains(&"9::resolution"), "{keys:?}");
        // 参考图 → 媒体输入。
        assert!(keys.contains(&"17::image"));
        assert_eq!(
            media_mapping["17::image"],
            json!({ "node": "17", "field": "image" })
        );
        // 旋钮与管道：**不默认暴露**（对话框里仍列出来让用户勾）。
        for key in [
            "8::seed",
            "8::steps",
            "8::cfg",
            "8::denoise",
            "8::sampler_name",
            "8::scheduler",
            "2::batch_size",
            "11::unet_name",
            "11::weight_dtype",
            "13::filename_prefix",
            // ⚠️ 它的值有 54 个字符，**撞过"长文本"那条兜底**：长不等于像句子，见 `is_prose_like`。
            "14::lora_name",
            "14::strength_model",
        ] {
            assert!(!keys.contains(&key), "{key} 不该出现在第一屏：{keys:?}");
        }
        // 但它们**仍然列在候选里**（`suggested: false`）——勾一下就能用。
        let candidates = preview_comfyui_fields(&prompt).unwrap();
        let cfg = candidates.iter().find(|f| f.key == "8::cfg").unwrap();
        assert!(!cfg.suggested, "旋钮要列出来，只是不默认勾");
        // **标签只许用别人说过的话，而且必须可追溯**（2026-10-14 的教训：别替用户起名字）。
        // 这里钉两条：① 一律是 `节点标题 · 字段名` 的形状；② **原始字段名必须在里面**——
        // 用户要拿它回 ComfyUI 图里找那个输入，也要用它对着改备注名。
        for candidate in &candidates {
            assert!(
                candidate.default_label.contains(&candidate.field_name),
                "标签必须含原始字段名（可追溯）：{:?}",
                candidate.default_label
            );
        }
        let label = |key: &str| {
            candidates
                .iter()
                .find(|f| f.key == key)
                .unwrap()
                .default_label
                .clone()
        };
        assert_eq!(label("12::value"), "PrimitiveStringMultiline · value");
        assert_eq!(
            label("9::negative_prompt"),
            "TextEncodeQwenImage21 · negative_prompt"
        );
        assert_eq!(label("8::steps"), "KSampler · steps");
    }

    /// **撞名的字段要能分清**：那张图上真有两台 `LoadImage`（标题都是「加载图像」、
    /// 字段都叫 `image`），两个框一模一样 —— 而"哪个槽放哪张图"直接改变结果。
    ///
    /// 做法：撞名时补**节点号**（这条字段在**图里的地址**，是数据里的事实，不是我们编的词），
    /// 用户据此能回图里找到那台节点。
    ///
    /// 负对照：把 `label_map` 里那段撞名处理去掉 ⇒ 这条会红。
    #[test]
    fn duplicate_labels_get_the_node_id_so_users_can_tell_them_apart() {
        let prompt = json!({
            "17": { "class_type": "LoadImage",
                    "_meta": { "title": "加载图像" }, "inputs": { "image": "a.png" } },
            "23": { "class_type": "LoadImage",
                    "_meta": { "title": "加载图像" }, "inputs": { "image": "b.png" } },
            "6": { "class_type": "CLIPTextEncode", "inputs": { "text": "a red house" } }
        });
        let (schema, _, _, _, _) = build_comfyui_schema(&prompt, None).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            properties["17::image"]["description"],
            "加载图像 · image（节点 17）"
        );
        assert_eq!(
            properties["23::image"]["description"],
            "加载图像 · image（节点 23）"
        );
        // 不撞名的字段**不加**后缀（别把每个标签都搞长）。
        assert_eq!(
            properties["6::text"]["description"],
            "CLIPTextEncode · text"
        );
        // 预览里的名字与导入后一致（同一次计算，勾多勾少不影响）。
        let preview = preview_comfyui_fields(&prompt).unwrap();
        let image = preview.iter().find(|f| f.key == "17::image").unwrap();
        assert_eq!(image.default_label, "加载图像 · image（节点 17）");
    }

    /// 引擎**类型修正**：值写成 `42.0` 也还是整数框（引擎说是 INT）。
    ///
    /// 值会骗人：ComfyUI 的 JSON 里 `seed` 可能是 `42`（整数）也可能是 `42.0`（浮点），
    /// 而它到底该是哪种，只有引擎定义能说了算。
    #[test]
    fn engine_definition_corrects_a_misleading_value_type() {
        let catalog = engine_catalog();
        let prompt = json!({
            "3": { "class_type": "KSampler", "inputs": { "steps": 20.0 } }
        });
        // 这两条问的是**类型**，不是"该不该暴露" ⇒ 用勾选把字段叫进来（策展那条路）。
        let selection = labels(&[("3::steps", "")]);
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), Some(&catalog)).unwrap();
        assert_eq!(schema["properties"]["3::steps"]["type"], "integer");
        // 反向控制：没有引擎定义时，只能给宽松的 `number`——**不许收紧成 integer**。
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), None).unwrap();
        assert_eq!(schema["properties"]["3::steps"]["type"], "number");
    }

    /// **猜出来的类型只能放宽，不能收紧**：这条规则单独钉一次。
    ///
    /// 真实场景（2026-10-14，RH 工作流 `Qwen Image 2.1`）：图上 `cfg` 写的是 `1`、`denoise` 写的是 `1`，
    /// 而引擎说它们是 FLOAT。按"看着像整数"判成整数框，用户输入 `7.5` 会被**我们自己**的校验
    /// 判成"必须是整数"——我们挡住了引擎接受的值。反过来宽松的代价只是平台明确拒掉一个坏值。
    ///
    /// 负对照：把 `comfyui_scalar_type` 改回 `as_i64` 那套 ⇒ 这条会红。
    #[test]
    fn inferred_number_types_are_never_stricter_than_the_truth() {
        let catalog = engine_catalog();
        // 图里写成整数、引擎说是 FLOAT（cfg 的真实遭遇）。
        let prompt = json!({
            "3": { "class_type": "KSampler", "inputs": { "cfg": 1, "steps": 8 } }
        });
        // 同理：这里问的是类型，用勾选把两个旋钮叫进来。
        let selection = labels(&[("3::cfg", ""), ("3::steps", "")]);
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), None).unwrap();
        assert_eq!(schema["properties"]["3::cfg"]["type"], "number");
        assert_eq!(schema["properties"]["3::steps"]["type"], "number");
        // 有引擎定义时，两条各自归位：FLOAT → number，INT → integer（这次是**声明**，不是猜）。
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), Some(&catalog)).unwrap();
        assert_eq!(schema["properties"]["3::cfg"]["type"], "number");
        assert_eq!(schema["properties"]["3::steps"]["type"], "integer");
    }

    /// **用户选过的控件，他最大**；而且来源要如实记下来。
    ///
    /// 这是"边界"落到代码上的那一条：我们只给**默认值**，用户一改就是他的决定
    /// （`aspect_ratio` 其实只有三种画幅、他常用的采样器就那五个 —— 这些只有他知道）。
    /// 来源写进 `x-widget-source`：界面能如实说明"这是引擎给的/我们建议的/你选的"，
    /// 以后我们改匹配逻辑时也**不许覆盖 source = user 的那些**。
    ///
    /// 负对照：把 `apply_widget` 那段去掉（让引擎/值的形状说了算）⇒ 前两条断言会红。
    #[test]
    fn user_chosen_widget_wins_over_engine_and_value_shape() {
        let catalog = engine_catalog();
        // 引擎说 `cfg` 是 FLOAT、`sampler_name` 是 44 选一；用户偏要把 cfg 变成下拉
        // （他知道自己只用三个值）、把 sampler_name 变成自由文本。
        let prompt = json!({
            "3": { "class_type": "KSampler",
                   "inputs": { "cfg": 8.0, "sampler_name": "euler" } }
        });
        let selection: BTreeMap<String, ComfyFieldChoice> = [
            widget_choice("3::cfg", Widget::Dropdown, &["4", "7", "10"]),
            widget_choice("3::sampler_name", Widget::Text, &[]),
        ]
        .into_iter()
        .collect();
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), Some(&catalog)).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        // 用户说它是下拉 ⇒ 就是他填的那三个（而不是引擎的 44 个）。
        assert_eq!(properties["3::cfg"]["enum"], json!(["4", "7", "10"]));
        assert_eq!(properties["3::cfg"]["x-widget-source"], "user");
        // 用户说自由文本 ⇒ 引擎的枚举让位（他的图他说了算）。
        assert!(properties["3::sampler_name"].get("enum").is_none());
        assert_eq!(properties["3::sampler_name"]["type"], "string");
        assert_eq!(properties["3::sampler_name"]["x-widget-source"], "user");

        // 没被用户改过的字段：来源如实标"引擎"；连引擎都问不到 ⇒ 标"建议"。
        let plain = labels(&[("3::cfg", ""), ("3::sampler_name", "")]);
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&plain), Some(&catalog)).unwrap();
        assert_eq!(schema["properties"]["3::cfg"]["x-widget-source"], "engine");
        let (schema, _, _, _, _) = build_comfyui_schema_with(&prompt, Some(&plain), None).unwrap();
        assert_eq!(
            schema["properties"]["3::cfg"]["x-widget-source"],
            "suggested"
        );
    }

    /// 用户选"下拉"却没填选项 ⇒ **退回文本，不硬造一个空下拉**（空下拉比文本框更糟：
    /// 点开发现没得选）。这是"说了但做不到"的唯一情形，代码与测试都明说。
    #[test]
    fn dropdown_without_options_falls_back_to_text() {
        let prompt = json!({
            "6": { "class_type": "ResolutionSelector", "inputs": { "aspect_ratio": "9:16 (Portrait Widescreen)" } }
        });
        let none: BTreeMap<String, ComfyFieldChoice> =
            [widget_choice("6::aspect_ratio", Widget::Dropdown, &[])]
                .into_iter()
                .collect();
        let (schema, _, _, _, _) = build_comfyui_schema_with(&prompt, Some(&none), None).unwrap();
        assert_eq!(schema["properties"]["6::aspect_ratio"]["type"], "string");
        assert!(schema["properties"]["6::aspect_ratio"]
            .get("enum")
            .is_none());
        // 填了选项就真的是下拉（这正是"只有用户知道的事"：这张图其实只有这三种画幅）。
        let with_options: BTreeMap<String, ComfyFieldChoice> = [widget_choice(
            "6::aspect_ratio",
            Widget::Dropdown,
            &[
                "9:16 (Portrait Widescreen)",
                "1:1 (Square)",
                "16:9 (Landscape)",
            ],
        )]
        .into_iter()
        .collect();
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&with_options), None).unwrap();
        assert_eq!(
            schema["properties"]["6::aspect_ratio"]["enum"],
            json!([
                "9:16 (Portrait Widescreen)",
                "1:1 (Square)",
                "16:9 (Landscape)"
            ])
        );
    }

    /// 用户把某条字段选成"图片或文件" ⇒ 它走**媒体那条路**（上传的资产按 `mediaMapping`
    /// 回填图谱），而不是普通字段映射。
    #[test]
    fn user_can_turn_a_field_into_a_file_input() {
        let prompt = json!({
            "17": { "class_type": "LoadImage", "inputs": { "image": "example.png" } }
        });
        let selection: BTreeMap<String, ComfyFieldChoice> =
            [widget_choice("17::image", Widget::File, &[])]
                .into_iter()
                .collect();
        let (schema, _, _, field_mapping, media_mapping) =
            build_comfyui_schema_with(&prompt, Some(&selection), None).unwrap();
        assert_eq!(schema["properties"]["17::image"]["format"], "file");
        assert_eq!(schema["properties"]["17::image"]["x-widget-source"], "user");
        assert_eq!(
            media_mapping["17::image"],
            json!({ "node": "17", "field": "image" })
        );
        assert!(!field_mapping.contains_key("17::image"));
    }

    /// 认不出来的控件名 = **没指定**：拼错一个字符串不许改变字段的行为。
    #[test]
    fn unknown_widget_names_are_ignored_not_guessed() {
        assert!(Widget::parse("slidr").is_none());
        assert!(Widget::parse("").is_none());
        assert!(Widget::parse("  ").is_none());
        let choice = ComfyFieldChoice::from_selection(&ComfyFieldSelection {
            key: "3::steps".to_string(),
            label: String::new(),
            widget: Some("slidr".to_string()),
            options: vec![],
        });
        assert!(choice.widget.is_none(), "认不出来就该当作没选");
        let prompt = json!({ "3": { "class_type": "KSampler", "inputs": { "steps": 20 } } });
        let mut map = BTreeMap::new();
        map.insert("3::steps".to_string(), choice);
        let (schema, _, _, _, _) = build_comfyui_schema_with(&prompt, Some(&map), None).unwrap();
        assert_eq!(schema["properties"]["3::steps"]["type"], "number");
        assert_eq!(
            schema["properties"]["3::steps"]["x-widget-source"],
            "suggested"
        );
    }

    /// 用户选过的字段（备注名 + 控件 + 候选项）在**再次导入**时照旧生效，
    /// 而且来源仍是 `user` —— 这是我们改匹配逻辑时**不许覆盖**的那一类。
    #[test]
    fn user_choices_survive_a_reimport() {
        let prompt = json!({
            "3": { "class_type": "KSampler", "inputs": { "cfg": 8.0 } }
        });
        let selection: BTreeMap<String, ComfyFieldChoice> = [(
            "3::cfg".to_string(),
            ComfyFieldChoice {
                label: "提示词贴合度".to_string(),
                widget: Some(Widget::Dropdown),
                options: vec!["4".to_string(), "7".to_string()],
            },
        )]
        .into_iter()
        .collect();
        let (schema, _, _, _, _) =
            build_comfyui_schema_with(&prompt, Some(&selection), None).unwrap();
        assert_eq!(
            schema["properties"]["3::cfg"]["description"],
            "提示词贴合度"
        );
        assert_eq!(schema["properties"]["3::cfg"]["enum"], json!(["4", "7"]));
        assert_eq!(schema["properties"]["3::cfg"]["x-widget-source"], "user");
    }
}
