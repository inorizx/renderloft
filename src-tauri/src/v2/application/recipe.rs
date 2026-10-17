//! Recipe 写操作用例：本地创建（含 binding 组装与 revision 回填）。

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::v2::application::field_widget::{apply_field_widgets, FieldWidgetChange};
use crate::v2::domain::actor::{ActorRef, Scope};
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::{ConnectionId, RecipeId, RecipeRevisionId};
use crate::v2::domain::media::MediaMapping;
use crate::v2::domain::model::{
    Recipe, RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility, Timestamp,
};
use crate::v2::ports::repositories::RecipeRepository;

/// 把**字段备注**盖到 schema 的 `description` 上，返回读 schema 的人看到的版本。
///
/// ⚠️ 为什么需要这一层：RunningHub 的 schema 里 `description` 就等于字段名
/// （实测 `"874::text": {"description": "text"}`）——**平台没给语义**，我们如实转达，
/// 于是每个消费者看到的都是读不出意思的键。语义的唯一来源是**人**。
///
/// ⚠️ 为什么合并放在这里、而不是写回 revision：revision 管的是**执行语义**
/// （跑哪个工作流、字段怎么映射、约束是什么），备注是**展示语义**。
/// 合并必须是**一个函数、所有消费方共用**（界面表单、MCP 的工具描述）——
/// 各写一份的话，两边会悄悄变成两份不同的 schema，而那种偏差最难查（谁都不报错）。
///
/// ⚠️ 没有备注的字段**保留平台给的描述**：读不到语义就说读不到，不编一个出来。
///
/// ⚠️ **唯一的例外是"撞名"**：同一个描述出现在多个字段上时，谁是谁根本分不清——
/// 实测一条 RunningHub 应用（31 个字段）里有 **5 个 `value`、5 个 `save_output`**，
/// 因为平台的 `description` 就等于字段名。这时给它们补上**键里的节点号**
/// （`value（节点 173）`）：那是**数据里的事实**、不是我们编的语义，而且「编辑」里同一行
/// 显示的键就是 `173::value`，两边对得上，用户能据此回图里找到那台节点。
/// 与 ComfyUI 导入那条路（`recipe_import.rs` 的 `label_map`）用的是**同一条规则**：
/// 只在撞名时补，不撞名的一个字都不加——别把每个标签都搞长。
///
/// 三条边界：**他自己写过备注的字段不动**（哪怕它跟别人撞名，那是他的选择）；
/// 键里没有位置信息的手工 schema 不补（不编一个位置出来）；补完就不撞了 ⇒ 这一步是幂等的。
pub fn apply_field_labels(schema: &Value, labels: &BTreeMap<String, String>) -> Value {
    let mut schema = schema.clone();
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return schema;
    };
    for (key, label) in labels {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(property) = properties.get_mut(key).and_then(Value::as_object_mut) else {
            // 备注指向 schema 里不存在的字段（例如应用换了版本）：跳过，**不新增字段**——
            // 备注是描述，不该让 schema 长出应用没有的参数。
            continue;
        };
        property.insert("description".to_string(), json!(trimmed));
    }

    // 撞名的字段补"位置"。注意这一步**不依赖 labels**：没写过任何备注的应用（`labels` 为空、
    // 平台描述就等于字段名）恰恰是最需要它的那一类。
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for property in properties.values() {
        if let Some(text) = description_text(property) {
            *seen.entry(text).or_insert(0) += 1;
        }
    }
    let crowded: Vec<String> = seen
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(text, _)| text)
        .collect();
    if crowded.is_empty() {
        return schema;
    }
    for (key, property) in properties.iter_mut() {
        let Some(text) = description_text(property) else {
            continue;
        };
        if !crowded.contains(&text) {
            continue;
        }
        if labels
            .get(key)
            .map(|label| !label.trim().is_empty())
            .unwrap_or(false)
        {
            continue;
        }
        let Some((node, _)) = key.split_once("::") else {
            continue;
        };
        if node.trim().is_empty() {
            continue;
        }
        property["description"] = json!(format!("{text}（节点 {node}）"));
    }
    schema
}

/// 字段当前的描述文字（空/缺省一律当作"没有"，免得把空串也拿去比撞名）。
fn description_text(property: &Value) -> Option<String> {
    property
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

pub struct RecipeDependencies {
    pub recipes: Arc<dyn RecipeRepository>,
}

/// 从 `provider_config` 里取一张 `key -> {node, field}` 形状的映射表（取不到就是空的）。
fn mapping_object(
    provider_config: &BTreeMap<String, Value>,
    name: &str,
) -> BTreeMap<String, Value> {
    provider_config
        .get(name)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// 从字段键推出映射项：键的形状是 `<节点>::<字段>`（导入时就是这么造的）。
///
/// 推不出来就返回 `None` —— **不猜**（非这类图的键本来就不带节点信息）。
fn derived_mapping_entry(key: &str) -> Option<Value> {
    let (node, field) = key.split_once("::")?;
    if node.is_empty() || field.is_empty() {
        return None;
    }
    Some(json!({ "node": node, "field": field }))
}

/// 把 revision 的 `mediaMapping` 归一化成**一律 keyed**（新 revision 一律写字段键）。
///
/// 旧 revision 的 `asset-N` 槽位在这一步按数字序展开成字段键——语义由值里的 node/field
/// 决定，与任何"顺序"无关（错位事故的根因就是顺序被当成了语义）。归一化失败（映射形状
/// 坏了）必须报错：带着一张读不懂的映射往前跑，等于继续静默丢资产。
fn keyed_media_mapping(
    provider_config: &BTreeMap<String, Value>,
) -> DomainResult<BTreeMap<String, Value>> {
    Ok(MediaMapping::from_provider_config(provider_config)?.keyed_entries())
}

/// 改一个已有配方的**字段控件**（导入之后、随时）。
///
/// 为什么这不是"就地改 schema"：revision 是**不可变**的，历史任务按 id 指回去。改了表单语义
/// 只能**追加一份新 revision**（旧那份留着），`current_revision_id` 指过去。
///
/// 为什么这件事必须动 revision 而不是像备注那样读时合并：换成"图片或文件"会改变**执行语义**
/// ——值要走资产上传那条路，按 `provider_config.mediaMapping` 回填图谱。所以映射表要跟着改，
/// 而映射表住在 revision 的 binding 里。
///
/// 边界（同 `field_widget` 模块的注释）：我们只给默认值；**用户改过的，来源记 `user`，
/// 以后我们改匹配逻辑不许覆盖它**。
pub struct UpdateFieldWidgets {
    pub dependencies: RecipeDependencies,
}

impl UpdateFieldWidgets {
    pub fn new(dependencies: RecipeDependencies) -> Self {
        Self { dependencies }
    }

    pub async fn execute(
        &self,
        actor: &ActorRef,
        recipe_id: RecipeId,
        changes: &[FieldWidgetChange],
    ) -> DomainResult<Recipe> {
        if !actor.has_scope(Scope::RecipesWrite) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "缺少 recipes:write 权限",
            ));
        }
        if changes.is_empty() {
            return Err(SafeError::validation("NO_FIELD_CHANGES", "没有要改的字段"));
        }
        let mut recipe = self
            .dependencies
            .recipes
            .find_recipe(recipe_id)
            .await?
            .ok_or_else(|| SafeError::not_found("RECIPE_NOT_FOUND", "Recipe 不存在"))?;
        let current = self
            .dependencies
            .recipes
            .find_revision(recipe.current_revision_id)
            .await?
            .ok_or_else(|| SafeError::not_found("RECIPE_REVISION_NOT_FOUND", "配方版本不存在"))?;

        let applied = apply_field_widgets(&current.input_schema, changes);
        if applied.effective.is_empty() {
            // 一个都没落下 ⇒ 键不存在或控件名认不出。**不静默成功**：用户以为改了、其实没改，
            // 比报错糟得多（他会一直奇怪为什么界面没变）。
            return Err(SafeError::validation(
                "FIELD_WIDGET_NOT_APPLIED",
                "这些字段没能在配方里找到（或控件名不认识）：没有改动任何东西",
            ));
        }

        // 映射表跟着控件走：变成"文件"的字段从 `fieldMapping` 挪进 `mediaMapping`（值要走
        // 资产上传），变回普通控件的字段把它的资产槽回收掉。**这一步才是"控件不是纯展示"
        // 的地方**。
        //
        // ⚠️ 两个形状规则：**先归一化**（旧 revision 的 asset-N 槽位换算成字段键），
        // **再写 keyed**（新 revision 一律字段键）。归一化失败直接报错——读不懂的映射
        // 不许被"跳过"，那会让资产在新 revision 里指向不存在的槽位。
        let mut provider_config = current.binding.provider_config.clone();
        let mut field_mapping = mapping_object(&provider_config, "fieldMapping");
        let mut media_mapping = keyed_media_mapping(&provider_config)?;
        for (key, (_, is_media)) in &applied.effective {
            // 键的形状是 `<节点>::<字段>`（导入时就是这么造的），据此推出映射项。
            // 推不出来（非这类图）就当没有映射可动 —— 不猜。
            let derived = derived_mapping_entry(key);
            if *is_media {
                let entry = field_mapping.remove(key).or_else(|| derived.clone());
                if let Some(entry) = entry {
                    media_mapping.insert(key.clone(), entry);
                }
            } else {
                let Some(derived) = derived else { continue };
                // 归一化之后键就是字段键：按**键**删，不再按值去猜哪一个槽像它。
                if media_mapping.remove(key).is_some() {
                    field_mapping.insert(key.clone(), derived);
                }
            }
        }
        provider_config.insert(
            "fieldMapping".to_string(),
            Value::Object(field_mapping.into_iter().collect()),
        );
        if media_mapping.is_empty() {
            provider_config.remove("mediaMapping");
        } else {
            provider_config.insert(
                "mediaMapping".to_string(),
                Value::Object(media_mapping.into_iter().collect()),
            );
        }

        let mut binding = current.binding.clone();
        binding.provider_config = provider_config;
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: recipe.id,
            revision: current.revision + 1,
            input_schema: applied.schema,
            output_schema: current.output_schema.clone(),
            binding,
            defaults: current.defaults.clone(),
            source: current.source.clone(),
            created_at: Timestamp::now(),
        };
        let revision = self.dependencies.recipes.append_revision(revision).await?;
        recipe.current_revision_id = revision.id;
        recipe.updated_at = revision.created_at.clone();
        self.dependencies.recipes.update_recipe(&recipe).await?;
        Ok(recipe)
    }
}

pub struct CreateRecipeRequest {
    pub actor: ActorRef,
    pub name: String,
    pub description: String,
    pub labels: Vec<String>,
    pub capability: String,
    pub driver_key: String,
    pub default_connection_id: ConnectionId,
    pub allowed_connection_ids: Vec<ConnectionId>,
    pub provider_config: Value,
    pub input_schema: Value,
    pub output_schema: Value,
    pub defaults: BTreeMap<String, Value>,
}

pub struct CreateRecipe {
    pub dependencies: RecipeDependencies,
}

impl CreateRecipe {
    pub fn new(dependencies: RecipeDependencies) -> Self {
        Self { dependencies }
    }

    pub async fn execute(&self, request: CreateRecipeRequest) -> DomainResult<Recipe> {
        if !request.actor.has_scope(Scope::RecipesWrite) {
            return Err(SafeError::validation(
                "SCOPE_REQUIRED",
                "缺少 recipes:write 权限",
            ));
        }
        if request.name.trim().is_empty() {
            return Err(SafeError::validation(
                "NAME_REQUIRED",
                "Recipe 名称不能为空",
            ));
        }
        if request.capability.trim().is_empty() {
            return Err(SafeError::validation(
                "CAPABILITY_REQUIRED",
                "Recipe 能力不能为空",
            ));
        }
        if request.driver_key.trim().is_empty() {
            return Err(SafeError::validation(
                "DRIVER_KEY_REQUIRED",
                "driverKey 不能为空",
            ));
        }
        let binding_value = json!({
            "driver_key": request.driver_key,
            "default_connection_id": request.default_connection_id.0,
            "allowed_connection_ids": request
                .allowed_connection_ids
                .iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
            "connection_override": "denied",
            "provider_config": request.provider_config,
        });
        let binding = serde_json::from_value::<RecipeBinding>(binding_value).map_err(|_| {
            SafeError::validation("BINDING_INVALID", "providerConfig 无法转换为绑定")
        })?;
        let now = Timestamp::now();
        let recipe = Recipe {
            id: RecipeId(0),
            name: request.name,
            description: request.description,
            labels: request.labels,
            capability: request.capability,
            current_revision_id: RecipeRevisionId(0),
            visibility: RecipeVisibility::Private,
            created_at: now.clone(),
            updated_at: now.clone(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            // 手工创建的 Recipe：字段名由创建者自己给（`input_schema` 里的 description
            // 就是他的原话），不需要额外备注。
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: RecipeId(0),
            revision: 1,
            input_schema: request.input_schema,
            output_schema: request.output_schema,
            binding,
            defaults: request.defaults,
            source: RecipeSource::Embedded,
            created_at: now,
        };
        // 仓库事务内分配 id；回填 current_revision_id 后写回（与既有行为一致）。
        let (mut recipe, revision) = self
            .dependencies
            .recipes
            .create_recipe(recipe, revision)
            .await?;
        recipe.current_revision_id = revision.id;
        self.dependencies.recipes.update_recipe(&recipe).await?;
        Ok(recipe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::ActorKind;
    use crate::v2::domain::model::{RecipeSource, RecipeVisibility};
    use crate::v2::infrastructure::fakes::InMemoryRepositories;
    use serde_json::json;

    fn test_actor() -> ActorRef {
        ActorRef {
            id: "test".to_string(),
            kind: ActorKind::Cli,
            client_id: crate::v2::domain::ids::ClientId("test".to_string()),
            scopes: vec![Scope::RecipesWrite],
            label: None,
        }
    }

    /// 造一个"ComfyUI 那张图"形状的配方：`17::image` 走媒体映射，`6::text` 走普通映射。
    async fn seeded_recipe() -> (Arc<InMemoryRepositories>, Recipe) {
        let repositories = Arc::new(InMemoryRepositories::new());
        let now = Timestamp::now();
        let recipe = Recipe {
            id: RecipeId(0),
            name: "测试应用".to_string(),
            description: String::new(),
            labels: vec![],
            capability: "workflow-execution".to_string(),
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
            input_schema: json!({
                "type": "object",
                "properties": {
                    "6::text": { "type": "string", "description": "CLIPTextEncode · text" },
                    "17::image": { "type": "string", "format": "file", "x-widget-source": "suggested" }
                }
            }),
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: "comfyui/comfyui".to_string(),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                provider_config: [
                    (
                        "fieldMapping".to_string(),
                        json!({ "6::text": { "node": "6", "field": "text" } }),
                    ),
                    (
                        "mediaMapping".to_string(),
                        json!({ "asset-0": { "node": "17", "field": "image" } }),
                    ),
                ]
                .into_iter()
                .collect(),
                connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
            },
            defaults: [("6::text".to_string(), json!("a red house"))]
                .into_iter()
                .collect(),
            source: RecipeSource::Embedded,
            created_at: now,
        };
        let (recipe, _) = repositories
            .create_recipe(recipe, revision)
            .await
            .expect("建配方");
        (repositories, recipe)
    }

    /// **用户改控件 = 追加一份 revision**，而且改的是"控件形状"，其它一律不碰。
    #[tokio::test]
    async fn field_widget_change_appends_a_revision_and_marks_the_source() {
        let (repositories, recipe) = seeded_recipe().await;
        let service = UpdateFieldWidgets::new(RecipeDependencies {
            recipes: repositories.clone(),
        });
        let updated = service
            .execute(
                &test_actor(),
                recipe.id,
                &[FieldWidgetChange {
                    key: "6::text".to_string(),
                    widget: "dropdown".to_string(),
                    options: vec!["晴天".to_string(), "雨天".to_string()],
                }],
            )
            .await
            .expect("改控件应当成功");

        assert_ne!(
            updated.current_revision_id, recipe.current_revision_id,
            "必须是一份**新** revision（就地改会改写历史任务看到的定义）"
        );
        let revision =
            RecipeRepository::find_revision(repositories.as_ref(), updated.current_revision_id)
                .await
                .expect("查 revision")
                .expect("revision 应当在");
        assert_eq!(revision.revision, 2, "版本号递增");
        let field = &revision.input_schema["properties"]["6::text"];
        assert_eq!(field["enum"], json!(["晴天", "雨天"]));
        assert_eq!(field["x-widget-source"], json!("user"));
        // 备注（展示语义）原样带过来；默认值、source、输出 schema 都不动。
        assert_eq!(field["description"], json!("CLIPTextEncode · text"));
        assert_eq!(revision.defaults["6::text"], json!("a red house"));
        assert_eq!(revision.source, recipe_source_local());
        // 没被改的字段一个字不动（含它的来源标记）。
        assert_eq!(
            revision.input_schema["properties"]["17::image"]["format"],
            json!("file")
        );
        assert_eq!(
            revision.input_schema["properties"]["17::image"]["x-widget-source"],
            json!("suggested")
        );

        // **旧 revision 还在，而且没被改写**——这是不可变的意义所在。
        let old =
            RecipeRepository::find_revision(repositories.as_ref(), recipe.current_revision_id)
                .await
                .expect("查旧 revision")
                .expect("旧 revision 必须留着（历史任务要指回来）");
        assert!(
            old.input_schema["properties"]["6::text"]
                .get("enum")
                .is_none(),
            "旧那份不许被改写：{old:?}"
        );
    }

    fn recipe_source_local() -> RecipeSource {
        RecipeSource::Embedded
    }

    /// 把一条字段改成"图片或文件" ⇒ 它要从 `fieldMapping` **挪进** `mediaMapping`
    /// （值要走资产上传那条路）——这就是"控件不是纯展示"的证据。
    #[tokio::test]
    async fn switching_a_field_to_file_moves_it_into_the_media_mapping() {
        let (repositories, recipe) = seeded_recipe().await;
        let service = UpdateFieldWidgets::new(RecipeDependencies {
            recipes: repositories.clone(),
        });
        let updated = service
            .execute(
                &test_actor(),
                recipe.id,
                &[FieldWidgetChange {
                    key: "6::text".to_string(),
                    widget: "file".to_string(),
                    options: vec![],
                }],
            )
            .await
            .expect("改控件应当成功");
        let revision =
            RecipeRepository::find_revision(repositories.as_ref(), updated.current_revision_id)
                .await
                .expect("查 revision")
                .expect("revision 应当在");
        let config = &revision.binding.provider_config;
        assert!(
            config["fieldMapping"].get("6::text").is_none(),
            "不再是普通字段映射：{config:?}"
        );
        // ⚠️ 新 revision 一律写 **keyed**（键 = 字段键）；旧 fixture 里的 `asset-0` 在
        // 归一化时被换算成了 `17::image`（见下面反向断言），6::text 以它的键落位。
        assert_eq!(
            config["mediaMapping"]["6::text"],
            json!({ "node": "6", "field": "text" }),
            "要挪进媒体映射：{config:?}"
        );
        assert!(
            !config["mediaMapping"]
                .as_object()
                .map(|map| map.keys().any(|key| key.starts_with("asset-")))
                .unwrap_or(false),
            "新 revision 不许再有位置槽键：{config:?}"
        );
        // 反向：把原本是文件的字段改回文本 ⇒ 资产槽回收、回到普通映射。
        let service = UpdateFieldWidgets::new(RecipeDependencies {
            recipes: repositories.clone(),
        });
        let updated = service
            .execute(
                &test_actor(),
                updated.id,
                &[FieldWidgetChange {
                    key: "17::image".to_string(),
                    widget: "text".to_string(),
                    options: vec![],
                }],
            )
            .await
            .expect("改回去也应当成功");
        let revision =
            RecipeRepository::find_revision(repositories.as_ref(), updated.current_revision_id)
                .await
                .expect("查 revision")
                .expect("revision 应当在");
        let config = &revision.binding.provider_config;
        assert_eq!(
            config["fieldMapping"]["17::image"],
            json!({ "node": "17", "field": "image" }),
            "回到普通字段映射：{config:?}"
        );
        assert!(
            config["mediaMapping"]
                .as_object()
                .map(|map| !map.contains_key("17::image"))
                .unwrap_or(true),
            "17::image 的资产槽要回收掉：{config:?}"
        );
        assert_eq!(
            config["mediaMapping"]["6::text"],
            json!({ "node": "6", "field": "text" }),
            "但 6::text 仍然是文件输入，它的槽要留着：{config:?}"
        );
    }

    /// 旧 revision（asset-N 形状）被追加新 revision 时**归一化成 keyed**：
    /// 旧槽位与它的语义（node/field）一一对应地带过去，位置名不再出现在新 revision 里。
    #[tokio::test]
    async fn appending_a_revision_normalizes_legacy_slots_to_field_keys() {
        let (repositories, recipe) = seeded_recipe().await;
        let service = UpdateFieldWidgets::new(RecipeDependencies {
            recipes: repositories.clone(),
        });
        // 只改一个**无关**字段的控件，让映射表原样被带进新 revision。
        let updated = service
            .execute(
                &test_actor(),
                recipe.id,
                &[FieldWidgetChange {
                    key: "6::text".to_string(),
                    widget: "text".to_string(),
                    options: vec![],
                }],
            )
            .await
            .expect("改控件应当成功");
        let revision =
            RecipeRepository::find_revision(repositories.as_ref(), updated.current_revision_id)
                .await
                .expect("查 revision")
                .expect("revision 应当在");
        let media = &revision.binding.provider_config["mediaMapping"];
        assert_eq!(
            media["17::image"],
            json!({ "node": "17", "field": "image" }),
            "旧 asset-0 槽位要归一化成 17::image：{media:?}"
        );
        assert!(
            media.get("asset-0").is_none(),
            "新 revision 里不许再有位置槽名：{media:?}"
        );
    }

    /// 一个都没落下（键不存在 / 控件名认不出）⇒ **报错，不许静默成功**：
    /// 用户以为改了、界面却没变，比报错糟得多。
    #[tokio::test]
    async fn a_change_that_lands_nowhere_is_an_error() {
        let (repositories, recipe) = seeded_recipe().await;
        let service = UpdateFieldWidgets::new(RecipeDependencies {
            recipes: repositories.clone(),
        });
        let error = service
            .execute(
                &test_actor(),
                recipe.id,
                &[FieldWidgetChange {
                    key: "999::typo".to_string(),
                    widget: "text".to_string(),
                    options: vec![],
                }],
            )
            .await
            .expect_err("没落到任何字段上就该报错");
        assert_eq!(error.code, "FIELD_WIDGET_NOT_APPLIED");
        // 而且什么都没动。
        let unchanged = repositories
            .find_recipe(recipe.id)
            .await
            .expect("查配方")
            .expect("配方应当在");
        assert_eq!(unchanged.current_revision_id, recipe.current_revision_id);
    }

    /// 备注盖在 `description` 上，且**只动被备注的字段**。
    ///
    /// 判别点：RunningHub 的 schema 里 `description` 就等于字段名
    /// （实测 `"874::text": {"description": "text"}`）——平台没给语义。
    /// 备注是人补的那一层，合并后 Agent 与用户才看得出那是正向提示词。
    #[test]
    fn field_labels_override_description_only_where_labelled() {
        let schema = json!({
            "type": "object",
            "properties": {
                "874::text": { "type": "string", "description": "text" },
                "871::batch_size": { "type": "integer", "description": "batch_size" }
            }
        });
        let labels = [("874::text".to_string(), "正向提示词".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        let merged = apply_field_labels(&schema, &labels);
        assert_eq!(
            merged["properties"]["874::text"]["description"],
            json!("正向提示词"),
            "被备注的字段要用备注"
        );
        assert_eq!(
            merged["properties"]["871::batch_size"]["description"],
            json!("batch_size"),
            "**没有备注的字段原样保留**：读不到语义就说读不到，不编一个出来"
        );
        assert_eq!(
            merged["properties"]["871::batch_size"]["type"],
            json!("integer"),
            "只动 description，其它一律不碰：{merged}"
        );
    }

    /// 备注指向 schema 里不存在的字段时**不新增字段**——备注是描述，不该让 schema
    /// 长出应用根本没有的参数（那会让 Agent 提交一个平台不认识的键）。
    #[test]
    fn field_labels_never_grow_the_schema() {
        let schema = json!({
            "type": "object",
            "properties": { "874::text": { "type": "string" } }
        });
        let labels = [("999::typo".to_string(), "打错的键".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        let merged = apply_field_labels(&schema, &labels);
        assert_eq!(
            merged["properties"].as_object().map(|map| map.len()),
            Some(1),
            "不存在的字段不得被备注带进来：{merged}"
        );
        assert!(merged["properties"].get("999::typo").is_none(), "{merged}");
    }

    /// 空备注 = 没备注（不覆盖原 description）；整体为空时原样返回。
    #[test]
    fn empty_labels_leave_the_schema_untouched() {
        let schema = json!({
            "type": "object",
            "properties": { "874::text": { "type": "string", "description": "text" } }
        });
        let blank = [("874::text".to_string(), "   ".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            apply_field_labels(&schema, &blank)["properties"]["874::text"]["description"],
            json!("text"),
            "空备注不该把原描述抹成空白"
        );

        let none = BTreeMap::new();
        assert_eq!(
            apply_field_labels(&schema, &none),
            schema,
            "既没有备注、也没有撞名时，schema 原样返回"
        );
    }

    /// **撞名要能分清**：平台描述就等于字段名时，一条应用里能有 5 个 `value`（实测）。
    /// 做法：撞名时补**键里的节点号**（那是数据里的事实），而且「编辑」里显示的键就是
    /// `173::value`，两边对得上。不撞名的字段一个字都不加。
    #[test]
    fn duplicate_descriptions_get_the_node_id_so_users_can_tell_them_apart() {
        let schema = json!({
            "type": "object",
            "properties": {
                "173::value": { "type": "integer", "description": "value" },
                "272::value": { "type": "integer", "description": "value" },
                "284::value": { "type": "integer", "description": "value" },
                "18::scheduler": { "type": "string", "description": "scheduler" }
            }
        });
        // 注意：**没有任何备注**。这与 ComfyUI 那条路不同——AI 应用的描述是平台给的，
        // 没写过备注的应用恰恰是最需要这一步的那一类（这条测试就是钉住这一点）。
        let merged = apply_field_labels(&schema, &BTreeMap::new());
        assert_eq!(
            merged["properties"]["173::value"]["description"],
            json!("value（节点 173）")
        );
        assert_eq!(
            merged["properties"]["272::value"]["description"],
            json!("value（节点 272）")
        );
        assert_eq!(
            merged["properties"]["284::value"]["description"],
            json!("value（节点 284）")
        );
        assert_eq!(
            merged["properties"]["18::scheduler"]["description"],
            json!("scheduler"),
            "不撞名的字段不加后缀（别把每个标签都搞长）"
        );
        // 补完就不撞了 ⇒ 幂等：再合一次不会变成「（节点 173）（节点 173）」。
        assert_eq!(apply_field_labels(&merged, &BTreeMap::new()), merged);
    }

    /// 他**自己写过名字**的字段一律不动：写了一个名字，那个撞名本来也就消失了。
    /// 两个都写过（哪怕是同样的名字）⇒ 都不动——那是他的选择，不是我们的。
    #[test]
    fn a_name_the_user_wrote_is_never_touched() {
        let schema = json!({
            "type": "object",
            "properties": {
                "173::value": { "type": "integer", "description": "value" },
                "272::value": { "type": "integer", "description": "value" }
            }
        });
        let one = [("173::value".to_string(), "补帧时长(帧)".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let merged = apply_field_labels(&schema, &one);
        assert_eq!(
            merged["properties"]["173::value"]["description"],
            json!("补帧时长(帧)"),
            "他起的名字原样保留"
        );
        assert_eq!(
            merged["properties"]["272::value"]["description"],
            json!("value"),
            "给其中一个起了名，撞名就没了 ⇒ 另一个也不需要补位置"
        );

        let both = [
            ("173::value".to_string(), "时长".to_string()),
            ("272::value".to_string(), "时长".to_string()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let merged = apply_field_labels(&schema, &both);
        assert_eq!(
            merged["properties"]["173::value"]["description"],
            json!("时长")
        );
        assert_eq!(
            merged["properties"]["272::value"]["description"],
            json!("时长"),
            "两个都是他起的名字 ⇒ 撞着也照他的来"
        );
    }

    /// 手工建的 schema（键里没有 `节点::字段` 这种位置信息）**不补**：
    /// 读不出位置就不编一个出来，与"读不到语义不编语义"是同一条纪律。
    #[test]
    fn keys_without_a_location_are_left_alone() {
        let schema = json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "提示词" },
                "text": { "type": "string", "description": "提示词" }
            }
        });
        let merged = apply_field_labels(&schema, &BTreeMap::new());
        assert_eq!(merged, schema, "没有位置可补时原样返回：{merged}");
    }
}
