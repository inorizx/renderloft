use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::{
    CancelOutcome, ConnectionCheck, ProviderDescriptor, SubmitOutcome, TaskSnapshot,
};
use crate::v2::domain::media::MediaMapping;
use crate::v2::domain::model::{is_api_key_slot, ConnectionRevision, LifecycleFeatures};
use crate::v2::ports::provider::{
    CancelRequest, PrepareRequest, PreparedExecution, ProviderDriver, ReconcileRequest,
    WebappSchema,
};
use crate::v2::ports::stores::SecretStore;

use super::safety::{validate_base_url, validate_resolved_url, NetworkPolicy};

pub fn descriptor() -> &'static ProviderDescriptor {
    static DESCRIPTOR: OnceLock<ProviderDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| ProviderDescriptor {
        vendor_id: "runninghub".to_string(),
        protocol_id: "runninghub".to_string(),
        display_name: "RunningHub".to_string(),
        business_capabilities: vec!["workflow-execution".to_string()],
        lifecycle_features: LifecycleFeatures {
            remote_reconciliation: true,
            remote_cancel: true,
            ..Default::default()
        },
        security_requirements: serde_json::from_value(json!({ "httpsOnly": true }))
            .unwrap_or_default(),
        connection_schema: json!({
            "type": "object",
            // `region` **不是必填**：它只进幂等指纹，端点由 baseUrl 决定，缺了就按 baseUrl 推
            // （见 `Self::region`）。把它写成必填等于把"纯标记"伪装成"运行时开关"。
            "required": ["baseUrl"],
            "properties": {
                "baseUrl": { "type": "string", "format": "uri" },
                "region": {
                    "type": "string",
                    "description": "出网站点标记（cn / global / custom）。只影响提交时的幂等指纹，端点由 baseUrl 决定；缺省时按 baseUrl 推导。"
                }
            }
        }),
        binding_schema: json!({
            "type": "object",
            "properties": {
                "webappId": { "type": "string" },
                "workflowId": { "type": "string" },
                "workflow": { "type": "object" },
                "fieldMapping": {
                    "type": "object",
                    "description": "Input key to workflow node field mapping: { inputKey: { node, field } }"
                },
                "mediaMapping": {
                    "type": "object",
                    "description": "Media field to workflow node field mapping, keyed by field key: { \"<nodeId>::<fieldName>\": { node, field } }. Legacy revisions may still carry positional slot names (asset-N -> {node, field}); those are read as a compatibility encoding and converted to field keys."
                },
                "instanceType": { "type": "string" }
            }
        }),
    })
}

pub struct RunningHubDriver {
    client: Client,
    secrets: Box<dyn SecretStore>,
}

impl RunningHubDriver {
    pub fn new(secrets: Box<dyn SecretStore>) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { client, secrets }
    }

    fn base_url(connection: &ConnectionRevision) -> Result<reqwest::Url, SafeError> {
        let raw = connection
            .config
            .get("baseUrl")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| SafeError::validation("CONNECTION_CONFIG_INVALID", "缺少 baseUrl"))?;
        validate_base_url(raw, NetworkPolicy::LOCAL_HTTP)
    }

    fn region(connection: &ConnectionRevision) -> DomainResult<String> {
        if let Some(region) = connection
            .config
            .get("region")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(region.to_string());
        }
        // ⚠️ 缺 `region` 不是错误——**按 baseUrl 推一个稳定的值**。
        //
        // 这个字段的作用只有一处：拼进提交时的幂等指纹（`{fingerprint}-{region}`）。
        // 端点是由 `baseUrl` 决定的，所以它**不是运行时开关**，不该逼调用方（CLI/MCP/手写的
        // 连接配置）去想它；此前它是 `required` 且缺失就报"缺少 region"，等于把一个
        // 纯粹用于去重的标记当成了必填项——"必填却几乎不影响行为"的字段是谎话的温床。
        let base = connection
            .config
            .get("baseUrl")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(if base.is_empty() {
            "custom".to_string()
        } else if base.contains("runninghub.ai") {
            "global".to_string()
        } else if base.contains("runninghub.cn") {
            "cn".to_string()
        } else {
            "custom".to_string()
        })
    }

    async fn api_key(&self, connection: &ConnectionRevision) -> DomainResult<Option<String>> {
        let binding = connection
            .secret_bindings
            .iter()
            .find(|binding| is_api_key_slot(&binding.slot));
        let Some(binding) = binding else {
            return Ok(None);
        };
        let secret = self.secrets.resolve(binding).await?;
        Ok(secret.map(|value| value.secret))
    }

    /// 取这次任务的消耗——**只转述平台自己报的数**。
    ///
    /// 官方文档（`api-425761034`，`POST /task/openapi/outputs`）的响应里带
    /// `data[].consumeCoins`。这是唯一诚实的来源：本产品**不推算成本**——"平台没说"
    /// （`None`）与"这次免费"（`0`）是两件不同的事，不能用 0 顶替。
    ///
    /// **取不到绝不影响任务本身**：网络错、格式变、字段消失，都只是"不知道成本"。
    async fn fetch_usage(&self, connection: &ConnectionRevision, task_id: &str) -> Option<Value> {
        let base = Self::base_url(connection).ok()?;
        let key = self.api_key(connection).await.ok()?;
        let url = super::safety::join_path(&base, "task/openapi/outputs").ok()?;
        let response = self
            .client
            .post(url)
            .bearer_auth(key.clone().unwrap_or_default())
            .json(&json!({ "apiKey": key.unwrap_or_default(), "taskId": task_id }))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let parsed: Value = serde_json::from_slice(
            &super::safety::read_limited(response, 1024 * 1024)
                .await
                .ok()?,
        )
        .ok()?;
        usage_from_outputs(&parsed)
    }

    /// Upload a local asset to RunningHub and return the fileName used by nodeInfoList.
    async fn upload_asset(
        &self,
        connection: &ConnectionRevision,
        bytes: Vec<u8>,
        filename: &str,
    ) -> DomainResult<String> {
        let base = Self::base_url(connection)?;
        let key = self.api_key(connection).await?;
        let url = super::safety::join_path(&base, "openapi/v2/media/upload/binary")?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
        let response = self
            .client
            .post(url.clone())
            .bearer_auth(key.clone().unwrap_or_default())
            .multipart(reqwest::multipart::Form::new().part("file", part))
            // 上传素材是**能合法地慢**的那一类（几百 MB 走家用上行），跟提交同一个预算。
            .timeout(crate::v2::domain::connection_options::request_timeout(
                &connection.config,
                crate::v2::domain::connection_options::DEFAULT_REQUEST_TIMEOUT_SECS,
            ))
            .send()
            .await
            .map_err(|error| super::safety::map_network_error(&error, "runninghub_upload"))?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 上传失败",
            ));
        }
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 1024 * 1024).await?)
                .map_err(|_| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON")
                })?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            return Err(SafeError::provider(
                "RUNNINGHUB_UPLOAD_REJECTED",
                parsed
                    .get("message")
                    .or_else(|| parsed.get("msg"))
                    .and_then(Value::as_str)
                    .unwrap_or("RunningHub 上传被拒绝"),
            ));
        }
        parsed
            .pointer("/data/fileName")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "上传响应缺少 fileName")
            })
    }

    /// Fetch the platform workflow JSON for a workflowId (recipe import helper).
    async fn fetch_workflow(
        &self,
        connection: &ConnectionRevision,
        workflow_id: &str,
    ) -> DomainResult<Value> {
        let base = Self::base_url(connection)?;
        let key = self.api_key(connection).await?;
        let url = super::safety::join_path(&base, "api/openapi/getJsonApiFormat")?;
        let response = self
            .client
            .post(url.clone())
            .bearer_auth(key.clone().unwrap_or_default())
            .json(&json!({
                "apiKey": key.unwrap_or_default(),
                "workflowId": workflow_id,
            }))
            .send()
            .await
            .map_err(|error| {
                super::safety::map_network_error(&error, "runninghub_workflow_json")
            })?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 获取工作流失败",
            ));
        }
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 4 * 1024 * 1024).await?)
                .map_err(|_| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON")
                })?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            return Err(SafeError::provider(
                "RUNNINGHUB_WORKFLOW_FETCH_REJECTED",
                parsed
                    .get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("RunningHub 获取工作流被拒绝"),
            ));
        }
        let prompt = parsed
            .pointer("/data/prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应缺少 data.prompt")
            })?;
        serde_json::from_str(prompt)
            .map_err(|_| SafeError::validation("PROVIDER_RESPONSE_INVALID", "prompt 不是有效 JSON"))
    }

    /// 查询平台账户/队列状态，返回响应中的 data 对象。
    async fn get_status(
        &self,
        connection: &ConnectionRevision,
        kind: &str,
    ) -> DomainResult<Option<Value>> {
        let base = Self::base_url(connection)?;
        let key = self.api_key(connection).await?;
        let key = key.ok_or_else(|| {
            SafeError::provider(
                "RUNNINGHUB_KEY_NOT_CONFIGURED",
                "RunningHub Connection 未绑定 API Key",
            )
        })?;
        let (path, body) = match kind {
            // ⚠️ 字段名是 **`apiKey`**（大写 K）。实测：发 `apikey` 会被平台忽略，回
            // `{"code":1601,"msg":"param apiKey is required"}`——于是**连接测试带着一个
            // 完全正确的 Key 也会失败**，而报错不会告诉你是名字大小写的问题。
            "account" => ("/uc/openapi/accountStatus", json!({ "apiKey": key })),
            "queue" => ("/openapi/v2/queue/status", Value::Null),
            _ => {
                return Err(SafeError::validation("INVALID_STATUS", "未知状态类型"));
            }
        };
        let url = super::safety::join_path(&base, path)?;
        let method = if body.is_null() {
            reqwest::Method::GET
        } else {
            reqwest::Method::POST
        };
        let mut builder = self
            .client
            .request(method, url.clone())
            .bearer_auth(key.clone());
        if !body.is_null() {
            builder = builder.json(&body);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| super::safety::map_network_error(&error, "runninghub_status"))?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 状态查询失败",
            ));
        }
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 1024 * 1024).await?)
                .map_err(|_| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON")
                })?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            return Err(rejection_error(
                parsed
                    .get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("RunningHub 拒绝状态查询"),
            ));
        }
        Ok(parsed.get("data").cloned())
    }

    /// 把远端失败响应构造成结构化错误，附带远端状态便于排查。
    fn failure(parsed: &Value, remote_status: &str) -> SafeError {
        let message = parsed
            .get("errorMessage")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "RunningHub 任务失败".to_string());
        let mut details = BTreeMap::new();
        details.insert("remoteStatus".to_string(), json!(remote_status));
        if let Some(task_id) = parsed.get("taskId").and_then(Value::as_str) {
            details.insert("remoteTaskId".to_string(), json!(task_id));
        }
        SafeError::provider("PROVIDER_TASK_FAILED", message).with_details(details)
    }
}

#[async_trait]
impl ProviderDriver for RunningHubDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        descriptor()
    }

    async fn fetch_workflow_prompt(
        &self,
        connection: &ConnectionRevision,
        workflow_id: &str,
    ) -> DomainResult<Value> {
        self.fetch_workflow(connection, workflow_id).await
    }

    async fn fetch_webapp_schema(
        &self,
        connection: &ConnectionRevision,
        webapp_id: &str,
    ) -> DomainResult<WebappSchema> {
        let base = Self::base_url(connection)?;
        let key = self.api_key(connection).await?;
        // 官方契约：GET /api/webapp/apiCallDemo?apiKey=..&webappId=.. ，
        // header 带 Authorization: Bearer。返回 data.nodeInfoList。
        let url = super::safety::join_path(&base, "api/webapp/apiCallDemo")?;
        let response = self
            .client
            .get(url.clone())
            .query(&[
                ("apiKey", key.as_deref().unwrap_or_default()),
                ("webappId", webapp_id),
            ])
            .bearer_auth(key.unwrap_or_default())
            .send()
            .await
            .map_err(|error| super::safety::map_network_error(&error, "runninghub_webapp_demo"))?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 获取应用信息失败",
            ));
        }
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 4 * 1024 * 1024).await?)
                .map_err(|_| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON")
                })?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            return Err(SafeError::provider(
                "RUNNINGHUB_WEBAPP_FETCH_REJECTED",
                parsed
                    .get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("RunningHub 获取应用信息被拒绝"),
            ));
        }
        let data = parsed
            .pointer("/data")
            .ok_or_else(|| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应缺少 data"))?;
        // 官方 nodeInfoList；兼容旧 webapp/detail 返回的 inputNodes。
        let webapp_name = data
            .get("webappName")
            .and_then(Value::as_str)
            .or_else(|| data.get("name").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        let nodes_data = data
            .get("nodeInfoList")
            .and_then(Value::as_array)
            .or_else(|| data.get("inputNodes").and_then(Value::as_array))
            .cloned()
            .unwrap_or_default();
        let nodes = nodes_data
            .into_iter()
            .map(|node| webapp_node_info(&node))
            .collect::<Vec<_>>();
        Ok(WebappSchema {
            webapp_id: webapp_id.to_string(),
            webapp_name,
            description: data
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description_en: data
                .get("descriptionEn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            nodes,
        })
    }

    async fn fetch_account_status(&self, connection: &ConnectionRevision) -> DomainResult<Value> {
        Self::get_status(self, connection, "account")
            .await?
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "RunningHub 账户接口缺少数据")
            })
    }

    async fn fetch_queue_status(&self, connection: &ConnectionRevision) -> DomainResult<Value> {
        Self::get_status(self, connection, "queue")
            .await?
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "RunningHub 队列接口缺少数据")
            })
    }

    async fn validate_connection(
        &self,
        connection: &ConnectionRevision,
    ) -> DomainResult<ConnectionCheck> {
        let base = Self::base_url(connection)?;
        let _ = Self::region(connection)?;
        let has_key = self
            .api_key(connection)
            .await?
            .is_some_and(|key| !key.trim().is_empty());
        if !has_key {
            return Ok(ConnectionCheck {
                ok: false,
                capabilities: connection.effective_capabilities.clone(),
                message: Some("缺少 API Key 绑定".to_string()),
            });
        }
        validate_resolved_url(&base, NetworkPolicy::LOCAL_HTTP).await?;
        // 连接测试必须验证一次真实的认证请求，而不只是检查 URL 和 keyring 元数据。
        self.fetch_account_status(connection).await?;
        Ok(ConnectionCheck {
            ok: true,
            capabilities: connection.effective_capabilities.clone(),
            message: None,
        })
    }

    async fn prepare(&self, request: PrepareRequest) -> DomainResult<PreparedExecution> {
        let _ = Self::base_url(&request.connection_revision)?;
        let region = Self::region(&request.connection_revision)?;
        let config = &request.recipe_binding.provider_config;

        let webapp_id = config.get("webappId").and_then(Value::as_str);
        let webapp_id_number = webapp_id.and_then(|id| id.parse::<i64>().ok());
        let workflow_id = config.get("workflowId").and_then(Value::as_str);
        let custom_workflow = config.get("workflow").cloned();

        let mut payload = serde_json::Map::new();
        if let Some(workflow_id) = workflow_id {
            payload.insert("workflowId".to_string(), json!(workflow_id));
        }
        // 官方契约要求 webappId 为 int64；配置里可能是字符串，提交时统一解析为数值，
        // 否则任务可能因类型不符而无法正确排队/推进。
        if let Some(webapp_id_number) = webapp_id_number {
            payload.insert("webappId".to_string(), json!(webapp_id_number));
        } else if let Some(webapp_id) = webapp_id {
            payload.insert("webappId".to_string(), json!(webapp_id));
        }
        if let Some(instance_type) = config.get("instanceType").and_then(Value::as_str) {
            payload.insert("instanceType".to_string(), json!(instance_type));
        }
        let mut node_info_list = Vec::new();
        // media 资产：先上传，再把 fileName 填进映射指向的节点字段。
        //
        // ⚠️ 这里**只认字段键**（`MediaMapping::bind`）：旧位置数组也在这一个入口被换算成
        // 字段键，别处不许再出现 `asset-{index}` 这种"位置即语义"的写法——实测的错位事故
        // 就是它：上游节点顺序、schema 键序、调用方顺序三者谁都不是资产的身份。
        if !request.assets.is_empty() {
            let mapping = MediaMapping::from_provider_config(config)?;
            let refs = request
                .assets
                .iter()
                .map(|resolved| resolved.asset_ref.clone())
                .collect::<Vec<_>>();
            let bindings = mapping.bind(&refs)?;
            for (index, binding) in bindings.iter().enumerate() {
                let file_name = self
                    .upload_asset(
                        &request.connection_revision,
                        request.assets[index].bytes.clone(),
                        "input.png",
                    )
                    .await?;
                node_info_list.push(json!({
                    "nodeId": binding.slot.node,
                    "fieldName": binding.slot.field,
                    "fieldValue": file_name,
                }));
            }
        }
        let mut workflow_override = custom_workflow;
        if let Some(workflow) = workflow_override.as_mut() {
            apply_inputs_to_workflow(workflow, &request.inputs, config);
            payload.insert("workflow".to_string(), workflow.clone());
        } else if let Some(mapping) = config.get("fieldMapping").and_then(Value::as_object) {
            for (input_key, value) in &request.inputs {
                if let Some(entry) = mapping.get(input_key) {
                    if let (Some(node_id), Some(field)) = (
                        entry.get("node").and_then(Value::as_str),
                        entry.get("field").and_then(Value::as_str),
                    ) {
                        node_info_list.push(json!({
                            "nodeId": node_id,
                            "fieldName": field,
                            "fieldValue": value,
                        }));
                    }
                }
            }
        }
        if !node_info_list.is_empty() {
            payload.insert("nodeInfoList".to_string(), Value::Array(node_info_list));
        }
        if webapp_id.is_none() && workflow_id.is_none() && workflow_override.is_none() {
            return Err(SafeError::validation(
                "RUNNINGHUB_BINDING_REQUIRED",
                "Recipe binding needs workflowId, workflow, or webappId",
            ));
        }
        let request_id = format!("{}-{region}", request.request_fingerprint);
        Ok(PreparedExecution {
            connection_revision: request.connection_revision,
            provider_request: Value::Object(payload),
            request_id,
        })
    }

    async fn submit(&self, execution: PreparedExecution) -> DomainResult<SubmitOutcome> {
        let base = Self::base_url(&execution.connection_revision)?;
        let key = self.api_key(&execution.connection_revision).await?;
        let has_webapp = execution
            .provider_request
            .get("webappId")
            .map(|value| value.is_string() || value.as_i64().is_some())
            .unwrap_or(false);
        let path = if has_webapp {
            "task/openapi/ai-app/run"
        } else {
            "task/openapi/create"
        };
        let url = super::safety::join_path(&base, path)?;
        let mut body = execution
            .provider_request
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(key) = key.as_deref() {
            body.insert("apiKey".to_string(), json!(key));
        }
        let response = self
            .client
            .post(url.clone())
            .bearer_auth(key.unwrap_or_default())
            .json(&body)
            // 提交是**能合法地慢**的那一类（大 payload、平台忙）：按连接的配置给预算，
            // 没配就是今天这个 120 秒。查询/取消/状态这类请求刻意**不**跟这个值——
            // 它们本来就该快，给同样的长超时只会让"平台挂了"更难发现。
            .timeout(crate::v2::domain::connection_options::request_timeout(
                &execution.connection_revision.config,
                crate::v2::domain::connection_options::DEFAULT_REQUEST_TIMEOUT_SECS,
            ))
            .send()
            .await
            .map_err(|error| super::safety::map_network_error(&error, "runninghub_submit"))?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 提交失败",
            ));
        }
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 1024 * 1024).await?)
                .map_err(|_| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON")
                })?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            let message = parsed
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("RunningHub 提交失败");
            return Err(rejection_error(message));
        }
        let data = parsed
            .get("data")
            .ok_or_else(|| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应缺少 data"))?;
        // 官方契约：data.taskId 为 integer<int64>；个别响应可能给字符串，两个类型都兼容。
        let task_id = data
            .get("taskId")
            .and_then(|value| {
                value.as_i64().map(|id| id.to_string()).or_else(|| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .filter(|text| !text.is_empty())
                })
            })
            .ok_or_else(|| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应缺少 taskId"))?;
        let task_status = data
            .get("taskStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if task_status == "FAILED" {
            return Err(SafeError::provider(
                "RUNNINGHUB_TASK_FAILED",
                "RunningHub 任务提交即失败",
            ));
        }
        Ok(SubmitOutcome::Accepted(
            crate::v2::domain::execution::RemoteHandle {
                provider: "runninghub".to_string(),
                remote_id: task_id,
            },
        ))
    }

    async fn reconcile(&self, request: ReconcileRequest) -> DomainResult<TaskSnapshot> {
        let base = Self::base_url(&request.connection_revision)?;
        let key = self.api_key(&request.connection_revision).await?;
        let url = super::safety::join_path(&base, "openapi/v2/query")?;
        let response = self
            .client
            .post(url.clone())
            .bearer_auth(key.unwrap_or_default())
            .json(&json!({ "taskId": request.remote_handle.remote_id }))
            .send()
            .await
            .map_err(|error| super::safety::map_network_error(&error, "runninghub_query"))?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 查询失败",
            ));
        }
        // 官方查询接口直接返回任务对象（无 code/data 包裹）
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 4 * 1024 * 1024).await?)
                .map_err(|_| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON")
                })?;
        let status = parsed
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match status.to_uppercase().as_str() {
            "SUCCESS" | "COMPLETED" | "SUCCEEDED" => {
                let mut results = Vec::new();
                if let Some(items) = parsed.get("results").and_then(Value::as_array) {
                    for (index, item) in items.iter().enumerate() {
                        let Some(url) = item.get("url").and_then(Value::as_str) else {
                            continue;
                        };
                        // 中转站定位：只记录远端结果 URL，不下载字节（预览时按需拉取）。
                        let output_type = item
                            .get("outputType")
                            .and_then(Value::as_str)
                            .unwrap_or("png");
                        // ⚠️ 兜底名**必须带 index**：平台不给 nodeId 时，只用扩展名会让同一个 run
                        // 的多个产物**全部同名**（`output.png`），而它们在库里是不同 id 的两条记录
                        // ⇒ 收进一个目录时只能靠 `--on-conflict` 兜，且人分不清哪张是哪张。
                        // index 就是它们在 `results` 里的位置（也正是 ArtifactId 用的那个序号）。
                        let fallback_name = format!("output-{index}.{output_type}");
                        let filename = item
                            .get("nodeId")
                            .and_then(Value::as_str)
                            .map(|node_id| format!("{node_id}.{output_type}"))
                            .unwrap_or(fallback_name);
                        results.push(crate::v2::domain::execution::ArtifactDraft {
                            filename,
                            mime_type: match output_type {
                                "jpg" | "jpeg" => "image/jpeg".to_string(),
                                "webp" => "image/webp".to_string(),
                                "mp4" => "video/mp4".to_string(),
                                "mp3" => "audio/mpeg".to_string(),
                                _ => "image/png".to_string(),
                            },
                            byte_size: 0,
                            checksum: String::new(),
                            data: crate::v2::domain::execution::ArtifactData::Url {
                                url: url.to_string(),
                                expires_at: None,
                            },
                        });
                    }
                }
                Ok(TaskSnapshot {
                    status: crate::v2::domain::execution::TaskStatus::Succeeded,
                    results,
                    error: None,
                    // 成本**只转述平台自己报的数**（`consumeCoins`），取不到就是"不知道"。
                    // 刻意不用"余额差"反推：那是我们算的，而且会被用户在网页端自己跑的任务污染。
                    usage: self
                        .fetch_usage(
                            &request.connection_revision,
                            &request.remote_handle.remote_id,
                        )
                        .await,
                })
            }
            "FAILED" | "ERROR" | "CANCELLED" => Ok(TaskSnapshot {
                status: crate::v2::domain::execution::TaskStatus::Failed,
                results: vec![],
                error: Some(Self::failure(&parsed, status)),
                usage: None,
            }),
            _ => Ok(TaskSnapshot {
                status: crate::v2::domain::execution::TaskStatus::Running,
                results: vec![],
                error: None,
                usage: None,
            }),
        }
    }

    async fn cancel(&self, request: CancelRequest) -> DomainResult<CancelOutcome> {
        let base = Self::base_url(&request.connection_revision)?;
        let key = self.api_key(&request.connection_revision).await?;
        let url = super::safety::join_path(&base, "task/openapi/cancel")?;
        let response = self
            .client
            .post(url.clone())
            .bearer_auth(key.clone().unwrap_or_default())
            .json(&json!({
                "apiKey": key.unwrap_or_default(),
                "taskId": request.remote_handle.remote_id,
            }))
            .send()
            .await
            .map_err(|error| super::safety::map_network_error(&error, "runninghub_cancel"))?;
        if !response.status().is_success() {
            return Err(super::safety::map_http_error(
                response.status().as_u16(),
                "RunningHub 取消失败",
            ));
        }
        let parsed: Value =
            serde_json::from_slice(&super::safety::read_limited(response, 1024 * 1024).await?)
                .unwrap_or(Value::Null);
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(0);
        // 807 = APIKEY_TASK_NOT_FOUND：任务已不存在（等同终态）
        Ok(if code == 0 || code == 807 {
            CancelOutcome::Cancelled
        } else {
            CancelOutcome::RemoteMayContinue
        })
    }
}

fn apply_inputs_to_workflow(
    workflow: &mut Value,
    inputs: &std::collections::BTreeMap<String, Value>,
    config: &std::collections::BTreeMap<String, Value>,
) {
    let Some(object) = workflow.as_object_mut() else {
        return;
    };
    let mapping = config.get("fieldMapping").and_then(Value::as_object);
    for (input_key, value) in inputs {
        if let Some(entry) = mapping.and_then(|map| map.get(input_key)) {
            if let (Some(node_id), Some(field)) = (
                entry.get("node").and_then(Value::as_str),
                entry.get("field").and_then(Value::as_str),
            ) {
                if let Some(inputs) = object
                    .get_mut(node_id)
                    .and_then(|node| node.get_mut("inputs"))
                    .and_then(Value::as_object_mut)
                {
                    inputs.insert(field.to_string(), value.clone());
                    continue;
                }
            }
        }
        for node in object.values_mut() {
            let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) else {
                continue;
            };
            if inputs.contains_key(input_key) {
                inputs.insert(input_key.clone(), value.clone());
            }
        }
    }
}

/// 把平台的拒绝消息归类成结构化错误。
///
/// **"队列满"不是失败，是"现在不行"。** RunningHub 在并发位满时**拒绝**新提交（不是排队），
/// 实测原文就是 `TASK_QUEUE_MAXED`（我们把它原样放在 `message` 里，便于排查）。
///
/// 实测（2026-10-08，本机，真实账号）：我们这边同时提交 2 个，平台当场拒掉第 2 个——而那次
/// 拒绝**一分钱没花**（余额只减了成功那一个任务的 10 coins）。这个事实是"不预测容量、只处理
/// 拒绝"这条路能成立的前提：拒了不亏，重试就行。
///
/// 所以它必须是 `retryable: true`。否则调用方会把它当成"任务失败"——而在我们自己的约定里
/// `retryable` 就是"该改参数还是该重试"的那个开关，报 `false` 等于**明确告诉 agent 别重试**。
/// 实测当时报的就是 `false`，听话的 agent 会照做，活就丢了。
///
/// 走已有的退避重试（`RETRY_WAIT` + 30 秒）即可，**不需要任何人知道并发上限**：
/// 让服务端说"不"，我们把它当"等一会儿"。
fn rejection_error(message: &str) -> SafeError {
    let error = SafeError::provider("RUNNINGHUB_REJECTED", message);
    if !message.to_ascii_uppercase().contains("TASK_QUEUE_MAXED") {
        // 其余拒绝（参数错、鉴权错）要人去改，不能无限重试。
        return error;
    }
    SafeError {
        message: format!("{message}（平台执行位已满，稍后自动重试）"),
        retryable: true,
        ..error
    }
}

/// 从 `/task/openapi/outputs` 的响应里取出消耗。
///
/// 形状（官方文档）：`{code, msg, data: [{fileUrl, fileType, taskCostTime, nodeId, consumeCoins}]}`，
/// 其中 `consumeCoins` 是**字符串**（与账号余额 `remainCoins` 一个口径），所以我们原样保留字符串，
/// 不做浮点格式化——钱的数字不该因为我们的表示法而变样。
///
/// ⚠️ **每一行报的都是「这个任务的总账」，所以只取一次，绝不求和。**（2026-10-11 真跑定论）
///
/// 此前这里逐行相加，假设"费用挂在每个输出文件上"；注释里当时就写了这是**必须用真跑核对**的
/// 假设。两次真跑把它否掉了：
///
/// | 跑法 | 平台自报 | 余额实减 |
/// |---|---|---|
/// | `batch_size=1`（1 行） | `amount: 10` | **10** ✓ |
/// | `batch_size=3`（3 行） | `amount: 42`（= 3 × 14） | **14** |
///
/// 单输出时两者必然相等（求和只有一行），批量时求和正好放大 N 倍——**用户会看到一个
/// 3 倍的价钱**。`taskCostTime` 同理（batch=3 求和得 207 = 3 × 69，而那次墙上时间 74.7 秒
/// ≈ 69，即每行报的都是任务自己那次耗时）。
///
/// `taskCostTime`（**任务耗时**）一起留着：平台**按时长计费**，耗时与币在同一行里，
/// 只留币的话"同一 recipe 为什么这张 10 币、那张 14 币"永远答不了（余额只有总额，拆不开），
/// 而且**事后补不回来**（平台的任务记录不会永远留着）。它与币同源，留它不花任何代价。
fn usage_from_outputs(parsed: &Value) -> Option<Value> {
    let rows = parsed.get("data")?.as_array()?;
    let mut amount: Option<f64> = None;
    let mut cost_seconds: Option<f64> = None;
    for row in rows {
        // 只认第一行报的数（每行都是同一份任务总额）。
        if cost_seconds.is_none() {
            cost_seconds = row
                .get("taskCostTime")
                .and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value.as_f64().map(|number| number.to_string()))
                })
                .and_then(|raw| raw.trim().parse::<f64>().ok());
        }
        if amount.is_none() {
            amount = row
                .get("consumeCoins")
                .and_then(Value::as_str)
                .and_then(|raw| raw.trim().parse::<f64>().ok());
        }
    }
    let amount = amount?;
    let amount = if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        format!("{amount}")
    };
    let mut usage = json!({ "unit": "coins", "amount": amount });
    // 只有平台真报了耗时才写这个字段：没有就是没有，不编一个 0（0 秒是"没花时间"）。
    if let Some(cost_seconds) = cost_seconds {
        let seconds = if cost_seconds.fract() == 0.0 {
            format!("{}", cost_seconds as i64)
        } else {
            format!("{cost_seconds}")
        };
        usage["taskCostTime"] = json!(seconds);
    }
    Some(usage)
}

/// 把 `apiCallDemo` 返回的一个节点翻译成我们的字段描述。
///
/// ⚠️ 字段名有三个槽位，两个真应用实测（2026-10-12，拿真 key 打真端点）：
///
/// | 应用 | `description` | `descriptionCn` | `descriptionEn` |
/// |---|---|---|---|
/// | Krea2-turbo（作者没填） | `"text"`（= 字段名） | null | null |
/// | Qwen Image 2.1（作者填了） | `"提示词"` | null | null |
///
/// ⇒ **作者的名字就走 `description`**，它没填时平台把字段名塞进同一个槽位（所以它永远非空）。
/// 另外两个槽位在这两个应用里都是 null，**没见过谁填**——所以顺序是"先用经过验证的那个，
/// 再用没见过的当兜底"，而不是反过来：拿一个来源不明的槽位压过已经验证过的，
/// 万一它装的是机翻或别的什么，就等于把作者的名字又弄丢了。
///
/// 刻意**不**把"等于字段名"的描述丢掉：那是平台的原话，界面据此判断"平台没给名字"
///（见 `V2Recipes.vue` 里导入后要不要请你命名那段），在这里抹掉反而让那层判断失去依据。
fn webapp_node_info(node: &Value) -> crate::v2::ports::provider::WebappNodeInfo {
    let text = |key: &str| -> String {
        node.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let generic = text("description");
    let cn = text("descriptionCn");
    let en = text("descriptionEn");
    crate::v2::ports::provider::WebappNodeInfo {
        node_id: text("nodeId"),
        field_name: text("fieldName"),
        field_type: text("fieldType"),
        field_value: node.get("fieldValue").cloned(),
        description: if !generic.is_empty() {
            generic.clone()
        } else if !cn.is_empty() {
            cn
        } else {
            en.clone()
        },
        description_en: if !en.is_empty() { en } else { generic },
        field_data: node.get("fieldData").cloned().unwrap_or(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::execution::SubmitOutcome;
    use crate::v2::domain::ids::{ConnectionId, ConnectionRevisionId};
    use crate::v2::domain::model::{RecipeBinding, SecretBinding, Timestamp};
    use crate::v2::ports::stores::SecretValue;
    use std::collections::BTreeMap;

    /// 关键是**"平台没说"必须返回 `None`，不能变成 0** —— 0 是"这次免费"，
    /// 两者混了就是在撒谎（本地自建 ComfyUI 才是真的 0）。
    #[test]
    fn usage_comes_from_the_platform_and_stays_unknown_when_it_says_nothing() {
        // 平台真报的形状：耗时（taskCostTime）与币（consumeCoins）在**同一行**里。
        // 平台按时长计费 ⇒ 只留币的话，"同一 recipe 为什么这张 11 币、那张 4 币"答不了。
        let reported = json!({
            "code": 0,
            "msg": "success",
            "data": [{
                "fileUrl": "https://x/out.png",
                "fileType": "png",
                "taskCostTime": "32",
                "nodeId": "12",
                "consumeCoins": "17"
            }]
        });
        assert_eq!(
            usage_from_outputs(&reported),
            Some(json!({ "unit": "coins", "amount": "17", "taskCostTime": "32" }))
        );

        // 耗时是数字（不是字符串）也要认——平台的类型口径不保证一致。
        let numeric_time = json!({
            "data": [{ "consumeCoins": "5", "taskCostTime": 12.5 }]
        });
        assert_eq!(
            usage_from_outputs(&numeric_time),
            Some(json!({ "unit": "coins", "amount": "5", "taskCostTime": "12.5" }))
        );

        // 平台报了币但**没报耗时** ⇒ 不写这个字段，绝不编一个 0（0 秒是"没花时间"）。
        let no_time = json!({ "data": [{ "consumeCoins": "6" }] });
        assert_eq!(
            usage_from_outputs(&no_time),
            Some(json!({ "unit": "coins", "amount": "6" }))
        );

        // ⚠️ 多行（多输出）**不相加**：每一行报的都是这个任务的总账。
        // 真跑定论（2026-10-11）：`batch_size=3` 那次平台报 42 = 3 × 14，而余额只减了 14；
        // `batch_size=1` 那次报 10、余额减 10（单行时求和与取值必然相等，所以只有批量能暴露）。
        // 照旧相加 ⇒ **用户看到一个 3 倍的价钱**。耗时同理。
        let three = json!({
            "data": [
                { "consumeCoins": "14", "taskCostTime": "69" },
                { "consumeCoins": "14", "taskCostTime": "69" },
                { "consumeCoins": "14", "taskCostTime": "69" }
            ]
        });
        assert_eq!(
            usage_from_outputs(&three),
            Some(json!({ "unit": "coins", "amount": "14", "taskCostTime": "69" })),
            "每行都是任务总额，必须只取一次——相加会把批量任务报成 N 倍价钱"
        );

        // 第一行缺币、第二行才有 ⇒ 用第二行的（只认第一份出现的数，不是"只看第一行"）。
        let partial = json!({
            "data": [
                { "taskCostTime": "10" },
                { "consumeCoins": "7", "taskCostTime": "10" }
            ]
        });
        assert_eq!(
            usage_from_outputs(&partial),
            Some(json!({ "unit": "coins", "amount": "7", "taskCostTime": "10" }))
        );

        // 平台没报：排队/运行中的响应里没有这个字段，形状也不一样。
        let running = json!({
            "code": 804,
            "msg": "APIKEY_TASK_IS_RUNNING",
            "data": { "netWssUrl": "wss://example/ws" }
        });
        assert_eq!(usage_from_outputs(&running), None);
        let no_field = json!({ "data": [{ "fileUrl": "https://x/out.png" }] });
        assert_eq!(usage_from_outputs(&no_field), None);
        // 值不是数字 ⇒ 同样按"没说"处理，绝不瞎猜一个 0。
        let broken = json!({ "data": [{ "consumeCoins": "free" }] });
        assert_eq!(usage_from_outputs(&broken), None);
        // 耗时坏了不影响币：币是真金白银的记录，不能因为另一个字段脏了就一起丢。
        let broken_time = json!({ "data": [{ "consumeCoins": "3", "taskCostTime": "unknown" }] });
        assert_eq!(
            usage_from_outputs(&broken_time),
            Some(json!({ "unit": "coins", "amount": "3" }))
        );
    }
    /// 两个方向都要钉住：漏报会把活丢掉，误报会让坏参数无限重试（还要花钱）。
    #[test]
    fn queue_full_is_retryable_but_parameter_errors_are_not() {
        let full = rejection_error("TASK_QUEUE_MAXED");
        assert_eq!(full.code, "RUNNINGHUB_REJECTED");
        assert!(
            full.retryable,
            "平台执行位满 ⇒ 必须告诉调用方「可以重试」，否则 agent 会当成参数错而放弃"
        );
        assert!(
            full.message.contains("TASK_QUEUE_MAXED"),
            "平台原文要保留，排查时它是唯一证据"
        );

        let auth = rejection_error("APIKEY_INVALID");
        assert!(!auth.retryable, "鉴权/参数类拒绝不该被无限重试");
    }

    struct NoSecrets;

    struct KeySecrets;

    #[async_trait]
    impl SecretStore for KeySecrets {
        async fn resolve(&self, _binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
            Ok(Some(SecretValue {
                secret: "sk-test".to_string(),
            }))
        }
        async fn set(&self, _slot: &str, _value: &str) -> DomainResult<()> {
            Ok(())
        }
        async fn delete(&self, _slot: &str) -> DomainResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl SecretStore for NoSecrets {
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

    fn connection(base_url: String) -> ConnectionRevision {
        ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(1),
            config: BTreeMap::from([
                ("baseUrl".to_string(), json!(base_url)),
                ("region".to_string(), json!("cn-south-1")),
            ]),
            secret_bindings: vec![SecretBinding {
                slot: "connection-1-api_key".to_string(),
                resolution: crate::v2::domain::model::SecretResolution::Current,
            }],
            effective_capabilities: vec!["workflow-execution".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        }
    }

    /// 字段描述的三个槽位：**作者的名字走 `description`**（两个真应用都这样，一个填了一个没填），
    /// 另外两个槽位没见过谁填，只当 `description` 缺席时的兜底。
    #[test]
    fn field_description_prefers_the_slot_that_carries_author_names() {
        // ① 作者填了（Qwen Image 2.1 实测形状）：直接用，别让别的槽位插队。
        let authored = json!({
            "nodeId": "12", "fieldName": "value", "fieldType": "STRING",
            "description": "提示词", "descriptionCn": null, "descriptionEn": null
        });
        let info = webapp_node_info(&authored);
        assert_eq!(info.description, "提示词");
        assert_eq!(info.description_en, "提示词", "英文槽位空着时退回同一个值");

        // ② 作者没填（Krea2 实测形状）：保留平台的原话（= 字段名），
        //    界面据此判断"该请你命名了"——在这里抹掉会让那层判断失去依据。
        let bare = json!({
            "nodeId": "871", "fieldName": "batch_size", "fieldType": "INT",
            "description": "batch_size", "descriptionCn": null, "descriptionEn": null
        });
        let info = webapp_node_info(&bare);
        assert_eq!(info.description, "batch_size");
        assert_eq!(info.description_en, "batch_size");

        // ③ `description` 缺席时才轮到那两个没见过的槽位（见过谁填再调整顺序）。
        let fallback = json!({
            "nodeId": "6", "fieldName": "aspect_ratio", "fieldType": "LIST",
            "description": null, "descriptionCn": "宽/高比", "descriptionEn": "Aspect ratio"
        });
        let info = webapp_node_info(&fallback);
        assert_eq!(info.description, "宽/高比");
        assert_eq!(info.description_en, "Aspect ratio");
    }

    /// `region` **不是必填**：缺了就按 `baseUrl` 推一个稳定的值，给了就照给。    ///
    /// 它只进提交时的幂等指纹，端点由 `baseUrl` 决定——所以"缺 region 就报错"是把
    /// 一个纯标记当成了运行时开关。这里钉住推导规则与被显式值覆盖的优先级。
    #[test]
    fn region_falls_back_to_the_base_url() {
        let with_config = |entries: Vec<(&str, Value)>| ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(1),
            config: entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            secret_bindings: vec![],
            effective_capabilities: vec!["workflow-execution".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        };
        let region_of = |entries: Vec<(&str, Value)>| {
            RunningHubDriver::region(&with_config(entries)).expect("region 不该失败")
        };

        assert_eq!(
            region_of(vec![("baseUrl", json!("https://www.runninghub.cn"))]),
            "cn"
        );
        assert_eq!(
            region_of(vec![("baseUrl", json!("https://www.runninghub.ai"))]),
            "global"
        );
        assert_eq!(
            region_of(vec![("baseUrl", json!("https://my-mirror.example.com"))]),
            "custom",
            "认不出来的地址归 custom，不要瞎猜成 cn"
        );
        assert_eq!(
            region_of(vec![
                ("baseUrl", json!("https://www.runninghub.cn")),
                ("region", json!("global")),
            ]),
            "global",
            "显式给了就照给——调用方说了算，哪怕与域名不一致"
        );
        assert_eq!(
            region_of(vec![
                ("baseUrl", json!("https://www.runninghub.ai")),
                ("region", json!("   ")),
            ]),
            "global",
            "空白值不算给了"
        );
    }

    #[tokio::test]
    async fn descriptor_declares_protocol_and_capabilities() {
        let descriptor = descriptor();
        assert_eq!(descriptor.vendor_id, "runninghub");
        assert_eq!(descriptor.protocol_id, "runninghub");
        assert!(descriptor
            .business_capabilities
            .contains(&"workflow-execution".to_string()));
        assert!(descriptor.lifecycle_features.remote_reconciliation);
    }

    #[tokio::test]
    async fn validate_connection_reports_missing_key() {
        let connection = connection("https://www.runninghub.ai".to_string());
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let check = driver
            .validate_connection(&connection)
            .await
            .expect("缺 key 应返回检查结果");
        assert!(!check.ok);
    }

    /// 连接测试最后一步是**发真实的认证请求**，而那个接口要的字段名是 **`apiKey`**（大写 K）。
    ///
    /// 实测（对着真平台探过）：发 `apikey` 会被忽略，平台回
    /// `{"code":1601,"msg":"param apiKey is required"}` ⇒ **带着一个完全正确的 Key
    /// 也会"连接失败"**，而报错只说"param apiKey is required"，不会指出是我们名字写错了。
    ///
    /// ⚠️ 这条测试钉的是**我们自己发出去的形状**——mock 不会替平台挑错，但至少有人把
    /// 字段名改回小写时它会红。
    #[tokio::test]
    async fn connection_check_sends_the_api_key_under_the_name_the_platform_requires() {
        let (base, state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(KeySecrets));
        let check = driver
            .validate_connection(&connection(base))
            .await
            .expect("连接测试应成功");
        assert!(check.ok, "带有效 key 的连接检查应当通过");
        let body = state.last_body.lock().unwrap().clone();
        assert_eq!(body["apiKey"], "sk-test", "字段名必须是 apiKey：{body}");
        assert!(
            body.get("apikey").is_none(),
            "小写 apikey 会被平台忽略（实测回 1601）：{body}"
        );
    }

    #[tokio::test]
    async fn prepare_maps_inputs_to_node_info_list_via_field_mapping() {
        let connection = connection("https://www.runninghub.ai".to_string());
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(PrepareRequest {
                connection_revision: connection,
                recipe_binding: RecipeBinding {
                    driver_key: "runninghub".to_string(),
                    provider_config: BTreeMap::from([
                        ("workflowId".to_string(), json!("1904136902449209346")),
                        (
                            "fieldMapping".to_string(),
                            json!({ "prompt": { "node": "6", "field": "text" } }),
                        ),
                    ]),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1)],
                    connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
                },
                inputs: BTreeMap::from([("prompt".to_string(), json!("1 girl in classroom"))]),
                assets: vec![],
                request_fingerprint: "fp-1".to_string(),
            })
            .await
            .expect("prepare 应成功");
        assert_eq!(
            prepared.provider_request["workflowId"],
            "1904136902449209346"
        );
        assert_eq!(prepared.provider_request["nodeInfoList"][0]["nodeId"], "6");
        assert_eq!(
            prepared.provider_request["nodeInfoList"][0]["fieldName"],
            "text"
        );
        assert_eq!(
            prepared.provider_request["nodeInfoList"][0]["fieldValue"],
            "1 girl in classroom"
        );
    }

    #[tokio::test]
    async fn prepare_fills_custom_workflow_and_requires_binding() {
        let connection = connection("https://www.runninghub.ai".to_string());
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(PrepareRequest {
                connection_revision: connection.clone(),
                recipe_binding: RecipeBinding {
                    driver_key: "runninghub".to_string(),
                    provider_config: BTreeMap::from([(
                        "workflow".to_string(),
                        json!({
                            "6": {
                                "class_type": "CLIPTextEncode",
                                "inputs": { "text": "placeholder" }
                            }
                        }),
                    )]),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1)],
                    connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
                },
                inputs: BTreeMap::from([("text".to_string(), json!("a red house"))]),
                assets: vec![],
                request_fingerprint: "fp-1".to_string(),
            })
            .await
            .expect("prepare 应成功");
        assert_eq!(
            prepared.provider_request["workflow"]["6"]["inputs"]["text"],
            "a red house"
        );
        // 无 binding → 拒绝
        let error = driver
            .prepare(PrepareRequest {
                connection_revision: connection,
                recipe_binding: RecipeBinding {
                    driver_key: "runninghub".to_string(),
                    provider_config: Default::default(),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1)],
                    connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
                },
                inputs: Default::default(),
                assets: vec![],
                request_fingerprint: "fp-2".to_string(),
            })
            .await
            .expect_err("缺少 binding 应报错");
        assert_eq!(error.code, "RUNNINGHUB_BINDING_REQUIRED");
    }

    struct MockState {
        authorization: std::sync::Mutex<Option<String>>,
        last_body: std::sync::Mutex<Value>,
        base_url: std::sync::Mutex<String>,
        cancel_code: std::sync::Mutex<i64>,
    }

    async fn mock_server() -> (String, std::sync::Arc<MockState>) {
        use axum::{extract::State as AxState, routing::post, Json as AxJson, Router};
        let state = std::sync::Arc::new(MockState {
            authorization: std::sync::Mutex::new(None),
            last_body: std::sync::Mutex::new(Value::Null),
            base_url: std::sync::Mutex::new(String::new()),
            cancel_code: std::sync::Mutex::new(0),
        });
        let app = Router::new()
            .route(
                "/task/openapi/create",
                post({
                    let state = state.clone();
                    move |AxState(_): AxState<std::sync::Arc<MockState>>,
                          headers: axum::http::HeaderMap,
                          AxJson(body): AxJson<Value>| {
                        let state = state.clone();
                        async move {
                            *state.authorization.lock().unwrap() = headers
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned);
                            *state.last_body.lock().unwrap() = body;
                            AxJson(json!({
                                "code": 0,
                                "msg": "success",
                                "data": {
                                    "taskId": "1910246754753896450",
                                    "taskStatus": "QUEUED",
                                    "clientId": "client-1"
                                }
                            }))
                        }
                    }
                }),
            )
            .route(
                "/openapi/v2/query",
                post({
                    move |AxState(state): AxState<std::sync::Arc<MockState>>,
                          AxJson(body): AxJson<Value>| {
                        let state = state.clone();
                        async move {
                            *state.last_body.lock().unwrap() = body;
                            let base = state.base_url.lock().unwrap().clone();
                            AxJson(json!({
                                "taskId": "1910246754753896450",
                                "status": "SUCCESS",
                                "errorCode": "",
                                "errorMessage": "",
                                "results": [{
                                    "url": format!("{base}/out.png"),
                                    "outputType": "jpg"
                                }],
                                "clientId": "",
                                "promptTips": ""
                            }))
                        }
                    }
                }),
            )
            .route(
                "/out.png",
                axum::routing::get({
                    move |AxState(_): AxState<std::sync::Arc<MockState>>| async move {
                        use axum::body::Body;
                        use axum::http::StatusCode;
                        use axum::response::Response;
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "image/png")
                            .body(Body::from(vec![1, 2, 3, 4]))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/task/openapi/cancel",
                post({
                    move |AxState(state): AxState<std::sync::Arc<MockState>>,
                          AxJson(_body): AxJson<Value>| async move {
                        let code = *state.cancel_code.lock().unwrap();
                        AxJson(json!({ "code": code, "msg": if code == 0 { "success" } else { "APIKEY_TASK_NOT_FOUND" } }))
                    }
                }),
            )
            .route(
                "/openapi/v2/media/upload/binary",
                post({
                    move |AxState(_): AxState<std::sync::Arc<MockState>>| async move {
                        AxJson(json!({
                            "code": 0,
                            "message": "success",
                            "data": {
                                "type": "image",
                                "downloadUrl": "https://rh-images.example.com/input/abc.png",
                                "fileName": "openapi/abc.png",
                                "size": "3490"
                            }
                        }))
                    }
                }),
            )
            .route(
                // 连接测试打的就是这一条（`validate_connection` → `fetch_account_status`）。
                "/uc/openapi/accountStatus",
                post({
                    let state = state.clone();
                    move |AxState(_): AxState<std::sync::Arc<MockState>>,
                          AxJson(body): AxJson<Value>| {
                        let state = state.clone();
                        async move {
                            *state.last_body.lock().unwrap() = body;
                            AxJson(json!({
                                "code": 0,
                                "msg": "success",
                                "data": { "userId": "u-1", "coins": "100" }
                            }))
                        }
                    }
                }),
            )
            .route(
                "/api/openapi/getJsonApiFormat",
                post({
                    move |AxState(_): AxState<std::sync::Arc<MockState>>,
                          AxJson(_body): AxJson<Value>| async move {
                        AxJson(json!({
                            "code": 0,
                            "msg": "SUCCESS",
                            "data": {
                                "prompt": "{\"6\":{\"class_type\":\"CLIPTextEncode\",\"inputs\":{\"text\":\"hello\"},\"_meta\":{\"title\":\"Prompt\"}}}"
                            }
                        }))
                    }
                }),
            );
        let app = app.with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let base = format!("http://127.0.0.1:{}", address.port());
        *state.base_url.lock().unwrap() = base.clone();
        (base, state)
    }

    fn workflow_binding() -> RecipeBinding {
        RecipeBinding {
            driver_key: "runninghub".to_string(),
            provider_config: BTreeMap::from([
                ("workflowId".to_string(), json!("1904136902449209346")),
                (
                    "fieldMapping".to_string(),
                    json!({ "prompt": { "node": "6", "field": "text" } }),
                ),
            ]),
            default_connection_id: ConnectionId(1),
            allowed_connection_ids: vec![ConnectionId(1)],
            connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
        }
    }

    #[tokio::test]
    async fn submit_posts_create_and_accepts_task_id() {
        let (base, state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(KeySecrets));
        let prepared = driver
            .prepare(PrepareRequest {
                connection_revision: connection(base),
                recipe_binding: workflow_binding(),
                inputs: BTreeMap::from([("prompt".to_string(), json!("a cat"))]),
                assets: vec![],
                request_fingerprint: "fp-1".to_string(),
            })
            .await
            .expect("prepare 应成功");
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        match outcome {
            SubmitOutcome::Accepted(handle) => {
                assert_eq!(handle.remote_id, "1910246754753896450");
                assert_eq!(handle.provider, "runninghub");
            }
            other => panic!("期望 Accepted，得到 {other:?}"),
        }
        assert!(state
            .authorization
            .lock()
            .unwrap()
            .as_deref()
            .unwrap_or_default()
            .starts_with("Bearer "));
        let body = state.last_body.lock().unwrap().clone();
        assert_eq!(body["workflowId"], "1904136902449209346");
        assert_eq!(body["nodeInfoList"][0]["fieldValue"], "a cat");
    }

    #[tokio::test]
    async fn reconcile_succeeded_downloads_results() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let snapshot = driver
            .reconcile(crate::v2::ports::provider::ReconcileRequest {
                connection_revision: connection(base),
                remote_handle: crate::v2::domain::execution::RemoteHandle {
                    provider: "runninghub".to_string(),
                    remote_id: "1910246754753896450".to_string(),
                },
            })
            .await
            .expect("对账应成功");
        assert_eq!(
            snapshot.status,
            crate::v2::domain::execution::TaskStatus::Succeeded
        );
        let _ = snapshot;
    }

    #[tokio::test]
    async fn cancel_returns_cancelled() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let outcome = driver
            .cancel(crate::v2::ports::provider::CancelRequest {
                connection_revision: connection(base),
                remote_handle: crate::v2::domain::execution::RemoteHandle {
                    provider: "runninghub".to_string(),
                    remote_id: "1910246754753896450".to_string(),
                },
            })
            .await
            .expect("取消应成功");
        assert_eq!(
            outcome,
            crate::v2::domain::execution::CancelOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn cancel_code_807_is_treated_as_cancelled() {
        let (base, state) = mock_server().await;
        *state.cancel_code.lock().unwrap() = 807;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let outcome = driver
            .cancel(crate::v2::ports::provider::CancelRequest {
                connection_revision: connection(base),
                remote_handle: crate::v2::domain::execution::RemoteHandle {
                    provider: "runninghub".to_string(),
                    remote_id: "missing-task".to_string(),
                },
            })
            .await
            .expect("取消应成功");
        assert_eq!(
            outcome,
            crate::v2::domain::execution::CancelOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn upload_asset_returns_file_name() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let file_name = driver
            .upload_asset(&connection(base), vec![1, 2, 3], "input.png")
            .await
            .expect("上传应成功");
        assert_eq!(file_name, "openapi/abc.png");
    }

    #[tokio::test]
    async fn fetch_workflow_parses_prompt() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let workflow = driver
            .fetch_workflow(&connection(base), "1904136902449209346")
            .await
            .expect("获取应成功");
        assert_eq!(workflow["6"]["inputs"]["text"], "hello");
    }

    /// 旧 revision（asset-N 映射）+ 旧位置数组：按 asset-N 数字序解释，兼容路径。
    #[tokio::test]
    async fn prepare_uploads_assets_via_legacy_positional_mapping() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(PrepareRequest {
                connection_revision: connection(base),
                recipe_binding: RecipeBinding {
                    driver_key: "runninghub".to_string(),
                    provider_config: BTreeMap::from([
                        ("workflowId".to_string(), json!("1904136902449209346")),
                        (
                            "mediaMapping".to_string(),
                            json!({ "asset-0": { "node": "10", "field": "image" } }),
                        ),
                    ]),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1)],
                    connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
                },
                inputs: BTreeMap::from([("prompt".to_string(), json!("a cat"))]),
                assets: vec![crate::v2::ports::provider::ResolvedAsset {
                    asset_ref: crate::v2::domain::media::AssetRef::new(
                        crate::v2::domain::ids::AssetId(1),
                        "abc",
                    ),
                    bytes: vec![1, 2, 3],
                }],
                request_fingerprint: "fp-1".to_string(),
            })
            .await
            .expect("prepare 应成功");
        let node_list = prepared.provider_request["nodeInfoList"]
            .as_array()
            .expect("应生成 nodeInfoList");
        assert!(node_list.iter().any(|entry| {
            entry["nodeId"] == "10"
                && entry["fieldName"] == "image"
                && entry["fieldValue"] == "openapi/abc.png"
        }));
    }

    /// 回归（用户场景）：4 槽换装配方只传 475::image + 520::image 时，
    /// nodeInfoList **恰好**只有 475 与 520 两条——数组位置不再决定语义，
    /// 而且 519（schema 字典序里的第二个）绝不会被顶上。
    #[tokio::test]
    async fn prepare_binds_keyed_assets_to_exactly_those_slots() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let qwen_mapping = json!({
            "asset-0": { "node": "475", "field": "image" },
            "asset-1": { "node": "519", "field": "image" },
            "asset-2": { "node": "520", "field": "image" },
            "asset-3": { "node": "521", "field": "image" }
        });
        let prepare = |assets: Vec<crate::v2::ports::provider::ResolvedAsset>| {
            let driver = &driver;
            let mapping = qwen_mapping.clone();
            let base = base.clone();
            async move {
                driver
                    .prepare(PrepareRequest {
                        connection_revision: connection(base),
                        recipe_binding: RecipeBinding {
                            driver_key: "runninghub".to_string(),
                            provider_config: BTreeMap::from([
                                ("workflowId".to_string(), json!("1904136902449209346")),
                                ("mediaMapping".to_string(), mapping),
                            ]),
                            default_connection_id: ConnectionId(1),
                            allowed_connection_ids: vec![ConnectionId(1)],
                            connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
                        },
                        inputs: BTreeMap::new(),
                        assets,
                        request_fingerprint: "fp-4".to_string(),
                    })
                    .await
                    .expect("prepare 应成功")
            }
        };
        let asset = |id: u64, key: &str| crate::v2::ports::provider::ResolvedAsset {
            asset_ref: crate::v2::domain::media::AssetRef::new(
                crate::v2::domain::ids::AssetId(id),
                "abc",
            )
            .with_field_key(key),
            bytes: vec![1, 2, 3],
        };
        let prepared = prepare(vec![asset(1, "475::image"), asset(2, "520::image")]).await;
        let node_list = prepared.provider_request["nodeInfoList"]
            .as_array()
            .expect("应生成 nodeInfoList");
        let pairs: Vec<(&str, &str)> = node_list
            .iter()
            .map(|entry| {
                (
                    entry["nodeId"].as_str().unwrap_or_default(),
                    entry["fieldName"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![("475", "image"), ("520", "image")],
            "只传两个槽位时 nodeInfoList 必须恰好两条：{node_list:?}"
        );

        // 顺序不决定语义：倒过来传，落的槽位集合不变。
        let reversed = prepare(vec![asset(2, "520::image"), asset(1, "475::image")]).await;
        let mut reversed_nodes = reversed.provider_request["nodeInfoList"]
            .as_array()
            .expect("应生成 nodeInfoList")
            .iter()
            .map(|entry| entry["nodeId"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        reversed_nodes.sort();
        assert_eq!(reversed_nodes, vec!["475".to_string(), "520".to_string()]);
    }

    /// 未知字段键必须报错（不再有"静默丢掉"这一种结局）。
    #[tokio::test]
    async fn prepare_rejects_unknown_field_keys() {
        let (base, _state) = mock_server().await;
        let driver = RunningHubDriver::new(Box::new(NoSecrets));
        let error = driver
            .prepare(PrepareRequest {
                connection_revision: connection(base),
                recipe_binding: RecipeBinding {
                    driver_key: "runninghub".to_string(),
                    provider_config: BTreeMap::from([
                        ("workflowId".to_string(), json!("1904136902449209346")),
                        (
                            "mediaMapping".to_string(),
                            json!({ "475::image": { "node": "475", "field": "image" } }),
                        ),
                    ]),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1)],
                    connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
                },
                inputs: BTreeMap::new(),
                assets: vec![crate::v2::ports::provider::ResolvedAsset {
                    asset_ref: crate::v2::domain::media::AssetRef::new(
                        crate::v2::domain::ids::AssetId(1),
                        "abc",
                    )
                    .with_field_key("519::image"),
                    bytes: vec![1, 2, 3],
                }],
                request_fingerprint: "fp-5".to_string(),
            })
            .await
            .expect_err("未知字段键必须报错");
        assert_eq!(error.code, "ASSET_FIELD_UNKNOWN");
    }
}
