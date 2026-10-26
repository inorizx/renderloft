use async_trait::async_trait;
use reqwest::{multipart, Client, Url};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::{
    ArtifactData, ArtifactDraft, CancelOutcome, ConnectionCheck, ProviderDescriptor, RecoveryProof,
    RemoteHandle, SubmitOutcome, TaskSnapshot, TaskStatus, UncertainSubmission,
};
use crate::v2::domain::model::{is_api_key_slot, ConnectionRevision, LifecycleFeatures};
use crate::v2::ports::provider::{
    CancelRequest, NodeClassInfo, NodeInfoCatalog, NodeInputSpec, PrepareRequest,
    PreparedExecution, ProviderDriver, ReconcileRequest,
};
use crate::v2::ports::stores::SecretStore;

use super::safety::{
    join_path, map_http_error, map_network_error, map_submit_http_error, read_limited,
    validate_base_url, validate_resolved_url, validate_same_origin, NetworkPolicy,
    MAX_RESULT_BYTES,
};

/// 把一个节点类名编进 URL 路径。
///
/// 只用最基本的百分号编码，把**路径分隔符与查询/片段起始符**这些会撕开 URL 的字符挡掉；
/// 类名允许的字符（字母、数字、`_`、`.`、`-`）原样保留。自定义节点名里真的塞了空格/斜杠时，
/// 这一步保证我们是"请求一个不存在的类"（→ 跳过），而不是"请求到别的地方"。
fn urlencoding_minimal(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'~');
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `{"input": {"required": {...}, "optional": {...}}, "display_name": ...}` → 规格表。
///
/// ComfyUI 的形态：每个输入是 `[类型或候选数组, 选项?]`
/// * 第一项是**数组** ⇒ 那是枚举，数组内容就是全部合法取值；
/// * 第一项是字符串 ⇒ 类型名（`INT` / `FLOAT` / `STRING` / `BOOLEAN` / 自定义类型）；
/// * 第二项（对象）里可能有 `default` / `min` / `max` / `step` / `multiline` / `tooltip`。
///
/// ⚠️ `min`/`max` 有时是**大到没意义的哨兵**（种子的上限是 u64 最大值），原样透传即可——
/// 拿它当滑块范围会很蠢，但那是界面的事，这里不能替它改数。
fn parse_node_class(value: &Value) -> Option<NodeClassInfo> {
    value.as_object()?;
    let display_name = value
        .get("display_name")
        .and_then(Value::as_str)
        .or_else(|| value.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    let mut inputs = BTreeMap::new();
    // optional 先放，required 覆盖同名（同一个输入不会两边都有，但真出现时以 required 为准）。
    for bucket in ["optional", "required"] {
        let Some(section) = value
            .get("input")
            .and_then(|input| input.get(bucket))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (name, spec) in section {
            if let Some(parsed) = parse_node_input(spec) {
                inputs.insert(name.clone(), parsed);
            }
        }
    }
    Some(NodeClassInfo {
        display_name,
        inputs,
    })
}

fn parse_node_input(spec: &Value) -> Option<NodeInputSpec> {
    let list = spec.as_array()?;
    let head = list.first()?;
    let options = list.get(1);
    let number = |key: &str| {
        options
            .and_then(|options| options.get(key))
            .and_then(Value::as_f64)
    };
    let (kind, choices) = if let Some(choices) = head.as_array() {
        (
            "COMBO".to_string(),
            choices
                .iter()
                .filter_map(|choice| match choice {
                    Value::String(text) => Some(text.clone()),
                    // 少数节点用数字当候选取值（如某些 seed 模式），转成字符串即可。
                    Value::Number(number) => Some(number.to_string()),
                    _ => None,
                })
                .collect(),
        )
    } else {
        (head.as_str().unwrap_or_default().to_string(), Vec::new())
    };
    if kind.is_empty() {
        return None;
    }
    Some(NodeInputSpec {
        kind,
        choices,
        default: options.and_then(|options| options.get("default")).cloned(),
        min: number("min"),
        max: number("max"),
        step: number("step"),
        multiline: options
            .and_then(|options| options.get("multiline"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub fn descriptor() -> &'static ProviderDescriptor {
    static DESCRIPTOR: OnceLock<ProviderDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| ProviderDescriptor {
        vendor_id: "comfyui".to_string(),
        protocol_id: "comfyui".to_string(),
        display_name: "ComfyUI".to_string(),
        business_capabilities: vec!["workflow-execution".to_string()],
        lifecycle_features: LifecycleFeatures {
            asset_upload: true,
            remote_reconciliation: true,
            ..Default::default()
        },
        security_requirements: serde_json::from_value(json!({ "allowLocalHttp": true }))
            .unwrap_or_default(),
        connection_schema: json!({
            "type": "object",
            "required": ["baseUrl"],
            "properties": {
                "baseUrl": { "type": "string", "format": "uri" },
                "apiKey": { "type": "string" }
            }
        }),
        binding_schema: json!({
            "type": "object",
            "required": ["workflow"],
            "properties": {
                "workflow": { "type": "object" }
            }
        }),
    })
}

pub struct ComfyUiDriver {
    client: Client,
    secrets: Box<dyn SecretStore>,
}

impl ComfyUiDriver {
    /// 这个连接的地址策略。
    ///
    /// ComfyUI 的典型部署就是"本机或局域网里的另一台机器"，所以：
    ///   - loopback 一律允许明文 http（本机 ComfyUI 默认就是 `http://127.0.0.1:8188`）；
    ///   - **局域网地址要连接里显式 `config.allowPrivateNetwork = true`**。
    ///
    /// 默认关是刻意的：那是 SSRF 守卫，放开等于允许 daemon 去打内网任意地址，
    /// 而连接的 URL 可能被"导入的应用 / 工作流"间接影响。要放开就一条连接一条连接地放。
    fn network_policy(connection: &ConnectionRevision) -> NetworkPolicy {
        if connection
            .config
            .get("allowPrivateNetwork")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            NetworkPolicy::LOCAL_HTTP_AND_PRIVATE
        } else {
            NetworkPolicy::LOCAL_HTTP
        }
    }

    pub fn new(secrets: Box<dyn SecretStore>) -> Self {
        // 本机 ComfyUI 由代理判据**永久直连**（回环地址），云端 rh-proxy 才会走代理；
        // 内网/局域网那类由系统的绕过列表（`ProxyOverride`）决定，与浏览器同一套语义。
        let client = super::proxy::configure(Client::builder())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { client, secrets }
    }

    /// 解析最终可访问的 Base URL。
    /// - standard：直接用 config.baseUrl
    /// - rh-proxy：config.baseUrl 存 `https://host/proxy`（不含 key），
    ///   提交时从 keyring 取 key，拼成 `{base}/proxy/{key}`，key 不落 SQLite。
    async fn base_url(&self, connection: &ConnectionRevision) -> DomainResult<Url> {
        let raw = connection
            .config
            .get("baseUrl")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| SafeError::validation("CONNECTION_CONFIG_INVALID", "缺少 baseUrl"))?;
        let mode = connection
            .config
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("standard");
        let mut url = validate_base_url(raw, Self::network_policy(connection))?;
        if mode == "rh-proxy" {
            let key = self.api_key(connection).await?.ok_or_else(|| {
                SafeError::provider(
                    "COMFYUI_PROXY_KEY_MISSING",
                    "RunningHub 云端 ComfyUI 需要 API Key",
                )
            })?;
            // 期望 baseUrl 形如 https://host （或 https://host/proxy）：
            // 若无 /proxy 段则补上，再追加 /{key} 得到最终云端 ComfyUI 地址。
            let path = url.path().to_string();
            let key_suffix = format!("/{key}");
            if path == "/" {
                url.set_path(&format!("/proxy{key_suffix}"));
            } else if !path.ends_with(&key_suffix) {
                let mut new_path = path.trim_end_matches('/').to_string();
                if !new_path.ends_with("/proxy") {
                    new_path.push_str("/proxy");
                }
                new_path.push_str(&key_suffix);
                url.set_path(&new_path);
            }
        }
        Ok(url)
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

    fn endpoint(base: &Url, path: &str) -> Result<Url, SafeError> {
        join_path(base, path)
    }

    fn history_endpoint(base: &Url, prompt_id: &str) -> Result<Url, SafeError> {
        let mut url = Self::endpoint(base, "history")?;
        url.path_segments_mut()
            .map_err(|_| SafeError::validation("PROVIDER_URL_INVALID", "ComfyUI URL 无效"))?
            .push(prompt_id);
        Ok(url)
    }

    fn authorize(
        &self,
        builder: reqwest::RequestBuilder,
        key: Option<String>,
    ) -> reqwest::RequestBuilder {
        match key {
            Some(key) if !key.trim().is_empty() => builder.bearer_auth(key),
            _ => builder,
        }
    }

    fn workflow(&self, request: &PrepareRequest) -> DomainResult<Value> {
        let workflow = request
            .recipe_binding
            .provider_config
            .get("workflow")
            .cloned()
            .ok_or_else(|| {
                SafeError::validation("BINDING_WORKFLOW_REQUIRED", "Recipe 绑定缺少 workflow")
            })?;
        let mut workflow = workflow;
        let object = workflow.as_object_mut().ok_or_else(|| {
            SafeError::validation("BINDING_WORKFLOW_INVALID", "workflow 必须是对象")
        })?;
        let mapping = request
            .recipe_binding
            .provider_config
            .get("fieldMapping")
            .and_then(Value::as_object);
        for (input_key, value) in &request.inputs {
            // 优先使用 fieldMapping：{ inputKey: { "node": "6", "field": "text" } }
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
            // 回退：在所有节点中填充同名字段
            for node in object.values_mut() {
                let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) else {
                    continue;
                };
                if inputs.contains_key(input_key) {
                    inputs.insert(input_key.clone(), value.clone());
                }
            }
        }
        Ok(workflow)
    }

    /// 从 /history 失败节点提取结构化错误：message + 节点/异常类型等细节。
    fn failure(entry: &Value) -> SafeError {
        let mut details = BTreeMap::new();
        // ComfyUI 格式：[type, { "message": "...", "node_type": "...", "exception_type": "..." }]
        if let Some(Value::Array(values)) = entry.pointer("/status/messages/0") {
            for value in values {
                if let Some(message) = value.get("message").and_then(Value::as_str) {
                    details.insert("message".to_string(), json!(message));
                }
                for key in ["node_type", "exception_type", "exception_message"] {
                    if let Some(value) = value.get(key) {
                        details.insert(key.to_string(), value.clone());
                    }
                }
            }
        }
        let message = details
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| match entry.get("error") {
                Some(Value::String(message)) if !message.trim().is_empty() => Some(message.clone()),
                Some(message) => Some(message.to_string()),
                None => None,
            })
            .unwrap_or_else(|| "ComfyUI 任务失败".to_string());
        SafeError::provider("PROVIDER_TASK_FAILED", message).with_details(details)
    }

    fn history_entry<'a>(history: &'a Value, prompt_id: &str) -> &'a Value {
        history.get(prompt_id).unwrap_or_else(|| {
            if history.get("status").is_some() || history.get("outputs").is_some() {
                history
            } else {
                &Value::Null
            }
        })
    }

    /// 队列里有没有这个 prompt_id？有就报"还在排队/在跑"，没有就 `None`。
    ///
    /// 为什么必须问这一句：ComfyUI 只在任务**执行完**才写 `/history`，排队中与运行中的都在
    /// `/queue`（`queue_running` + `queue_pending`，元素是队列项元组序列化成的数组：
    /// 下标 1 是 prompt_id、下标 3 是 extra_data）。只看 `/history` 会把"平台正在跑"读成
    /// "平台没收到"——而这两者的处理完全相反。
    ///
    /// 判据只用 **prompt_id**（我们自己定的那个），不靠 client_id：client_id 在服务端是全局字段，
    /// 并发提交会互相覆盖，拿它当身份会认错任务。
    async fn queue_snapshot(
        &self,
        base: &Url,
        prompt_id: &str,
        key: Option<String>,
    ) -> DomainResult<Option<TaskSnapshot>> {
        let url = Self::endpoint(base, "queue")?;
        let response = self
            .authorize(self.client.get(url.clone()), key)
            .send()
            .await
            .map_err(|error| map_network_error(&error, "comfyui_queue"))?;
        if !response.status().is_success() {
            return Err(map_http_error(
                response.status().as_u16(),
                "ComfyUI 队列查询失败",
            ));
        }
        let queue: Value = serde_json::from_slice(&read_limited(response, 4 * 1024 * 1024).await?)
            .map_err(|_| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "队列响应不是有效 JSON")
            })?;
        for (field, status) in [
            ("queue_running", TaskStatus::Running),
            ("queue_pending", TaskStatus::Queued),
        ] {
            let Some(items) = queue.get(field).and_then(Value::as_array) else {
                continue;
            };
            let found = items
                .iter()
                .any(|item| item.get(1).and_then(Value::as_str) == Some(prompt_id));
            if found {
                return Ok(Some(TaskSnapshot {
                    status,
                    results: vec![],
                    error: None,
                    usage: None,
                }));
            }
        }
        Ok(None)
    }

    fn view_url(base: &Url, image: &Value) -> Result<Url, SafeError> {
        let filename = image
            .get("filename")
            .and_then(Value::as_str)
            .ok_or_else(|| SafeError::validation("PROVIDER_RESULT_INVALID", "结果缺少文件名"))?;
        let mut url = Self::endpoint(base, "view")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("filename", filename);
            if let Some(subfolder) = image.get("subfolder").and_then(Value::as_str) {
                query.append_pair("subfolder", subfolder);
            }
            if let Some(kind) = image.get("type").and_then(Value::as_str) {
                query.append_pair("type", kind);
            }
        }
        Ok(url)
    }

    async fn fetch_view(
        &self,
        base: &Url,
        image: &Value,
        key: Option<String>,
        allowed_path: &str,
        allow_redirect: bool,
    ) -> Result<ArtifactDraft, SafeError> {
        let url = Self::view_url(base, image)?;
        let checked = validate_same_origin(base, url.as_str(), allowed_path)?;
        let response = self
            .authorize(self.client.get(checked.clone()), key)
            .send()
            .await
            .map_err(|error| map_network_error(&error, "comfyui_view"))?;
        // standard 模式：禁止重定向，防止结果跳转到任意地址。
        if !allow_redirect && response.url() != &checked {
            return Err(SafeError::validation(
                "PROVIDER_RESULT_REDIRECTED",
                "ComfyUI 结果下载发生重定向",
            ));
        }
        let bytes = if response.status() == reqwest::StatusCode::FOUND
            || response.status() == reqwest::StatusCode::MOVED_PERMANENTLY
        {
            // rh-proxy：/view 会 302/301 到真实 CDN 图片地址，跟随该地址下载。
            if !allow_redirect {
                return Err(map_http_error(
                    response.status().as_u16(),
                    "ComfyUI 结果下载失败",
                ));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    SafeError::validation("PROVIDER_RESULT_INVALID", "重定向响应缺少 Location")
                })?;
            let location_url = reqwest::Url::parse(location).map_err(|_| {
                SafeError::validation("PROVIDER_RESULT_INVALID", "重定向 Location 无效")
            })?;
            // 有界多跳跟随：CDN 链路可能连续 302（如代理→签名 URL→区域节点）。
            // 不带凭据（client 无 Authorization），上限 5 跳防重定向环。
            let mut current = location_url;
            let mut downloaded: Option<Vec<u8>> = None;
            for _hop in 0..5 {
                let redirected = self
                    .client
                    .get(current.clone())
                    .send()
                    .await
                    .map_err(|error| map_network_error(&error, "comfyui_view_redirect"))?;
                if redirected.status().is_success() {
                    downloaded = Some(read_limited(redirected, MAX_RESULT_BYTES).await?);
                    break;
                }
                if matches!(
                    redirected.status(),
                    reqwest::StatusCode::FOUND
                        | reqwest::StatusCode::MOVED_PERMANENTLY
                        | reqwest::StatusCode::SEE_OTHER
                        | reqwest::StatusCode::TEMPORARY_REDIRECT
                        | reqwest::StatusCode::PERMANENT_REDIRECT
                ) {
                    let next = redirected
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| {
                            SafeError::validation(
                                "PROVIDER_RESULT_INVALID",
                                "重定向响应缺少 Location",
                            )
                        })?;
                    current = redirected.url().join(next).map_err(|_| {
                        SafeError::validation("PROVIDER_RESULT_INVALID", "重定向 Location 无效")
                    })?;
                    continue;
                }
                return Err(map_http_error(
                    redirected.status().as_u16(),
                    "ComfyUI 结果下载失败",
                ));
            }
            match downloaded {
                Some(bytes) => bytes,
                None => {
                    return Err(SafeError::validation(
                        "PROVIDER_RESULT_REDIRECTED",
                        "ComfyUI 结果下载重定向次数过多",
                    ))
                }
            }
        } else {
            if !response.status().is_success() {
                return Err(map_http_error(
                    response.status().as_u16(),
                    "ComfyUI 结果下载失败",
                ));
            }
            read_limited(response, MAX_RESULT_BYTES).await?
        };
        let filename = image
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or("output.png")
            .to_string();
        Ok(ArtifactDraft {
            filename,
            mime_type: "image/png".to_string(),
            byte_size: bytes.len() as u64,
            checksum: format!("sha1-{:x}", simple_checksum(&bytes)),
            data: ArtifactData::Bytes(bytes),
        })
    }

    async fn upload_asset(
        &self,
        base: &Url,
        asset: &crate::v2::ports::provider::ResolvedAsset,
        key: Option<String>,
    ) -> Result<String, SafeError> {
        let filename = asset.asset_ref.asset_id.0.to_string();
        let url = Self::endpoint(base, "upload/image")?;
        let part = multipart::Part::bytes(asset.bytes.clone()).file_name(filename);
        let response = self
            .authorize(
                self.client
                    .post(url.clone())
                    .multipart(multipart::Form::new().part("image", part)),
                key,
            )
            .send()
            .await
            .map_err(|error| map_network_error(&error, "comfyui_upload"))?;
        if response.url() != &url {
            return Err(SafeError::validation(
                "PROVIDER_UPLOAD_REDIRECTED",
                "ComfyUI 上传发生重定向",
            ));
        }
        if !response.status().is_success() {
            return Err(map_http_error(
                response.status().as_u16(),
                "ComfyUI 上传失败",
            ));
        }
        let uploaded: Value = serde_json::from_slice(&read_limited(response, 1024 * 1024).await?)
            .map_err(|_| {
            SafeError::validation("PROVIDER_RESPONSE_INVALID", "上传响应不是有效 JSON")
        })?;
        let name = uploaded
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && !name.contains('/') && !name.contains('\\'))
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_UPLOAD_INVALID", "ComfyUI 上传响应缺少有效文件名")
            })?;
        let subfolder = uploaded
            .get("subfolder")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("");
        Ok(if subfolder.is_empty() {
            name.to_string()
        } else {
            format!("{subfolder}/{name}")
        })
    }

    /// 把上传好的资产按**字段键**写进 workflow 对应节点的输入。
    ///
    /// ⚠️ 不再扫图找 `LoadImage` / `asset://N`：资产的身份是字段键（`MediaMapping::bind`），
    /// 写入点是映射表里写明的 (node, field)。旧的位置数组在这个入口之前就已经被换算成
    /// 字段键——"按位置猜"正是错位事故的来源。
    fn apply_media_uploads(
        workflow: &mut Value,
        bindings: &[crate::v2::domain::media::BoundAsset<'_>],
        uploads: &[String],
    ) -> DomainResult<()> {
        for (binding, upload) in bindings.iter().zip(uploads.iter()) {
            let node = workflow.get_mut(&binding.slot.node).ok_or_else(|| {
                SafeError::validation(
                    "MEDIA_MAPPING_INVALID",
                    format!(
                        "媒体映射指向的节点 `{}` 在这份 workflow 里不存在（字段键 `{}`）。\
                         这通常是 revision 与 workflow 漂移了：按平台上最新的图重新导入这个工作流。",
                        binding.slot.node, binding.field_key
                    ),
                )
            })?;
            let inputs = node
                .get_mut("inputs")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    SafeError::validation(
                        "MEDIA_MAPPING_INVALID",
                        format!(
                            "媒体映射指向的节点 `{}` 没有 inputs 对象（字段键 `{}`）。",
                            binding.slot.node, binding.field_key
                        ),
                    )
                })?;
            inputs.insert(binding.slot.field.clone(), json!(upload));
        }
        Ok(())
    }

    /// **兼容分支**：workflow 里用 `asset://N` 占位的历史写法（我们自己的导入不产生它，
    /// 但手写/历史配方可能有）。`N` 是请求里第 N 个资产（位置只是这里的旧编码）。请求里
    /// 没有第 N 个资产时**报错**，不把 `asset://N` 这种垃圾值静默发给引擎。
    fn replace_asset_scheme_refs(workflow: &mut Value, uploads: &[String]) -> DomainResult<()> {
        let Some(object) = workflow.as_object_mut() else {
            return Ok(());
        };
        for (node_id, node) in object.iter_mut() {
            let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) else {
                continue;
            };
            for (field, value) in inputs.iter_mut() {
                let index = value
                    .as_str()
                    .and_then(|text| text.strip_prefix("asset://"))
                    .and_then(|index| index.parse::<usize>().ok());
                let Some(index) = index else {
                    continue;
                };
                let Some(upload) = uploads.get(index) else {
                    return Err(SafeError::validation(
                        "ASSET_FIELD_UNKNOWN",
                        format!(
                            "workflow 的节点 `{node_id}` 输入 `{field}` 引用了 `asset://{index}`，\
                             但请求只带了 {} 个资产。补上这个资产再提交。",
                            uploads.len()
                        ),
                    ));
                };
                *value = json!(upload);
            }
        }
        Ok(())
    }
}

/// 一句话说清"这个响应为什么不是节点定义"。
///
/// 只放**形状**，不放内容（可能很长、也可能是平台的错误文案）：顶层键的名字就够判断了——
/// 是 `{"KSampler": {…}}`（对）、还是 `{"code":…,"msg":…}`（平台的错误信封）、
/// 还是 `{"detail":"Not Found"}`（网关的 404 文案）。
/// **提交结果不明**（发送后超时/断连，或 5xx）⇒ 用**我们自己定的 `prompt_id`** 报一个
/// "不确定 + 可按 id 回查"。
///
/// 为什么两条路共用这一个函数：网络失败与 5xx 在这里的**处理必须一模一样** ——
/// 都是"平台可能已经收下"，都该去问同一个 `prompt_id`。各写一份的话，早晚只改一处
/// （这个仓库已经因为"判据各写一份"踩过一次：库里的状态与计数各说各话）。
fn uncertain_with_lookup(error: &SafeError, request_id: &str) -> SubmitOutcome {
    SubmitOutcome::Uncertain(UncertainSubmission {
        reason: error.message.clone(),
        recovery: RecoveryProof::LookupByRequestId(request_id.to_string()),
    })
}

fn describe_shape(parsed: &Value, wanted: &str) -> String {
    match parsed.as_object() {
        Some(object) if object.is_empty() => "响应是个空对象".to_string(),
        Some(object) => {
            let keys: Vec<&str> = object.keys().take(6).map(String::as_str).collect();
            format!("响应里没有 {wanted}，顶层键是 {}", keys.join("、"))
        }
        None => "响应不是对象".to_string(),
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

/// 我们自己给 ComfyUI 任务定的 job id（提交体里的 `prompt_id`）：**规范小写带连字符的 UUID**。
///
/// 为什么要自己定：ComfyUI 收客户端传来的 `prompt_id` 并**原样采用**，于是"提交响应丢了"之后
/// 我们**知道该去查哪个 id** —— `GET /history/{prompt_id}`（排队中/运行中在 `GET /queue`）
/// 能回答"平台到底收没收到"。这是四个驱动里**唯一**存在这种抓手的地方（RunningHub 的创建接口
/// 没有任何客户端标识字段、所有查询只认它自己发的 taskId；OpenAI 图像 API 连响应都没有 id）。
///
/// ⚠️ 形状必须**规范**：服务端对不合规的值直接 400 `invalid_prompt_id`
/// （"must be a UUID string in canonical lowercase hyphenated form"），所以这不是"随便一个 id"。
/// 这里按 UUIDv5 的形状来：sha256 取前 16 字节，再按 UUID 规范写版本位（5 = 名字型）与变体位。
/// 从指纹派生 ⇒ **同一次提交永远同一个 id**（重投是新 attempt ⇒ 新指纹 ⇒ 新 id，不会串）。
///
/// 负对照：把版本位那行去掉 ⇒ `comfy_job_id_is_a_canonical_uuid` 会红（形状不再合规）。
fn comfy_job_id(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[async_trait]
impl ProviderDriver for ComfyUiDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        descriptor()
    }

    /// 逐个类拉 `GET /object_info/<class>`。
    ///
    /// 为什么按类取而不是拉一次全量：全量在装了几百个自定义节点的机器上是**几 MB**，
    /// 而我们只要用到的那几个类（一张图通常 5~15 个）。`/object_info/<class>` 是 ComfyUI
    /// 支持的形态，一个类一个请求，各自很小。
    ///
    /// 语义（与端口注释一致）：**部分成功算成功**——某个自定义节点没装，那个类缺席、其余照用；
    /// 一个都没拿到才返回 `Err`，调用方据此降级成通用控件。
    async fn fetch_node_info(
        &self,
        connection: &ConnectionRevision,
        class_types: &[String],
    ) -> DomainResult<NodeInfoCatalog> {
        const MAX_CLASSES: usize = 60;
        const MAX_BYTES: usize = 4 * 1024 * 1024;
        let base = self.base_url(connection).await?;
        let key = self.api_key(connection).await?;
        let mut catalog = NodeInfoCatalog::default();
        let mut first_error: Option<SafeError> = None;
        // 二级线索：响应是**合法 JSON 却不含那个类**时，把"它到底回了什么"记下来。
        // 没有这一条，失败原因只能是一句"引擎没有返回任何节点定义"——而排查要看的是原文
        // （2026-10-14 实测：某个云端网关回 200 + 合法 JSON，但顶层键根本不是类名，
        //  光看笼统的原因根本判断不出是"没这个节点"还是"这条路根本不通"）。
        let mut unexpected_shape: Option<String> = None;
        let mut succeeded = 0usize;
        for class_type in class_types.iter().take(MAX_CLASSES) {
            // 类名进 URL 路径：先做一次 URL 编码，免得自定义节点名里的空格/斜杠把路径撕开。
            let encoded = urlencoding_minimal(class_type);
            let url = match Self::endpoint(&base, &format!("object_info/{encoded}")) {
                Ok(url) => url,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            let response = match self
                .authorize(self.client.get(url), key.clone())
                // 探定义不是探活：给 15 秒（大节点包解析慢），但不给客户端那 120 秒。
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    first_error
                        .get_or_insert_with(|| map_network_error(&error, "comfyui_object_info"));
                    continue;
                }
            };
            if !response.status().is_success() {
                first_error.get_or_insert_with(|| {
                    map_http_error(response.status().as_u16(), "ComfyUI 获取节点定义失败")
                });
                continue;
            }
            let bytes = match read_limited(response, MAX_BYTES).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
                first_error.get_or_insert_with(|| {
                    SafeError::validation("PROVIDER_RESPONSE_INVALID", "节点定义不是有效 JSON")
                });
                continue;
            };
            // 回的是 `{"<class>": {...}}`（取单个类时也只有这一个键）。
            let Some(info) = parsed.get(class_type).and_then(parse_node_class) else {
                // 引擎不认识这个类（自定义节点没装）——不是错误，跳过即可。
                // 但**要看一眼它回的是什么形状**：如果压根不是"类名 → 定义"的形状，
                // 那就不是"节点没装"，而是"这条路不通"（网关回了个别的 JSON）。
                if unexpected_shape.is_none() {
                    unexpected_shape = Some(describe_shape(&parsed, class_type));
                }
                continue;
            };
            catalog.classes.insert(class_type.clone(), info);
            succeeded += 1;
        }
        if succeeded == 0 {
            if let Some(error) = first_error {
                return Err(error);
            }
            // 每个类都拿到了 200 + 合法 JSON，但没有一个是"类名 → 定义"的形状。
            let detail = unexpected_shape.unwrap_or_else(|| "响应里没有任何键".to_string());
            return Err(SafeError::validation(
                "NODE_INFO_EMPTY",
                format!("引擎没有返回这些节点的定义（{detail}）"),
            ));
        }
        Ok(catalog)
    }

    async fn validate_connection(
        &self,
        connection: &ConnectionRevision,
    ) -> DomainResult<ConnectionCheck> {
        let base = self.base_url(connection).await?;
        validate_resolved_url(&base, Self::network_policy(connection)).await?;
        /*
         * 真的探一次。以前这里只校验 URL 就无条件 `ok: true`：本机 ComfyUI 根本没起，
         * 界面照样显示「已配置」+ 绿点，一直到提交任务才炸 —— 而 RunningHub 那边早就
         * 写着这条规矩：「连接测试必须验证一次真实的认证请求，而不只是检查 URL 和 keyring」。
         *
         * `/system_stats` 是 ComfyUI 的公开端点，本仓库的 live 全链路测试
         * （`live_comfyui_full_chain`）也用它探活。超时给 5 秒，不用客户端那 120 秒 ——
         * 探活卡两分钟比探活失败更糟。
         */
        let url = Self::endpoint(&base, "system_stats")?;
        let key = self.api_key(connection).await?;
        let response = self
            .authorize(self.client.get(url), key)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|error| map_network_error(&error, "comfyui_status"))?;
        if !response.status().is_success() {
            return Ok(ConnectionCheck {
                ok: false,
                capabilities: connection.effective_capabilities.clone(),
                message: Some(format!(
                    "ComfyUI /system_stats 返回 HTTP {}",
                    response.status().as_u16()
                )),
            });
        }
        Ok(ConnectionCheck {
            ok: true,
            capabilities: connection.effective_capabilities.clone(),
            message: None,
        })
    }

    async fn prepare(&self, request: PrepareRequest) -> DomainResult<PreparedExecution> {
        let base = self.base_url(&request.connection_revision).await?;
        validate_resolved_url(&base, Self::network_policy(&request.connection_revision)).await?;
        let key = self.api_key(&request.connection_revision).await?;
        let mut workflow = self.workflow(&request)?;
        if !request.assets.is_empty() {
            // 资产按**字段键**绑定（旧位置数组也在这一处被换算成字段键，见 `MediaMapping::bind`）。
            let mapping = crate::v2::domain::media::MediaMapping::from_provider_config(
                &request.recipe_binding.provider_config,
            )?;
            let refs = request
                .assets
                .iter()
                .map(|resolved| resolved.asset_ref.clone())
                .collect::<Vec<_>>();
            let bindings = mapping.bind(&refs)?;
            let mut uploads = Vec::with_capacity(bindings.len());
            for (index, _binding) in bindings.iter().enumerate() {
                uploads.push(
                    self.upload_asset(&base, &request.assets[index], key.clone())
                        .await?,
                );
            }
            Self::apply_media_uploads(&mut workflow, &bindings, &uploads)?;
            // 兼容：图里以 `asset://N` 占位的历史写法（见函数注释）。
            Self::replace_asset_scheme_refs(&mut workflow, &uploads)?;
        }
        let request_id = format!(
            "{}-{:x}",
            request.request_fingerprint,
            simple_checksum(&serde_json::to_vec(&workflow).unwrap_or_default(),)
        );
        // 提交时**我们自己定 job id**：ComfyUI 收客户端传来的 `prompt_id` 并原样采用
        // （官方示例就是这么用的），于是"提交响应丢了"之后我们**知道该去查哪个 id**
        // ——`GET /history/{prompt_id}`，排队中/运行中在 `GET /queue`。
        //
        // 这是四个驱动里**唯一**存在这种抓手的地方（RunningHub / OpenAI 图像 / 通用异步平台
        // 都没有客户端幂等键，见 `docs/agent-cookbook.md` §11d）。所以它值得一次专门的改造：
        // 没有它，"提交结果不明"只能等到死线（默认 1 小时）判失败——而平台上那份可能早就跑完了。
        let prompt_id = comfy_job_id(&request_id);
        Ok(PreparedExecution {
            connection_revision: request.connection_revision,
            provider_request: json!({
                "prompt": workflow,
                "prompt_id": prompt_id,
                // `client_id` 在服务端是**全局**字段（websocket 路由用），并发提交会互相覆盖
                // ⇒ 每次提交给一个唯一值。与 prompt_id 同源即可：一次提交只有一个。
                "client_id": prompt_id,
            }),
            request_id: prompt_id,
        })
    }

    async fn submit(&self, execution: PreparedExecution) -> DomainResult<SubmitOutcome> {
        let base = self.base_url(&execution.connection_revision).await?;
        let url = Self::endpoint(&base, "prompt")?;
        let key = self.api_key(&execution.connection_revision).await?;
        let sent = self
            .authorize(
                self.client
                    .post(url.clone())
                    .json(&execution.provider_request)
                    // 提交是**能合法地慢**的那一类（巨大的 workflow graph、远端/自建机器在
                    // 冷启动）：按连接的配置给预算，没配就是今天这个 120 秒。
                    // 探活（`/system_stats`，5 秒）刻意不跟这个值——探活卡两分钟比探活失败更糟。
                    .timeout(crate::v2::domain::connection_options::request_timeout(
                        &execution.connection_revision.config,
                        crate::v2::domain::connection_options::DEFAULT_REQUEST_TIMEOUT_SECS,
                    )),
                key,
            )
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_network_error(&error, "comfyui_submit");
                if !mapped.submission_is_unknown() {
                    // 两种情况都**原样**把错误交回去，语义不变：
                    // ① 证明得了"字节没出去"（连接被拒 / DNS 失败 / 握手超时）⇒ scheduler 走
                    //    退避重试（30 秒后自己再来，不可能重复计费）；
                    // ② 压根不是网络类失败（如被包成 validation 的 4xx）。
                    return Err(mapped);
                }
                // 证明不了 ⇒ **平台可能已经收下了**。这时"我们提交时自己定的 prompt_id"
                // 就是抓手：报一个"不确定 + 可按 id 回查"，于是 run 拿到远端句柄、进对账队列
                // ——查得到就正常收尾（**不再白等 1 小时判失败**），查不到就等到死线
                // （与改造前一样，只多了一次廉价的查询）。
                //
                // ⚠️ 这是 `RecoveryProof::LookupByRequestId` 在**生产代码里的第一个生产者**
                // （此前只有测试在造 `SubmitOutcome::Uncertain`，所以 scheduler 里那条
                // `ToRetryWait` 分支不可达）。
                return Ok(uncertain_with_lookup(&mapped, &execution.request_id));
            }
        };
        if response.url() != &url {
            return Err(SafeError::validation(
                "PROVIDER_REQUEST_REDIRECTED",
                "ComfyUI 提交发生重定向",
            ));
        }
        if !response.status().is_success() {
            // ⚠️ 提交路径专用：**5xx 是"平台可能已经收下"**（500 可能是网关/内部错误发生在
            // 受理之后）⇒ 与上面那条网络失败**同一条处理**：用我们自己定的 `prompt_id`
            // 报"不确定 + 可按 id 回查"，查得到就正常收尾（几秒），查不到才等到死线。
            // 4xx 不受影响（明确拒绝 ⇒ 交回去重试）。
            let mapped = map_submit_http_error(response.status().as_u16(), "ComfyUI 提交失败");
            if !mapped.submission_is_unknown() {
                return Err(mapped);
            }
            return Ok(uncertain_with_lookup(&mapped, &execution.request_id));
        }
        let parsed: Value = serde_json::from_slice(&read_limited(response, 1024 * 1024).await?)
            .map_err(|_| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "提交响应不是有效 JSON")
            })?;
        let prompt_id = parsed
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SafeError::validation("PROVIDER_RESPONSE_INVALID", "ComfyUI 响应缺少 prompt_id")
            })?;
        Ok(SubmitOutcome::Accepted(RemoteHandle {
            provider: "comfyui".to_string(),
            remote_id: prompt_id.to_string(),
        }))
    }

    async fn reconcile(&self, request: ReconcileRequest) -> DomainResult<TaskSnapshot> {
        let base = self.base_url(&request.connection_revision).await?;
        let url = Self::history_endpoint(&base, &request.remote_handle.remote_id)?;
        let key = self.api_key(&request.connection_revision).await?;
        let response = self
            .authorize(self.client.get(url.clone()), key.clone())
            .send()
            .await
            .map_err(|error| map_network_error(&error, "comfyui_history"))?;
        if !response.status().is_success() {
            return Err(map_http_error(
                response.status().as_u16(),
                "ComfyUI 查询失败",
            ));
        }
        let history: Value = serde_json::from_slice(
            &read_limited(response, 4 * 1024 * 1024).await?,
        )
        .map_err(|_| SafeError::validation("PROVIDER_RESPONSE_INVALID", "查询响应不是有效 JSON"))?;
        let entry = Self::history_entry(&history, &request.remote_handle.remote_id);
        if entry.is_null() {
            // ⚠️ **历史里没有 ≠ 平台没收到**（2026-10-18 补）：ComfyUI 只在任务**执行完**才写
            // `/history`，排队中与运行中的都在 `/queue`；而且 history 是内存态（上限 10000 条、
            // 可被清空、重启即丢）。所以"历史里没有"必须先问一次队列，否则"提交响应丢了但平台
            // 收下了"的任务会在队列里跑着、我们这边却一路 Unknown 到死线判失败——那正是这次
            // 改造要消灭的假失败。
            if let Some(snapshot) = self
                .queue_snapshot(&base, &request.remote_handle.remote_id, key.clone())
                .await?
            {
                return Ok(snapshot);
            }
            return Ok(TaskSnapshot {
                status: TaskStatus::Unknown,
                results: vec![],
                error: None,
                usage: None,
            });
        }
        // ComfyUI 0.32+ 使用 status_str；旧版本使用 status.status
        let status = entry
            .pointer("/status/status_str")
            .or_else(|| entry.pointer("/status/status"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        match status {
            "success" | "completed" => {
                let mut results = Vec::new();
                if let Some(outputs) = entry.get("outputs").and_then(Value::as_object) {
                    for output in outputs.values() {
                        let Some(images) = output.get("images").and_then(Value::as_array) else {
                            continue;
                        };
                        for image in images {
                            // rh-proxy 模式下结果 URL 位于 /proxy/{key}/view，
                            // 校验前缀用 /proxy/ 而非 /view（同源校验仍生效，只放宽路径前缀）。
                            let rh_proxy = request
                                .connection_revision
                                .config
                                .get("mode")
                                .and_then(Value::as_str)
                                == Some("rh-proxy");
                            let allowed_path = if rh_proxy { "/proxy/" } else { "/view" };
                            results.push(
                                self.fetch_view(&base, image, key.clone(), allowed_path, rh_proxy)
                                    .await?,
                            );
                        }
                    }
                }
                Ok(TaskSnapshot {
                    status: TaskStatus::Succeeded,
                    results,
                    error: None,
                    usage: None,
                })
            }
            "error" | "failed" | "cancelled" => Ok(TaskSnapshot {
                status: TaskStatus::Failed,
                results: vec![],
                error: Some(Self::failure(entry)),
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::ids::{AssetId, ConnectionId, ConnectionRevisionId};
    use crate::v2::domain::media::AssetRef;
    use crate::v2::domain::model::{RecipeBinding, SecretBinding, Timestamp};
    use crate::v2::ports::provider::ResolvedAsset;
    use crate::v2::ports::stores::SecretValue;
    use axum::{
        body::Body,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
        Router,
    };
    use std::collections::BTreeMap;
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

    /// `/object_info/<class>` 的形态 → 规格表。
    ///
    /// 这个解析是"控件从通用变真"的唯一入口：枚举的**全部取值**、数字的范围、多行标记
    /// 都在这里被读出来。所以形状的每一种都要钉住。
    #[test]
    fn parses_object_info_into_input_specs() {
        let info = parse_node_class(&json!({
            "display_name": "KSampler",
            "input": {
                "required": {
                    "seed": ["INT", { "default": 0, "min": 0, "max": 18446744073709551615u64 }],
                    "steps": ["INT", { "default": 20, "min": 1, "max": 100, "step": 1 }],
                    "cfg": ["FLOAT", { "default": 8.0, "min": 0.0, "max": 100.0 }],
                    "sampler_name": [["euler", "dpmpp_2m"]],
                    "denoise": ["FLOAT", { "default": 1.0 }]
                },
                "optional": {
                    "model": ["MODEL"],
                    "text": ["STRING", { "multiline": true }]
                }
            }
        }))
        .expect("应能解析");

        assert_eq!(info.display_name, "KSampler");
        // 枚举：候选数组就是全部合法取值。
        assert_eq!(info.inputs["sampler_name"].kind, "COMBO");
        assert_eq!(
            info.inputs["sampler_name"].choices,
            vec!["euler", "dpmpp_2m"]
        );
        // 数字：范围与步长（seed 的上限是 u64 最大值那种**哨兵**，原样透传，不替界面改数）。
        assert_eq!(info.inputs["steps"].min, Some(1.0));
        assert_eq!(info.inputs["steps"].max, Some(100.0));
        assert_eq!(info.inputs["steps"].step, Some(1.0));
        assert_eq!(info.inputs["steps"].default, Some(json!(20)));
        assert_eq!(info.inputs["denoise"].min, None);
        // 多行标记。
        assert!(info.inputs["text"].multiline);
        // `optional` 里的输入也要收进来（它们同样可以出现在 prompt 里）。
        assert_eq!(info.inputs["model"].kind, "MODEL");
        assert!(!info.inputs["model"].multiline);
    }

    /// 反例：不是节点定义形状的东西不许panic，也不许编出一个空的规格。
    #[test]
    fn object_info_parser_rejects_non_definitions() {
        assert!(parse_node_class(&json!("KSampler")).is_none());
        assert!(parse_node_class(&json!({ "input": { "required": {} } })).is_some());
        // 一个输入既不是数组也没有类型名 ⇒ 跳过它，但不影响其它输入。
        let info = parse_node_class(&json!({
            "input": { "required": { "broken": 42, "good": ["INT"] } }
        }))
        .expect("应能解析");
        assert!(!info.inputs.contains_key("broken"));
        assert_eq!(info.inputs["good"].kind, "INT");
    }

    /// 类名里混进路径分隔符时，编出来的必须是一个**不存在的类**的请求，而不是别的地方。
    #[test]
    fn class_names_are_escaped_into_the_path() {
        assert_eq!(urlencoding_minimal("KSampler"), "KSampler");
        assert_eq!(urlencoding_minimal("My Node"), "My%20Node");
        assert_eq!(urlencoding_minimal("a/b"), "a%2Fb");
        assert_eq!(urlencoding_minimal("../secrets"), "..%2Fsecrets");
    }

    struct KeySecrets(&'static str);
    #[async_trait]
    impl SecretStore for KeySecrets {
        async fn resolve(&self, _binding: &SecretBinding) -> DomainResult<Option<SecretValue>> {
            Ok(Some(crate::v2::ports::stores::SecretValue {
                secret: self.0.to_string(),
            }))
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
            config: BTreeMap::from([("baseUrl".to_string(), json!(base_url))]),
            secret_bindings: vec![],
            effective_capabilities: vec!["workflow-execution".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp::now(),
        }
    }

    fn rh_proxy_connection() -> ConnectionRevision {
        ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(1),
            config: BTreeMap::from([
                (
                    "baseUrl".to_string(),
                    json!("https://www.runninghub.cn/proxy"),
                ),
                ("mode".to_string(), json!("rh-proxy")),
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

    #[tokio::test]
    async fn rh_proxy_base_url_joins_keyring_key() {
        let driver = ComfyUiDriver::new(Box::new(KeySecrets("secret-key-123")));
        let connection = rh_proxy_connection();
        let url = driver.base_url(&connection).await.expect("应可拼接");
        assert_eq!(
            url.as_str(),
            "https://www.runninghub.cn/proxy/secret-key-123"
        );
    }

    #[tokio::test]
    async fn standard_base_url_unaffected() {
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let connection = connection("http://127.0.0.1:8188".to_string());
        let url = driver.base_url(&connection).await.expect("应可解析");
        assert_eq!(url.as_str(), "http://127.0.0.1:8188/");
    }

    fn request(connection: ConnectionRevision, workflow: Value) -> PrepareRequest {
        PrepareRequest {
            connection_revision: connection,
            recipe_binding: RecipeBinding {
                driver_key: "comfyui".to_string(),
                provider_config: BTreeMap::from([
                    ("workflow".to_string(), workflow),
                    (
                        "fieldMapping".to_string(),
                        json!({ "positive": { "node": "6", "field": "text" } }),
                    ),
                ]),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: crate::v2::domain::model::ConnectionOverride::Denied,
            },
            inputs: BTreeMap::from([("positive".to_string(), json!("a red house"))]),
            assets: vec![],
            request_fingerprint: "fp-1".to_string(),
        }
    }

    #[derive(Default, Clone)]
    struct MockState {
        prompt_bodies: Arc<Mutex<Vec<Value>>>,
        history: Arc<Mutex<Value>>,
        queue: Arc<Mutex<Value>>,
        view_bytes: Arc<Mutex<Vec<u8>>>,
    }

    async fn prompt_handler(
        State(state): State<MockState>,
        axum::Json(body): axum::Json<Value>,
    ) -> Response {
        state.prompt_bodies.lock().unwrap().push(body);
        axum::Json(json!({ "prompt_id": "prompt-42" })).into_response()
    }

    async fn history_handler(
        State(state): State<MockState>,
        axum::extract::Path(prompt_id): axum::extract::Path<String>,
    ) -> Response {
        if prompt_id == "prompt-42" {
            axum::Json(state.history.lock().unwrap().clone()).into_response()
        } else {
            // 真服务对**不认识**的 prompt_id 回的是**空对象**（不是 404）：排队中/运行中的任务
            // 也还没进 history，所以"空"是这一层的正常答复，而不是错误。
            axum::Json(json!({})).into_response()
        }
    }

    async fn queue_handler(State(state): State<MockState>) -> Response {
        axum::Json(state.queue.lock().unwrap().clone()).into_response()
    }

    async fn view_handler(State(state): State<MockState>) -> Response {
        let bytes = state.view_bytes.lock().unwrap().clone();
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "image/png")
            .body(Body::from(bytes))
            .unwrap()
    }

    async fn upload_handler() -> Response {
        axum::Json(json!({ "name": "uploaded.png", "subfolder": "" })).into_response()
    }

    async fn mock_server() -> (String, MockState) {
        let state = MockState::default();
        let app = Router::new()
            .route("/prompt", post(prompt_handler).with_state(state.clone()))
            .route(
                "/history/{prompt_id}",
                get(history_handler).with_state(state.clone()),
            )
            .route("/queue", get(queue_handler).with_state(state.clone()))
            .route("/view", get(view_handler).with_state(state.clone()))
            .route("/upload/image", post(upload_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 服务应成功");
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}", address.port()), state)
    }

    fn workflow_fixture() -> Value {
        json!({
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "seed": 1,
                    "steps": 20,
                    "positive": ["6", 0]
                }
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "placeholder" }
            }
        })
    }

    #[tokio::test]
    async fn prepare_fills_inputs_into_workflow() {
        let (base, state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(connection(base), workflow_fixture()))
            .await
            .expect("prepare 应成功");
        let workflow = &prepared.provider_request["prompt"];
        assert_eq!(workflow["6"]["inputs"]["text"], "a red house");
        let _ = state;
    }

    #[tokio::test]
    async fn submit_accepts_with_prompt_id() {
        let (base, state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(connection(base), workflow_fixture()))
            .await
            .expect("prepare 应成功");
        let outcome = driver.submit(prepared).await.expect("submit 应成功");
        match outcome {
            SubmitOutcome::Accepted(handle) => {
                assert_eq!(handle.remote_id, "prompt-42");
                assert_eq!(handle.provider, "comfyui");
            }
            other => panic!("期望 Accepted，得到 {other:?}"),
        }
        let bodies = state.prompt_bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["prompt"]["6"]["inputs"]["text"], "a red house");
    }

    /// `comfy_job_id` 必须是**规范 UUID**：服务端对不合规的 `prompt_id` 直接
    /// 400 `invalid_prompt_id`（"must be a UUID string in canonical lowercase hyphenated form"），
    /// 而那只有真机上才看得见——本地不钉形状，上线就是"所有 ComfyUI 提交都失败"。
    ///
    /// 负对照：去掉版本位那行（`bytes[6] = (bytes[6] & 0x0f) | 0x50`）⇒ 这条红。
    #[test]
    fn comfy_job_id_is_a_canonical_uuid() {
        let id = comfy_job_id("run-7:attempt-1-abc");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "必须是 8-4-4-4-12 五段：{id}"
        );
        assert!(
            id.chars()
                .all(|c| c == '-' || (c.is_ascii_hexdigit() && !c.is_ascii_uppercase())),
            "必须是**小写**十六进制：{id}"
        );
        assert_eq!(
            parts[2].as_bytes()[0],
            b'5',
            "版本位必须是 5（名字型）：{id}"
        );
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "变体位必须是 8/9/a/b：{id}"
        );
        // 确定性：同一次提交永远同一个 id（重投是**新 attempt** ⇒ 新指纹 ⇒ 新 id，不会串）。
        assert_eq!(comfy_job_id("run-7:attempt-1-abc"), id);
        assert_ne!(
            comfy_job_id("run-7:attempt-2-abc"),
            id,
            "不同 attempt 不能撞 id"
        );
    }

    /// 提交体里必须带上**我们自己定的** `prompt_id` 与唯一的 `client_id`。
    ///
    /// 这是"响应丢了还能回查"的全部依据：没有它，`LookupByRequestId` 就是空话。
    #[tokio::test]
    async fn prepare_sends_our_own_job_id_so_a_lost_response_can_be_looked_up() {
        let (base, state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(connection(base), workflow_fixture()))
            .await
            .expect("prepare 应成功");
        let body = &prepared.provider_request;
        let prompt_id = body["prompt_id"]
            .as_str()
            .expect("必须带 prompt_id")
            .to_string();
        assert_eq!(
            prompt_id, prepared.request_id,
            "回查用的 id 必须与提交体里那个一致，否则查的是别人"
        );
        assert_eq!(
            body["client_id"].as_str(),
            Some(prompt_id.as_str()),
            "client_id 也要给（服务端全局字段，必须每次唯一）"
        );
        // 形状：规范 UUID（与 `comfy_job_id` 同一条判据，这里只再钉一次"真的发出去了"）。
        assert_eq!(prompt_id.len(), 36);
        assert_eq!(prompt_id.split('-').count(), 5);
        let _ = driver.submit(prepared).await.expect("submit 应成功");
        let bodies = state.prompt_bodies.lock().unwrap().clone();
        assert_eq!(
            bodies[0]["prompt_id"].as_str(),
            Some(prompt_id.as_str()),
            "提交体里必须真的带上我们自己定的 id"
        );
    }

    /// 一个"接了连接、读到请求、但**不回话**直接断开"的服务器。
    ///
    /// 这是"提交响应丢了"最贴近真实的复现：字节**确实发出去了**（所以不许说"没出去"），
    /// 而响应永远不来。
    async fn silent_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能监听");
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    // `peek` 不消费，只用来确认"请求字节到了"——然后不回话直接断开。
                    let _ = socket.peek(&mut buffer).await;
                    drop(socket);
                });
            }
        });
        format!("http://127.0.0.1:{}", address.port())
    }

    /// **响应丢了**（可能已经收下）⇒ 报"不确定 + 可按 id 回查"，而不是把错误交回去。
    ///
    /// 这是 `RecoveryProof::LookupByRequestId` 在**生产代码里的第一个生产者**：有了它，
    /// run 会拿到远端句柄并进对账队列，查得到就正常收尾（不再白等 1 小时判失败）。
    ///
    /// 反向控制见下一条 `a_refused_submit_is_left_to_the_retry_path`：**证明得了"字节没出去"**
    /// 时不许走这条路（那一档该直接退避重试，重投不可能重复计费）。
    #[tokio::test]
    async fn a_lost_submit_response_becomes_a_lookup_by_our_own_job_id() {
        let base = silent_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(connection(base), workflow_fixture()))
            .await
            .expect("prepare 应成功");
        let job_id = prepared.request_id.clone();
        let outcome = driver.submit(prepared).await.expect("应当是 Ok(Uncertain)");
        match outcome {
            SubmitOutcome::Uncertain(submission) => {
                assert_eq!(
                    submission.recovery,
                    RecoveryProof::LookupByRequestId(job_id),
                    "回查用的必须是我们自己定的那个 job id"
                );
            }
            other => panic!("期望 Uncertain(LookupByRequestId)，得到 {other:?}"),
        }
    }

    /// 提交时平台回 **5xx** 的服务（只答 `/prompt`，一律 503）。
    async fn five_hundred_server() -> String {
        let app = Router::new().route(
            "/prompt",
            post(|| async {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "platform blew up",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能监听");
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://127.0.0.1:{}", address.port())
    }

    /// **提交时平台回 5xx** ⇒ 与"响应丢了"**同一条处理**：报"不确定 + 可按 id 回查"。
    ///
    /// 为什么（这是钱，2026-10-18 定的取舍）：500 可能是**网关 / 内部错误发生在受理之后**
    /// ⇒ 平台**可能已经收下**并照常计费。盲投 = 平台上可能有两个任务、扣两次钱。
    /// ComfyUI 的抓手是**我们自己定的 `prompt_id`**，所以它通常**几秒内就能查清**
    /// （查得到就正常收尾），不像 RunningHub 只能等到死线。
    ///
    /// 负对照：把 `submit` 里那段 5xx 分支改回 `return Err(map_submit_http_error(..))` ⇒ 这条红
    /// （会退化成"交回去重试"，也就是盲投）。
    #[tokio::test]
    async fn a_five_hundred_on_submit_becomes_a_lookup_by_our_own_job_id() {
        let base = five_hundred_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(connection(base), workflow_fixture()))
            .await
            .expect("prepare 应成功");
        let job_id = prepared.request_id.clone();
        let outcome = driver
            .submit(prepared)
            .await
            .expect("5xx 不该把错误直接抛回去（那会退化成盲投）");
        match outcome {
            SubmitOutcome::Uncertain(submission) => {
                assert_eq!(
                    submission.recovery,
                    RecoveryProof::LookupByRequestId(job_id),
                    "回查用的必须是我们自己定的那个 job id"
                );
            }
            other => panic!("期望 Uncertain(LookupByRequestId)，得到 {other:?}"),
        }
    }

    /// **证明得了「字节没出去」**（连接被拒）⇒ 原样把错误交回去，让 scheduler 走退避重试。
    ///
    /// 负对照：把 `submit` 里那句 `if !mapped.submission_is_unknown() { return Err(mapped) }`
    /// 删掉 ⇒ 这条红（连接被拒也会被报成"可按 id 回查"，于是白白多等一轮对账）。
    #[tokio::test]
    async fn a_refused_submit_is_left_to_the_retry_path() {
        // 没人听的端口：连接**被拒**（对端主动拒绝握手）⇒ 一个字节都没发出去。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("应能占用端口");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let prepared = driver
            .prepare(request(
                connection(format!("http://127.0.0.1:{port}")),
                workflow_fixture(),
            ))
            .await
            .expect("prepare 应成功");
        let error = driver
            .submit(prepared)
            .await
            .expect_err("连接被拒必须交回错误，而不是报不确定");
        assert!(
            error.proves_nothing_was_sent(),
            "连接被拒 = 一个字节都没出去，证据必须留下：{:?}",
            error.details
        );
        assert!(
            !error.submission_is_unknown(),
            "证明得了就不该走等确认/回查那条路"
        );
    }

    /// **历史里没有 ≠ 平台没收到**：排队中的任务只在 `/queue` 里。
    ///
    /// ComfyUI 只在任务**执行完**才写 `/history`，而且 history 是内存态（上限 10000 条、可被清空、
    /// 重启即丢）。所以"历史里没有"必须先问一次队列——否则"提交响应丢了但平台收下了"的任务
    /// 会在队列里跑着、我们这边却一路 Unknown 到死线判失败。
    ///
    /// 判别点两条：① 在 `queue_running` 里 ⇒ `Running`；② 在 `queue_pending` 里 ⇒ `Queued`；
    /// ③ 反向控制：队列里**没有**这个 id ⇒ 仍然是 `Unknown`（不许猜成"失败"，也不许猜成"在跑"）。
    #[tokio::test]
    async fn reconcile_finds_a_queued_job_when_history_is_empty() {
        let (base, state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let handle = RemoteHandle {
            provider: "comfyui".to_string(),
            remote_id: "job-lost".to_string(),
        };
        let reconcile = |handle: RemoteHandle| {
            let driver = &driver;
            let connection = connection(base.clone());
            async move {
                driver
                    .reconcile(ReconcileRequest {
                        connection_revision: connection,
                        remote_handle: handle,
                    })
                    .await
            }
        };

        // ③ 先看"哪儿都没有"这一档：必须是 Unknown（不许猜）。
        *state.queue.lock().unwrap() = json!({ "queue_running": [], "queue_pending": [] });
        let snapshot = reconcile(handle.clone()).await.expect("reconcile 应成功");
        assert_eq!(snapshot.status, TaskStatus::Unknown);

        // ① 在跑。
        *state.queue.lock().unwrap() = json!({
            "queue_running": [[0, "job-lost", {}, { "client_id": "job-lost" }, [], {}]],
            "queue_pending": []
        });
        let snapshot = reconcile(handle.clone()).await.expect("reconcile 应成功");
        assert_eq!(snapshot.status, TaskStatus::Running);

        // ② 排队中。
        *state.queue.lock().unwrap() = json!({
            "queue_running": [],
            "queue_pending": [[1, "job-lost", {}, {}, [], {}]]
        });
        let snapshot = reconcile(handle.clone()).await.expect("reconcile 应成功");
        assert_eq!(snapshot.status, TaskStatus::Queued);

        // ④ 别人的任务在跑（id 不匹配）⇒ 不能认成自己的。
        *state.queue.lock().unwrap() = json!({
            "queue_running": [[0, "someone-else", {}, {}, [], {}]],
            "queue_pending": []
        });
        let snapshot = reconcile(handle).await.expect("reconcile 应成功");
        assert_eq!(
            snapshot.status,
            TaskStatus::Unknown,
            "id 不匹配就不能认成自己的任务"
        );
    }

    #[tokio::test]
    async fn reconcile_succeeded_downloads_view_results() {
        let (base, state) = mock_server().await;
        *state.history.lock().unwrap() = json!({
            "prompt-42": {
                "status": { "status": "success" },
                "outputs": {
                    "9": {
                        "images": [{
                            "filename": "ComfyUI_00001_.png",
                            "subfolder": "",
                            "type": "output"
                        }]
                    }
                }
            }
        });
        *state.view_bytes.lock().unwrap() = vec![0x89, 0x50, 0x4e, 0x47];
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision: connection(base),
                remote_handle: RemoteHandle {
                    provider: "comfyui".to_string(),
                    remote_id: "prompt-42".to_string(),
                },
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Succeeded);
        assert_eq!(snapshot.results.len(), 1);
        assert_eq!(
            snapshot.results[0].data,
            ArtifactData::Bytes(vec![0x89, 0x50, 0x4e, 0x47])
        );
        assert_eq!(snapshot.results[0].filename, "ComfyUI_00001_.png");
    }

    #[tokio::test]
    async fn reconcile_running_returns_running_status() {
        let (base, state) = mock_server().await;
        *state.history.lock().unwrap() = json!({
            "prompt-42": { "status": { "status": "executing" } }
        });
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision: connection(base),
                remote_handle: RemoteHandle {
                    provider: "comfyui".to_string(),
                    remote_id: "prompt-42".to_string(),
                },
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Running);
    }

    #[tokio::test]
    async fn reconcile_failed_reports_error() {
        let (base, state) = mock_server().await;
        *state.history.lock().unwrap() = json!({
            "prompt-42": {
                "status": {
                    "status": "error",
                    "messages": [["execution_error", { "message": "CUDA out of memory" }]]
                }
            }
        });
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let snapshot = driver
            .reconcile(ReconcileRequest {
                connection_revision: connection(base),
                remote_handle: RemoteHandle {
                    provider: "comfyui".to_string(),
                    remote_id: "prompt-42".to_string(),
                },
            })
            .await
            .expect("对账应成功");
        assert_eq!(snapshot.status, TaskStatus::Failed);
        let error = snapshot.error.expect("失败应带错误");
        assert!(error.message.contains("CUDA"));
        assert!(error.details.contains_key("message"));
    }

    #[tokio::test]
    async fn validate_connection_rejects_insecure_remote_url() {
        let connection = connection("http://api.example.com".to_string());
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let error = driver
            .validate_connection(&connection)
            .await
            .expect_err("非本地 http 地址应被拒绝");
        assert_eq!(error.code, "PROVIDER_URL_INSECURE");
    }

    /// 回归：媒体资产按**字段键**写进映射写明的 (node, field)——不再扫图猜 LoadImage。
    #[tokio::test]
    async fn prepare_writes_keyed_asset_into_the_mapped_node_input() {
        let (base, state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let mut request = request(
            connection(base),
            json!({
                "6": { "class_type": "CLIPTextEncode", "inputs": { "text": "placeholder" } },
                "17": { "class_type": "LoadImage", "inputs": { "image": "placeholder.png" } }
            }),
        );
        request.recipe_binding.provider_config.insert(
            "mediaMapping".to_string(),
            json!({ "17::image": { "node": "17", "field": "image" } }),
        );
        request.assets = vec![ResolvedAsset {
            asset_ref: AssetRef::new(AssetId(1), "abc").with_field_key("17::image"),
            bytes: vec![1, 2, 3],
        }];
        let prepared = driver.prepare(request).await.expect("prepare 应成功");
        let workflow = &prepared.provider_request["prompt"];
        assert_eq!(workflow["17"]["inputs"]["image"], "uploaded.png");
        let _ = state;
    }

    /// 字段键不在映射里 ⇒ 明确报错（不再有"静默丢掉"这一种结局）。
    #[tokio::test]
    async fn prepare_rejects_unknown_field_keys() {
        let (base, _state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let mut request = request(connection(base), workflow_fixture());
        request.recipe_binding.provider_config.insert(
            "mediaMapping".to_string(),
            json!({ "17::image": { "node": "17", "field": "image" } }),
        );
        request.assets = vec![ResolvedAsset {
            asset_ref: AssetRef::new(AssetId(1), "abc").with_field_key("19::image"),
            bytes: vec![1, 2, 3],
        }];
        let error = driver.prepare(request).await.expect_err("未知键必须报错");
        assert_eq!(error.code, "ASSET_FIELD_UNKNOWN");
    }

    /// 映射指向的节点/字段在图里不存在 ⇒ `MEDIA_MAPPING_INVALID`（revision 与图漂移）。
    #[tokio::test]
    async fn prepare_rejects_mapping_that_points_at_a_missing_node() {
        let (base, _state) = mock_server().await;
        let driver = ComfyUiDriver::new(Box::new(NoSecrets));
        let mut request = request(connection(base), workflow_fixture());
        request.recipe_binding.provider_config.insert(
            "mediaMapping".to_string(),
            json!({ "99::image": { "node": "99", "field": "image" } }),
        );
        request.assets = vec![ResolvedAsset {
            asset_ref: AssetRef::new(AssetId(1), "abc").with_field_key("99::image"),
            bytes: vec![1, 2, 3],
        }];
        let error = driver.prepare(request).await.expect_err("缺节点必须报错");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
    }

    /// 兼容分支：`asset://N` 占位仍被替换；但没有第 N 个资产时明确报错，
    /// 不把 `asset://N` 静默发给引擎。
    #[test]
    fn asset_scheme_placeholders_are_replaced_or_reported() {
        let mut workflow = json!({
            "5": {
                "class_type": "LoadImage",
                "inputs": { "image": "asset://0" }
            }
        });
        ComfyUiDriver::replace_asset_scheme_refs(&mut workflow, &["uploaded.png".to_string()])
            .expect("有对应资产时应替换");
        assert_eq!(workflow["5"]["inputs"]["image"], "uploaded.png");

        let mut workflow = json!({
            "5": {
                "class_type": "LoadImage",
                "inputs": { "image": "asset://2" }
            }
        });
        let error =
            ComfyUiDriver::replace_asset_scheme_refs(&mut workflow, &["uploaded.png".to_string()])
                .expect_err("没有对应资产必须报错");
        assert_eq!(error.code, "ASSET_FIELD_UNKNOWN");
        assert_eq!(
            workflow["5"]["inputs"]["image"], "asset://2",
            "报错时不改图"
        );
    }

    /// `apply_media_uploads` 直接对着 (node, field) 写；节点缺失是明确错误。
    #[test]
    fn apply_media_uploads_writes_by_slot_and_reports_missing_nodes() {
        let asset_ref = AssetRef::new(AssetId(1), "abc").with_field_key("17::image");
        let binding = crate::v2::domain::media::BoundAsset {
            asset: &asset_ref,
            field_key: "17::image".to_string(),
            slot: crate::v2::domain::media::MediaSlot {
                node: "17".to_string(),
                field: "image".to_string(),
            },
        };
        let mut workflow = json!({ "17": { "inputs": { "image": "old.png" } } });
        ComfyUiDriver::apply_media_uploads(
            &mut workflow,
            std::slice::from_ref(&binding),
            &["uploaded.png".to_string()],
        )
        .expect("写入应成功");
        assert_eq!(workflow["17"]["inputs"]["image"], "uploaded.png");

        let mut missing = json!({ "6": { "inputs": {} } });
        let error = ComfyUiDriver::apply_media_uploads(
            &mut missing,
            std::slice::from_ref(&binding),
            &["uploaded.png".to_string()],
        )
        .expect_err("缺节点必须报错");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
    }
}
