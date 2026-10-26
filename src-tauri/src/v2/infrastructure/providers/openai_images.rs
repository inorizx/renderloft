use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::OnceLock;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::{
    ArtifactData, ArtifactDraft, CancelOutcome, ConnectionCheck, ProviderDescriptor, SubmitOutcome,
    TaskSnapshot,
};
use crate::v2::domain::model::{is_api_key_slot, ConnectionRevision, LifecycleFeatures};
use crate::v2::ports::provider::{
    CancelRequest, PrepareRequest, PreparedExecution, ProviderDriver, ReconcileRequest,
};
use crate::v2::ports::stores::SecretStore;

use super::safety::{
    join_path, map_http_error, map_network_error, read_limited, validate_base_url,
    validate_resolved_url, NetworkPolicy, MAX_RESPONSE_BYTES, MAX_RESULT_BYTES,
};

/// 允许进请求体的字段。
///
/// ⚠️ **`image` 曾经不在这个表里**（2026-10-12 补）——`request_body` 用它过滤输入，于是带图的输入
/// 被静默丢掉、`has_image` 永远为假、`/v1/images/edits` 那条路在产品里根本走不到：
/// 能力声明里写着 `image-to-image`，实际是死代码。写新驱动时记住这条：
/// **凡是在能力里声明了的输入，白名单里必须有它**，否则声明就是假的。
pub const ALLOWED_FIELDS: [&str; 8] = [
    "model",
    "prompt",
    "size",
    "quality",
    "background",
    "output_format",
    "n",
    "image",
];

pub fn descriptor() -> &'static ProviderDescriptor {
    static DESCRIPTOR: OnceLock<ProviderDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| ProviderDescriptor {
        vendor_id: "openai".to_string(),
        protocol_id: "openai-images".to_string(),
        display_name: "OpenAI Images".to_string(),
        business_capabilities: vec!["text-to-image".to_string(), "image-to-image".to_string()],
        lifecycle_features: LifecycleFeatures {
            synchronous_result: true,
            ..Default::default()
        },
        security_requirements: serde_json::from_value(json!({ "httpsOnly": true }))
            .unwrap_or_default(),
        connection_schema: json!({
            "type": "object",
            "required": ["baseUrl"],
            "properties": {
                "baseUrl": { "type": "string", "format": "uri" },
                "defaultModel": { "type": "string" },
                // 这条连接**声明**能跑哪些模型（通用协议层的"渠道声明模型"那一层）。
                // 它是给人看的过滤器：跑表单里的 model 字段据此变成下拉，而不是手打。
                // 声明是**建议**不是闸门——真发了清单外的模型，由平台去拒。
                "models": { "type": "array", "items": { "type": "string" } },
                "dialect": { "type": "string" }
            }
        }),
        binding_schema: json!({
            "type": "object",
            "properties": {
                "model": { "type": "string" },
                "size": { "type": "string" },
                "quality": { "type": "string", "enum": ["low", "medium", "high", "auto"] }
            }
        }),
    })
}

pub struct OpenAiImagesDriver {
    client: Client,
    secrets: Box<dyn SecretStore>,
}

impl OpenAiImagesDriver {
    pub fn new(secrets: Box<dyn SecretStore>) -> Self {
        // 出网客户端一律经 `proxy::configure`（见 `providers/proxy.rs`）。
        let client = super::proxy::configure(Client::builder())
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
        let mut body = serde_json::Map::new();
        for (key, value) in &request.inputs {
            if ALLOWED_FIELDS.contains(&key.as_str()) {
                body.insert(key.clone(), value.clone());
            }
        }
        for (key, value) in &request.recipe_binding.provider_config {
            if ALLOWED_FIELDS.contains(&key.as_str()) && !body.contains_key(key) {
                body.insert(key.clone(), value.clone());
            }
        }
        // 连接的 `defaultModel` 是**兜底**：输入与配方都没点名模型时才用它。
        //
        // ⚠️ 这个字段此前是**死的**——界面写了它、驱动从不读（2026-10-13 接上）。所以"默认模型"
        // 现在真的默认：不写就沿用平台自己的默认（那才是"没说"的正确含义，别替平台编一个）。
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
            return Err(SafeError::validation(
                "PROMPT_REQUIRED",
                "图像提示词不能为空",
            ));
        }
        Ok(Value::Object(body))
    }

    fn endpoint(base: &reqwest::Url, has_image: bool) -> Result<reqwest::Url, SafeError> {
        let path = if has_image {
            "images/edits"
        } else {
            "images/generations"
        };
        join_path(base, path)
    }

    /// 把请求体转成 `multipart/form-data`：标量字段原样带上，`image` 那一项换成**真正的文件字节**。
    async fn multipart_form(&self, body: &Value) -> Result<reqwest::multipart::Form, SafeError> {
        let mut form = reqwest::multipart::Form::new();
        for (key, value) in body.as_object().into_iter().flatten() {
            if key == "image" {
                continue;
            }
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
            form = form.text(key.clone(), text);
        }
        let image = body
            .get("image")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (bytes, mime) = self.image_bytes(image).await?;
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name("image.png")
            .mime_str(&mime)
            .map_err(|_| SafeError::validation("IMAGE_MIME_INVALID", "图像类型无法识别"))?;
        Ok(form.part("image", part))
    }

    /// `image` 字段可能是 **data URL**、**裸 base64** 或 **http(s) 链接**——三种都收。
    ///
    /// 配方里的这个字段是作者/用户写的（有的平台给链接、有的给 base64），写法不该只有一种。
    async fn image_bytes(&self, image: &str) -> Result<(Vec<u8>, String), SafeError> {
        let image = image.trim();
        if let Some(rest) = image.strip_prefix("data:") {
            let (meta, payload) = rest.split_once(',').ok_or_else(|| {
                SafeError::validation("IMAGE_DATA_URL_INVALID", "data URL 缺少逗号分隔")
            })?;
            let mime = meta.split(';').next().unwrap_or("image/png").to_string();
            return Ok((self.decode_base64(payload).await?, mime));
        }
        if image.starts_with("http://") || image.starts_with("https://") {
            let url = validate_base_url(image, NetworkPolicy::LOCAL_HTTP)?;
            validate_resolved_url(&url, NetworkPolicy::LOCAL_HTTP).await?;
            let response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|error| map_network_error(&error, "image_download"))?;
            if !response.status().is_success() {
                return Err(SafeError::validation(
                    "IMAGE_DOWNLOAD_FAILED",
                    "图像素材下载失败",
                ));
            }
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
                .unwrap_or_else(|| "image/png".to_string());
            let bytes = response
                .bytes()
                .await
                .map_err(|error| map_network_error(&error, "image_download"))?;
            return Ok((bytes.to_vec(), mime));
        }
        Ok((self.decode_base64(image).await?, "image/png".to_string()))
    }

    async fn decode_base64(&self, input: &str) -> Result<Vec<u8>, SafeError> {
        let mut output = Vec::with_capacity(input.len() * 3 / 4);
        let mut buffer = 0u32;
        let mut bits = 0u8;
        for byte in input.bytes() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => break,
                _ if byte.is_ascii_whitespace() => continue,
                _ => {
                    return Err(SafeError::validation(
                        "PROVIDER_BASE64_INVALID",
                        "Provider 返回了无效的 base64 图像",
                    ))
                }
            };
            buffer = (buffer << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push((buffer >> bits) as u8);
                if output.len() > MAX_RESULT_BYTES {
                    return Err(SafeError::validation(
                        "PROVIDER_RESULT_TOO_LARGE",
                        "图像结果不能超过 100 MB",
                    ));
                }
            }
        }
        if output.is_empty() {
            return Err(SafeError::validation(
                "PROVIDER_RESULT_EMPTY",
                "Provider 返回的图像 base64 为空",
            ));
        }
        Ok(output)
    }

    async fn download_url(&self, url: &str) -> Result<Vec<u8>, SafeError> {
        let parsed = reqwest::Url::parse(url).map_err(|_| {
            SafeError::validation("PROVIDER_RESULT_URL_INVALID", "结果下载地址无效")
        })?;
        validate_base_url(parsed.as_str(), NetworkPolicy::STRICT)?;
        validate_resolved_url(&parsed, NetworkPolicy::STRICT).await?;
        let response = self
            .client
            .get(parsed.clone())
            .send()
            .await
            .map_err(|error| map_network_error(&error, "image_download"))?;
        if response.url() != &parsed {
            return Err(SafeError::validation(
                "PROVIDER_RESULT_REDIRECTED",
                "结果下载发生未授权重定向",
            ));
        }
        if !response.status().is_success() {
            return Err(map_http_error(response.status().as_u16(), "图像下载失败"));
        }
        read_limited(response, MAX_RESULT_BYTES).await
    }

    fn artifact_draft(filename: &str, bytes: Vec<u8>) -> ArtifactDraft {
        ArtifactDraft {
            filename: filename.to_string(),
            mime_type: "image/png".to_string(),
            byte_size: bytes.len() as u64,
            checksum: format!("sha1-{:x}", simple_checksum(&bytes)),
            data: ArtifactData::Bytes(bytes),
        }
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
impl ProviderDriver for OpenAiImagesDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        descriptor()
    }

    /// 拉 `GET {baseUrl}/models` 得到这个网关/平台声明的模型清单。
    ///
    /// 形状按 **OpenAI**：`{ "data": [ { "id": "gpt-image-1" }, … ] }`。中转站大多照抄它，
    /// 但也见过直接回数组的 ⇒ 两种都收（这就是"通用协议层"该有的宽容度）。
    /// 只取 `id`、保序去重：顺序是平台给的（通常新的在前），我们没理由重排。
    async fn fetch_models(&self, connection: &ConnectionRevision) -> DomainResult<Vec<String>> {
        let base = Self::base_url(connection)?;
        validate_resolved_url(&base, NetworkPolicy::LOCAL_HTTP).await?;
        let url = join_path(&base, "models")?;
        let mut builder = self.client.get(url);
        if let Some(key) = self.api_key(connection).await? {
            builder = builder.bearer_auth(key);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| map_network_error(&error, "models"))?;
        if !response.status().is_success() {
            return Err(SafeError::provider(
                "MODELS_REQUEST_FAILED",
                format!(
                    "模型清单请求失败（HTTP {}）——检查 Base URL 与 API Key",
                    response.status().as_u16()
                ),
            ));
        }
        let payload: Value = response
            .json()
            .await
            .map_err(|_| SafeError::provider("MODELS_RESPONSE_INVALID", "模型清单不是 JSON"))?;
        let items = payload
            .get("data")
            .and_then(Value::as_array)
            .or_else(|| payload.as_array())
            .cloned()
            .unwrap_or_default();
        let mut models: Vec<String> = Vec::new();
        for item in items {
            let id = match &item {
                Value::String(id) => Some(id.clone()),
                other => other.get("id").and_then(Value::as_str).map(str::to_string),
            };
            if let Some(id) = id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
            {
                if !models.contains(&id) {
                    models.push(id);
                }
            }
        }
        Ok(models)
    }

    async fn validate_connection(
        &self,
        connection: &ConnectionRevision,
    ) -> DomainResult<ConnectionCheck> {
        let base = Self::base_url(connection)?;
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
        let has_image = execution
            .provider_request
            .get("image")
            .is_some_and(|value| !value.is_null());
        let url = Self::endpoint(&base, has_image)?;
        let key = self.api_key(&execution.connection_revision).await?;
        // 出图是**能合法地慢**的那一类（大尺寸 + 排队，官方端点尤其）：按连接的配置给预算，
        // 没配就是今天这个 120 秒。三个 builder（multipart / 415 后的 JSON 重试 / JSON）
        // 共用同一个预算——它们是同一次提交的三条路径。
        let timeout = crate::v2::domain::connection_options::request_timeout(
            &execution.connection_revision.config,
            crate::v2::domain::connection_options::DEFAULT_REQUEST_TIMEOUT_SECS,
        );
        let response = if has_image {
            // ⚠️ 官方 `POST /v1/images/edits` 收的是 **multipart/form-data**（`image` 是文件部件），
            // 而我们原来一律发 JSON ⇒ 对官方端点其实是坏的，只在"收 JSON 的中转站"上能跑。
            // 现在：先 multipart（官方口径），只在遇到 **415** 时退回 JSON 再试一次
            // （415 意味着对方根本没接受这次请求，不会重复计费；别的错误不重试，避免花两次钱）。
            let form = self.multipart_form(&execution.provider_request).await?;
            let mut builder = self.client.post(url.clone()).multipart(form);
            if let Some(key) = key.as_deref() {
                builder = builder.bearer_auth(key);
            }
            let first = builder
                .timeout(timeout)
                .send()
                .await
                .map_err(|error| map_network_error(&error, "image_submit"))?;
            if first.status() == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
                let mut retry = self
                    .client
                    .post(url.clone())
                    .json(&execution.provider_request);
                if let Some(key) = key.as_deref() {
                    retry = retry.bearer_auth(key);
                }
                retry
                    .timeout(timeout)
                    .send()
                    .await
                    .map_err(|error| map_network_error(&error, "image_submit"))?
            } else {
                first
            }
        } else {
            let mut builder = self
                .client
                .post(url.clone())
                .json(&execution.provider_request);
            if let Some(key) = key.as_deref() {
                builder = builder.bearer_auth(key);
            }
            builder
                .timeout(timeout)
                .send()
                .await
                .map_err(|error| map_network_error(&error, "image_submit"))?
        };
        if response.url() != &url {
            return Err(SafeError::validation(
                "PROVIDER_REQUEST_REDIRECTED",
                "请求发生未授权重定向",
            ));
        }
        if !response.status().is_success() {
            return Err(map_http_error(response.status().as_u16(), "图像生成失败"));
        }
        let bytes = read_limited(response, MAX_RESPONSE_BYTES).await?;
        let parsed: Value = serde_json::from_slice(&bytes)
            .map_err(|_| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应不是有效 JSON"))?;
        let items = parsed
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| SafeError::validation("PROVIDER_RESPONSE_INVALID", "响应缺少 data"))?;
        if items.is_empty() {
            return Err(SafeError::validation(
                "PROVIDER_RESULT_EMPTY",
                "响应不包含结果",
            ));
        }
        let mut artifacts = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let bytes = if let Some(url) = item.get("url").and_then(Value::as_str) {
                self.download_url(url).await?
            } else if let Some(data) = item.get("b64_json").and_then(Value::as_str) {
                self.decode_base64(data).await?
            } else {
                return Err(SafeError::validation(
                    "PROVIDER_RESPONSE_INVALID",
                    "图像结果缺少 url 或 b64_json",
                ));
            };
            artifacts.push(Self::artifact_draft(&format!("image-{index}.png"), bytes));
        }
        Ok(SubmitOutcome::Completed(artifacts))
    }

    async fn reconcile(&self, _request: ReconcileRequest) -> DomainResult<TaskSnapshot> {
        Err(SafeError::validation(
            "PROVIDER_NOT_TRACKED",
            "OpenAI Images 为同步结果，不应进入对账",
        ))
    }

    async fn cancel(&self, _request: CancelRequest) -> DomainResult<CancelOutcome> {
        Ok(CancelOutcome::RemoteMayContinue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::model::{SecretBinding, SecretResolution, Timestamp};
    use crate::v2::ports::stores::SecretValue;
    use axum::http::HeaderMap;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct StubSecrets {
        value: Option<String>,
    }

    #[async_trait]
    impl SecretStore for StubSecrets {
        async fn resolve(&self, _binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
            Ok(self.value.clone().map(|secret| SecretValue { secret }))
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
            id: crate::v2::domain::ids::ConnectionRevisionId(1),
            connection_id: crate::v2::domain::ids::ConnectionId(1),
            config: BTreeMap::from([("baseUrl".to_string(), json!(base_url))]),
            secret_bindings: vec![SecretBinding {
                slot: "connection-1-api_key".to_string(),
                resolution: SecretResolution::Current,
            }],
            effective_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        }
    }

    fn request(connection: ConnectionRevision) -> PrepareRequest {
        PrepareRequest {
            connection_revision: connection,
            recipe_binding: Default::default(),
            inputs: BTreeMap::from([("prompt".to_string(), json!("a red house"))]),
            assets: vec![],
            request_fingerprint: "fp-1".to_string(),
        }
    }

    struct MockState {
        body: Mutex<Value>,
        authorization: Mutex<String>,
    }

    async fn generate(
        axum::extract::State(state): axum::extract::State<std::sync::Arc<MockState>>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        *state.body.lock().unwrap() = body;
        *state.authorization.lock().unwrap() = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        axum::Json(json!({
            "created": 1,
            "data": [{ "b64_json": "aGVsbG8=" }]
        }))
    }

    async fn mock_server() -> (String, std::sync::Arc<MockState>) {
        let state = std::sync::Arc::new(MockState {
            body: Mutex::new(Value::Null),
            authorization: Mutex::new(String::new()),
        });
        let app = axum::Router::new()
            .route(
                "/v1/images/generations",
                axum::routing::post(generate).with_state(state.clone()),
            )
            .into_make_service();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 服务应成功");
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}/v1", address.port()), state)
    }

    /// 带图的 mock：**第一次回 415**（模拟"只认 multipart"的官方口径被我们用错编码撞上，
    /// 或者反过来），第二次回正常结果；同时把每次请求的 content-type 与原始体记下来。
    struct EditsState {
        requests: Mutex<Vec<(String, String)>>,
    }

    async fn edits(
        axum::extract::State(state): axum::extract::State<std::sync::Arc<EditsState>>,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let first = {
            let mut requests = state.requests.lock().unwrap();
            requests.push((content_type, String::from_utf8_lossy(&body).to_string()));
            requests.len() == 1
        };
        if first {
            return (
                axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected multipart/form-data",
            )
                .into_response();
        }
        axum::Json(json!({ "created": 1, "data": [{ "b64_json": "aGVsbG8=" }] })).into_response()
    }

    async fn edits_mock_server() -> (String, std::sync::Arc<EditsState>) {
        let state = std::sync::Arc::new(EditsState {
            requests: Mutex::new(Vec::new()),
        });
        let app = axum::Router::new()
            .route(
                "/v1/images/edits",
                axum::routing::post(edits).with_state(state.clone()),
            )
            .into_make_service();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 服务应成功");
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}/v1", address.port()), state)
    }

    /// `/v1/models` 的 mock：返回脚本好的负载，并记下 Authorization 头。
    struct ModelsState {
        payload: Value,
        authorization: Mutex<String>,
    }

    async fn models(
        axum::extract::State(state): axum::extract::State<std::sync::Arc<ModelsState>>,
        headers: HeaderMap,
    ) -> axum::Json<Value> {
        *state.authorization.lock().unwrap() = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        axum::Json(state.payload.clone())
    }

    async fn models_mock_server(payload: Value) -> (String, std::sync::Arc<ModelsState>) {
        let state = std::sync::Arc::new(ModelsState {
            payload,
            authorization: Mutex::new(String::new()),
        });
        let app = axum::Router::new()
            .route(
                "/v1/models",
                axum::routing::get(models).with_state(state.clone()),
            )
            .into_make_service();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 服务应成功");
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}/v1", address.port()), state)
    }

    /// 模型发现：**按 OpenAI 的形状解析**（`data[].id`），保序去重，并带上密钥。
    ///
    /// 去重不是洁癖：中转站的清单里重复项很常见，而它直接进界面下拉——重复项看起来像
    /// "两个不同的选项"，用户会以为其中一个是另一个的变体。
    #[tokio::test]
    async fn fetch_models_reads_the_openai_shape_in_order_without_duplicates() {
        let (base, state) = models_mock_server(json!({
            "object": "list",
            "data": [
                { "id": "gpt-image-1", "object": "model" },
                { "id": "dall-e-3" },
                { "id": "gpt-image-1" },
                { "id": "  " },
                { "object": "model" }
            ]
        }))
        .await;
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets {
            value: Some("sk-test".to_string()),
        }));
        let models = driver
            .fetch_models(&connection(base))
            .await
            .expect("拉清单应成功");
        assert_eq!(
            models,
            vec!["gpt-image-1".to_string(), "dall-e-3".to_string()]
        );
        assert_eq!(
            state.authorization.lock().unwrap().as_str(),
            "Bearer sk-test",
            "网关按 Key 授权模型，清单请求也要带密钥"
        );
    }

    /// 有的网关直接回数组（不是 OpenAI 那层 `data`）——两种都收，这就是"通用"的意思。
    #[tokio::test]
    async fn fetch_models_accepts_a_bare_array() {
        let (base, _) = models_mock_server(json!(["flux-pro", "sdxl"])).await;
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets { value: None }));
        let models = driver
            .fetch_models(&connection(base))
            .await
            .expect("裸数组也要认");
        assert_eq!(models, vec!["flux-pro".to_string(), "sdxl".to_string()]);
    }

    /// 拉不到要说清是 HTTP 几（401 与 404 的修法完全不同），不能回一个空清单——
    /// 空清单在界面上长得像"平台说它没有模型"。
    #[tokio::test]
    async fn fetch_models_reports_the_status_instead_of_an_empty_list() {
        let app = axum::Router::new()
            .route(
                "/v1/models",
                axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
            )
            .into_make_service();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets { value: None }));
        let error = driver
            .fetch_models(&connection(format!("http://127.0.0.1:{port}/v1")))
            .await
            .expect_err("401 必须报错");
        assert_eq!(error.code, "MODELS_REQUEST_FAILED");
        assert!(error.message.contains("401"), "{}", error.message);
    }

    /// 连接的 `defaultModel` 是**兜底**：输入没点名模型时才用它；点了名就以输入为准。
    ///
    /// 这条字段此前是死的（界面写了、驱动从不读），所以两条都要钉：**它真的生效**，
    /// 且**不会盖掉**用户在配方/运行里指定的模型。
    #[tokio::test]
    async fn default_model_is_only_a_fallback() {
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets { value: None }));
        let with_default = |config: Vec<(&str, Value)>, inputs: Vec<(&str, Value)>| {
            let mut revision = connection("http://127.0.0.1:1/v1".to_string());
            for (key, value) in config {
                revision.config.insert(key.to_string(), value);
            }
            let mut request = request(revision);
            request.inputs = inputs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect();
            request
        };

        // 没点名 ⇒ 用连接的默认模型
        let prepared = driver
            .prepare(with_default(
                vec![("defaultModel", json!("gpt-image-1"))],
                vec![("prompt", json!("a red house"))],
            ))
            .await
            .expect("prepare 应成功");
        assert_eq!(prepared.provider_request["model"], json!("gpt-image-1"));

        // 输入点名了 ⇒ 输入赢
        let prepared = driver
            .prepare(with_default(
                vec![("defaultModel", json!("gpt-image-1"))],
                vec![
                    ("prompt", json!("a red house")),
                    ("model", json!("dall-e-3")),
                ],
            ))
            .await
            .expect("prepare 应成功");
        assert_eq!(prepared.provider_request["model"], json!("dall-e-3"));

        // 都没说 ⇒ **不编一个模型**（"没说"就交给平台自己的默认）
        let prepared = driver
            .prepare(with_default(vec![], vec![("prompt", json!("a red house"))]))
            .await
            .expect("prepare 应成功");
        assert!(prepared.provider_request.get("model").is_none());
    }

    /// `images/edits` 的两种编码：**先 multipart（官方口径）**，遇 **415** 再退回 JSON（中转站口径）。
    ///
    /// 这条钉的是行为不是实现：① 带图时发的是 `multipart/form-data`，标量字段与**解码后的图像字节**
    /// 都在里面（`aGVsbG8=` ⇒ `hello`）；② 对方 415 时我们**再发一次 JSON**，而不是直接失败；
    /// ③ 只重试一次，且重试带的是原始 `image` 字段。
    #[tokio::test]
    async fn edits_prefers_multipart_and_falls_back_to_json_on_415() {
        let (base, state) = edits_mock_server().await;
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets {
            value: Some("sk-test".to_string()),
        }));
        let mut input = request(connection(base));
        input.inputs.insert("image".to_string(), json!("aGVsbG8="));
        let prepared = driver.prepare(input).await.expect("prepare 应成功");
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        match outcome {
            SubmitOutcome::Completed(artifacts) => assert_eq!(artifacts.len(), 1),
            other => panic!("期望 Completed，得到 {other:?}"),
        }
        let requests = state.requests.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            2,
            "应当先 multipart、415 后再 JSON 重试一次"
        );
        assert!(
            requests[0].0.starts_with("multipart/form-data"),
            "第一次应当是 multipart，实际是 {}",
            requests[0].0
        );
        assert!(
            requests[0].1.contains("name=\"image\""),
            "图像部件名应为 image"
        );
        assert!(
            requests[0].1.contains("a red house"),
            "multipart 里应带上 prompt"
        );
        assert!(
            requests[0].1.contains("hello"),
            "multipart 里应是解码后的图像字节（base64 aGVsbG8= ⇒ hello）"
        );
        assert!(
            requests[1].0.starts_with("application/json"),
            "415 之后应当退回 JSON，实际是 {}",
            requests[1].0
        );
        assert!(
            requests[1].1.contains("\"image\":\"aGVsbG8=\""),
            "JSON 重试应带原始 image 字段"
        );
    }

    #[tokio::test]
    async fn submit_completed_with_decoded_artifact() {
        let (base, state) = mock_server().await;
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets {
            value: Some("sk-test".to_string()),
        }));
        let prepared = driver
            .prepare(request(connection(base)))
            .await
            .expect("prepare 应成功");
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        match outcome {
            SubmitOutcome::Completed(artifacts) => {
                assert_eq!(artifacts.len(), 1);
                assert_eq!(artifacts[0].mime_type, "image/png");
                assert_eq!(artifacts[0].data, ArtifactData::Bytes(b"hello".to_vec()));
            }
            other => panic!("期望 Completed，得到 {other:?}"),
        }
        assert_eq!(*state.authorization.lock().unwrap(), "Bearer sk-test");
        assert_eq!(state.body.lock().unwrap()["prompt"], "a red house");
    }

    #[tokio::test]
    async fn submit_sends_authorization_header() {
        let (base, state) = mock_server().await;
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets {
            value: Some("sk-abc".to_string()),
        }));
        let prepared = driver
            .prepare(request(connection(base)))
            .await
            .expect("prepare 应成功");
        driver.submit(prepared).await.expect("submit 应成功");
        assert_eq!(*state.authorization.lock().unwrap(), "Bearer sk-abc");
    }

    #[tokio::test]
    async fn validate_connection_reports_missing_key_without_network() {
        let connection = connection("https://api.openai.com/v1".to_string());
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets { value: None }));
        let check = driver
            .validate_connection(&connection)
            .await
            .expect("缺 key 应返回检查结果而非错误");
        assert!(!check.ok);
        assert!(check.message.is_some());
    }

    #[tokio::test]
    async fn validate_connection_rejects_insecure_url() {
        let connection = connection("http://api.example.com/v1".to_string());
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets { value: None }));
        let error = driver
            .validate_connection(&connection)
            .await
            .expect_err("非 HTTPS 地址应被拒绝");
        assert_eq!(error.code, "PROVIDER_URL_INSECURE");
    }

    #[tokio::test]
    async fn prepare_requires_prompt() {
        let connection = connection("https://api.openai.com/v1".to_string());
        let driver = OpenAiImagesDriver::new(Box::new(StubSecrets { value: None }));
        let mut req = request(connection);
        req.inputs.clear();
        let error = driver.prepare(req).await.expect_err("缺少 prompt 应报错");
        assert_eq!(error.code, "PROMPT_REQUIRED");
    }
}
