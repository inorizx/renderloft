use async_trait::async_trait;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::v2::domain::errors::{DomainResult, FaultScope, SafeError};
use crate::v2::domain::execution::{
    CancelOutcome, ConnectionCheck, ProviderDescriptor, RemoteHandle, SubmitOutcome, TaskSnapshot,
};
use crate::v2::domain::media::AssetRef;
use crate::v2::domain::model::{ConnectionRevision, RecipeBinding};

pub fn driver_key(vendor: &str, protocol: &str) -> String {
    format!("{vendor}/{protocol}")
}

#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    pub asset_ref: AssetRef,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PrepareRequest {
    pub connection_revision: ConnectionRevision,
    pub recipe_binding: RecipeBinding,
    pub inputs: BTreeMap<String, Value>,
    pub assets: Vec<ResolvedAsset>,
    pub request_fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct PreparedExecution {
    pub connection_revision: ConnectionRevision,
    pub provider_request: Value,
    pub request_id: String,
}

#[derive(Debug, Clone)]
pub struct ReconcileRequest {
    pub connection_revision: ConnectionRevision,
    pub remote_handle: RemoteHandle,
}

#[derive(Debug, Clone)]
pub struct CancelRequest {
    pub connection_revision: ConnectionRevision,
    pub remote_handle: RemoteHandle,
}

/// 平台应用声明（recipe 导入用）。由 Provider 驱动提供；
/// 不支持的驱动返回 `IMPORT_NOT_SUPPORTED`。
#[derive(Debug, Clone)]
pub struct WebappSchema {
    pub webapp_id: String,
    pub webapp_name: String,
    pub description: String,
    pub description_en: String,
    pub nodes: Vec<WebappNodeInfo>,
}

#[derive(Debug, Clone)]
pub struct WebappNodeInfo {
    pub node_id: String,
    pub field_name: String,
    pub field_type: String,
    pub field_value: Option<Value>,
    pub description: String,
    pub description_en: String,
    pub field_data: Value,
}

/// 引擎声明的**一个节点类**的定义（ComfyUI 的 `GET /object_info/<class>`）。
#[derive(Debug, Clone, Default)]
pub struct NodeClassInfo {
    /// 引擎给这个类起的显示名（一般与类名相同，自定义节点可能不同）。
    pub display_name: String,
    /// 输入名 → 规格（`required` 与 `optional` 合并；同名时以 `required` 为准）。
    pub inputs: BTreeMap<String, NodeInputSpec>,
}

/// 一个输入的规格。**这就是 API prompt 里没有的那部分信息**。
#[derive(Debug, Clone, Default)]
pub struct NodeInputSpec {
    /// `INT` / `FLOAT` / `STRING` / `BOOLEAN` / `COMBO`（下拉）/ 其它自定义类型名。
    pub kind: String,
    /// `COMBO` 时的全部合法取值。
    pub choices: Vec<String>,
    pub default: Option<Value>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    /// 引擎说这个字符串输入是多行文本。
    pub multiline: bool,
}

/// 一批节点类的定义。
///
/// 为什么需要它：API prompt 里每个输入只有**当前选中的那个值**——`sampler_name` 的合法取值、
/// `steps` 的范围、哪个输入是多行，**全都不在文件里**，而在引擎里。不查它就只能生成通用控件
/// （文本框 + 没有范围的数字框），而"这个字段本该是下拉"恰恰是用户一眼就能看出来、
/// 也最影响好不好用的地方。
#[derive(Debug, Clone, Default)]
pub struct NodeInfoCatalog {
    pub classes: BTreeMap<String, NodeClassInfo>,
}

impl NodeInfoCatalog {
    pub fn input(&self, class_type: &str, field_name: &str) -> Option<&NodeInputSpec> {
        self.classes.get(class_type)?.inputs.get(field_name)
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }
}

/// 平台**此刻**声明的并发容量。
///
/// 为什么不是"我们自己数自己"：本地账本只知道**这条连接上我们自己提交了几个**，而同一把
/// API Key 还可能被网页控制台、别的工具用着。平台把 `runningCount` 报得很清楚（含所有来源），
/// 读它才是"现在到底还有没有位子"的事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueCapacity {
    /// 这个连接允许同时跑几个（RunningHub 的 `concurrentLimit`）。
    pub limit: usize,
    /// 平台侧正在跑的个数（含不是我们提交的）。
    pub running: usize,
}

#[async_trait]
pub trait ProviderDriver: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn available_slots(&self) -> usize {
        1
    }

    /// 从 `fetch_queue_status` 的原始返回值里读出并发容量。`None` = 平台没有这个说法
    /// （ComfyUI / 模型 API），调度退回 `available_slots()`。
    ///
    /// 刻意读**原始 Value** 而不是让驱动多开一个端点：`fetch_queue_status` 已经是同一次
    /// 查询，解析放在驱动里，调度只认 `QueueCapacity` 这一个形状。
    fn queue_capacity(&self, _status: &Value) -> Option<QueueCapacity> {
        None
    }

    /// 这次失败该怪谁？默认按 `SafeError::fault_scope` 判；平台把多种拒绝压成同一个码时，
    /// 只有驱动知道那句话是什么意思（见 RunningHub 的实现）。
    fn fault_scope(&self, error: &SafeError) -> FaultScope {
        error.fault_scope()
    }

    /// 这次提交失败是不是"平台并发已满"？
    ///
    /// 调度用它做两件事：**本轮不再领**（连着撞三次只会让三条 run 一起进退避），
    /// 以及**丢掉容量缓存**（下一个 tick 重新问平台，别拿旧数字接着撞）。
    fn is_capacity_rejection(&self, _error: &SafeError) -> bool {
        false
    }

    /// 拉取节点定义（ComfyUI 的 `GET /object_info/<class>`），用于把"通用控件"升级成真控件。
    ///
    /// 语义（调用方依赖它）：
    /// * **部分成功也算成功**：某个自定义节点没装 ⇒ 那个类缺席，其余照用；
    /// * **一个都没拿到** ⇒ 返回 `Err`，调用方据此降级成通用控件并说明原因；
    /// * 不支持这条路的驱动报错（同 `fetch_models` 那条规矩：不支持就说，不能回空目录
    ///   —— 空目录的意思是"引擎说它一个节点都没有"）。
    async fn fetch_node_info(
        &self,
        _connection: &ConnectionRevision,
        _class_types: &[String],
    ) -> DomainResult<NodeInfoCatalog> {
        Err(SafeError::validation(
            "NODE_INFO_NOT_SUPPORTED",
            "该 Provider 不支持节点定义查询",
        ))
    }

    /// 拉取平台应用声明，用于把平台工作流导入为本地 Recipe。
    async fn fetch_webapp_schema(
        &self,
        _connection: &ConnectionRevision,
        _webapp_id: &str,
    ) -> DomainResult<WebappSchema> {
        Err(SafeError::validation(
            "IMPORT_NOT_SUPPORTED",
            "该 Provider 不支持导入应用",
        ))
    }

    /// 拉取平台 ComfyUI 工作流的 API 格式 JSON，用于云端 ComfyUI 导入。
    async fn fetch_workflow_prompt(
        &self,
        _connection: &ConnectionRevision,
        _workflow_id: &str,
    ) -> DomainResult<Value> {
        Err(SafeError::validation(
            "IMPORT_NOT_SUPPORTED",
            "该 Provider 不支持拉取工作流",
        ))
    }

    /// 平台/网关声明的可用模型清单（OpenAI 兼容的 `GET {baseUrl}/models` 那类）。
    ///
    /// ⚠️ 不支持时**必须报错，不能回空数组**：空数组的意思是"平台说它一个模型都没有"，
    /// 与"这个协议没有清单这条路"是两件事——界面会据此显示"没有可用模型"，那是假话。
    async fn fetch_models(&self, _connection: &ConnectionRevision) -> DomainResult<Vec<String>> {
        Err(SafeError::validation(
            "MODELS_NOT_SUPPORTED",
            "该 Provider 不支持模型清单",
        ))
    }

    /// 平台账号状态（余额、并发等）。返回平台数据对象；不支持的驱动报错。
    async fn fetch_account_status(&self, _connection: &ConnectionRevision) -> DomainResult<Value> {
        Err(SafeError::provider(
            "STATUS_NOT_SUPPORTED",
            "该 Provider 不支持账号状态查询",
        ))
    }

    /// 平台任务队列状态。返回平台数据对象；不支持的驱动报错。
    async fn fetch_queue_status(&self, _connection: &ConnectionRevision) -> DomainResult<Value> {
        Err(SafeError::provider(
            "STATUS_NOT_SUPPORTED",
            "该 Provider 不支持队列状态查询",
        ))
    }

    async fn validate_connection(
        &self,
        connection: &ConnectionRevision,
    ) -> DomainResult<ConnectionCheck>;

    async fn prepare(&self, request: PrepareRequest) -> DomainResult<PreparedExecution>;

    async fn submit(&self, execution: PreparedExecution) -> DomainResult<SubmitOutcome>;

    async fn reconcile(&self, request: ReconcileRequest) -> DomainResult<TaskSnapshot>;

    async fn cancel(&self, request: CancelRequest) -> DomainResult<CancelOutcome>;
}

#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: HashMap<String, Arc<dyn ProviderDriver>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, key: impl Into<String>, driver: Arc<dyn ProviderDriver>) {
        self.drivers.insert(key.into(), driver);
    }

    pub fn get(&self, key: &str) -> DomainResult<Arc<dyn ProviderDriver>> {
        self.drivers.get(key).cloned().ok_or_else(|| {
            SafeError::validation(
                "DRIVER_NOT_REGISTERED",
                format!("Provider 驱动未注册：{key}"),
            )
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.drivers.keys()
    }
}
