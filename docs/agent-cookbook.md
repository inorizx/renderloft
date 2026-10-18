# Agent Cookbook：从零跑通一个任务（尤其带图的）

给 **AI Agent / 自动化脚本** 的实操手册。前置认知：daemon 是唯一业务宿主，GUI / CLI / MCP
都只是 `/api/v1` 的客户端；你（Agent）通常走 **MCP**（`renderloft_catalog` + `renderloft_call`
两个元工具）或 **CLI**（`renderloft-cli`）。两条路底层完全同构，本文给两版写法。

> 产品边界（先读这条，能省掉一半的调试）：**槽位怎么定义、节点是什么意思，是上游应用/工作流
> 说了算，不是 RenderLoft 能管的。** 我们只做适配：把资产放进「食谱修订里声明的那个字段」。
> 不解析上游的「图1」「图4」这类文案，也不按任何顺序重排你的字段。

---

## 0. 发现：找到「字段键」

媒体任务的寻址单位是**字段键**：`<节点ID>::<字段名>`，例如 `475::image`。它就是你提交时用的
**键**。怎么拿到：

```text
# CLI
renderloft-cli recipe list                 # 有哪些应用（拿 recipeId）
renderloft-cli recipe describe --recipe 1  # 拿 currentRevisionId 与字段 schema
```

```text
# MCP：catalog 列出动词；call 调具体动词
renderloft_call { "verb": "recipe.list", "args": {} }
renderloft_call { "verb": "recipe.describe", "args": { "recipeId": 1 } }
```

`recipe describe` 返回的 `inputSchema.properties` 就是字段键的权威列表。媒体字段长这样：

```json
{ "properties": {
    "475::image": { "type": "string", "format": "file", "description": "…" },
    "520::image": { "type": "string", "format": "file", "description": "…" },
    "prompt":    { "type": "string" }
} }
```

- `format: "file"` ⇒ 这是媒体槽，值走**资产**（见下）；
- 其它字段照常放进 `inputs`；
- **键就是地址**：只填 `475::image` + `520::image` 是合法的（不必从第一个文件字段开始连续填）。

---

## 1. 文本任务（最简路径）

```bash
renderloft-cli operation run --recipe-revision 7 --input inputs.json
```

```text
renderloft_call { "verb": "operation.run",
  "args": { "recipeRevisionId": 7, "inputs": { "prompt": "a cat on a leash" } } }
```

长任务**别在工具调用里等**，给一个完成回调（见 §4），或短超时轮询 `operation.wait`。

---

## 2. 媒体任务端到端

三步：**拿字段键 → 上传资产 → 按字段键提交**。

### 2a. CLI（一条命令搞定上传+提交）

```bash
renderloft-cli operation run \
  --recipe-revision 7 \
  --input inputs.json \
  --asset '475::image=./person.png' \
  --asset '520::image=./cloth.png'
```

- `--asset` 可重复；形状 `<字段键>=<本机文件路径>`；
- CLI 会**先上传**每个文件（上传不计费），再把 `{assetId, checksum}` 组装成 keyed 对象提交；
- 缺 `=`、同一字段给两次、文件读不到、不是本机路径，都会在提交前报清楚。

### 2b. MCP（先 asset.upload，再按字段键放进 runs[].assets）

```text
# ① 上传（每次一个文件；返回 assetId 与 checksum）
renderloft_call { "verb": "asset.upload", "args": { "path": "/abs/person.png" } }
→ { "assetId": 12, "checksum": "sha1-…", "filename": "person.png" }

# ② 提交：资产放进 **assets**，键 = 字段键；不要放进 inputs、不要按位置排
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7,
  "inputs": { "prompt": "换装" },
  "assets": {
    "475::image": { "assetId": 12, "checksum": "sha1-…" },
    "520::image": { "assetId": 13, "checksum": "sha1-…" }
  }
} }
```

> 只传你想传的槽位：`{475::image, 520::image}` 只会落这两个；其它槽位保持工作流默认。
> **键就是地址**——位置由上游节点顺序决定，**位置不是语义**。

---

## 3. 批量（一个 Operation 多个 Run）

批量用一个调用提交（每行**单独计费**；上限 100 行）。每行可以有**自己的** assets：

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7,
  "runs": [
    { "clientRunId": "look-1", "inputs": { "prompt": "blue" },
      "assets": { "475::image": { "assetId": 12, "checksum": "sha1-…" } } },
    { "clientRunId": "look-2", "inputs": { "prompt": "red" },
      "assets": { "520::image": { "assetId": 13, "checksum": "sha1-…" } } }
  ]
} }
```

CLI 一次一个任务；批量走 MCP 的 `runs`（或 GUI 运行页的「批量运行」模式）。`clientRunId` 是你的对齐锚，
不依赖返回顺序。

---

## 4. 长任务：提交完就走（callback）

```bash
renderloft-cli operation run --recipe-revision 7 --input inputs.json \
  --callback-url http://127.0.0.1:8787/hook
```

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7, "inputs": { … },
  "callbackUrl": "https://your-receiver.example/hook" } }
```

任务进终态时 daemon 会 POST 一份快照（`{operationId, status, runs:[…]}`），失败按退避重试，
投递记录留在库里。地址必须 https，或回环地址的 http。**不要**在 `operation.wait` 里等几分钟——
那会占住 MCP 客户端唯一的请求通道。

> 接收端怎么写、怎么用 HMAC-SHA256 验签（`X-RenderLoft-Signature`）、没收到怎么排查：
> 见 [`callbacks.md`](callbacks.md)。

---

## 5. 提交前预检（dry-run，不花钱）

形状对不上时你先知道，而不是先计费一次再知道：

```bash
renderloft-cli operation run --recipe-revision 7 --input inputs.json \
  --asset '475::image=./person.png' --dry-run
# → {"dryRun": true, "valid": true, "runs": [{ "clientRunId": "…",
#      "assets": [{ "fieldKey": "475::image", "assetId": 12 }] }]}
```

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7, "inputs": { … },
  "assets": { "475::image": { "assetId": 12, "checksum": "sha1-…" } },
  "dryRun": true } }
```

预检与真实提交**共用同一套判据**（revision、输入 schema、字段键、资产存在与 checksum），
但**不创建任务、不调度、不调用上游、不花钱**。CLI 的 `--dry-run` 会先上传资产（上传本身不计费）。

---

## 6. 常见静默失败（都真的发生过）

| 症状 | 原因 | 正确做法 |
|---|---|---|
| 提交"成功"但图是空的/默认图 | assetId 塞进了 `inputs`（schema 媒体字段 format 是 `"file"`，不是 `"asset"`） | 放进 `assets`，键 = 字段键 |
| 图错位（第二格变成了上游的第 4 个槽） | 按**数组位置**猜字段（`assets:[a,b]` 里的第 2 个并不对应界面的第 2 格） | 用字段键；位置数组只在旧 revision 的兼容路径里被换算，新 revision 直接拒绝（`POSITIONAL_ASSETS_UNSUPPORTED`） |
| `ASSET_FIELD_UNKNOWN` | 字段键写错（不在该 revision 的媒体字段集合里） | `recipe describe` 后逐个核对键 |
| `ASSET_COUNT_EXCEEDED` | 走旧的位置数组，且多给了槽位 | 位置数组只是给老客户端/老版本的兼容编码；新提交一律用字段键 |
| `ASSETS_NOT_ALLOWED_WITH_RUNS` | 顶层 `assets` 与 `runs` 同时给了 | 那是两种写法：批量每一行的资产写在该行自己的 `runs[i].assets` 里。歧义不猜，宁可报错也不静默丢一份 |
| `ASSET_NOT_FOUND` / `ASSET_CHECKSUM_MISMATCH` | assetId 不存在、或 checksum 不是该资产的 | 用 `asset.upload` 原样返回的 `assetId`+`checksum` |
| 上游报错但看不出哪个字段 | 只看错误、不看提交的 `inputs` | `operation.get` 回执里带提交时的 inputs；一起读 |

---

## 7. 边界与事实

- **字段键 =（recipe revision, `<节点>::<字段>`）**，这是资产的真实身份；数组位置只是旧传输
  编码（HTTP 上写作 `asset-N` 的位置数组，只给老客户端/老 revision 兼容），内部一律先换算成
  字段键再用。
- RenderLoft **不解释**上游槽位的含义（「图1」是上游文案）；不改上游语义、不重排字段。
- 上传不计费；**提交计费**（RunningHub 每个 run 单独计费）。预检不花钱。
- 自主调用方（MCP）有并发上限（按 Operation 计），铺开大量小调用不如一个 `runs` 批量。
- **回执也是有界的**：列表超过 20 条会截断（上限 20 条 / 64KB），并**明说**"共多少、只给了多少、
  怎么拿更多"——**绝不悄悄丢**（悄悄截断比不截断更坏：模型会以为看到的就是全部）。
  要翻页就传 `limit` / `after`：`operation.list` 与 `artifact.list` 都支持（CLI 上就是
  `operation list` / `artifact list` 的 `--limit` / `--after`）。
- 更细的协议细节（错误信封、分页、截断）见 [`mcp-surface.md`](mcp-surface.md)；
  回调与验签见 [`callbacks.md`](callbacks.md)；CLI 的完整旗标表见 §10
  （MCP 那侧用 `renderloft_catalog` 取每个动词的完整 schema）。

---

## 8. 产物落盘：完整规则

`artifact list` 里 `byteSize` 为 0 的产物，**字节还在平台上**（中转站定位，不落本地）——
用这一条按需拉下来：

```powershell
renderloft-cli artifact download --artifact 999002000 --to D:\pics\        # 给目录（不存在会建）
renderloft-cli artifact download --artifact 999002000 --to D:\pics\cat.png # 或指定文件
```

**文件名一律照抄平台给的**（浏览器、`curl -O`、`wget` 都不改名）。但**往一个目录里收一批**时会撞名：
RunningHub 按输出节点命名，同一个应用的每个 run 都产出 `870.png`。所以目录落点带冲突策略——
默认 `rename`，加序号（`870.png` → `870 (1).png`，与浏览器 / yt-dlp 同形），并在输出里说明改存到哪：

```powershell
renderloft-cli artifact download --artifact 999002001 --to D:\pics\                    # 默认 rename
renderloft-cli artifact download --artifact 999002001 --to D:\pics\ --on-conflict skip # 收过就不动
renderloft-cli artifact download --artifact 999002001 --to D:\pics\ --on-conflict overwrite
```

- `--to <文件>` 是**显式落点**：你说了算，照写（与 `curl -o` 一致）；
- `--to <目录>` 是"你帮我决定名字"，所以由动词兜底。**静默覆盖是最坏的一种**——它不报错，
  事后只能靠翻目录发现少了几张；
- 不给 `--to` = 当前目录 + 产物原名。

**想自己定名字就 `--name-template`**（对应 yt-dlp 的 `-o`）——占位符只有两个，都来自产物元数据：

```powershell
renderloft-cli artifact download --artifact 999002001 --to D:\pics\ --name-template "{artifactId}/{filename}"
#   → D:\pics\999002001\870.png        （一个产物一个子目录：不污染文件名，也不会撞）
renderloft-cli artifact download --artifact 999002001 --to D:\pics\ --name-template "{artifactId}-{filename}"
#   → D:\pics\999002001-870.png        （前缀就是 id，随时能回查）
```

> 为什么只给 `{artifactId}` / `{filename}`：模板是"把已有的事实换个排法"，不是"生成新信息"。
> `{date}` 这类会让同一条命令在不同时刻产生不同结果。模板必须是 `--to` 目录内的**相对路径**
> （可含子目录）——绝对路径与 `..` 一律当场拒绝，否则"往哪写"就不再由你决定了。

**同一个 run 内的产物名保证互不相同**（`870.png` → `870-2.png`）：RunningHub 按输出节点命名，
一个节点吐多张图时会撞，而那是两条不同的记录——不区分开就没法认哪张是哪张。跨 run 重名是正常的
（同一个应用每次跑都产出 `870.png`），那由上面的落点策略管。

两个实测出来的坑：

- `operation list` 给的是**摘要**（没有 `runs`），要按 runId 找产物得逐个 `operation get`；
- 文件名可能是 `.png` 而**实际字节是 JPEG**（我们就遇到了）——看图软件一般不在乎，但严格按扩展名
  解析的工具会在意。它和上面那条撞名不同：**字节类型是平台说了算，名字的冲突是"我们把它收进一个
  目录"造成的**，所以后者由我们兜底。

---

## 9. 定时收产物（不写接收端也能闭环）

不想写接收端、也不想挂着 daemon 等通知？仓库里带了一个示例脚本，它只调 CLI：

```powershell
node scripts\collect-artifacts.mjs --to D:\pics     # 收"已结束任务"里还没收过的产物
```

它把收过的记在 `D:\pics\.collected.json`，重复跑不会重复下载；每轮结果追加在 `D:\pics\.collect.log`
（**后台任务不能是哑的**——挂上任务计划后 stdout 没人看，日志是排查"为什么没收到图"的唯一线索）；
挂到任务计划就是全自动：

```powershell
schtasks /create /tn RenderLoft收图 /sc minute /mo 10 ^
  /tr "node D:\path\to\collect-artifacts.mjs --to D:\pics --only-if-running"
```

> `--only-if-running`：daemon 没在跑就跳过。不加的话，普通动词会顺手把 daemon 启动
> （"启动但不拥有"），于是一个每 10 分钟跑一次的后台任务会把你**手动停掉**的 daemon 反复拉活
> ——那不是它该管的事。`daemon` 那一组（`status` / `stop`）**不自动拉起**，脚本问的就是 `daemon status`。

---

## 10. CLI 旗标表（完整）

`renderloft-cli <组> <动词> --help` 看参数。全部动词与旗标：

| 动词 | 旗标 |
|---|---|
| `connection list` | — |
| `connection models` | `--connection <id>` |
| `connection set-key` | `--connection <id>` · `--from-file <文件>`（值只从文件读）· `[--force]`（默认拒绝覆盖） |
| `daemon status` | — |
| `daemon stop` | — |
| `callback set-secret` | `--from-file <文件>` · `[--force]` |
| `callback status` | — |
| `callback clear-secret` | — |
| `recipe list` | `[--limit <1-200>]`（缺省 50）· `[--removed]`（列回收站） |
| `recipe describe` | `--recipe <id>` |
| `recipe import` | `--connection <id>` · `--source <链接或 ID>` |
| `recipe import-workflow` | `--connection <id>` · `--source <链接或 ID>` · `[--name <名字>]` |
| `recipe remove` | `--recipe <id>` |
| `recipe restore` | `--recipe <id>` |
| `operation list` | `[--limit <1-200>]`（缺省 50）· `[--after <id>]`（游标：只看更早的） |
| `operation run` | `--recipe-revision <id>` · `[--input <JSON 文件>]` · `[--asset '<字段键>=<文件>']`（可重复）· `[--dry-run]` · `[--idempotency-key <键>]` · `[--callback-url <地址>]` |
| `operation get` | `--operation <id>` |
| `operation wait` | `--operation <id>` · `[--interval <1-60 秒>]`（缺省 5）· `[--timeout <秒>]`（缺省 0 = 一直等） |
| `operation cancel` | `--operation <id>` |
| `operation retry` | `--operation <id>` · `[--run <id>]`（只重试这一条；不给就重跑该任务的全部 run——已成功的那几条会**再计费一次**） |
| `artifact list` | `[--limit <1-200>]`（缺省 50）· `[--after <id>]` |
| `artifact get` | `--artifact <id>` |
| `artifact download` | `--artifact <id>` · `[--to <文件或目录>]` · `[--on-conflict rename\|skip\|overwrite]`（缺省 rename）· `[--name-template <模板>]` |
| `asset upload` | `--file <本机文件>` · `[--filename <名字>]` · `[--mime-type <类型>]` |

> 批量只有 MCP 有（`runs`，每行单独计费、上限 100）：**CLI 一次提交一个任务**。见 §3 与 §7。

几条 CLI 的机制：

- **PATH**：安装包会把安装目录加进**用户 PATH**（卸载时自动移除），装完开一个**新**终端即可；
  免安装 zip 不改 PATH，用解压目录里的全路径（`& "D:\你解压的位置\renderloft-cli.exe" recipe list`）。
- **`--input` 是 JSON 文件**，内容要匹配该应用的 schema；**文件不要带 BOM**
  （用 `Set-Content -Encoding utf8NoBOM` 或直接写）。
- **退出码与输出流**：成功时信封走 **stdout**、退出码 **0**；失败时信封走 **stderr**、退出码 **1**。
  所以在任务计划里 `if ($LASTEXITCODE -ne 0) { 报警 }` 是有效的，`> result.json` 也不会把错误混进去。
- **daemon 没在跑也没关系**：CLI 会自己把旁边的 `renderloft.exe` 拉起来（**只启动，不拥有**——
  它不会替你关掉 daemon，也不会改你的环境变量）。冷启动约 **0.6 秒**，之后每次调用约 **70 毫秒**。
  `daemon status` / `daemon stop` 是例外：它们**不自动拉起**（问"它在不在"却先把它启动起来是自相矛盾的）。
- **`--idempotency-key`**：同键同请求会重放原来的任务，而不是再花一次钱。
- **`--asset` 的字段键**（`475::image` 这样的 `节点::字段`）见 §0 与 §2；
  字段键错了会明确报 `ASSET_FIELD_UNKNOWN`，不会静默丢。

典型的"跑一次并拿到结果"：

```powershell
renderloft-cli recipe list                            # 拿 recipeId 与 defaultConnectionId
renderloft-cli recipe describe --recipe 1             # 拿 currentRevisionId 与字段 schema
renderloft-cli operation run --recipe-revision 1 --input cat.json --idempotency-key daily-2026-10-09
renderloft-cli operation run --recipe-revision 1 --input attrs.json --asset '475::image=person.png'   # 带图（先上传后提交）
renderloft-cli operation wait --operation 70 --interval 5 --timeout 600
renderloft-cli artifact list                          # 产物（byteSize 0 = 字节还在平台上，不是空文件）
```

因为是普通的命令行程序，它也能被任何调度器调用——例如「每天 9 点生成一张」用 Windows 任务计划即可。

> **只给 CLI、不给 AI 的动词**（动词表里 `mcp_name: None`，MCP 那侧没有对应工具）：
> `connection set-key` · `daemon status` · `daemon stop` · `callback set-secret` · `callback status` ·
> `callback clear-secret` · `recipe remove` · `recipe restore` · `artifact download`。
> 它们碰的是「谁能跑什么」这条授权边界本身，或者往磁盘任意路径写文件——而 AI 正受这条边界约束。

## 11. 任务卡住怎么查、怎么恢复

「卡住」几乎总是这四种之一：**在排队**、**在退避重试**、**等确认（提交结果不明）**、**在提交**。
它们的共同点是 op 级状态**都显示 `RUNNING`** —— 所以别用状态判断它有没有在动，看 `attention`：

```powershell
renderloft-cli operation list --limit 20      # 摘要里就有：attention {reason, deadline}
renderloft-cli operation get --operation 70   # 原因码 + 人话 + 死线 + 到点会怎样 + 现在能做什么
```

```text
renderloft_call { "verb": "operation.list", "args": { "limit": 20 } }
renderloft_call { "verb": "operation.get",  "args": { "operationId": 70 } }
```

原因码只有四种（严重度从高到低）：

| reason | 在等什么 | 死线 |
|---|---|---|
| `SUBMISSION_UNCERTAIN` | 提交时连接断了或超时，**平台到底收没收到不知道** | 有（默认 1 小时）→ 到点判失败 |
| `RETRY_BACKOFF` | 上一次提交没成功，**每 30 秒自动重试一次**，不用人管：平台拒了（如队列满）、限流、5xx，或者**证明字节没出去**的失败（连接被拒 / DNS 解析失败 / 握手超时） | 无（一直重试到你取消） |
| `SUBMITTING` | 正在提交（已落库租约） | 无；超过恢复窗口会转成「等确认」 |
| `QUEUED` | 排队：这条连接同时只跑一个（与平台账号的实际并发一致） | 无 |

> **`attention: null` 才是「正常在动」**（`ACCEPTED` / `RUNNING`，或已进终态）。
> `RUNNING` + `null` 没问题；`RUNNING` + 有 `attention` 就是**它其实没在动** ——
> 这时别对外报「预计 N 分钟出片」（这条正是本项目吃过的那次 40 分钟误报）。

> **`RETRY_BACKOFF` 与 `SUBMISSION_UNCERTAIN` 的分界是「能不能证明字节没出去」**：
> 能证明（连接被拒 / DNS 失败 / 握手超时）⇒ 自动重试，**不可能**重复计费，因为你这边一个字节
> 都没发出去；证明不了（发送后超时、连接被重置）⇒ 落「等确认」，等对账或死线，**绝不自动重投**。

### 11a. 死线语义（最容易被误读的一条）

- 默认 **1 小时**；连接的高级项里可配 `trackingDeadlineSeconds`（60 秒 ~ 24 小时，越界夹到边界）；
- 到点**判失败**，**不会自动重投** —— 因为"可能已经花了钱"的提交再投一次就是再花一次钱
  （见 `AGENTS.md` 的铁律二）。到点判失败不是"白等了"：它把**不确定**变成**确定的失败**，
  这时重试才是安全的下一步；
- 死线是**按"这一次提交"**算的：提交成功那一刻写定，之后的迁移不会把它往后推
  （否则你在连接上配的上限永远不生效）。

### 11b. 日志：位置、哪一份是活的、grep 什么

```powershell
renderloft-cli daemon status     # data.workspace 与 data.logFile 就是那两个路径
```

- 目录：`%LOCALAPPDATA%\renderloft-workspace`（`daemon status` 的 `workspace` 字段）；
- `daemon.log` = **正在写的**那一份；`daemon.log.1` = 上一次轮转下来的
  （5 MB 轮转，只留一份）——**崩溃现场往往正好在 `.1` 里**，别只看 `daemon.log`；
- **「哪个才是活的」看最后写入时间**：活着的 daemon 即使空转也**每分钟**写一行心跳
  `[renderloft-reconcile] tick active_runs=0`。文件几分钟没动 = 那个 daemon 不在了，
  或者它的输出压根没落到这个文件（daemon 是被**别人**启动的：外壳 / CLI / MCP / 手工，
  输出也可能被重定向到别处——所以"目录里有个 daemon.log"本身不是"它在跑"的证据）。
- 出事时写的是**固定形状**的一行，可以直接按错误码 grep：
  `[renderloft] run run=<runId> op=<operationId> <CODE> | <说明>`。卡住相关的是：
  `TRACKING_UNRESOLVED`（提交状态无法确认且无对账句柄）、
  `TRACKING_EXHAUSTED`（远端任务在对账期限内未进入终态）。

### 11c. 事件流：`run.submission_uncertain` 就在里面

每个 operation 都有事件流：`GET /api/v1/operations/{id}/events?after=0`
（`?stream=1` 是 SSE；每条是 `{eventId, eventType, runId, createdAt, payload}`）：

| eventType | 意思 |
|---|---|
| `run.submission_uncertain` | 提交结果不明（`payload.reason` 是原因）——**这就是"它为什么没在动"的原始记录** |
| `run.retry_wait` | 被平台拒了 / 可重试地失败，已排退避重试 |
| `run.tracking_unresolved` | 到死线仍确认不了，已判失败（`payload.error` 是结构化错误） |
| `run.created` / `run.accepted` / `run.succeeded` / `run.failed` | 正常路径 |

> ⚠️ 事件流**没有对应的 CLI / MCP 动词**（它是界面实时刷新用的通道，要 token）。
> Agent 该看的是 `operation get` 的 `attention`；事件流是给"要看原始时间线"的人或工具用的。

### 11d. 「它到底到没到平台」——先分清「能证明」与「不知道」

这条最要紧，因为**猜错的代价是重复扣费**。库里每个 run 的每一次提交尝试是 `v2_run_attempts`
一行（`number` 递增，最新那条就是当前这次）：

| 列 | 说明 |
|---|---|
| `status` | `SUBMITTING` / `ACCEPTED` / `SUCCEEDED` / `FAILED` / `UNCERTAIN` / `CANCELLED` |
| `remote_handle_json` | 远端任务句柄；**不确定态的 attempt 一定是 `NULL`** |
| `error_json` | 这次失败的结构化错误：`code` / `message` / `cause`（原因链）/ `nothingSent` |

⚠️ **`remote_handle_json = NULL` 不是「没到平台」的证据**：它的意思是**我们连一个可以拿去问
平台的远端 id 都没拿到**（拿到了就不会停在不确定态）⇒ 对账循环没有句柄可查，只能等死线。

「到底出没出去」要看 `error_json.details`：

| 看什么 | 含义 |
|---|---|
| `nothingSent: true` + `nothingSentEvidence` | **能证明字节没出去**，证据是 `CONNECTION_REFUSED` / `DNS_FAILURE` / `CONNECT_TIMEOUT` |
| `nothingSent: false` | **证明不了** —— 请求可能已经在平台上跑起来了 |
| `cause` | 原因链原文（`connection refused` / `dns error` / `timed out`…），排查全靠它 |

判据（`nothing_sent_evidence`，与上面那张表一一对应）：

- **能证明「字节没出去」**：连接被拒、DNS 解析失败、连接握手阶段就超时；
- **证明不了**：发送之后才超时、连接被重置、响应读到一半断 —— TCP 层已经通了。

> **`nothingSent` 决定系统怎么处理这次失败**（这一步就是钱）：
> `true` ⇒ **自动退避重试**（`RETRY_BACKOFF`，30 秒后自己再来）—— 字节都没出去，
> 重投**不可能**重复计费，所以不该让用户为一个"对面根本没收到"的提交等一小时；
> `false` ⇒ 落「等确认」（`SUBMISSION_UNCERTAIN`），等对账或死线，**绝不自动重投**。
>
> ⚠️ 但 `nothingSent` **不是给你自己去重投的许可**，也不是"平台侧已经去重"的证明：模糊类要能安全
> 重投，需要**平台侧去重**，而幂等指纹从来没有发给平台（见 `domain::execution::RecoveryProof`：
> `Idempotent` / `LookupByRequestId` 在生产代码里没有生产者）。所以对模糊类，恢复永远是显式动作
> （见 11e）。

要在库里看 attempt（**只读**；`operation get` 目前只到 run 级）：

```powershell
sqlite3 "$env:LOCALAPPDATA\renderloft-workspace\v2-workspace.db" `
  "SELECT number,status,remote_handle_json,error_json FROM v2_run_attempts WHERE run_id=<runId> ORDER BY number DESC LIMIT 3;"
```

### 11e. 恢复：先决定放不放弃旧的，再 `retry`

```powershell
renderloft-cli operation cancel --operation 70                 # ① 明确结束旧的（可选，但推荐）
renderloft-cli operation retry  --operation 70                 # ② 新建一个同样的任务
renderloft-cli operation retry  --operation 70 --run 1234      # ②' 只重试批量里失败的那一条
```

```text
renderloft_call { "verb": "operation.cancel", "args": { "operationId": 70 } }
renderloft_call { "verb": "operation.retry",  "args": { "operationId": 70 } }
renderloft_call { "verb": "operation.retry",  "args": { "operationId": 70, "runId": 1234 } }
```

三条必须记住的：

1. **`retry` 返回的是新 operationId**（它新建一个任务、复用原 run 的配方修订与输入），
   旧的那个原样留着 —— **要改盯新的那个**，别再轮询旧的；
2. **每个重试的 run 都会再计费一次**；批量里只失败了几条时**一定加 `--run`**，
   否则已经成功的那几条会被**再提交一次**（也就是再花一次钱）；
3. **`retry` 不要求源任务已经结束** —— 对一个还在跑的 operation 调它，就是**把同一批活再提交一遍**。
   想重试，先确认你要放弃的是哪一个（这也是"先 cancel"的意义：它让"旧的还算不算数"没有歧义）。

`cancel` 的返回分四类：`cancelled`（已在本机结束）、`alreadyTerminal`（本来就是终态）、
`pending`（本机没能结束它）、`remoteMayContinue`（**远端可能还在跑** —— 包括"有句柄但远端没确认
取消"与"**根本没有句柄**"两种）。

> ⚠️ **`cancelled` 与 `remoteMayContinue` 可以同时出现**，而且这正是最需要看清的一种：
> 「等确认」（`SUBMISSION_UNCERTAIN`）与「提交中」（`SUBMITTING`）这两档**没有**远端句柄
> （句柄只有拿到平台回执时才有），所以本机确实取消了（进 `cancelled`），但**远端那一份可能正在跑、
> 并会照常计费**（同时进 `remoteMayContinue`）。别把 `cancelled` 单独读成「平台上一定没有残留」。
>
> 还没提交的 `QUEUED` run 则**只**进 `cancelled`：它一定没到平台。

---

## 12. 迁移到另一台机器

**第一步永远是备份**，而且**不要自己拷工作区目录**：

```bash
renderloft-cli workspace backup --to /path/on/this/machine/backup
# → {"path": "...", "databaseBytes": 1155072, "artifactCount": 2, "artifactBytes": 542177}
```

**为什么不能自己拷**：工作区的 SQLite 跑在 WAL 模式下，库文件和 `-wal` / `-shm` 是活的，
直接拷可能漏掉最近的事务、甚至拷出一个打不开的库——而且**不会报错**。`workspace backup`
用 SQLite 官方的 `VACUUM INTO` 导一份一致的库，再把 `artifacts/` 整份拷过去。
（目标目录里已经有一份备份时它会**报错**，不覆盖；换一个目录。）

**换机器**：

1. 在新机器上装好 RenderLoft；
2. `renderloft-cli daemon stop`（在**新**机器上）；
3. 把备份里的 `v2-workspace.db` 与 `artifacts/` 放进新机器的工作区目录——`daemon status` 回的
   `workspace` 字段就是那个目录；旧的那份先挪走，别混着放；
4. 跑任意一条命令（例如 `renderloft-cli recipe list`），CLI 会把 daemon 拉起来；
5. **自检**：`recipe list` / `operation list` 的数量与源机器一致，才算迁移成功。

> ⚠️ **必须交回给人的一步**：平台 API Key **不在这份快照里**（它们在系统凭据库，设计上不落库）。
> 所以迁移的最后一步是**人在界面的「连接」页重新填一次**。在人来填之前**不要报告"迁移完成"**
> ——之后的任务会因为缺少凭据而失败，而那看起来像是你的迁移做错了。
