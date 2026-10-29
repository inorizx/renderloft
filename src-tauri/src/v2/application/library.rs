//! 应用库的**导出 / 导入**：只搬「应用」这一个资产。
//!
//! 为什么需要它：应用库是这个产品里唯一被**人策展过**的东西——名字、分组、收藏、
//! 每个字段的中文备注名、每个字段该用什么控件。这些判断花的是人的时间，而它们此前
//! **只活在那台机器的 SQLite 里**：换电脑要拷整个工作区（连任务历史和产物缓存一起），
//! 想把自己整理好的一套应用给同事更不可能。`workspace backup` 解决的是"搬家"，
//! 这里解决的是"这套东西本身"。
//!
//! 包里**刻意不带**四样东西：
//! * `id` —— 新机器上重新编号，硬搬别人的主键只会撞车；
//! * `connection id` —— 那是本机的自增主键，`3` 在新机器上可能是另一个平台。
//!   包只记**驱动键**（`runninghub/runninghub`），导入时按它去本机找一条连接；
//! * 任何密钥 —— 它们本来就只在系统凭据库，连库都不进；
//! * 任务历史与产物 —— 那是"跑过什么"，不是"我有什么应用"，换个机器没有意义。
//!
//! 为什么只带**当前** revision：revision 不可变是为了让历史任务指得回去，而"应用
//! 现在长什么样"只需要当前那一份。把历史 revision 一起搬过来只会让新库凭空多出
//! 一堆没人引用的版本。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::{RecipeId, RecipeRevisionId};
use crate::v2::domain::model::{
    ConnectionOverride, Recipe, RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility,
    Timestamp,
};
use crate::v2::ports::repositories::{ConnectionRepository, RecipeRepository};

/// 包格式标识。写在文件里，导入时先认它——认不出就拒绝，而不是猜一个能解析的读法。
pub const PACKAGE_FORMAT: &str = "renderloft.library";
/// 包版本。将来改形状时靠它分流；不认识的版本直接拒绝（不猜）。
pub const PACKAGE_VERSION: u32 = 1;

/// 一个应用库文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPackage {
    pub format: String,
    pub version: u32,
    pub exported_at: String,
    pub recipes: Vec<RecipeEntry>,
}

/// 一个应用。**只含展示语义与执行语义，不含任何本机标识。**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub capability: String,
    #[serde(default)]
    pub visibility: RecipeVisibility,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// 字段备注：`874::text → "提示词"`。**这是人写的，必须跟着走。**
    #[serde(default)]
    pub field_labels: BTreeMap<String, String>,
    pub revision: RevisionEntry,
}

/// 应用当前版本的执行定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionEntry {
    pub input_schema: Value,
    #[serde(default)]
    pub output_schema: Value,
    /// 驱动键，`"vendor/protocol"` 形如 `runninghub/runninghub`。
    /// ⚠️ 这是**唯一**用来找连接的线索——不记 connection id。
    pub driver_key: String,
    #[serde(default)]
    pub provider_config: BTreeMap<String, Value>,
    #[serde(default)]
    pub connection_override: ConnectionOverride,
    #[serde(default)]
    pub defaults: BTreeMap<String, Value>,
    #[serde(default)]
    pub source: RecipeSource,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub path: String,
    pub recipe_count: usize,
    pub bytes: u64,
}

/// 导入时被跳过的条目。跳过**永远带原因**——静默少导入几个应用是最难查的失败。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedEntry {
    pub name: String,
    pub reason: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: Vec<String>,
    pub skipped: Vec<SkippedEntry>,
    pub failed: Vec<SkippedEntry>,
}

impl ImportReport {
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty() && self.failed.is_empty()
    }
}

pub struct LibraryService {
    recipes: Arc<dyn RecipeRepository>,
    connections: Arc<dyn ConnectionRepository>,
}

impl LibraryService {
    pub fn new(
        recipes: Arc<dyn RecipeRepository>,
        connections: Arc<dyn ConnectionRepository>,
    ) -> Self {
        Self {
            recipes,
            connections,
        }
    }

    /// 把整个应用库导到 `target`（一个文件）。
    pub async fn export(&self, target: &str) -> DomainResult<ExportReport> {
        let target = target.trim();
        if target.is_empty() {
            return Err(SafeError::validation(
                "EXPORT_TARGET_REQUIRED",
                "请给出导出文件路径（CLI 是 `--to <文件>`）。",
            ));
        }

        let entries = self.collect_entries().await?;
        if entries.is_empty() {
            // 空库导出**不是错误**，但也不该假装成功：给一个空包，调用方自己判断。
            // 这里仍写文件——"我导了一份空的"比"命令没反应"诚实。
        }

        let package = LibraryPackage {
            format: PACKAGE_FORMAT.to_string(),
            version: PACKAGE_VERSION,
            exported_at: Timestamp::now().0,
            recipes: entries,
        };
        let json = serde_json::to_string_pretty(&package)
            .map_err(|error| SafeError::storage(format!("无法序列化应用库：{error}")))?;

        let path = std::path::Path::new(target);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    SafeError::storage(format!("无法创建导出目录：{error}"))
                })?;
            }
        }
        // 覆盖已有文件是**导出**的正常语义（就是"再导一次"），与备份的"不覆盖"不同：
        // 备份是留证据，导出是产出当前状态。
        std::fs::write(path, &json)
            .map_err(|error| SafeError::storage(format!("无法写入导出文件：{error}")))?;

        Ok(ExportReport {
            path: target.to_string(),
            recipe_count: package.recipes.len(),
            bytes: json.len() as u64,
        })
    }

    /// 从 `source` 导入。`replace` 为真时覆盖同名应用（追加一份 revision，不改写历史）。
    pub async fn import(&self, source: &str, replace: bool) -> DomainResult<ImportReport> {
        let source = source.trim();
        if source.is_empty() {
            return Err(SafeError::validation(
                "IMPORT_SOURCE_REQUIRED",
                "请给出要导入的文件（CLI 是 `--from <文件>`）。",
            ));
        }
        let raw = std::fs::read_to_string(source).map_err(|error| {
            SafeError::validation(
                "IMPORT_SOURCE_UNREADABLE",
                format!("读不到 {source}：{error}"),
            )
        })?;
        self.import_text(&raw, replace).await
    }

    /// 从**内容**导入。界面用这条：浏览器的文件选择器给不到绝对路径（安全限制），
    /// 所以前端读成文本再发过来；CLI 那边走 `import`（路径）。
    pub async fn import_text(&self, raw: &str, replace: bool) -> DomainResult<ImportReport> {
        let package: LibraryPackage = serde_json::from_str(raw).map_err(|error| {
            SafeError::validation(
                "IMPORT_PACKAGE_INVALID",
                format!("这不是一个应用库文件（{error}）。"),
            )
        })?;

        if package.format != PACKAGE_FORMAT {
            return Err(SafeError::validation(
                "IMPORT_PACKAGE_INVALID",
                format!(
                    "文件格式是 `{}`，不是 `{PACKAGE_FORMAT}`。",
                    package.format
                ),
            ));
        }
        if package.version > PACKAGE_VERSION {
            return Err(SafeError::validation(
                "IMPORT_PACKAGE_TOO_NEW",
                format!(
                    "这个文件是版本 {} 的，当前只认到 {PACKAGE_VERSION}。升级之后再导入。",
                    package.version
                ),
            ));
        }

        let connections = self.connections.list_connections().await?;
        let existing = self.existing_names().await?;

        let mut report = ImportReport {
            imported: Vec::new(),
            skipped: Vec::new(),
            failed: Vec::new(),
        };

        for entry in package.recipes {
            let name = entry.name.trim().to_string();
            if name.is_empty() {
                report.failed.push(SkippedEntry {
                    name: "(无名字)".to_string(),
                    code: "IMPORT_NAME_REQUIRED".to_string(),
                    reason: "应用没有名字，跳过。".to_string(),
                });
                continue;
            }

            // 同名：默认跳过。导入是把别人的东西拿进来，不该无声改动你自己的库。
            if existing.contains(&name) && !replace {
                report.skipped.push(SkippedEntry {
                    name: name.clone(),
                    code: "IMPORT_NAME_EXISTS".to_string(),
                    reason: "库里已有同名应用；要覆盖请加 `--replace`。".to_string(),
                });
                continue;
            }

            let Some(connection) = pick_connection(&connections, &entry.revision.driver_key) else {
                // **不导入一个跑不起来的空壳**：没有对应平台的连接时，明说缺什么。
                report.failed.push(SkippedEntry {
                    name: name.clone(),
                    code: "IMPORT_CONNECTION_MISSING".to_string(),
                    reason: format!(
                        "本机没有 {} 的连接。先在「连接」页配一条，再导入这个应用。",
                        human_driver(&entry.revision.driver_key)
                    ),
                });
                continue;
            };

            if existing.contains(&name) && replace {
                match self.replace_existing(&name, &entry, connection.id).await {
                    Ok(()) => report.imported.push(name.clone()),
                    Err(error) => report.failed.push(SkippedEntry {
                        name: name.clone(),
                        code: error.code.clone(),
                        reason: error.message.clone(),
                    }),
                }
                continue;
            }

            match self.create_new(&entry, connection.id).await {
                Ok(()) => report.imported.push(name.clone()),
                Err(error) => report.failed.push(SkippedEntry {
                    name: name.clone(),
                    code: error.code.clone(),
                    reason: error.message.clone(),
                }),
            }
        }

        Ok(report)
    }

    /// 逐个应用读当前 revision，组装成可搬的条目。已移除（墓碑）的应用不导出。
    async fn collect_entries(&self) -> DomainResult<Vec<RecipeEntry>> {
        let mut entries = Vec::new();
        let mut after: Option<RecipeId> = None;
        loop {
            let page = self.recipes.list_recipes(200, after).await?;
            if page.is_empty() {
                break;
            }
            let last = page.last().map(|(recipe, _)| recipe.id);
            for (recipe, _binding) in page {
                if recipe.removed_at.is_some() {
                    continue;
                }
                let Some(revision) = self.recipes.find_revision(recipe.current_revision_id).await?
                else {
                    // 当前版本读不到 = 库坏了。**不编一个空 schema 顶上**：那会让导出的包
                    // 看起来正常、导入后才炸。宁可整次导出失败。
                    return Err(SafeError::storage(format!(
                        "应用「{}」的当前版本（#{}）读不到，导出中止。",
                        recipe.name, recipe.current_revision_id.0
                    )));
                };
                entries.push(RecipeEntry {
                    name: recipe.name,
                    description: recipe.description,
                    labels: recipe.labels,
                    capability: recipe.capability,
                    visibility: recipe.visibility,
                    favorite: recipe.favorite,
                    group: recipe.group,
                    note: recipe.note,
                    field_labels: recipe.field_labels,
                    revision: RevisionEntry {
                        input_schema: revision.input_schema,
                        output_schema: revision.output_schema,
                        driver_key: revision.binding.driver_key,
                        provider_config: revision.binding.provider_config,
                        connection_override: revision.binding.connection_override,
                        defaults: revision.defaults,
                        source: revision.source,
                    },
                });
            }
            match last {
                Some(id) => after = Some(id),
                None => break,
            }
        }
        Ok(entries)
    }

    async fn existing_names(&self) -> DomainResult<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let mut after: Option<RecipeId> = None;
        loop {
            let page = self.recipes.list_recipes(200, after).await?;
            if page.is_empty() {
                break;
            }
            let last = page.last().map(|(recipe, _)| recipe.id);
            for (recipe, _) in page {
                if recipe.removed_at.is_none() {
                    names.insert(recipe.name);
                }
            }
            match last {
                Some(id) => after = Some(id),
                None => break,
            }
        }
        Ok(names)
    }

    async fn create_new(
        &self,
        entry: &RecipeEntry,
        connection_id: crate::v2::domain::ids::ConnectionId,
    ) -> DomainResult<()> {
        let now = Timestamp::now();
        let recipe = Recipe {
            id: RecipeId(0),
            name: entry.name.clone(),
            description: entry.description.clone(),
            labels: entry.labels.clone(),
            capability: entry.capability.clone(),
            current_revision_id: RecipeRevisionId(0),
            visibility: entry.visibility.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            favorite: entry.favorite,
            group: entry.group.clone(),
            note: entry.note.clone(),
            last_opened_at: None,
            field_labels: entry.field_labels.clone(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: RecipeId(0),
            revision: 1,
            input_schema: entry.revision.input_schema.clone(),
            output_schema: entry.revision.output_schema.clone(),
            binding: RecipeBinding {
                driver_key: entry.revision.driver_key.clone(),
                provider_config: entry.revision.provider_config.clone(),
                default_connection_id: connection_id,
                // 账号池是**这台机器**上的多条账号，别人的池子搬过来只会指向不存在的连接。
                allowed_connection_ids: Vec::new(),
                connection_override: entry.revision.connection_override,
            },
            defaults: entry.revision.defaults.clone(),
            source: entry.revision.source.clone(),
            created_at: now,
        };
        // 仓库在事务里分配 revision id，**回写 `current_revision_id` 是调用方的责任**
        // （见 `CreateRecipe::execute` 里同一行的做法）——漏了它，应用在库里就指向
        // revision 0，重启 daemon 之后读不到自己的版本。
        let (mut recipe, revision) = self.recipes.create_recipe(recipe, revision).await?;
        recipe.current_revision_id = revision.id;
        self.recipes.update_recipe(&recipe).await?;
        Ok(())
    }

    /// 覆盖同名应用：改元数据 + **追加**一份 revision。
    ///
    /// ⚠️ 不就地改 current revision：revision 是不可变的，旧任务要指得回去（见
    /// `RecipeRepository::append_revision` 的说明）。覆盖 = 元数据更新 + 新版本，
    /// 而不是"抹掉昨天那个应用的样子"。
    async fn replace_existing(
        &self,
        name: &str,
        entry: &RecipeEntry,
        connection_id: crate::v2::domain::ids::ConnectionId,
    ) -> DomainResult<()> {
        let mut found = None;
        let mut after: Option<RecipeId> = None;
        'outer: loop {
            let page = self.recipes.list_recipes(200, after).await?;
            if page.is_empty() {
                break;
            }
            let last = page.last().map(|(recipe, _)| recipe.id);
            for (recipe, _) in page {
                if recipe.removed_at.is_none() && recipe.name == name {
                    found = Some(recipe);
                    break 'outer;
                }
            }
            match last {
                Some(id) => after = Some(id),
                None => break,
            }
        }
        let Some(mut recipe) = found else {
            return Err(SafeError::conflict(
                "IMPORT_NAME_EXISTS",
                format!("找不到名为「{name}」的应用。"),
            ));
        };
        let Some(current) = self.recipes.find_revision(recipe.current_revision_id).await? else {
            return Err(SafeError::storage(format!(
                "应用「{name}」的当前版本读不到，无法覆盖。"
            )));
        };

        let now = Timestamp::now();
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: recipe.id,
            revision: current.revision + 1,
            input_schema: entry.revision.input_schema.clone(),
            output_schema: entry.revision.output_schema.clone(),
            binding: RecipeBinding {
                driver_key: entry.revision.driver_key.clone(),
                provider_config: entry.revision.provider_config.clone(),
                default_connection_id: connection_id,
                allowed_connection_ids: Vec::new(),
                connection_override: entry.revision.connection_override,
            },
            defaults: entry.revision.defaults.clone(),
            source: entry.revision.source.clone(),
            created_at: now.clone(),
        };
        let revision = self.recipes.append_revision(revision).await?;

        recipe.description = entry.description.clone();
        recipe.labels = entry.labels.clone();
        recipe.capability = entry.capability.clone();
        recipe.visibility = entry.visibility.clone();
        recipe.favorite = entry.favorite;
        recipe.group = entry.group.clone();
        recipe.note = entry.note.clone();
        recipe.field_labels = entry.field_labels.clone();
        recipe.current_revision_id = revision.id;
        recipe.updated_at = now;
        self.recipes.update_recipe(&recipe).await?;
        Ok(())
    }
}

/// 按驱动键在**本机**的连接里挑一条。
///
/// 驱动键是 `"vendor/protocol"`（与 `DriverRegistry` 的注册键同形）。匹配不上就返回
/// `None`——调用方据此把这条记成"缺连接"，而不是绑一条不相干的连接。
fn pick_connection<'a>(
    connections: &'a [crate::v2::domain::model::Connection],
    driver_key: &str,
) -> Option<&'a crate::v2::domain::model::Connection> {
    let (vendor, protocol) = split_driver_key(driver_key)?;
    connections
        .iter()
        .find(|connection| connection.vendor == vendor && connection.protocol == protocol)
}

/// 把驱动键拆开。带斜杠的按斜杠拆；只有一段的（历史上的裸键）当成 vendor 自己也当协议——
/// 那种数据确实存在（见 `RecipeBinding::driver_key` 的说明），不认它就等于把老应用全判死。
fn split_driver_key(driver_key: &str) -> Option<(&str, &str)> {
    let trimmed = driver_key.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.split_once('/') {
        Some((vendor, protocol)) => Some((vendor, protocol)),
        None => Some((trimmed, trimmed)),
    }
}

fn human_driver(driver_key: &str) -> String {
    match split_driver_key(driver_key) {
        Some((vendor, _)) => match vendor {
            "runninghub" => "RunningHub".to_string(),
            "comfyui" => "ComfyUI".to_string(),
            "openai" => "OpenAI 兼容 API".to_string(),
            other => other.to_string(),
        },
        None => driver_key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::ids::ConnectionId;
    use crate::v2::domain::model::{
        Connection, ConnectionRevision, ConnectionStatus, LifecycleFeatures, SecretBinding,
        SecretResolution,
    };
    use crate::v2::infrastructure::sqlite::SqliteRepositories;
    use crate::v2::ports::repositories::WORKSPACE_DB_FILE;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    /// 每个用例一个独立目录（测试并行跑，别互相踩）。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-library-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    /// **真库**：这条路径的价值全在"schema 里的备注名能不能原样往返"，
    /// 替身库验不到序列化那一层。
    struct Harness {
        service: LibraryService,
        recipes: Arc<dyn RecipeRepository>,
        connections: Arc<dyn ConnectionRepository>,
    }

    fn harness(dir: &Path) -> Harness {
        let repos =
            Arc::new(SqliteRepositories::open(&dir.join(WORKSPACE_DB_FILE)).expect("开库"));
        Harness {
            service: LibraryService::new(
                repos.clone() as Arc<dyn RecipeRepository>,
                repos.clone() as Arc<dyn ConnectionRepository>,
            ),
            recipes: repos.clone() as Arc<dyn RecipeRepository>,
            connections: repos as Arc<dyn ConnectionRepository>,
        }
    }

    /// 造一条 RunningHub 连接，返回它的 id。
    fn seed_connection(harness: &Harness) -> ConnectionId {
        let connection = Connection {
            id: ConnectionId(0),
            vendor: "runninghub".to_string(),
            protocol: "runninghub".to_string(),
            display_name: "国内站".to_string(),
            current_revision_id: crate::v2::domain::ids::ConnectionRevisionId(0),
            status: ConnectionStatus::Active,
        };
        let revision = ConnectionRevision {
            id: crate::v2::domain::ids::ConnectionRevisionId(0),
            connection_id: ConnectionId(0),
            config: BTreeMap::from([("baseUrl".to_string(), json!("https://www.runninghub.cn"))]),
            secret_bindings: vec![SecretBinding {
                slot: "runninghub-api_key".to_string(),
                resolution: SecretResolution::Current,
            }],
            effective_capabilities: vec![],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        };
        let (connection, _) = futures::executor::block_on(
            harness.connections.create_connection(connection, revision),
        )
        .expect("建连接");
        connection.id
    }

    /// 造一个被**人策展过**的应用：分组、收藏、字段备注名、控件全都有。
    fn seed_recipe(harness: &Harness, connection_id: ConnectionId, name: &str) {
        let now = Timestamp::now();
        let recipe = Recipe {
            id: RecipeId(0),
            name: name.to_string(),
            description: "人像生成".to_string(),
            labels: vec!["人像".to_string()],
            capability: "image.generate".to_string(),
            current_revision_id: RecipeRevisionId(0),
            visibility: RecipeVisibility::Private,
            created_at: now.clone(),
            updated_at: now.clone(),
            favorite: true,
            group: Some("常用".to_string()),
            note: Some("跑 1024 最稳".to_string()),
            last_opened_at: None,
            field_labels: BTreeMap::from([
                ("475::image".to_string(), "人物图".to_string()),
                ("874::text".to_string(), "提示词".to_string()),
            ]),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(0),
            recipe_id: RecipeId(0),
            revision: 1,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "475::image": { "type": "string", "description": "image", "x-widget": "file" },
                    "874::text": { "type": "string", "description": "text", "x-widget": "textarea" }
                }
            }),
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: "runninghub/runninghub".to_string(),
                provider_config: BTreeMap::from([(
                    "webappId".to_string(),
                    json!("1234567890"),
                )]),
                default_connection_id: connection_id,
                allowed_connection_ids: vec![],
                connection_override: ConnectionOverride::Denied,
            },
            defaults: BTreeMap::from([("874::text".to_string(), json!("一只猫"))]),
            source: RecipeSource::Embedded,
            created_at: now,
        };
        // 与生产路径同形：create 分配 id，调用方回写 current_revision_id。
        let (mut recipe, revision) =
            futures::executor::block_on(harness.recipes.create_recipe(recipe, revision))
                .expect("建应用");
        recipe.current_revision_id = revision.id;
        futures::executor::block_on(harness.recipes.update_recipe(&recipe)).expect("回写当前版本");
    }

    fn only_recipe(harness: &Harness) -> (Recipe, RecipeBinding) {
        let page = futures::executor::block_on(harness.recipes.list_recipes(50, None))
            .expect("列应用");
        assert_eq!(page.len(), 1, "应当恰好有一个应用");
        page.into_iter().next().unwrap()
    }

    /// **验收判据**：导出再导入到**另一个库**，人策展过的东西一样都不能少，
    /// 而连接要落在**新库自己的**那条上（不是把旧主键搬过来）。
    #[tokio::test]
    async fn a_library_round_trips_into_another_workspace() {
        let source_dir = temp_dir("src");
        let source = harness(&source_dir);
        let source_connection = seed_connection(&source);
        seed_recipe(&source, source_connection, "Krea2-turbo");

        let package_path = temp_dir("pkg").join("library.json");
        let export = source
            .service
            .export(&package_path.to_string_lossy())
            .await
            .expect("导出应当成功");
        assert_eq!(export.recipe_count, 1);
        assert!(export.bytes > 0);

        // 包本身**不许**带连接主键与密钥。
        let raw = std::fs::read_to_string(&package_path).expect("读包");
        assert!(
            !raw.contains("defaultConnectionId") && !raw.contains("api_key"),
            "包里不该出现连接主键或密钥：{raw}"
        );

        let dest_dir = temp_dir("dst");
        let dest = harness(&dest_dir);
        let dest_connection = seed_connection(&dest);
        let report = dest
            .service
            .import(&package_path.to_string_lossy(), false)
            .await
            .expect("导入应当成功");
        assert_eq!(report.imported, vec!["Krea2-turbo".to_string()]);
        assert!(report.failed.is_empty() && report.skipped.is_empty());

        let (recipe, binding) = only_recipe(&dest);
        assert_eq!(recipe.name, "Krea2-turbo");
        assert_eq!(recipe.group.as_deref(), Some("常用"));
        assert!(recipe.favorite, "收藏必须跟着走");
        assert_eq!(
            recipe.note.as_deref(),
            Some("跑 1024 最稳"),
            "本地备注必须跟着走"
        );
        assert_eq!(
            recipe.field_labels.get("475::image").map(String::as_str),
            Some("人物图"),
            "字段备注名是这个功能的核心，必须原样"
        );
        assert_eq!(
            binding.default_connection_id, dest_connection,
            "连接必须落在新库自己的那条上，而不是搬旧主键"
        );
        assert_eq!(binding.driver_key, "runninghub/runninghub");

        let revision = dest
            .recipes
            .find_revision(recipe.current_revision_id)
            .await
            .expect("读版本")
            .expect("版本应当存在");
        assert_eq!(
            revision.input_schema["properties"]["874::text"]["x-widget"],
            json!("textarea"),
            "控件是用户的决定，必须原样"
        );
        assert_eq!(revision.defaults["874::text"], json!("一只猫"));

        let _ = std::fs::remove_dir_all(&source_dir);
        let _ = std::fs::remove_dir_all(&dest_dir);
    }

    /// 负对照：新机器上**没有对应平台的连接** ⇒ 这条不导入，且**说清缺什么**。
    /// 塞一个跑不起来的空壳是这个功能最容易犯的错。
    #[tokio::test]
    async fn an_import_without_the_platform_connection_is_refused_not_faked() {
        let source_dir = temp_dir("src2");
        let source = harness(&source_dir);
        let source_connection = seed_connection(&source);
        seed_recipe(&source, source_connection, "Krea2-turbo");
        let package_path = temp_dir("pkg2").join("library.json");
        source
            .service
            .export(&package_path.to_string_lossy())
            .await
            .expect("导出");

        // 目标库一条连接都没有。
        let dest_dir = temp_dir("dst2");
        let dest = harness(&dest_dir);
        let report = dest
            .service
            .import(&package_path.to_string_lossy(), false)
            .await
            .expect("导入流程本身应当跑完");

        assert!(report.imported.is_empty(), "不该导入任何应用");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, "IMPORT_CONNECTION_MISSING");
        assert!(
            report.failed[0].reason.contains("RunningHub"),
            "原因里要说清缺哪种连接：{}",
            report.failed[0].reason
        );
        let page = dest.recipes.list_recipes(50, None).await.expect("列应用");
        assert!(page.is_empty(), "库里不该多出一个空壳");

        let _ = std::fs::remove_dir_all(&source_dir);
        let _ = std::fs::remove_dir_all(&dest_dir);
    }

    /// 负对照：同名应用默认**跳过**（不悄悄改你自己的库）；`replace` 才覆盖，
    /// 且覆盖是**追加 revision**，不是改写历史。
    #[tokio::test]
    async fn an_existing_name_is_skipped_unless_replace_is_asked_for() {
        let source_dir = temp_dir("src3");
        let source = harness(&source_dir);
        let source_connection = seed_connection(&source);
        seed_recipe(&source, source_connection, "Krea2-turbo");
        let package_path = temp_dir("pkg3").join("library.json");
        source
            .service
            .export(&package_path.to_string_lossy())
            .await
            .expect("导出");

        let dest_dir = temp_dir("dst3");
        let dest = harness(&dest_dir);
        let dest_connection = seed_connection(&dest);
        seed_recipe(&dest, dest_connection, "Krea2-turbo");

        let skipped = dest
            .service
            .import(&package_path.to_string_lossy(), false)
            .await
            .expect("导入");
        assert_eq!(skipped.skipped.len(), 1);
        assert_eq!(skipped.skipped[0].code, "IMPORT_NAME_EXISTS");
        assert!(skipped.imported.is_empty());

        let replaced = dest
            .service
            .import(&package_path.to_string_lossy(), true)
            .await
            .expect("覆盖导入");
        assert_eq!(replaced.imported, vec!["Krea2-turbo".to_string()]);

        let (recipe, _) = only_recipe(&dest);
        let revision = dest
            .recipes
            .find_revision(recipe.current_revision_id)
            .await
            .expect("读版本")
            .expect("版本应当在");
        assert_eq!(revision.revision, 2, "覆盖是追加新版本，不是改写历史");

        let _ = std::fs::remove_dir_all(&source_dir);
        let _ = std::fs::remove_dir_all(&dest_dir);
    }

    /// 负对照：不是应用库文件 ⇒ 按**格式**拒绝，不猜一个能解析的读法。
    #[tokio::test]
    async fn a_foreign_file_is_refused_by_format() {
        let dir = temp_dir("foreign");
        let dest = harness(&dir);
        let path = dir.join("other.json");
        std::fs::write(
            &path,
            json!({ "format": "something.else", "version": 1, "recipes": [] }).to_string(),
        )
        .expect("写文件");

        let error = dest
            .service
            .import(&path.to_string_lossy(), false)
            .await
            .expect_err("格式不对必须被拒");
        assert_eq!(error.code, "IMPORT_PACKAGE_INVALID");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 负对照：没给路径 ⇒ 拒绝（不猜一个默认位置）。
    #[tokio::test]
    async fn blank_paths_are_refused() {
        let dir = temp_dir("blank");
        let service = harness(&dir).service;
        assert_eq!(
            service.export("   ").await.expect_err("空导出路径").code,
            "EXPORT_TARGET_REQUIRED"
        );
        assert_eq!(
            service.import("", false).await.expect_err("空导入路径").code,
            "IMPORT_SOURCE_REQUIRED"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
