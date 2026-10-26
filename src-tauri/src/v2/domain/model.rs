use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::SystemTime;

use super::errors::SafeError;
use super::ids::{ConnectionId, ConnectionRevisionId, RecipeId, RecipeRevisionId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp(pub String);

impl Timestamp {
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        Self(format!("{millis}"))
    }

    pub fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0) as u64
    }

    pub fn from_millis(millis: u64) -> Self {
        Self(format!("{millis}"))
    }

    pub fn millis(&self) -> u64 {
        self.0.parse::<u64>().unwrap_or(0)
    }

    pub fn is_expired(&self) -> bool {
        self.millis() <= Self::now_millis()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecipeVisibility {
    #[default]
    Private,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub id: RecipeId,
    pub name: String,
    pub description: String,
    pub labels: Vec<String>,
    pub capability: String,
    pub current_revision_id: RecipeRevisionId,
    pub visibility: RecipeVisibility,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// 资源库展示元数据：是否收藏。
    #[serde(default)]
    pub favorite: bool,
    /// 资源库展示元数据：分组名。
    #[serde(default)]
    pub group: Option<String>,
    /// 资源库展示元数据：本地备注。
    #[serde(default)]
    pub note: Option<String>,
    /// 资源库展示元数据：最近一次打开时间。
    #[serde(default)]
    pub last_opened_at: Option<Timestamp>,
    /// 展示元数据：**字段备注**（schema 的 key → 人话名字）。
    ///
    /// ⚠️ 为什么放在 `Recipe` 而不是 revision 的 `input_schema` 里：
    /// revision 管的是**执行语义**（跑哪个工作流、字段怎么映射到节点、约束是什么），
    /// 而「`874::text` 其实是正向提示词」是**展示语义**——它跟 `note` / `group` 同类，
    /// 所以跟它们放在一起。这带来一个实际好处：**已导入的应用可以事后补标注**，
    /// 不必重新导入（重导会产生重复 Recipe，还要改 Agent 白名单，代价远大于标注本身）。
    ///
    /// ⚠️ 为什么需要它：RunningHub 的 schema 里 `description` 就等于字段名
    /// （实测 `"874::text": {"description": "text"}`）——平台**没有**给语义，我们如实转达，
    /// 于是 Agent 和用户看到的都是 `874::text` 这种读不出意思的键。语义的唯一来源是**人**。
    /// ComfyUI 导入路径早就有「导入时给字段起名」的机制，RunningHub 路径没有。
    #[serde(default)]
    pub field_labels: BTreeMap<String, String>,
    /// **墓碑**：非空 = 已从资源库移除（可恢复），`NULL` = 在。
    ///
    /// ⚠️ 为什么是时间戳而不是 `hidden: bool`：
    /// 1. 记下"什么时候"——"最近删除"视图与审计都需要；
    /// 2. 一个字段表达两件事（`NULL` = 在），布尔要配一个时间戳才是两个字段；
    /// 3. **能同步**——硬删除**无法传播**（没法告诉另一个副本"这东西没了"），
    ///    墓碑可以。跨设备/多副本时布尔不够，墓碑是标准答案。
    ///
    /// ⚠️ 为什么需要它：`archive_recipe` 此前是**硬删除**，且遇到"已被历史任务引用"
    /// 就报错——于是**只要一个应用跑过一次，它就永远无法从库里移除**。
    /// 而"用户自己策展的那一份应用"正是本产品相对平台扁平目录的价值，用户却没法策展它。
    /// 根因是把两件不同的事塞进了一个动作：**清理**（不想再看到）与**销毁**（抹掉记录）。
    /// 清理不需要销毁历史。
    ///
    /// ⚠️ 移除 = "别再用了"：Agent 的应用目录跳过它，`run_app` 拒绝它。
    /// 否则"移除"只是视觉上消失，Agent 还能跑它。
    #[serde(default)]
    pub removed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeRevision {
    pub id: RecipeRevisionId,
    pub recipe_id: RecipeId,
    pub revision: u32,
    pub input_schema: Value,
    pub output_schema: Value,
    pub binding: RecipeBinding,
    pub defaults: BTreeMap<String, Value>,
    pub source: RecipeSource,
    pub created_at: Timestamp,
}

/// 这份 recipe 的**定义**存在哪 —— 只此一个轴，别再混进"从哪来"。
///
/// ⚠️ 两个变体的**线格式刻意保持历史值**（`"imported"` / `"local"`）：`source` 是
/// `serde_json::to_string` 落进 `v2_recipe_revisions.source` 的（见 `sqlite/mod.rs`），
/// 读回来是 `unwrap_or_default()` —— 改线格式会让**老数据静默变成默认值**，
/// 而默认值恰好是"自带定义"，于是平台托管的那些应用会被悄悄当成自带定义。
/// 所以：**Rust 侧改名叫得清楚，线格式钉死不动。**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecipeSource {
    /// 平台托管：我们只存一个**引用**（RunningHub「AI 应用」存 `webappId`），
    /// 定义在平台上——作者改了、删了，我们这边跟着变。
    ///
    /// 旧名 `Imported`。改成这个是因为 `Imported` 与 `Local` 不在一个轴上
    /// （一个说"从哪来"、一个说"放哪"），读的人没法一眼比出区别。
    #[serde(rename = "imported")]
    Referenced,
    /// 自带定义：**整份定义存在我们这边**（粘贴的 ComfyUI / RunningHub 工作流 JSON），
    /// 平台只负责跑。
    ///
    /// 旧名 `Local`。那个名字读起来像"本地执行"（即本机 ComfyUI），
    /// 但它说的是"定义放哪"——RunningHub 工作流导入也走这里，跟本地执行毫无关系。
    #[default]
    #[serde(rename = "local")]
    Embedded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RecipeBinding {
    /// 驱动键。**权威形态是 `driver_key(vendor, protocol)` = `"vendor/protocol"`**
    /// （见 `v2::ports::provider::driver_key`），与 `DriverRegistry` 的注册键同形，
    /// 例如 `"runninghub/runninghub"`、`"openai/openai-images"`。
    ///
    /// ⚠️ 别写裸键（`"runninghub"`）。历史上导入路径写过裸键，于是同一张表里出现两种形态，
    /// 前端按 `driverKey` 判配方类型时对一半数据判错；更麻烦的是**测试夹具里裸键成片存在**
    /// （`grep -rn 'driver_key: *"' src-tauri/src/`），照夹具推会推出错误结论。
    /// 该字段在运行时**只用于展示**（`transports/http/handlers.rs` 输出 `driverKey` 给前端）；
    /// 执行分派是从**连接**的 vendor/protocol 推导键（`application/scheduler.rs`），与本字段无关。
    pub driver_key: String,
    pub provider_config: BTreeMap<String, Value>,
    pub default_connection_id: ConnectionId,
    pub allowed_connection_ids: Vec<ConnectionId>,
    pub connection_override: ConnectionOverride,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOverride {
    #[default]
    Denied,
    Scoped,
}

impl RecipeBinding {
    pub fn allows(&self, connection_id: ConnectionId) -> bool {
        self.allowed_connection_ids.contains(&connection_id)
    }

    /// 这一批可以铺到哪几条连接（默认连接排第一，去重）。
    ///
    /// **列表里有第二条 = 这是一只账号池**：配方自己声明了"这几条连接可以互换"。
    /// 只列一条（绝大多数配方）时下面的分配就是恒等于那条，行为与以前逐字节一致。
    pub fn batch_connections(&self) -> Vec<ConnectionId> {
        let mut pool = vec![self.default_connection_id];
        for connection_id in &self.allowed_connection_ids {
            if !pool.contains(connection_id) {
                pool.push(*connection_id);
            }
        }
        pool
    }

    /// 这一批里的第 `sequence` 条 run 该落在哪条连接。
    ///
    /// 调用方**没指定**连接、而配方列了多条可用连接时，按序号轮转：
    /// 调用方只提交批次，不选账号——这是账号池的全部语义。
    /// 指定了连接仍走 `connection_allowed`（连同 scope 校验）。
    ///
    /// ⚠️ 轮转**不做容量加权**：创建时不知道各账号此刻有几个空位（那要问平台）。
    /// 分歪了由调度器的"改派"兜底——见 `scheduler` 里对未尝试 run 的重绑。
    pub fn connection_for_run(
        &self,
        requested: Option<ConnectionId>,
        sequence: u32,
    ) -> Result<ConnectionId, SafeError> {
        if requested.is_some() {
            return self.connection_allowed(requested);
        }
        let pool = self.batch_connections();
        Ok(pool[(sequence as usize) % pool.len()])
    }

    pub fn connection_allowed(
        &self,
        requested: Option<ConnectionId>,
    ) -> Result<ConnectionId, SafeError> {
        match requested {
            None => Ok(self.default_connection_id),
            Some(connection_id) if connection_id == self.default_connection_id => Ok(connection_id),
            Some(connection_id) if self.connection_override == ConnectionOverride::Scoped => {
                if self.allows(connection_id) {
                    Ok(connection_id)
                } else {
                    Err(SafeError::validation(
                        "CONNECTION_NOT_ALLOWED",
                        "请求的 Connection 不在该 Recipe 允许范围内",
                    ))
                }
            }
            Some(_) => Err(SafeError::validation(
                "CONNECTION_OVERRIDE_DENIED",
                "该 Recipe 不允许覆盖默认 Connection",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Active,
    Disabled,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    pub vendor: String,
    pub protocol: String,
    pub display_name: String,
    pub current_revision_id: ConnectionRevisionId,
    pub status: ConnectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRevision {
    pub id: ConnectionRevisionId,
    pub connection_id: ConnectionId,
    pub config: BTreeMap<String, Value>,
    pub secret_bindings: Vec<SecretBinding>,
    pub effective_capabilities: Vec<String>,
    pub lifecycle_features: LifecycleFeatures,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretBinding {
    pub slot: String,
    pub resolution: SecretResolution,
}

/// 仅识别连接级 scoped API key 槽位（旧的无作用域 `api_key` 已由
/// `migrate_legacy_api_key_slots` 一次性迁移并删除，启动后不再识别）。
pub fn is_api_key_slot(slot: &str) -> bool {
    slot.ends_with("-api_key")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretResolution {
    Current,
    Pinned(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LifecycleFeatures {
    #[serde(default)]
    pub asset_upload: bool,
    #[serde(default)]
    pub progress_events: bool,
    #[serde(default)]
    pub remote_cancel: bool,
    #[serde(default)]
    pub synchronous_result: bool,
    #[serde(default)]
    pub remote_reconciliation: bool,
}

impl LifecycleFeatures {
    pub fn supports_remote_cancel(&self) -> bool {
        self.remote_cancel
    }

    pub fn supports_remote_reconciliation(&self) -> bool {
        self.remote_reconciliation
    }

    pub fn supports_synchronous_result(&self) -> bool {
        self.synchronous_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(default: ConnectionId) -> RecipeBinding {
        RecipeBinding {
            driver_key: "openai-images".to_string(),
            provider_config: BTreeMap::new(),
            default_connection_id: default,
            allowed_connection_ids: vec![default],
            connection_override: ConnectionOverride::Denied,
        }
    }

    #[test]
    fn default_connection_is_resolved_when_not_specified() {
        let recipe = binding(ConnectionId(1));
        assert_eq!(recipe.connection_allowed(None), Ok(ConnectionId(1)));
    }

    /// **线格式契约**：`RecipeSource` 的 Rust 名字改过（`Imported`→`Referenced`、
    /// `Local`→`Embedded`），但落库的字符串**必须还是历史值**。
    ///
    /// 为什么值得一条专门的测试：`source` 是 `serde_json::to_string` 进
    /// `v2_recipe_revisions.source` 的，而读回来是 `unwrap_or_default()` ——
    /// 线格式一变，**老数据会静默变成默认值**（= `Embedded`），于是平台托管的那些
    /// 应用被悄悄当成"自带定义"。这种错不报任何异常，只是行为慢慢不对。
    ///
    /// 所以这里钉死三件事：两个历史值都能读、写回去还是那两个值、以及那个已删掉的
    /// `"generated"` 落到默认值（不会 panic）。
    #[test]
    fn recipe_source_wire_format_is_pinned_to_historical_values() {
        // 读：历史行必须还原成正确的变体，不能靠"恰好等于默认值"蒙对。
        let referenced: RecipeSource = serde_json::from_str("\"imported\"").unwrap();
        assert_eq!(referenced, RecipeSource::Referenced);
        let embedded: RecipeSource = serde_json::from_str("\"local\"").unwrap();
        assert_eq!(embedded, RecipeSource::Embedded);
        // 两者必须是**不同**的变体——否则上面两条断言在"全塌成默认值"时也会通过。
        assert_ne!(referenced, embedded);

        // 写：落库的字符串不许跟着 Rust 名字走。
        assert_eq!(
            serde_json::to_string(&RecipeSource::Referenced).unwrap(),
            "\"imported\""
        );
        assert_eq!(
            serde_json::to_string(&RecipeSource::Embedded).unwrap(),
            "\"local\""
        );

        // 已删除的死代码值：读不了就落默认（`sqlite` 那边用的就是 `unwrap_or_default`）。
        assert!(serde_json::from_str::<RecipeSource>("\"generated\"").is_err());
        assert_eq!(RecipeSource::default(), RecipeSource::Embedded);
    }

    #[test]
    fn scoped_override_allows_allowlisted_connection() {
        let mut recipe = binding(ConnectionId(1));
        recipe.allowed_connection_ids.push(ConnectionId(2));
        recipe.connection_override = ConnectionOverride::Scoped;
        assert_eq!(
            recipe.connection_allowed(Some(ConnectionId(2))),
            Ok(ConnectionId(2))
        );
    }

    #[test]
    fn denied_override_rejects_non_default_connection() {
        let recipe = binding(ConnectionId(1));
        assert!(recipe.connection_allowed(Some(ConnectionId(2))).is_err());
    }

    #[test]
    fn scoped_override_rejects_connection_outside_allowlist() {
        let mut recipe = binding(ConnectionId(1));
        recipe.connection_override = ConnectionOverride::Scoped;
        assert!(recipe.connection_allowed(Some(ConnectionId(2))).is_err());
    }

    #[test]
    fn lifecycle_features_report_capabilities() {
        let features = LifecycleFeatures {
            remote_cancel: true,
            remote_reconciliation: true,
            synchronous_result: true,
            ..Default::default()
        };
        assert!(features.supports_remote_cancel());
        assert!(features.supports_remote_reconciliation());
        assert!(features.supports_synchronous_result());
    }
}
