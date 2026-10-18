use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::{
    ArtifactData, ArtifactDraft, CancelOutcome, ConnectionCheck, ProviderDescriptor, SubmitOutcome,
    TaskSnapshot, TaskStatus,
};
use crate::v2::domain::model::{is_api_key_slot, ConnectionRevision, LifecycleFeatures};
use crate::v2::ports::provider::{
    CancelRequest, PrepareRequest, PreparedExecution, ProviderDriver, ReconcileRequest,
};
use crate::v2::ports::stores::SecretStore;

use super::safety::{
    join_path, map_http_error, map_network_error, map_submit_http_error, read_limited,
    validate_base_url, validate_resolved_url, NetworkPolicy, MAX_RESPONSE_BYTES,
};

/// 通用异步任务协议（介质无关）。
///
/// 协议形状：提交返回 task_id，轮询状态，completed 后下载结果。
/// 所有平台差异（提交路径、task_id 位置、状态映射、结果路径、提交字段）
/// 由 connection config 的 `dialect` 驱动：支持内置命名方言
/// （apimart / generic-image / generic-video / generic-audio / generic-model），
/// 也支持显式覆盖。介质类型（text-to-image / text-to-video /
/// text-to-audio / text-to-3d / ...）由连接 capabilities 声明，driver 不写死。
pub fn descriptor() -> &'static ProviderDescriptor {
    static DESCRIPTOR: OnceLock<ProviderDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| ProviderDescriptor {
        vendor_id: "async".to_string(),
        protocol_id: "async-task".to_string(),
        display_name: "Async Task".to_string(),
        // capabilities 由连接声明，driver 不限定介质
        business_capabilities: vec![],
        lifecycle_features: LifecycleFeatures {
            remote_reconciliation: true,
            ..Default::default()
        },
        security_requirements: serde_json::from_value(json!({ "httpsOnly": true }))
            .unwrap_or_default(),
        connection_schema: json!({
            "type": "object",
            "required": ["baseUrl"],
            "properties": {
                "baseUrl": { "type": "string", "format": "uri" },
                "dialect": {
                    "type": "string",
                    "enum": ["apimart", "generic-image", "generic-video", "generic-audio", "generic-model", "custom"],
                    "description": "内置方言或 custom"
                },
                "submitPath": { "type": "string", "description": "custom 方言：提交路径" },
                "taskIdPath": { "type": "string", "description": "custom 方言：响应中 task_id 的 JSON 路径（如 /data/task_id）" },
                "statusPath": { "type": "string", "description": "custom 方言：状态 JSON 路径（如 /data/status）" },
                "resultUrlPaths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "custom 方言：结果 URL JSON 路径候选列表"
                },
                "defaultModel": { "type": "string" },
                // 这条连接声明能跑哪些模型（跑表单的 model 字段据此过滤成下拉）。
                "models": { "type": "array", "items": { "type": "string" } }
            }
        }),
        binding_schema: json!({
            "type": "object",
            "properties": {
                "model": { "type": "string" },
                "dialect": {
                    "type": "object",
                    "description": "平台私有提交参数（透传）"
                }
            }
        }),
    })
}

#[derive(Debug, Clone)]
struct Dialect {
    submit_path: String,
    task_id_paths: Vec<String>,
    status_path: String,
    result_url_paths: Vec<String>,
}

fn builtin_dialect(name: &str) -> Option<Dialect> {
    Some(match name {
        // OpenAI-compatible images submission with task polling (e.g. apimart)
        "apimart" => Dialect {
            submit_path: "v1/images/generations".to_string(),
            task_id_paths: vec!["/data/0/task_id".to_string(), "/data/task_id".to_string()],
            status_path: "/data/status".to_string(),
            result_url_paths: vec![
                "/data/result/images/0/url/0".to_string(),
                "/data/result/url".to_string(),
            ],
        },
        // Generic image async task
        "generic-image" => Dialect {
            submit_path: "v1/tasks".to_string(),
            task_id_paths: vec!["/data/task_id".to_string(), "/task_id".to_string()],
            status_path: "/data/status".to_string(),
            result_url_paths: vec![
                "/data/result/images/0/url".to_string(),
                "/data/result/url".to_string(),
                "/data/url".to_string(),
            ],
        },
        // Generic video async task
        "generic-video" => Dialect {
            submit_path: "v1/videos".to_string(),
            task_id_paths: vec!["/data/task_id".to_string(), "/task_id".to_string()],
            status_path: "/data/status".to_string(),
            result_url_paths: vec![
                "/data/result/video_url".to_string(),
                "/data/result/video/url".to_string(),
                "/data/result/url".to_string(),
                "/data/video_url".to_string(),
            ],
        },
        // Generic audio async task
        "generic-audio" => Dialect {
            submit_path: "v1/audio".to_string(),
            task_id_paths: vec!["/data/task_id".to_string(), "/task_id".to_string()],
            status_path: "/data/status".to_string(),
            result_url_paths: vec![
                "/data/result/audio_url".to_string(),
                "/data/result/audio/url".to_string(),
                "/data/result/url".to_string(),
            ],
        },
        // Generic 3D/model async task
        "generic-model" => Dialect {
            submit_path: "v1/models".to_string(),
            task_id_paths: vec!["/data/task_id".to_string(), "/task_id".to_string()],
            status_path: "/data/status".to_string(),
            result_url_paths: vec![
                "/data/result/model_url".to_string(),
                "/data/result/url".to_string(),
                "/data/result/glb_url".to_string(),
            ],
        },
        _ => return None,
    })
}

fn custom_dialect(config: &BTreeMap<String, Value>) -> Option<Dialect> {
    let submit_path = config
        .get("submitPath")
        .and_then(Value::as_str)?
        .to_string();
    let task_id_paths = config
        .get("taskIdPath")
        .and_then(Value::as_str)
        .map(|path| vec![path.to_string()])?;
    let status_path = config
        .get("statusPath")
        .and_then(Value::as_str)
        .map(str::to_owned)?;
    let result_url_paths = config
        .get("resultUrlPaths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })?;
    Some(Dialect {
        submit_path,
        task_id_paths,
        status_path,
        result_url_paths,
    })
}

fn dialect_for(connection: &ConnectionRevision) -> Option<Dialect> {
    let name = connection
        .config
        .get("dialect")
        .and_then(Value::as_str)
        .unwrap_or("generic-video");
    if name == "custom" {
        custom_dialect(&connection.config)
    } else {
        builtin_dialect(name)
    }
}

pub struct AsyncTaskDriver {
    client: Client,
    secrets: Box<dyn SecretStore>,
}

impl AsyncTaskDriver {
    pub fn new(secrets: Box<dyn SecretStore>) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(180))
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

    fn request_body(&self, request: &PrepareRequest) -> DomainResult<Value> {
        // 提交字段不写死：inputs 全部透传（prompt/model/duration/resolution/...）
        let mut body = serde_json::Map::new();
        for (key, value) in &request.inputs {
            body.insert(key.clone(), value.clone());
        }
        for (key, value) in &request.recipe_binding.provider_config {
            if key == "dialect" {
                continue;
            }
            if !body.contains_key(key) {
                body.insert(key.clone(), value.clone());
            }
        }
        // binding dialect 私有参数透传
        if let Some(dialect) = request
            .recipe_binding
            .provider_config
            .get("dialect")
            .and_then(Value::as_object)
        {
            for (key, value) in dialect {
                if !body.contains_key(key) {
                    body.insert(key.clone(), value.clone());
                }
            }
        }
        // 连接的 `defaultModel` 兜底（与 openai_images 同一个口径）：输入与配方都没点名模型时
        // 才用它。此前这个字段在**两个驱动里都是死的**——界面写了、没人读（2026-10-13 接上）。
        if !body.contains_key("model") {
            if let Some(model) = request
                .connection_revision
                .config
                .get("defaultModel")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                body.insert("model".to_string(), Value::String(model.to_string()));
            }
        }
        let has_prompt = body
            .get("prompt")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        if !has_prompt {
            return Err(SafeError::validation("PROMPT_REQUIRED", "提示词不能为空"));
        }
        Ok(Value::Object(body))
    }

    /// 在 value 的多个候选路径中查找第一个存在的值。
    fn pick<'a>(value: &'a Value, paths: &[String]) -> Option<&'a Value> {
        paths.iter().find_map(|path| value.pointer(path))
    }
}

/// 从 URL 推断产物文件名与 MIME，产物以远端 URL 记录（中转站定位，不下载字节）。
fn artifact_for_url(url: &str) -> ArtifactDraft {
    let ext = url
        .split('?')
        .next()
        .unwrap_or("")
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
    let (filename, mime_type) = match ext.as_str() {
        "mp4" | "webm" | "mov" => (format!("output.{ext}"), "video/mp4".to_string()),
        "png" | "jpg" | "jpeg" | "webp" => {
            let mime = if ext == "jpg" || ext == "jpeg" {
                "image/jpeg"
            } else if ext == "webp" {
                "image/webp"
            } else {
                "image/png"
            };
            (format!("output.{ext}"), mime.to_string())
        }
        "mp3" | "wav" | "flac" | "m4a" => {
            let mime = if ext == "wav" {
                "audio/wav"
            } else if ext == "flac" {
                "audio/flac"
            } else {
                "audio/mpeg"
            };
            (format!("output.{ext}"), mime.to_string())
        }
        "glb" | "gltf" | "obj" | "fbx" => {
            let mime = if ext == "gltf" {
                "model/gltf+json"
            } else {
                "model/gltf-binary"
            };
            (format!("output.{ext}"), mime.to_string())
        }
        _ => (
            format!("output.{ext}"),
            "application/octet-stream".to_string(),
        ),
    };
    ArtifactDraft {
        filename,
        mime_type,
        byte_size: 0,
        checksum: String::new(),
        data: ArtifactData::Url {
            url: url.to_string(),
            expires_at: None,
        },
    }
}

fn simple_checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[async_trait]
impl ProviderDriver for AsyncTaskDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        descriptor()
    }

    async fn validate_connection(
        &self,
        connection: &ConnectionRevision,
    ) -> DomainResult<ConnectionCheck> {
        let base = Self::base_url(connection)?;
        if dialect_for(connection).is_none() {
            return Ok(ConnectionCheck {
                ok: false,
                capabilities: connection.effective_capabilities.clone(),
                message: Some("dialect 无效或缺少必需配置".to_string()),
            });
        }
        if self.api_key(connection).await?.is_none() {
            return Ok(ConnectionCheck {
                ok: false,
                capabilities: connection.effective_capabilities.clone(),
                message: Some("缺少 API Key 绑定".to_string()),
            });
        }
        validate_resolved_url(&base, NetworkPolicy::LOCAL_HTTP).await?;
        Ok(ConnectionCheck {
            ok: true,
            capabilities: connection.effective_capabilities.clone(),
            message: None,
        })
    }

    async fn prepare(&self, request: PrepareRequest) -> DomainResult<PreparedExecution> {
        let body = self.request_body(&request)?;
        let base = Self::base_url(&request.connection_revision)?;
        validate_resolved_url(&base, NetworkPolicy::LOCAL_HTTP).await?;
        let request_id = format!(
            "{}-{:x}",
            request.request_fingerprint,
            simple_checksum(&serde_json::to_vec(&body).unwrap_or_default())
        );
        Ok(PreparedExecution {
            connection_revision: request.connection_revision,
            provider_request: body,
            request_id,
        })
    }

    async fn submit(&self, execution: PreparedExecution) -> DomainResult<SubmitOutcome> {
        let base = Self::base_url(&execution.connection_revision)?;
        validate_resolved_url(&base, NetworkPolicy::LOCAL_HTTP).await?;
        let dialect = dialect_for(&execution.connection_revision).ok_or_else(|| {
            SafeError::validation("DIALECT_INVALID", "dialect 无效或缺少必需配置")
        })?;
        let url = join_path(&base, &dialect.submit_path)?;
        let key = self.api_key(&execution.connection_revision).await?;
        let mut builder = self
            .client
            .post(url.clone())
            .json(&execution.provider_request);
        if let Some(key) = key {
            builder = builder.bearer_auth(key);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| map_network_error(&error, "async_task_submit"))?;
        if !response.status().is_success() {
            // ⚠️ 提交路径专用：5xx 标成"提交结果不明"（平台可能已经收下）⇒ 不盲投。
            return Err(map_submit_http_error(
                response.status().as_u16(),
                "任务提交失败",
            ));
        }
        let parsed: Value = serde_json::from_slice(
            &read_limited(response, MAX_RESPONSE_BYTES).await?,
        )
        .map_err(|_| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON"))?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(200);
        if code != 200 && code != 0 {
            return Err(SafeError::provider(
                "TASK_SUBMIT_REJECTED",
                parsed
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("任务提交被拒绝"),
            ));
        }
        let task_id = Self::pick(&parsed, &dialect.task_id_paths)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应缺少 task_id")
            })?;
        Ok(SubmitOutcome::Accepted(
            crate::v2::domain::execution::RemoteHandle {
                provider: "async-task".to_string(),
                remote_id: task_id.to_string(),
            },
        ))
    }

    async fn reconcile(&self, request: ReconcileRequest) -> DomainResult<TaskSnapshot> {
        let base = Self::base_url(&request.connection_revision)?;
        validate_resolved_url(&base, NetworkPolicy::LOCAL_HTTP).await?;
        let dialect = dialect_for(&request.connection_revision).ok_or_else(|| {
            SafeError::validation("DIALECT_INVALID", "dialect 无效或缺少必需配置")
        })?;
        let mut url = join_path(&base, "v1/tasks")?;
        url.path_segments_mut()
            .map_err(|_| SafeError::validation("PROVIDER_URL_INVALID", "任务 URL 无效"))?
            .push(&request.remote_handle.remote_id);
        let key = self.api_key(&request.connection_revision).await?;
        let mut builder = self.client.get(url.clone());
        if let Some(key) = key {
            builder = builder.bearer_auth(key);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| map_network_error(&error, "async_task_query"))?;
        if !response.status().is_success() {
            return Err(map_http_error(response.status().as_u16(), "任务查询失败"));
        }
        let parsed: Value = serde_json::from_slice(&read_limited(response, 4 * 1024 * 1024).await?)
            .map_err(|_| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON"))?;
        let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(200);
        if code != 200 && code != 0 {
            return Err(SafeError::provider(
                "TASK_QUERY_REJECTED",
                parsed
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("任务查询被拒绝"),
            ));
        }
        // 所有 dialect 路径均相对完整响应（含 code/data 包裹）
        let status = parsed
            .pointer(&dialect.status_path)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        match status.as_str() {
            "completed" | "succeeded" | "success" | "done" => {
                let mut results = Vec::new();
                if let Some(url) =
                    Self::pick(&parsed, &dialect.result_url_paths).and_then(Value::as_str)
                {
                    // 中转站定位：只记录结果 URL，不下载字节。
                    results.push(artifact_for_url(url));
                }
                Ok(TaskSnapshot {
                    status: TaskStatus::Succeeded,
                    results,
                    error: None,
                    usage: None,
                })
            }
            "failed" | "error" => Ok(TaskSnapshot {
                status: TaskStatus::Failed,
                results: vec![],
                error: Some(Self::failure(&parsed, &status)),
                usage: None,
            }),
            _ => Ok(TaskSnapshot {
                status: TaskStatus::Running,
                results: vec![],
                error: None,
                usage: None,
            }),
        }
    }

    async fn cancel(&self, _request: CancelRequest) -> DomainResult<CancelOutcome> {
        Ok(CancelOutcome::RemoteMayContinue)
    }
}

impl AsyncTaskDriver {
    /// 从失败响应提取 message 并附带远端状态码。
    fn failure(parsed: &Value, remote_status: &str) -> SafeError {
        let message = parsed
            .pointer("/data/error/message")
            .or_else(|| parsed.pointer("/data/error_message"))
            .or_else(|| parsed.pointer("/data/message"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "云端任务失败".to_string());
        let mut details = std::collections::BTreeMap::new();
        details.insert("remoteStatus".to_string(), json!(remote_status));
        if let Some(code) = parsed.get("code").and_then(Value::as_i64) {
            details.insert("remoteCode".to_string(), json!(code));
        }
        SafeError::provider("PROVIDER_TASK_FAILED", message).with_details(details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::ids::{ConnectionId, ConnectionRevisionId};
    use crate::v2::domain::model::{RecipeBinding, SecretBinding, Timestamp};
    use crate::v2::ports::stores::SecretValue;
    use axum::{extract::State as AxState, routing::get, routing::post, Router};
    use std::sync::{Arc, Mutex};

    struct NoSecrets;

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

    struct MockState {
        task_status: Mutex<&'static str>,
        base_url: Mutex<String>,
        last_body: Mutex<Value>,
    }

    fn connection(base_url: String, dialect: &str, capabilities: &[&str]) -> ConnectionRevision {
        ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(1),
            config: BTreeMap::from([
                ("baseUrl".to_string(), json!(base_url)),
                ("dialect".to_string(), json!(dialect)),
            ]),
            secret_bindings: vec![],
            effective_capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        }
    }

    fn request(connection: ConnectionRevision, extra: BTreeMap<String, Value>) -> PrepareRequest {
        let mut provider_config = BTreeMap::from([("model".to_string(), json!("some-model"))]);
        provider_config.extend(extra);
        PrepareRequest {
            connection_revision: connection,
            recipe_binding: RecipeBinding {
                driver_key: "async-task".to_string(),
                provider_config,
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
            },
            inputs: BTreeMap::from([("prompt".to_string(), json!("a flying bird"))]),
            assets: vec![],
            request_fingerprint: "fp-1".to_string(),
        }
    }

    async fn mock_server() -> (String, Arc<MockState>) {
        let state = Arc::new(MockState {
            task_status: Mutex::new("processing"),
            base_url: Mutex::new(String::new()),
            last_body: Mutex::new(Value::Null),
        });
        let app = Router::new()
            .route(
                "/v1/images/generations",
                post({
                    move |AxState(state): AxState<Arc<MockState>>,
                          axum::Json(body): axum::Json<Value>| {
                        let state = state.clone();
                        async move {
                            *state.last_body.lock().unwrap() = body;
                            axum::Json(json!({
                                "code": 200,
                                "data": [{ "status": "submitted", "task_id": "img_task_1" }]
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/videos",
                post({
                    move |AxState(state): AxState<Arc<MockState>>,
                          axum::Json(body): axum::Json<Value>| {
                        let state = state.clone();
                        async move {
                            *state.last_body.lock().unwrap() = body;
                            axum::Json(json!({ "data": { "task_id": "vid_task_1" } }))
                        }
                    }
                }),
            )
            .route(
                "/v1/audio",
                post({
                    move |AxState(_): AxState<Arc<MockState>>,
                          axum::Json(_body): axum::Json<Value>| async move {
                        axum::Json(json!({ "task_id": "aud_task_1" }))
                    }
                }),
            )
            .route(
                "/v1/tasks/{task_id}",
                get({
                    move |AxState(state): AxState<Arc<MockState>>| {
                        let state = state.clone();
                        async move {
                            let status = *state.task_status.lock().unwrap();
                            match status {
                                "processing" => axum::Json(json!({
                                    "code": 200,
                                    "data": { "id": "t", "status": "processing" }
                                })),
                                _ => axum::Json(json!({
                                    "code": 200,
                                    "data": {
                                        "id": "t",
                                        "status": "completed",
                                        "result": {
                                            "video_url": format!(
                                                "{}/out.mp4",
                                                state.base_url.lock().unwrap()
                                            ),
                                            "images": [{
                                                "url": [format!(
                                                    "{}/out.png",
                                                    state.base_url.lock().unwrap()
                                                )]
                                            }],
                                            "audio_url": format!(
                                                "{}/out.mp3",
                                                state.base_url.lock().unwrap()
                                            )
                                        }
                                    }
                                })),
                            }
                        }
                    }
                }),
            )
            .route(
                "/out.mp4",
                get({
                    move |AxState(_): AxState<Arc<MockState>>| async move {
                        use axum::body::Body;
                        use axum::http::StatusCode;
                        use axum::response::Response;
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "video/mp4")
                            .body(Body::from(vec![0, 0, 0, 24, 0x66, 0x74, 0x79, 0x70]))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/out.png",
                get({
                    move |AxState(_): AxState<Arc<MockState>>| async move {
                        use axum::body::Body;
                        use axum::http::StatusCode;
                        use axum::response::Response;
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "image/png")
                            .body(Body::from(vec![0x89, 0x50, 0x4e, 0x47]))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/out.mp3",
                get({
                    move |AxState(_): AxState<Arc<MockState>>| async move {
                        use axum::body::Body;
                        use axum::http::StatusCode;
                        use axum::response::Response;
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "audio/mpeg")
                            .body(Body::from(vec![0x49, 0x44, 0x33]))
                            .unwrap()
                    }
                }),
            );
        let app = app.with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://127.0.0.1:{}", address.port());
        *state.base_url.lock().unwrap() = base.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (base, state)
    }

    #[tokio::test]
    async fn descriptor_is_media_agnostic() {
        let descriptor = descriptor();
        assert_eq!(descriptor.protocol_id, "async-task");
        assert!(descriptor.business_capabilities.is_empty());
        assert!(descriptor.lifecycle_features.remote_reconciliation);
    }

    #[tokio::test]
    async fn apimart_dialect_submits_image_task() {
        let (base, state) = mock_server().await;
        let driver = AsyncTaskDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(
                connection(base, "apimart", &["text-to-image"]),
                BTreeMap::from([
                    ("size".to_string(), json!("16:9")),
                    ("resolution".to_string(), json!("2k")),
                ]),
            ))
            .await
            .expect("prepare 应成功");
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        match outcome {
            SubmitOutcome::Accepted(handle) => {
                assert_eq!(handle.remote_id, "img_task_1");
            }
            other => panic!("期望 Accepted，得到 {other:?}"),
        }
        let body = state.last_body.lock().unwrap().clone();
        assert_eq!(body["resolution"], "2k");
    }

    #[tokio::test]
    async fn generic_video_dialect_submits_and_downloads_video() {
        let (base, state) = mock_server().await;
        let driver = AsyncTaskDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(
                connection(base, "generic-video", &["text-to-video"]),
                BTreeMap::from([("duration".to_string(), json!(5))]),
            ))
            .await
            .expect("prepare 应成功");
        let connection_revision = prepared.connection_revision.clone();
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        let handle = match outcome {
            SubmitOutcome::Accepted(handle) => handle,
            other => panic!("期望 Accepted，得到 {other:?}"),
        };
        *state.task_status.lock().unwrap() = "completed";
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision,
                remote_handle: handle,
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Succeeded);
        assert_eq!(snapshot.results.len(), 1);
        assert_eq!(snapshot.results[0].filename, "output.mp4");
        assert_eq!(snapshot.results[0].mime_type, "video/mp4");
    }

    #[tokio::test]
    async fn generic_audio_dialect_downloads_audio() {
        let (base, state) = mock_server().await;
        let driver = AsyncTaskDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(
                connection(base, "generic-audio", &["text-to-audio"]),
                BTreeMap::new(),
            ))
            .await
            .expect("prepare 应成功");
        let connection_revision = prepared.connection_revision.clone();
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        let handle = match outcome {
            SubmitOutcome::Accepted(handle) => handle,
            other => panic!("期望 Accepted，得到 {other:?}"),
        };
        *state.task_status.lock().unwrap() = "completed";
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision,
                remote_handle: handle,
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Succeeded);
        assert_eq!(snapshot.results[0].filename, "output.mp3");
        assert_eq!(snapshot.results[0].mime_type, "audio/mpeg");
    }

    #[tokio::test]
    async fn processing_maps_to_running_and_failed_to_error() {
        let (base, state) = mock_server().await;
        let driver = AsyncTaskDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(
                connection(base, "generic-video", &["text-to-video"]),
                BTreeMap::new(),
            ))
            .await
            .expect("prepare 应成功");
        let handle = match driver
            .submit(prepared.clone())
            .await
            .expect("submit 应成功")
        {
            SubmitOutcome::Accepted(handle) => handle,
            other => panic!("期望 Accepted，得到 {other:?}"),
        };
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision: prepared.connection_revision.clone(),
                remote_handle: handle.clone(),
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Running);
        *state.task_status.lock().unwrap() = "completed";
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision: prepared.connection_revision.clone(),
                remote_handle: handle.clone(),
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Succeeded);
        let _ = (state, handle, prepared);
    }

    #[tokio::test]
    async fn validate_connection_rejects_unknown_dialect() {
        let (base, _) = mock_server().await;
        let driver = AsyncTaskDriver::new(Box::new(NoSecrets));
        let check = driver
            .validate_connection(&connection(base, "not-a-dialect", &["text-to-image"]))
            .await
            .expect("校验应成功");
        assert!(!check.ok);
    }
}
