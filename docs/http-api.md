# HTTP API：把它当成一个后端

这份文档写给**要把 RenderLoft 接进自己流水线的人**：认证怎么来、一次调用按什么顺序、
拿到的字段是什么意思、哪些错误可以重试。它不教产品怎么用（那是
[`agent-cookbook.md`](agent-cookbook.md)），也不讲内部设计（那是
[`../ARCHITECTURE.md`](../ARCHITECTURE.md)）。

daemon 是唯一业务宿主；GUI、CLI、MCP 都只是它的客户端。所以"纯后端"用法是**原生**用法，
不是改造出来的：脚本直接打 HTTP，界面可以一次都不开。

---

## 1. 认证

所有请求（除 `/health`）都要：

```http
Authorization: Bearer <token>
```

token 是 **actor token**，权限是一串 scope。签发/吊销都走 API（要 `actor-tokens:manage`）：

```http
POST /api/v1/actor-tokens
{ "actorId": "pipeline-a", "clientId": "renderloft", 
  "scopes": ["recipes:read", "operations:read", "operations:execute", "assets:write", "artifacts:read"],
  "label": "上游流水线", "ttlSeconds": 2592000 }
```

常用 scope：

| scope | 给谁 |
|---|---|
| `recipes:read` | 读应用与字段 schema（**最常被忽略但必须要**：不知道字段键就填不对） |
| `operations:execute` | 提交任务（花钱的那一个） |
| `operations:read` | 查进度、读交付清单 |
| `assets:write` | 上传素材换 `assetId` |
| `artifacts:read` | 取产物元数据与字节 |
| `operations:cancel` / `operations:delete` | 取消 / 删除（不是必需品） |

⚠️ 启动时 daemon 会用一个本机的 **bootstrap token**（数据目录里的 `v2-bootstrap.token`，
以及 `daemon-endpoint.json` 里的 endpoint+token）给界面用。**不要把它发给外部集成方**——
按调用方各签一把受限 token，出事了吊销一把就走。

## 2. 暴露边界

- 默认只监听 **`127.0.0.1:31337`**。跨机器访问要设 `RENDERLOFT_BIND=0.0.0.0:31337`，
  并配 `RENDERLOFT_ALLOW_HOST=<允许的 Host>`（防 DNS rebinding）。
- **没有 TLS**。对外必须挂反向代理（Caddy / nginx），并想清楚"token 即全部权限"。
- 一个工作区一份数据（SQLite + 产物目录）；没有多租户，也不需要。

## 3. 一次调用的顺序

```http
# ① 这个应用能填什么
GET /api/v1/recipes                       → 应用列表（含 currentRevisionId）
GET /api/v1/recipes/35/schema             → { revisionId, inputSchema, defaults, modelOptions }

# ② 素材（可选）：先换成 assetId
POST /api/v1/assets?filename=a.png&mimeType=image%2Fpng
Content-Type: image/png
<原始字节>                                  （单件 ≤ 32 MB，body 就是文件本身）
                                           → { assetId, checksum }

# ③ 预检：只读，不建任务、不调度、不花钱
POST /api/v1/operations/validate
{ "runs": [ { "recipeRevisionId": 39, "inputs": { "5::text": "…" } } ] }

# ④ 提交
POST /api/v1/operations
{ "idempotencyKey": "batch-2026-10-28-a",
  "metadata": { "batch": "a" },
  "callback": { "url": "https://your.svc/hook", "events": ["operation.terminal"] },
  "runs": [
    { "clientRunId": "SKU135-正面", "recipeRevisionId": 39,
      "inputs": { "5::text": "海报设计：…", "7::width": 1024, "7::height": 1024 } },
    { "clientRunId": "SKU135-背面", "recipeRevisionId": 39,
      "inputs": { "5::text": "海报设计：…" } }
  ] }

← { "operationId": 36, "status": "QUEUED", "runIds": [57,58],
    "statusUrl": "/api/v1/operations/36", "eventsUrl": "/api/v1/operations/36/events" }
```

字段规则：

- `recipeRevisionId` **每条 run 各自带**（一批里可以混不同应用）。
- `clientRunId` **可选**：给了才进产物文件名（`SKU135-正面-应用名-57000.png`），
  不给服务端按序号补 `run-N`，那种 id 不进文件名。
- `assets` 按**字段键**挂：`{ "475::image": { "assetId": 12, "checksum": "…" } }`，
  别用数组下标（位置不是语义，这是错位事故的根因）。
- 一次最多 **100 条 run**；超了直接拒（`RUN_LIMIT_EXCEEDED`）。

## 4. 等结果：三种姿势

| 方式 | 接口 | 适合 |
|---|---|---|
| 轮询 | `GET /api/v1/operations/{id}` | 最简单；`status` 到终态就停 |
| SSE | `GET /api/v1/operations/{id}/events` | 想实时推进度 |
| Webhook | 提交时给 `callback.url` | 提交完就走，别占着连接 |

回调是一份**终态快照**，带签名头 `X-RenderLoft-Signature: t=<毫秒>,v1=<hex>`，接收端按
原始字节验签（见 [`callbacks.md`](callbacks.md)）。失败会重试。

## 5. 拿结果：交付清单

```http
GET /api/v1/operations/36/manifest
```

一次给全（不用再拼"任务 → run → 产物"三个接口）：

| 层级 | 字段 |
|---|---|
| 批次 | `operationId`、`status`（`SUCCEEDED` / `PARTIAL` / `FAILED` / `CANCELLED`）、`createdAt`、`runCount`、`artifactCount` |
| 每条 run | `runId`、`label`（调用方起的名字，没起就是 `null`）、`sequence`（**0 基**）、`status`、`connectionId` / `connectionName`、`usage`（平台报的 `{amount, unit, taskCostTime}`）、`inputs`（提交时的原样输入）、`error`、`createdAt` |
| 每个产物 | `artifactId`、`filename`、`mimeType`、`byteSize`、`checksum`、`remoteUrl`、`createdAt`、`bytesPath`、`savePath` |

三处语义必须看清：

- **`byteSize == 0`**：字节还没落本地（在平台上）。取一次（`bytesPath`）之后就变成真实大小。
- **`checksum`**：`null` = 字节还没经过我们、指纹未知；取过字节后是 `sha256-<hex>`。
  ⚠️ 它**不是**"某种固定算法"的承诺——驱动自报的旧数据是 `sha1-…`（历史命名，实为 64 位 FNV）。
- **`remoteUrl`**：平台原始链接，**会过期**（经验上约两周）。它是"此刻能取"，不是交付地址；
  长期引用请用 `bytesPath`。

## 6. 语义保证（这些可以直接写进你的重试逻辑）

| 场景 | 保证 |
|---|---|
| 同一 `idempotencyKey` + 同一请求 | 回放原任务，**不会再花一次钱** |
| 同一 key + 不同请求 | `IDEMPOTENCY_CONFLICT`，拒 |
| 一条 run 失败 | 其余照跑；批次状态是 `PARTIAL`，失败那条带 `error` 与 `retryable` |
| 提交后超时 / 5xx | `SUBMISSION_UNCERTAIN`——**结果不明**，服务端不会自动重投（可能已计费） |
| 传输层能证明"字节没出去"的失败 | 自动退避重试（连接被拒 / DNS 失败 / 握手超时） |
| 提交频率 | 每个调用方 60 次/分钟；超了 `429` + `SUBMISSION_RATE_LIMITED`，带 `retryAfterSeconds` |

**你该做的事**：拿 `retryable` 决定要不要重试；拿 `status == PARTIAL` 决定这批能不能交付；
拿 `SUBMISSION_UNCERTAIN` 去对账（或问人），**不要**自己盲目重投。

## 7. 错误信封

所有失败都是同一种形状，`code` 是稳定契约（`message` 可能变）：

```json
{ "contractVersion": "1", "ok": false,
  "error": { "kind": "Validation", "code": "RUN_LIMIT_EXCEEDED",
             "message": "单个 Operation 最多 100 个 Run", "retryable": false, "details": {} } }
```

常见码：`SCOPE_REQUIRED`（403）、`OPERATION_NOT_FOUND`（404）、`RUN_LIMIT_EXCEEDED`（400）、
`DUPLICATE_CLIENT_RUN_ID`（400）、`IDEMPOTENCY_CONFLICT`（409）、`SUBMISSION_RATE_LIMITED`（429）、
`PROVIDER_RATE_LIMITED`（429，平台侧忙）。

## 8. 这个后端**不**负责什么（边界）

- 不做编排：多步串行、条件分支、失败后"换什么再跑"都是你的上游决定。
- 不聚合报表：失败率、成本趋势自己按 `manifest` 算（数据都给全了）。
- 不改写提示词、不评估质量、不替你挑图。
- 不长期托管你的字节：产物缓存跟着任务走，删除/过期一起清（保留期可设，`0` = 永久）。

## 9. 已知缺口（诚实清单）

- `GET /api/v1/openapi.json` 只声明**路径与方法**，没有请求/响应 schema —— 集成以本文为准。
- 回调**投递状态**没有查询接口：重试记录在库里、日志里能看，但拿不到"我的 hook 失败了几次"。
- 没有服务化封装（Windows 服务 / systemd / 容器）：daemon 需要被拉起并守住，
  现在只有 CLI 的"顺手启动"，那只适合本机。
