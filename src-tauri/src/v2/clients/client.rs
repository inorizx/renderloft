use reqwest::{header, Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClientError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Value,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ClientError {}

impl ClientError {
    pub fn network(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: "CLIENT_NETWORK_ERROR".to_string(),
            message: message.into(),
            retryable: true,
            details: Value::Null,
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.status == 404
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointFile {
    pub endpoint: String,
    pub token: String,
}

/// 调用方自报身份的头名。daemon 侧读它来区分"自主调用方"与"人在操作"。
pub const DRIVER_HEADER: &str = "x-renderloft-driver";

pub struct V2Client {
    base_url: String,
    token: String,
    /// 调用方自报的身份（`X-RenderLoft-Driver` 头）。
    ///
    /// ⚠️ 为什么需要它：daemon 侧的花钱闸门要区分"自主调用方"与"人在操作"，
    /// 而 MCP 与 GUI **走的是同一个 HTTP 端点**（origin 都是 `http`），
    /// 光看端点分不出来。所以由客户端**自报**。
    ///
    /// ⚠️ 这不是安全机制，是**如实声明**：持合法 token 的人本来就能直接写脚本花自己的钱。
    /// 闸门防的是意外（Agent 跑飞），不是防对手——所以不做防伪造的安全剧场。
    driver: Option<String>,
    client: Client,
}

impl V2Client {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            driver: None,
            client,
        }
    }

    /// 自报身份（如 `"mcp"`）。daemon 侧据此判断要不要给这个调用方设花钱闸门。
    pub fn with_driver(mut self, driver: impl Into<String>) -> Self {
        self.driver = Some(driver.into());
        self
    }

    pub fn from_endpoint_file() -> Result<Self, String> {
        let endpoint = Self::read_endpoint_file()?;
        Ok(Self::new(endpoint.endpoint, endpoint.token))
    }

    pub fn from_endpoint_value(endpoint: EndpointFile) -> Self {
        Self::new(endpoint.endpoint, endpoint.token)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn read_endpoint_file() -> Result<EndpointFile, String> {
        let path = dirs::data_local_dir()
            .ok_or_else(|| "无法定位本地 daemon 目录".to_string())?
            .join("renderloft-workspace")
            .join("daemon-endpoint.json");
        let content = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        serde_json::from_str(&content).map_err(|error| error.to_string())
    }

    pub async fn get(&self, path: &str) -> Result<Value, ClientError> {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value, ClientError> {
        self.request(Method::POST, path, Some(body)).await
    }

    /// `PUT`。部分更新：服务端对**没给的字段**按"保持原值"处理
    /// （见 `application/connection.rs` 的 `unwrap_or_else(|| current...)`）。
    pub async fn put(&self, path: &str, body: Value) -> Result<Value, ClientError> {
        self.request(Method::PUT, path, Some(body)).await
    }

    /// `DELETE`。删除类接口没有请求体，但可能有查询参数（如 `?purge=true`），由调用方拼进 path。
    pub async fn delete(&self, path: &str) -> Result<Value, ClientError> {
        self.request(Method::DELETE, path, None).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ClientError> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self
            .client
            .request(method, &url)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(driver) = &self.driver {
            request = request.header(DRIVER_HEADER, driver);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| ClientError::network(format!("无法连接 daemon：{error}")))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| ClientError::network(format!("无法读取 daemon 响应：{error}")))?;
        let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Self::unwrap_envelope(status, parsed)
    }

    fn unwrap_envelope(status: u16, body: Value) -> Result<Value, ClientError> {
        if body.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(body.get("data").cloned().unwrap_or(Value::Null));
        }
        // 204（无响应体）是**成功**：`POST /api/daemon/shutdown` 就是这样回的。
        // 鉴权失败会带 body 回 401，所以"空 body + 204"不会有歧义。
        if status == 204 && body.is_null() {
            return Ok(Value::Null);
        }
        let error = body.get("error").cloned().unwrap_or(Value::Null);
        Err(ClientError {
            status,
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("DAEMON_ERROR")
                .to_string(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("daemon 请求失败")
                .to_string(),
            retryable: error
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            details: error.get("details").cloned().unwrap_or(Value::Null),
        })
    }

    // ---------- recipes ----------

    pub async fn list_recipes(&self, limit: usize) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/recipes?limit={limit}")).await
    }

    /// 列出**已移除**（墓碑）的应用。`?removed=true` 是**另一个集合**，不是筛选。
    pub async fn list_removed_recipes(&self, limit: usize) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/recipes?removed=true&limit={limit}"))
            .await
    }

    pub async fn get_recipe(&self, recipe_id: u64) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/recipes/{recipe_id}")).await
    }

    /// 从资源库**移除**一个应用（打墓碑：历史照旧可解析、随时可恢复）。
    ///
    /// ⚠️ 名字是 remove 而不是 delete：这不是销毁。HTTP 动词仍是 DELETE
    ///（与既有客户端保持兼容），但语义与界面文案都必须是"移除"。
    pub async fn remove_recipe(&self, recipe_id: u64) -> Result<Value, ClientError> {
        self.delete(&format!("/api/v1/recipes/{recipe_id}")).await
    }

    /// 生成一份**一致的**工作区快照（迁移 / 备份）。`to` 是备份目录。
    ///
    /// ⚠️ 恢复**没有**对应的方法：`daemon stop` → 用备份替换工作区目录 → 再跑任意一条命令把
    /// daemon 拉起来。密钥永远不在这份快照里（它们在系统凭据库），换机器必须**由人重填**。
    pub async fn backup_workspace(&self, to: &str) -> Result<Value, ClientError> {
        self.post("/api/v1/workspace/backup", json!({ "to": to }))
            .await
    }

    /// 从「已移除」里恢复一个应用（不在墓碑里的会拿到 404 `RECIPE_NOT_IN_TRASH`）。
    pub async fn restore_recipe(&self, recipe_id: u64) -> Result<Value, ClientError> {
        self.post(&format!("/api/v1/recipes/{recipe_id}/restore"), json!({}))
            .await
    }

    /// 从平台链接/应用 ID 导入一个应用。
    ///
    /// ⚠️ 路径与请求体都只写这一处：此前 CLI 自己 `format!` 路径 + 手写 `json!` 请求体，
    /// 而 MCP 根本没有这个动作——"帮我把这个应用加进来"这类请求从 MCP 完全够不着。
    pub async fn import_recipe(
        &self,
        connection_id: u64,
        source: &str,
    ) -> Result<Value, ClientError> {
        self.post(
            "/api/v1/recipes/import",
            json!({
                "connectionId": connection_id,
                "source": source,
            }),
        )
        .await
    }

    /// 从平台链接/工作流 ID 导入一个**原生 RunningHub 工作流**。
    ///
    /// 与 `import_recipe`（AI 应用存 `webappId`）对称：这条存 `workflowId`，定义仍在平台上，
    /// 提交时走 `task/openapi/create` + `nodeInfoList`。
    ///
    /// 为什么要有它：界面早就导得进工作流（「从 RunningHub 工作流拉取」），但那条路只到
    /// "导入成 ComfyUI 配方"，而 CLI/MCP **完全够不着**——代价是"只有 MCP 没有界面时，
    /// 这个工作流根本进不来"。
    pub async fn import_recipe_workflow(
        &self,
        connection_id: u64,
        source: &str,
        name: Option<&str>,
    ) -> Result<Value, ClientError> {
        self.post(
            "/api/v1/recipes/import-workflow",
            json!({
                "connectionId": connection_id,
                "source": source,
                "name": name,
            }),
        )
        .await
    }

    // ---------- connections ----------

    pub async fn list_connections(&self) -> Result<Value, ClientError> {
        self.get("/api/v1/connections").await
    }

    pub async fn get_connection(&self, connection_id: u64) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/connections/{connection_id}"))
            .await
    }

    /// 这条连接背后平台声明的可用模型（`GET {baseUrl}/models` 那类）。
    pub async fn list_connection_models(&self, connection_id: u64) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/connections/{connection_id}/models"))
            .await
    }

    /// 写**一个**连接的 API Key。
    ///
    /// 发的是 `slot: "api_key"`，由 daemon 收敛成连接级 scoped slot
    /// （`connection-<id>-api_key`）——与界面走的是同一条路，所以这里**不硬编码槽位名**；
    /// 驱动侧只按 `-api_key` 后缀找（`is_api_key_slot`）。
    ///
    /// 值只来自调用方（CLI 从文件读），**绝不进命令行参数**：那会同时留在 shell history、
    /// 进程列表与 CI 日志里，一处泄漏变三处。
    pub async fn set_connection_api_key(
        &self,
        connection_id: u64,
        value: &str,
    ) -> Result<Value, ClientError> {
        self.put(
            &format!("/api/v1/connections/{connection_id}"),
            json!({
                "secretBindings": [
                    { "slot": "api_key", "resolution": "current", "value": value }
                ]
            }),
        )
        .await
    }

    /// 设置回调签名密钥。**值只进不出**：读回来只有"配没配"（见 `callback_secret_status`）。
    ///
    /// 与连接 API Key 一样，值只来自文件、绝不进命令行参数。
    pub async fn set_callback_secret(&self, value: &str) -> Result<Value, ClientError> {
        self.put("/api/v1/callback-secret", json!({ "value": value }))
            .await
    }

    /// 回调签名密钥配没配。**永远不回值**。
    pub async fn callback_secret_status(&self) -> Result<Value, ClientError> {
        self.get("/api/v1/callback-secret").await
    }

    /// 清掉回调签名密钥（关掉签名）。
    pub async fn clear_callback_secret(&self) -> Result<Value, ClientError> {
        self.delete("/api/v1/callback-secret").await
    }

    /// 让 daemon **优雅退出**（`POST /api/daemon/shutdown`，成功时是 204 无响应体）。
    ///
    /// 只给 `daemon stop` 用。任务不会因此丢：平台侧继续跑，daemon 下次启动会把结果对账回来
    /// （run 的远端句柄与对账死线都在库里）。
    pub async fn shutdown_daemon(&self) -> Result<Value, ClientError> {
        self.post("/api/daemon/shutdown", Value::Null).await
    }

    /// 一个**不指向任何在跑的东西**的 client：默认端口 + 空 token。
    ///
    /// ⚠️ 只给 `daemon.*` 这类"关于 daemon 本身"的动词用：它们必须在 daemon 没跑时也能回答，
    /// 否则"它到底在不在"就只能靠"启动它试试"来回答（荒谬）。别的动词拿到它会得到一个连接
    /// 错误——那是对的，它们本来就需要 daemon。
    pub fn offline() -> Self {
        Self::new(format!("http://{}", super::daemon_boot::DEFAULT_BIND), "")
    }

    /// 取产物的**原始字节**（`GET /api/v1/artifacts/{id}/bytes`）。
    ///
    /// 为什么单独一条、不走 `request`：别的客户端方法拿的都是 JSON 信封，而这条回的是二进制。
    /// 而且 `artifact list` 里 `byteSize` 为 0 的产物本来就是"**字节还在平台上、按需拉取**"
    /// （中转站定位，不落本地）——所以"看得到元数据、拿不到文件"是个真缺口：
    /// 脚本与 AI 都没法把图取到手，只有界面能预览。
    pub async fn artifact_bytes(&self, artifact_id: u64) -> Result<Vec<u8>, ClientError> {
        let url = format!("{}/api/v1/artifacts/{artifact_id}/bytes", self.base_url);
        let mut request = self
            .client
            .get(&url)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(driver) = &self.driver {
            request = request.header(DRIVER_HEADER, driver);
        }
        let response = request
            .send()
            .await
            .map_err(|error| ClientError::network(format!("无法连接 daemon：{error}")))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| ClientError::network(format!("无法读取产物字节：{error}")))?;
        if !(200..300).contains(&status) {
            // 出错时 daemon 回的是 JSON 信封：解出来，别让调用方只看到一句 "HTTP 500"。
            let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return match Self::unwrap_envelope(status, parsed) {
                Err(error) => Err(error),
                Ok(_) => Err(ClientError::network(format!(
                    "取产物字节失败：HTTP {status}"
                ))),
            };
        }
        Ok(bytes.to_vec())
    }

    // ---------- operations ----------

    /// 创建任务。
    ///
    /// `callback` 是**可选**的完成通知订阅（`{url, events}`）：给了就带上，没给就一个字段都不加。
    /// 为什么不让调用方自己拼 body：`create_operation` 是唯一的提交入口，形状在这里收口，
    /// 加字段时不会漏掉某条路径。
    pub async fn create_operation(
        &self,
        idempotency_key: &str,
        runs: Value,
        callback: Option<Value>,
    ) -> Result<Value, ClientError> {
        let mut body = json!({ "idempotencyKey": idempotency_key, "runs": runs });
        if let Some(callback) = callback {
            body["callback"] = callback;
        }
        self.post("/api/v1/operations", body).await
    }

    /// 只读预检（dry-run）。
    ///
    /// 与 `create_operation` 走**同一套**服务端解析（验证 revision/连接/能力/字段键/资产
    /// 存在与 checksum），但**不落库、不调度、不调用 provider、不花钱**。
    /// 形状与 create 一致：`runs` 是数组。
    pub async fn validate_operation(&self, runs: Value) -> Result<Value, ClientError> {
        self.post("/api/v1/operations/validate", json!({ "runs": runs }))
            .await
    }

    pub async fn get_operation(&self, operation_id: u64) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/operations/{operation_id}"))
            .await
    }

    pub async fn list_operations(
        &self,
        limit: usize,
        after: Option<u64>,
    ) -> Result<Value, ClientError> {
        match after {
            // 游标翻页：服务端的 `nextCursor` 就是这个 `after`，不回传等于让调用方拿到一个用不上的字段。
            Some(cursor) => {
                self.get(&format!("/api/v1/operations?limit={limit}&after={cursor}"))
                    .await
            }
            None => self.get(&format!("/api/v1/operations?limit={limit}")).await,
        }
    }

    pub async fn cancel_operation(&self, operation_id: u64) -> Result<Value, ClientError> {
        // cancel 是变更操作 ⇒ POST（服务端已由 GET 改为 POST；与 retry/delete 一致）。
        self.post(
            &format!("/api/v1/operations/{operation_id}/cancel"),
            json!({}),
        )
        .await
    }

    /// 重试：**新建一个 operation**（复用原 run 的配方修订、inputs 与 assets），不是"接着跑旧的"。
    ///
    /// 服务端返回的是**新** operationId —— 调用方必须改盯新的那个，旧的原样留着
    /// （还在不确定态就自己取消它）。
    ///
    /// `run_id` 给了就只重试那一条 run（批量里失败的那几条）；不给就重跑该任务的全部 run
    /// ——**后者会把已经成功的那几条也再提交一次，也就是再花一次钱**。
    pub async fn retry_operation(
        &self,
        operation_id: u64,
        run_id: Option<u64>,
    ) -> Result<Value, ClientError> {
        let path = match run_id {
            Some(run_id) => format!("/api/v1/operations/{operation_id}/runs/{run_id}/retry"),
            None => format!("/api/v1/operations/{operation_id}/retry"),
        };
        self.post(&path, json!({})).await
    }

    pub async fn operation_events(
        &self,
        operation_id: u64,
        after: u64,
    ) -> Result<Value, ClientError> {
        self.get(&format!(
            "/api/v1/operations/{operation_id}/events?after={after}"
        ))
        .await
    }

    pub async fn wait_operation(
        &self,
        operation_id: u64,
        interval_seconds: u64,
        timeout_seconds: u64,
    ) -> Result<Value, ClientError> {
        let started = std::time::Instant::now();
        loop {
            let operation = self.get_operation(operation_id).await?;
            let status = operation
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if is_terminal_status(status) {
                return Ok(json!({
                    "operationId": operation_id,
                    "status": status,
                    "complete": true,
                    "operation": operation,
                }));
            }
            if timeout_seconds > 0 && started.elapsed().as_secs() >= timeout_seconds {
                return Ok(json!({
                    "operationId": operation_id,
                    "status": status,
                    "complete": false,
                    "operation": operation,
                }));
            }
            tokio::time::sleep(Duration::from_secs(interval_seconds.max(1))).await;
        }
    }

    // ---------- artifacts ----------

    /// 产物列表（分页）。
    ///
    /// ⚠️ 此前这里**一个参数都不带**：daemon 那边本来支持 `limit`（默认 50、上限 200）与 `after`，
    /// 但动词层没透出来 ⇒ 模型既不能缩小（回执白占上下文）也不能翻页（看不到第 51 条之后）。
    /// 现在两者都能给。
    /// 产物列表。`order` / `before` 是**反方向**（最新在前）—— 见动词表里 `artifact.list` 的说明。
    ///
    /// ⚠️ 这个函数是**手写查询串**，与动词表里的参数**没有编译期联系**。实测踩过一次
    /// （2026-10-18）：表里加了 `order` / `before`（CLI 于是认得这两个旗标），但这里没转发 ——
    /// `artifact list --order desc` 于是**静默地按升序返回**：有数据、格式也对，看起来"能用"，
    /// 只是内容是**最旧**的一批。现在有 `artifact_list_forwards_the_direction_flags` 钉住它。
    pub async fn list_artifacts(
        &self,
        limit: Option<u64>,
        after: Option<u64>,
        before: Option<u64>,
        order: Option<String>,
    ) -> Result<Value, ClientError> {
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        if let Some(after) = after {
            query.push(format!("after={after}"));
        }
        if let Some(before) = before {
            query.push(format!("before={before}"));
        }
        if let Some(order) = order {
            query.push(format!("order={order}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/api/v1/artifacts{suffix}")).await
    }

    pub async fn get_artifact(&self, artifact_id: u64) -> Result<Value, ClientError> {
        self.get(&format!("/api/v1/artifacts/{artifact_id}")).await
    }

    // ---------- assets ----------

    /// 上传一份资产（图片等）。**body 就是原始字节**：文件名与 MIME 走 query，`Content-Type`
    /// 是文件自己的类型。
    ///
    /// ⚠️ 这里曾经发的是 `"bytes": [1, 2, 3, 4, …]`（JSON 数字数组）。4 个字节的测试全绿，
    /// 而真实图片（2.36 MB）会膨胀成约 10 MB 的 JSON 文本，撞上 axum 默认 **2 MB** 的请求体
    /// 上限——daemon 回的是 `INVALID_REQUEST_BODY`（"请求体格式不正确"），完全指不到病根，
    /// 而 CLI 那侧看到的是连接被重置（`CLIENT_NETWORK_ERROR`）。
    /// **字节就是字节，别过文本编码。**
    pub async fn upload_asset(
        &self,
        filename: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, ClientError> {
        let url = format!(
            "{}/api/v1/assets?filename={}&mimeType={}",
            self.base_url,
            percent_encode(filename),
            percent_encode(mime_type),
        );
        let mut request = self
            .client
            .post(&url)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, mime_type)
            .body(bytes);
        if let Some(driver) = &self.driver {
            request = request.header(DRIVER_HEADER, driver);
        }
        let response = request
            .send()
            .await
            .map_err(|error| ClientError::network(format!("无法连接 daemon：{error}")))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|error| ClientError::network(format!("无法读取 daemon 响应：{error}")))?;
        let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        Self::unwrap_envelope(status, parsed)
    }

    // ---------- capabilities ----------

    pub async fn capabilities(&self) -> Result<Value, ClientError> {
        self.get("/api/v1/capabilities").await
    }
}

/// 百分号编码（query 里的文件名可能是中文、带空格、带 `&`——不编码就会把请求拆坏）。
///
/// 只放行 RFC 3986 的 unreserved 字符，其余一律 `%XX`。自己写而不是引依赖：本仓库在离线
/// 依赖解析上栽过（加一个 crate 触发全量重解析、下载失败），十几行不值得冒那个险。
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "SUCCEEDED" | "FAILED" | "CANCELLED")
}

pub fn run_request(recipe_revision_id: u64, inputs: Value, assets: Option<Value>) -> Value {
    let mut run = json!({
        "clientRunId": format!("run-{}", uuid::Uuid::new_v4().simple()),
        "recipeRevisionId": recipe_revision_id,
        "inputs": inputs,
    });
    // 资产是**可选的**：没给就一个字段都不加（"没给"与"给了空"对服务端不是一回事）。
    if let Some(assets) = assets {
        run["assets"] = assets;
    }
    json!([run])
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliEnvelope {
    pub contract_version: &'static str,
    pub command: String,
    pub ok: bool,
    pub data: Value,
}

pub fn cli_success(command: &str, data: Value) -> String {
    serde_json::to_string(&CliEnvelope {
        contract_version: "1",
        command: command.to_string(),
        ok: true,
        data,
    })
    .unwrap_or_default()
}

pub fn cli_error(command: &str, error: &ClientError) -> String {
    serde_json::to_string(&json!({
        "contractVersion": "1",
        "command": command,
        "ok": false,
        "error": {
            "status": error.status,
            "code": error.code,
            "message": error.message,
            "retryable": error.retryable,
        }
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{header, StatusCode},
        routing::get,
        Router,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    struct MockState {
        authorization: Arc<Mutex<Option<String>>>,
        driver: Arc<Mutex<Option<String>>>,
        operation_polls: Arc<Mutex<usize>>,
        /// 产物列表那次请求**实际带上的查询串**（用来钉住"旗标真的发出去了"）。
        artifact_query: Arc<Mutex<Option<String>>>,
    }

    async fn mock_app() -> (String, MockState) {
        let state = MockState::default();
        let app = Router::new()
            .route(
                "/api/v1/recipes",
                get(list_recipes).with_state(state.clone()),
            )
            .route(
                "/api/v1/operations/{id}",
                get(get_operation).with_state(state.clone()),
            )
            .route(
                "/api/v1/operations/{id}/events",
                get(events).with_state(state.clone()),
            )
            .route(
                "/api/v1/artifacts",
                get(list_artifacts).with_state(state.clone()),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}", address.port()), state)
    }

    async fn list_recipes(
        State(state): State<MockState>,
        headers: axum::http::HeaderMap,
    ) -> axum::Json<Value> {
        *state.authorization.lock().unwrap() = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        *state.driver.lock().unwrap() = headers
            .get(DRIVER_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        axum::Json(json!({
            "contractVersion": "1",
            "ok": true,
            "data": {
                "items": [{ "recipeId": 1, "name": "Image Recipe" }],
                "nextCursor": null,
                "hasMore": false
            }
        }))
    }

    async fn get_operation(
        State(state): State<MockState>,
        axum::extract::Path(operation_id): axum::extract::Path<u64>,
    ) -> (StatusCode, axum::Json<Value>) {
        let polls = *state.operation_polls.lock().unwrap();
        *state.operation_polls.lock().unwrap() = polls + 1;
        if polls < 2 {
            (
                StatusCode::OK,
                axum::Json(json!({
                    "contractVersion": "1",
                    "ok": true,
                    "data": { "operationId": operation_id, "status": "RUNNING" }
                })),
            )
        } else {
            (
                StatusCode::OK,
                axum::Json(json!({
                    "contractVersion": "1",
                    "ok": true,
                    "data": { "operationId": operation_id, "status": "SUCCEEDED" }
                })),
            )
        }
    }

    async fn events(
        axum::extract::Path(operation_id): axum::extract::Path<u64>,
    ) -> axum::Json<Value> {
        axum::Json(json!({
            "contractVersion": "1",
            "ok": true,
            "data": [{
                "eventId": 1,
                "eventType": "run.created",
                "operationId": operation_id,
                "runId": 1,
                "sequence": 0,
                "createdAt": "1",
                "payload": {}
            }]
        }))
    }

    async fn list_artifacts(
        State(state): State<MockState>,
        uri: axum::http::Uri,
    ) -> axum::Json<Value> {
        // 把**实际收到的查询串**记下来：客户端那侧是手写拼接，漏转发时这里才看得出来。
        *state.artifact_query.lock().unwrap() = uri.query().map(str::to_string);
        axum::Json(json!({
            "contractVersion": "1",
            "ok": true,
            "data": { "items": [], "nextCursor": null, "hasMore": false }
        }))
    }

    /// **方向旗标必须真的发出去**（`--order desc` / `--before`）。
    ///
    /// ⚠️ 为什么值得单独测（2026-10-18 实测踩到）：动词表里加了参数（CLI 于是认得旗标），
    /// 而客户端的查询串是**手写拼接**、与表**没有编译期联系** —— 漏转发时
    /// `artifact list --order desc` 会**静默按升序返回**：有数据、格式也对，看起来"能用"，
    /// 只是内容是**最旧**的一批（正是「结果」视角最不想要的那一批）。
    ///
    /// 端到端那条链是"CLI 旗标 → 查询串 → 服务端行为"：服务端那半已有契约测试，
    /// **中间这一段只有这里能钉住**。同类风险适用于**任何**手写查询串/请求体的动词。
    ///
    /// 负对照：把 `list_artifacts` 里 `order` / `before` 那两段删掉 ⇒ 这条红。
    #[tokio::test]
    async fn artifact_list_forwards_the_direction_flags() {
        let (base, state) = mock_app().await;
        let client = V2Client::new(base, "token".to_string());
        client
            .list_artifacts(Some(5), None, Some(42), Some("desc".to_string()))
            .await
            .expect("列举应成功");
        let query = state
            .artifact_query
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        assert!(query.contains("limit=5"), "limit 要发出去：{query}");
        assert!(query.contains("order=desc"), "方向要发出去：{query}");
        assert!(query.contains("before=42"), "反向游标要发出去：{query}");
    }

    /// 自报身份必须**真的发出去了**——整个花钱闸门都挂在它上面。
    ///
    /// 判别点两条：
    /// 1. `with_driver("mcp")` ⇒ 请求带上 `x-renderloft-driver: mcp`（`DRIVER_HEADER`）；
    /// 2. 不设时 ⇒ **不带这个头**（而不是带一个空值）。
    ///
    /// ⚠️ 为什么值得单独测：daemon 侧的闸门判据是 `origin == "mcp"`，
    /// 而 origin 来自这个头。头没发出去的话，**闸门永远不触发、而且不报任何错**——
    /// 正是那种"测试全绿、线上裸奔"的失效。HTTP 层那头（头 → origin）已有测试，
    /// 这一条补的是**客户端 → 头**那一段。
    #[tokio::test]
    async fn driver_header_is_sent_only_when_declared() {
        let (base, state) = mock_app().await;

        let _ = V2Client::new(base.clone(), "rh-token-1")
            .with_driver("mcp")
            .list_recipes(50)
            .await
            .expect("请求应成功");
        assert_eq!(
            state.driver.lock().unwrap().clone(),
            Some("mcp".to_string()),
            "自报 mcp 时请求必须带上 driver 头"
        );

        let _ = V2Client::new(base, "rh-token-1")
            .list_recipes(50)
            .await
            .expect("请求应成功");
        assert_eq!(
            state.driver.lock().unwrap().clone(),
            None,
            "没自报时不该带这个头（带空值会让白名单外的值混进来）"
        );
    }

    #[tokio::test]
    async fn get_sends_bearer_and_unwraps_envelope() {
        let (base, state) = mock_app().await;
        let client = V2Client::new(base, "rh-token-1");
        let data = client.list_recipes(50).await.expect("请求应成功");
        assert_eq!(data["items"][0]["name"], "Image Recipe");
        let authorization = state.authorization.lock().unwrap().clone().unwrap();
        assert_eq!(authorization, "Bearer rh-token-1");
    }

    #[tokio::test]
    async fn error_envelope_parses_code_and_retryable() {
        let (base, _) = mock_app().await;
        let _client = V2Client::new(base, "rh-token-1");
        // 直接测解析函数
        let error = V2Client::unwrap_envelope(
            409,
            json!({
                "contractVersion": "1",
                "ok": false,
                "error": {
                    "code": "IDEMPOTENCY_CONFLICT",
                    "kind": "conflict",
                    "message": "幂等冲突",
                    "retryable": false,
                    "details": {}
                }
            }),
        )
        .expect_err("错误信封应返回 ClientError");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, "IDEMPOTENCY_CONFLICT");
        assert!(!error.retryable);
        assert!(!error.is_not_found());
    }

    #[tokio::test]
    async fn wait_operation_polls_until_terminal() {
        let (base, state) = mock_app().await;
        let client = V2Client::new(base, "rh-token-1");
        let result = client.wait_operation(9, 1, 30).await.expect("等待应成功");
        assert_eq!(result["complete"], true);
        assert_eq!(result["status"], "SUCCEEDED");
        assert!(*state.operation_polls.lock().unwrap() >= 3);
    }

    #[tokio::test]
    async fn operation_events_uses_after_cursor() {
        let (base, _) = mock_app().await;
        let client = V2Client::new(base, "rh-token-1");
        let data = client.operation_events(9, 5).await.expect("请求应成功");
        assert_eq!(data[0]["eventType"], "run.created");
        drop(client);
    }

    #[tokio::test]
    async fn is_terminal_status_recognizes_terminal_states() {
        assert!(is_terminal_status("SUCCEEDED"));
        assert!(is_terminal_status("FAILED"));
        assert!(is_terminal_status("CANCELLED"));
        assert!(!is_terminal_status("RUNNING"));
        assert!(!is_terminal_status("QUEUED"));
    }

    /// `run_request` 的请求体形状（此前这条测试住在 `cli.rs`，CLI 改成表驱动后挪到这里
    /// ——它测的本来就是 client 的构造函数）。资产为 `None` 时**一个字段都不加**，
    /// 给了就原样带上（keyed 对象）。
    #[test]
    fn run_request_uses_supplied_revision() {
        let request = run_request(7, json!({ "prompt": "a cat" }), None);
        assert_eq!(request[0]["recipeRevisionId"], 7);
        assert_eq!(request[0]["inputs"]["prompt"], "a cat");
        assert!(request[0]["clientRunId"]
            .as_str()
            .unwrap()
            .starts_with("run-"));
        assert!(
            request[0].get("assets").is_none(),
            "没给资产就不许凭空加字段"
        );

        let assets = json!({ "475::image": { "assetId": 12, "checksum": "sha1-x" } });
        let request = run_request(7, json!({ "prompt": "a cat" }), Some(assets.clone()));
        assert_eq!(request[0]["assets"], assets, "资产必须真的进请求体");
    }

    /// 204（无响应体）是**成功**——`POST /api/daemon/shutdown` 就是这样回的。
    ///
    /// 负对照：删掉 `unwrap_envelope` 里 `status == 204` 那条分支 ⇒ 第一条断言红
    /// （`daemon stop` 会把一次成功的关闭报成 `DAEMON_ERROR`）。
    #[test]
    fn an_empty_204_is_a_success_not_an_error() {
        assert!(
            V2Client::unwrap_envelope(204, Value::Null).is_ok(),
            "空 body + 204 是成功"
        );
        // 带 body 的错误仍然是错误（鉴权失败回 401 + 信封），不能一起放过。
        let error = V2Client::unwrap_envelope(
            401,
            json!({ "ok": false, "error": { "code": "UNAUTHENTICATED" } }),
        )
        .expect_err("401 应当报错");
        assert_eq!(error.code, "UNAUTHENTICATED");
    }
}
