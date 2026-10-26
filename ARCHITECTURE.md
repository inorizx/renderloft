# RenderLoft 架构

这是一份稳定的架构说明，只记录长期边界、核心不变量和技术选择。代码、测试和
HTTP DTO 是当前实现的最终事实来源；普通功能变化不需要同步维护本文。

## 总体模型

RenderLoft 是一个本地优先的模块化单体：Rust Daemon 持有业务状态和执行生命周期，
Vue、Tauri、CLI、MCP 和外部 Agent 通过统一接口使用它。

```text
Vue GUI / Tauri / CLI / MCP / Agent
                 │
             HTTP / SSE
                 │
          Rust local Daemon
                 │
       Application + Domain Core
                 │
                Ports
       ┌─────────┼─────────┐
    SQLite   ArtifactStore  SecretStore
                 │
          Provider Drivers
   RunningHub / ComfyUI / Model APIs
```

## 核心产品模型

外部平台提供生成能力，RenderLoft 将它们包装成可执行的 AI 应用：

```text
RunningHub App / Workflow
ComfyUI API Prompt
Model API
        ↓
      Recipe
        ↓
  Schema + Binding + Connection
        ↓
 Operation → Run → Attempt
        ↓
      Artifact
```

| 领域模型 | 产品含义 |
|---|---|
| `Recipe` / `RecipeRevision` | AI 应用及其可执行版本 |
| `Connection` / `ConnectionRevision` | 平台或模型 API 的连接及配置快照 |
| `Operation` | 用户发起的一次任务，可包含多个 Run |
| `Run` | 单次或批量任务中的一项执行 |
| `RunAttempt` | 提交、重试和恢复历史 |
| `Asset` | 用户提供的输入素材 |
| `Artifact` | 生成的结果 |
| `Agent` / `Delivery` | 建立在应用之上的受限目标编排入口 |

RunningHub、ComfyUI 和模型 API 的差异留在 Provider Driver 中；核心只处理应用、
输入、任务、状态和结果。ComfyUI 负责工作流创作，RenderLoft 不做节点编辑器。

## 长期不变量

### 一个执行核心

GUI、CLI、MCP 和 Agent 不能各自实现运行逻辑。所有执行都必须经过同一套应用用例，
从而共享输入校验、权限、幂等、调度、取消、重试和结果处理。

### Daemon 是唯一宿主

Rust Daemon 负责：

- 工作区状态和 SQLite 持久化；
- Provider 请求和凭据解析；
- Scheduler、Reconcile 和回调投递；
- Operation、Run、Attempt、Event 和 Artifact 生命周期；
- HTTP / SSE API。

Vue 只负责界面、临时表单状态和客户端缓存。Tauri 只负责桌面窗口和 Daemon 生命周期。
CLI、MCP 和外部 Agent 都是客户端。

### Provider 只负责外部平台适配

Provider Driver 负责构造请求、注入凭据、上传素材、解析响应和状态、下载并规范化结果。
Provider Driver 不得修改核心数据库、决定核心状态转换、绕过 Scheduler 提交任务、
管理 Actor 权限或持久化密钥。

### 任务必须可恢复

任务先在本地持久化，再调用外部服务。Daemon 重启、网络超时或 UI 断开不能让任务状态
消失，也不能因为不确定就盲目重复提交。

```text
创建任务 → 持久化 → 调度提交 → 查询 / 对账 → 规范化结果
```

批量执行表现为一个 Operation 包含多个独立 Run；每个 Run 可以单独成功、失败、取消
和重试。提交超时或断连形成 `SUBMISSION_UNCERTAIN`，必须先对账或明确处理，不能盲目重投。

### 资产的身份是字段键，不是数组位置

媒体资产（用户上传的素材）的语义身份是 **(recipe revision, 字段键)**：字段键是 `inputSchema`
里的键，形如 `<节点ID>::<字段名>`（`475::image`）。**数组位置只是旧的传输编码**——同一份数据
在不同环节的顺序由上游节点声明、schema 键序、调用方拼装各自决定，把它们当成语义就会产生
「第二格贴了第 4 张图」这类静默错位。

- 内部与 Provider 驱动只认字段键：上传后的素材写入媒体映射（`mediaMapping`）指定的 `(节点, 字段)`；
- 位置数组仅在 HTTP 兼容层被接受，且必须先经统一入口换算成字段键再用；新 revision 的 keyed
  映射收到位置数组会被明确拒绝（`POSITIONAL_ASSETS_UNSUPPORTED`），不猜测；
- 新导入/新追加的 revision 一律写 keyed `mediaMapping`；旧 revision 通过同一条归一化路径继续可读；
- 未知字段键、缺槽位、资产缺失或 checksum 不符都在**花钱之前**报稳定错误码，不允许静默丢弃。

上游应用/工作流怎么定义槽位是上游的事，RenderLoft 只做适配，不解析上游文案、不重排字段。

### 花钱的闸门写在代码里

**闸门必须是代码里的一条硬规则，不能是提示词里的一句「请谨慎」**——提示词约束不是约束。
MCP 是**自动批准**的入口（外界 AI 自己决定要跑什么），所以它按 actor 限并发
（`MAX_AUTONOMOUS_CONCURRENT_OPERATIONS`，见 `application/create_operation.rs`）；
界面与 CLI 是人自己决定跑多少的，不设同一道闸（界面的批量是**一个 Operation 带 N 个 Run**，
按 Operation 计数不会误伤批量）。

⚠️ 但**闸门不复制平台的排队**：本地并发上限（我们自己的）与平台的排队长度是两件事。
平台**声明**的并发容量（RunningHub 的 `concurrentLimit`）是用户买到的东西，读它、用它，
否则买 3 个位只用 1 个；而**排队**（`queuedCount`）属于服务商的作业调度，我们只观测、
不参与决策。

三条边界保证"读它"不会变成"依赖它"：

- **读不到就退回保守值**（`available_slots()`，默认 1），接口失败只让速度变慢，不让任务卡住；
- **占用取两侧的较大值**：平台报的 `runningCount`（含网页控制台、别的工具的提交）与本地账本，
  谁大信谁——宁可少领一条，也不要撞一次拒绝；
- **缓存 30 秒 + 被拒即失效**：2 秒一轮不能每轮都问平台；平台说"满了"时立刻丢掉旧数字。

平台拒绝（`TASK_QUEUE_MAXED`）仍然是安全网：拒一次就停手本轮、退避 30 秒，且有死线兜底。
我们只负责约束自己的副作用。

### 凭据不进入业务数据

用户输入的 API Key 可以通过经过认证的本地 GUI 管理请求传给 Daemon；之后只由 Rust
通过 `SecretStore` 解析，默认保存在操作系统凭据库中。浏览器只持有编辑期间的临时表单值，
不拥有凭据事实来源。用户明确请求时，认证的本地 GUI 可以读取已保存的密钥用于显示或复制。
原始密钥不进入 SQLite，也不出现在 Recipe、ConnectionRevision、事件、日志、CLI、MCP、
诊断或错误响应中。

## 分层

- **Domain**：纯领域模型、状态和不变量，不做 IO。
- **Application**：应用用例和事务边界。
- **Ports**：Repository、Provider、Secret、Policy 等接口。
- **Infrastructure**：SQLite、凭据库、存储和 Provider Driver。
- **Transports / Clients**：HTTP、SSE、CLI、MCP 等协议适配器。
- **Vue / Tauri**：Vue 负责界面和客户端状态，Tauri 只负责桌面窗口与 Daemon 生命周期。

依赖方向保持向内：`transports → application → ports/domain`，具体实现由
`infrastructure` 提供并在 Daemon 组合根注入。Application 和 transports 不得反向
依赖具体 infrastructure。代码目录以仓库实际结构为准，不在本文重复维护。

## 技术选择

- **Rust 本地服务**：负责长期运行、网络与文件控制、状态边界，以及 GUI、CLI 和 MCP 共享的本地服务。
- **SQLite**：本地工作区的唯一事实来源；保存任务、重试、事件、连接、应用和结果元数据，不需要用户安装数据库服务。
- **Vue + TypeScript**：构建参数表单、批量任务和结果工作区，并保持 API DTO、Schema 表单和组件契约清晰。
- **HTTP / SSE**：GUI、CLI、MCP 和未来客户端共享的机器接口；Tauri 不提供第二套业务 IPC。
- **显式 Provider Driver**：平台认证、字段映射、上传、轮询和结果格式差异很大，显式 Driver 比任意 HTTP 模板更容易测试、限制和审计。
- **SecretStore**：把 Windows Credential Manager 等系统凭据实现隔离在基础设施层。

当前具体框架、库版本和命令以 `package.json`、`Cargo.toml` 和脚本为准，本文不重复维护易变细节。

## 关键架构决策

1. **本地 Daemon 是唯一宿主**：桌面壳和客户端不持有业务状态。
2. **Recipe 是统一应用模型**：不同来源通过各自适配器落到同一套参数、任务和结果模型。
3. **Provider 使用显式 Driver**：平台差异不泄漏到核心，也不暴露任意网络能力。
4. **任务先持久化再提交**：Operation → Run → Attempt 记录批量、重试、恢复和不确定提交。
5. **自主调用方必须设闸**：MCP 是自动批准的入口，按 actor 限并发；闸门写在代码里，
   不写在提示词里（见上面「花钱的闸门写在代码里」）。**表单路径不依赖 LLM，始终是第一公民。**
6. **密钥留在 SecretStore**：API Key 可由本地认证 GUI 提交给 Daemon，但不进入 SQLite，
   也不暴露给 CLI、MCP、日志、诊断或错误响应。
7. **资产按字段键定位**：媒体资产的语义身份是 `(recipe revision, 字段键)`；位置数组只是
   HTTP 兼容编码，进入核心前统一换算成字段键，新 revision 直接拒绝数组（见「资产的身份是
   字段键，不是数组位置」）。

## 修改本文的条件

只有以下变化需要更新本文：

- 改变 Daemon 的宿主职责；
- 增加新的持久化事实来源；
- 改变 Provider Driver 与 Scheduler 的边界；
- 让客户端绕过统一应用用例；
- 改变密钥、文件或远程访问的信任边界；
- 改变 Recipe、Operation、Run 或 Artifact 的核心语义；
- 放宽自主调用方（MCP）的闸门，或改变「闸门不复制平台约束」那条取舍。

重大架构变化应在提交说明中记录背景、取舍和验证方式。普通 UI 调整、新增字段、单个
Provider 的 API 适配和内部重构，优先通过代码、测试和局部注释表达。
