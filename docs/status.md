# RenderLoft 当前状态

这是一份会随项目变化而更新的状态摘要。具体行为以代码和测试为准。

## 已完成

- V2 本地 Rust Daemon、SQLite 工作区和 `/api/v1` HTTP/SSE 接口。
- Recipe / Connection / Operation / Run / Attempt / Asset / Artifact 核心模型。
- 持久化调度、Provider 提交、远端对账、取消、重试和结果归档。
- RunningHub 国内 / 国际连接、API Key 凭据存储、应用链接或 ID 导入、参数读取、媒体上传、任务查询和结果下载。
- 稳定架构和技术边界见根目录 [`ARCHITECTURE.md`](../ARCHITECTURE.md)。
- ComfyUI 本地或远程 API 工作流执行。
- ComfyUI API Prompt 导入与参数发现：导出「保存(API 格式)」的 workflow JSON 后粘贴导入，自动识别文本、采样（seed/steps/cfg/denoise）与 LoadImage 字段，生成 inputSchema 与 fieldMapping/mediaMapping，见应用库「从 ComfyUI API Prompt 添加」。
- OpenAI Images 及兼容图像 API 接入：连接上可**拉取平台声明的模型清单**并勾选这条连接能跑的
  几个，运行表单的 `model` 字段据此变成下拉；连接级 `defaultModel` 在运行与配方都没点名时兜底。
- 凭据引用：ComfyUI 跑在 RunningHub 云端（地址带 `/proxy`）时**借用**一条 RunningHub 连接的密钥
  ——引用的是**绑定**不是密钥，所以密钥不出凭据库、那边轮换自动生效、被引用的连接删掉/停用会立刻报错。
- 密钥「身份可见、值不可见」：连接详情只回**派生**的指纹（尾 4 位 + 长度），界面显示「已保存 · 尾号 …」
  徽章；密钥本体永不出凭据库。
- GUI、CLI、MCP 共享同一套应用执行核心。
- 常驻托盘：点 X **不是退出而是收进托盘**（一次云端出图是分钟级，用户必然切走）。
  关闭后外壳与 daemon 都留着：托盘悬停显示"正在跑几个任务"，任务进终态发系统通知，
  左键/菜单打开控制台，只有托盘菜单的「退出」才停 daemon（平台侧的任务照旧跑完、下次对账回来）。
  **单实例**：第二次启动把已有窗口叫到前台后自己退出（不会出现两个托盘图标）；
  托盘建不出来时**退回"关窗即退出"**（不会给出一个关不掉的窗口）。
  外壳另有自己的 `shell.log`（生命周期事件；GUI 程序没有控制台，不留日志就等于没有）。
- 前端、Rust 单元测试、分层检查和构建验证链路。
- 任务产品化：右侧实时任务画廊、任务历史画廊、结果预览、应用名展示、失败重试、任务删除、清空已结束任务、完成通知和未读角标。
- 任务可靠性：run 对账截止时间、陈旧操作卫生清扫、终态历史保留清理、provider 轮询异常恢复和孤儿 Operation 兜底。
- 凭据布局：**每条连接 revision 一个独立槽位**（`connection-{id}-api_key`），不再是全局共享槽位；
  密钥只进系统凭据库，daemon 从不回传它（界面只显示派生指纹）。跨连接借用见「凭据引用」。
- 界面双语（简体中文 / English）：全部页面、共享组件与 API 层运行时错误消息的可见文案已接入 i18n（中英各一份），
  覆盖表单校验提示、原生确认框、无障碍标签与空态文案；语言在「服务配置」中切换，未选择时按系统语言确定。
- 打包分发可用：NSIS 安装包现在自带控制台静态资源（`dist/` 作为 bundle resource），daemon 按
  「显式覆盖 → 可执行文件旁的 `dist` → 源码树」解析 UI 根目录 —— 装到没有源码的机器上也能打开界面
  （此前 UI 根目录是**编译期**源码路径，装到别的机器只会回一屏 JSON）。
- 命名与目录收尾：数据目录为 `%LOCALAPPDATA%\renderloft-workspace`、凭据服务为 `renderloft-v2`
  （四处手写字符串必须同源，`scripts/check-layers.mjs` 跨文件守门）——
  目录名刻意**避开安装目录**（NSIS 默认装到 `%LOCALAPPDATA%\RenderLoft`，同名会让程序文件与用户数据
  混在一个文件夹里，实测过）。
- 发布链路：版本号收敛为**单一真相来源**（根 `package.json`，其余 5 处由 `scripts/version.mjs` 同步，
  `pnpm verify` 与 CI 都校验一致）；`v*` tag 触发 `.github/workflows/release.yml` 打包并发布
  GitHub Release（安装包 + 免安装 zip + Linux 便携包 tar.gz + `SHA256SUMS.txt`），发布前校验 tag 与版本号一致。
- 命令行可用性：安装包会把安装目录加进**用户 PATH**（`desktop/nsis/hooks.nsh`，卸载时摘除；
  逻辑有 30 条断言的独立测试 `scripts/test-nsis-path-hook.mjs`）；CLI 失败时信封走 stderr、
  退出码 1，成功走 stdout、退出码 0 —— 所以任务计划里 `if ($LASTEXITCODE -ne 0)` 是有效的。
- 客户端自愈：CLI 与 MCP 发现 daemon 没在跑时会自己把旁边的 `renderloft.exe` 拉起来并等它健康
  （**只启动，不拥有**：不代管其生死、不改环境变量、输出落 `daemon.log`），连接失败时把
  "自动启动也失败"的原因一并报出；`tools/list` 仍是本地常量，daemon 不在也能先看见工具。
- 完成回调（webhook）：`operation run --callback-url` / MCP 的 `callbackUrl`。任务进终态时
  daemon POST 一份终态快照（`operationId` / `status` / `runs`）到该地址，失败按退避重试并留投递记录。
  触发方式是**后台按状态补扫**（不是在各终态出口各挂一处）：于是 reconcile / scheduler / cancel /
  hygiene 哪条路把任务推到终态都覆盖，**daemon 当时没在跑、重启后也会补上**（24 小时窗口内），
  且幂等（已有投递记录就不再补）。
  地址规则是 **https，或回环地址的 http**（`127.0.0.1` / `localhost` / `[::1]`；内网与公网只收 https）。
  可配 **HMAC-SHA256 签名**（头 `X-RenderLoft-Signature: t=…,v1=…`，被签名的是 `"{t}.{请求体原始字节}"`），
  接收端据此验证来源并防重放；密钥存系统凭据库、**只进不出**，写入只有 CLI 一条路
  （`callback set-secret` / `status` / `clear-secret`，MCP 无对应工具）——没配密钥时不带签名头。
- 后台循环的日志改为"写失败就算了"：此前 stdout/stderr 一旦断开（管道关闭、日志文件被删等），
  循环每轮的日志写入会 panic 并把该循环静默带走——症状是任务提交成功却永远停在 `ACCEPTED`，
  看起来像平台卡住。启动期日志仍保持"写不出去就大声失败"。
- Linux 无头交付：CLI / 网页控制台 / MCP 三入口共用同一个 daemon；`pnpm build:linux` 产出
  `RenderLoft_<版本>_x64-linux.tar.gz`（三件套 + `dist/`，解压即用），CI 的 `linux-portable`
  job 从**解压出来的包**做全链路冒烟（CLI → daemon → 控制台 → MCP），防打包静默烂掉。
  Linux 不做桌面壳；密钥走系统密钥环（Secret Service）。
- 媒体资产按**字段键**定位（根因修复）：资产的语义身份是 `(recipe revision, 字段键)`（如 `475::image`），
  数组位置只是 HTTP 兼容编码。新导入/新追加的 revision 一律写 keyed `mediaMapping`；旧 revision 经同一
  归一化路径继续可读；keyed revision 收到位置数组明确拒绝（`POSITIONAL_ASSETS_UNSUPPORTED`）。
  Provider（RunningHub / ComfyUI）按字段键写入节点槽位，不再按下标或扫描工作流猜目标；
  未知字段键/资产缺失/checksum 不符在提交前报稳定错误码（避免 scheduler 读到空字节把空文件发上去）。
  媒体提交闭环补齐：CLI `operation run --asset '字段键=文件'`（自动上传）与 `--dry-run`（只读预检，
  与提交共用判据、不落库不花钱）、MCP 的 `assets`（keyed 对象）与 `dryRun`（runs[] 里的 assets 此前会被
  静默丢弃）；GUI 解除「必须连续填文件槽位」的提交限制。Agent 实操见 [`agent-cookbook.md`](agent-cookbook.md)。

- **迁移与备份**：工作区本身就是普通文件（`%LOCALAPPDATA%\renderloft-workspace` 的库 + `artifacts\`），
  换机器＝拷目录、恢复＝换回去；README 的「你的东西在哪」写明了路径与"密钥必须由人重填"。
  只读动词 `workspace backup`（CLI `workspace backup --to <目录>`、MCP `renderloft_backup_workspace`）
  用 `VACUUM INTO` 导出一份**一致**的快照并整份拷 `artifacts\`——因为运行中的 WAL 库直接拷会缺数据
  甚至打不开，而且**不报错**。恢复**没有**对应动词（`daemon stop` → 换目录 → 任意 CLI 命令把 daemon
  拉起来）。验收判据（备份出来的库能重新打开、数据与源一致）有**真库**测试钉住；四道校验各有负对照
  （停用守卫后那四条测试实测变红）。界面上还没做按钮（CLI / MCP 已可用）。

- **任务卡住时「为什么没在动」**：每个"在等什么"的非终态 run 都给出稳定原因码 + 一句人话 +
  死线 + 到点会怎样 + 现在能做什么（`operation get` 的 operation 级与每个 run 上都有）。
  摘要列表（`operation list`）也带同一判据的**精简版**（只有原因码与死线，人话与 nextActions
  留在 `get`）—— 因为调用方是**先 list 再 get** 的：只有 `get` 有，等于逼它为列表里每一条各打
  一次 `get` 才知道该看哪一个。原因码与严重度（不确定 > 退避 > 提交中 > 排队）**只有一张表**
  （`domain::operation::ATTENTION_LADDER`）：`get` 的挑选与 `list` 摘要那条相关子查询的
  `IN` / `ORDER BY CASE` 都由它生成，不手抄（一条相关子查询，不是 N+1）。
- **`operation retry` 进 CLI / MCP**：`renderloft-cli operation retry --operation <id> [--run <id>]`
  / MCP `renderloft_retry_operation`。此前它**只有 HTTP**（界面点得到，CLI / MCP 够不着），
  而 `attention` 的 `nextActions` 早就把 `operation.retry` 写成"下一步能做什么"——名字够不到就是
  死路（有守卫钉住"`nextActions` 点到的名字必须是表里真实存在的动词"）。它是**新建一个 operation**
  （复用原 run 的配方修订、inputs 与 assets；`clientRunId` 带 `-retry-<时间戳>`、新 idempotencyKey），
  所以**返回的是新 operationId，要改盯新的那个**，且每个重试的 run 都会**再计费一次**；
  `--run` 只重试一条 —— 批量里只失败了几条时用它，重跑整个任务会把已成功的那几条也再提交一次
  （也就是再花一次钱）。
- **提交结果不明时，「字节到底出没出去」有证据了**：此前 run 落 `SUBMISSION_UNCERTAIN` 时那次
  attempt 的 `remote_handle_json` 与 `error_json` **都是空的**，事件里只有一句不含原因链的话，
  日志那一行要等到死线才写 ⇒「连接被拒」（字节一定没出去）与「发出去后超时」（可能已经在平台上
  跑起来了）在库里长得一模一样。现在网络失败的结构化错误会**存到那次 attempt 上**，并带一个
  判据 `nothingSent`：只有**连接被拒 / DNS 解析失败 / 握手阶段超时**才为 `true`
  （证据码写在 `nothingSentEvidence`），其余（发送后超时、连接被重置…）一律 `false`。
  ⚠️ 它**不是"可以重投"的许可**：模糊类（`false`）要能安全重投，需要**平台侧去重**，而幂等指纹
  从来没发给平台（`RecoveryProof` 在生产代码里没有生产者）——所以那一类**绝不自动重投**
  （哪一类会自己重试，见下一条）。判据与查法见 [`agent-cookbook.md`](agent-cookbook.md) §11d。
- **证明得了「字节没出去」的提交失败会自己重试**：连接被拒 / DNS 解析失败 / 握手阶段超时这三类，
  平台**什么都没收到**（重投不可能重复扣费），所以不再落「等确认」空转到死线（默认 1 小时），
  而是走**已有的**退避重试（`RETRY_BACKOFF`，30 秒后自动重试，与平台拒绝/限流/5xx 同一档语义）。
  证明不了的那一类（发送后超时、连接被重置）**照旧等死线、绝不自动重投**——那才是"可能已经收下"。
  判据只有一处（`submission_is_unknown`）：**落状态与 tick 的计数同源**（写这条时实测抓到过一处
  分歧：只改落状态那处，run 进了 `RETRY_BACKOFF` 而计数仍报 uncertain）。
- **等待确认的 run 不再占住连接唯一的执行位**：一个 `SUBMISSION_UNCERTAIN` 的 run 此前会占满
  这条连接唯一的执行位（默认并发 1）**直到死线（默认 1 小时）**，后面的 run 全排队 —— 而平台上
  可能一个都没在跑。现在占位只算 `ACCEPTED` / `RUNNING`（真的在平台上跑的）；撞上平台上限时
  平台会拒，而 RunningHub 的拒绝（`TASK_QUEUE_MAXED`，实测"拒了一分钱没花"）本来就是可重试的
  ⇒ 退避 30 秒自动重试。对账队列（`list_active_runs`）**照旧**包含不确定的 run，否则它永远不会
  被确认。

## 当前进行中

- 应用参数预设、批量运行和结果工作区的产品化体验。

## 暂未实现或暂缓

- **模糊类（"可能已经收下"）的自动重投还没接**：能证明"字节没出去"的那一类已经会自己重试了；
  剩下的那一类（发送后超时、连接被重置）要能安全重投，需要**平台侧去重**——而幂等指纹
  （`PreparedExecution.request_id`）从来没有发给平台，`RecoveryProof::Idempotent` /
  `LookupByRequestId` 在生产代码里**没有生产者**（scheduler 里那条 `ToRetryWait` 因此不可达）。
  要接它，先查平台能不能收客户端幂等键（或按时间列任务做对账）——那是另一件事（判据见
  [`agent-cookbook.md`](agent-cookbook.md) §11d）。
- A2A Transport 及其 conformance 测试；当前没有 A2A 路由、代码或测试。
- Prompt Assistant 的生成端点：**评估后不做**（2026-09-22）。参数层的"这个字段该填什么"已由提示词模板与官方示例变量覆盖
  （确定性、可复现），任务层的"给个目标自动完成"由外部 AI 通过 MCP 驱动（见 README 的「用 AI 驱动它」）；
  居中的"AI 帮忙润色单个字段"两边都不需要。
  现有的 profile CRUD 与模板管理继续保留（它们服务的是模板配置，不是生成）。
- RunningHub 应用市场的站内搜索、作者浏览和完整发现体验。
- 发布渠道：**GitHub 是发布渠道，Gitee 是代码镜像**。CI（`.github/workflows/ci.yml`，push 触发）
  与 Release（`.github/workflows/release.yml`，`v*` tag 触发）都在 GitHub 上跑；Release 产出
  NSIS 安装包 + 免安装 zip + Linux 便携包 tar.gz + `SHA256SUMS.txt`（Linux 构建失败会拦截发布）。Gitee 同步代码、tag 与 Release 附件（附件由本机手动搬运，Gitee 不跑 CI）。

## 当前产品优先级

```text
连接平台 → 导入或配置 AI 应用 → 参数化运行 → 批量提高效率 → 统一管理任务与结果 → MCP / CLI 自动化
```

表单路径不依赖 LLM，始终是第一公民。

## 如何更新本文

只有当功能状态、完成范围或近期重点发生变化时才更新本文。不要把临时调试信息、分支名称、提交列表或详细实现说明写进来；这些内容应留在 Git、代码、测试或临时工作记录中。
