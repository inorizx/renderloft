# RenderLoft

> **RenderLoft 是你在本机跑 AIGC 应用的工作台。**
> 连上你的平台账号，把工作流导入成「应用」，填参数就能出图。
> **给字段写一次中文备注名 —— 之后人和 AI 都看得懂。**
> 任何支持 MCP 的 AI 都能直接驱动它；不想用 AI，就用表单和命令行。

![应用库](docs/screenshots/library.jpg)

它不是画图工具（它不生成图，它让**别的机器**去生成），不是云平台（连接、密钥、历史、产物都在你自己的机器上），不是「内置了一个 AI」（也不用 LLM 才能用），不是 ComfyUI 的节点编辑器（工作流还在你自己的 ComfyUI 里搭），不是 RunningHub 的替代平台或市场镜像（应用与工作流仍归平台和作者），不是模型训练平台，也不是让模型任意访问网络和本地文件的通用 Agent——它是**接口**。

**给谁用**：

- **普通用户**：连上平台、导入一个应用，填参数就出图。
- **ComfyUI 用户**：把已经搭好的工作流变成可复用的应用，再批量跑。
- **开发者与自动化**：用 CLI 把它接进自己的脚本与调度器。
- **外部 AI**：通过 MCP 发现并调用你配置好的应用（怎么编排由它自己决定）。

**表单路径始终是第一公民——即使没有 LLM，也能直接运行应用。**

## 现在能连什么

**平台是可插拔的**：daemon 只认「驱动」不认品牌——接一家新的就是加一个驱动，而参数化、批量、
任务恢复、产物管理这些能力不随平台变（接缝写在 [ARCHITECTURE.md](ARCHITECTURE.md)）。

| 能力来源                                                     | 怎么接                                           | 现在能做什么                                                    |
| ------------------------------------------------------------ | ------------------------------------------------ | --------------------------------------------------------------- |
| **RunningHub**（国内站 / 国际站）                            | 平台 API Key                                     | 粘链接或 ID 导入平台上的**应用**与**工作流**，参数化成表单再跑  |
| **ComfyUI**（本机 / 云端，含跑在 RunningHub 云端的代理地址） | 服务地址；云端那档复用一条 RunningHub 连接的 Key | 导入 ComfyUI「保存(API 格式)」的 JSON，再勾选要暴露成表单的字段 |
| **OpenAI 兼容的图像 / 模型 API**                             | 平台地址 + Key                                   | 拉取平台声明的模型清单，勾选这条连接要跑的模型                  |

> 下面各节里会出现很多 RunningHub 的具体字样（Key 在哪、去哪注册、字段为什么长那样）——
> 那是**拿它当例子**讲清楚一条连接怎么配通，不是这个产品只认它。

## 获取

到 [Releases](../../releases) 下载 Windows 安装包（NSIS），装完直接打开就是完整应用——daemon、控制台、CLI、MCP 都在里面，不需要另装 Node 或 Rust。同一个 release 里还有一个免安装 zip（四个 exe + `dist/`，解压即用）。

> **仓库还没发布过 Release 时**（`../../releases` 是空的），按下面的[开发](#开发)一节自己编译：装好 Node + pnpm + Rust 之后一条 `pnpm build:windows` 就能产出安装包与免安装 zip。

**升级**：下载新版本、覆盖安装即可。**工作区（应用库、任务历史、产物）与 API Key 都在安装目录之外**——分别是 `%LOCALAPPDATA%\renderloft-workspace` 与 Windows 凭据库——所以覆盖安装不会丢数据。安装包目前**没有代码签名**，升级后首次运行仍可能看到 SmartScreen 的「未知发布者」，点「仍要运行」即可。

### Linux（无头版）

Linux 的形态是**无头**：没有桌面壳、也没有托盘——**CLI、网页控制台（daemon 伺服）、MCP 三入口共用同一个 daemon**。发布时会连同 `RenderLoft_<版本>_x64-linux.tar.gz` 一起提供（v0.1.1 起）；也可以自行从源码构建（需要 Node + pnpm + Rust）：

```bash
pnpm install
pnpm build:linux   # → RenderLoft_<版本>_x64-linux.tar.gz（三件套 + dist/，解压即用）
pnpm smoke:linux   # （可选）从解压包全链路自检：CLI → daemon → 控制台 → MCP
```

用法与下文「用命令行（可选）」一节相同，只是没有 `.exe` 后缀：解压目录里直接 `./renderloft-cli`；控制台在 `http://127.0.0.1:31337/`；MCP 客户端指向解压目录里的 `renderloft-mcp`；`./renderloft-cli daemon stop` 收工。**便携包不碰系统目录、不写 PATH。**

> 已知点：Linux 的密钥存进系统密钥环（Secret Service，经 D-Bus）——桌面发行版自带；纯服务器 / 容器环境里没有它时，保存平台连接密钥需要先备好 keyring 服务（如 gnome-keyring）。便携包不打包系统库：运行需要系统提供 `libdbus-1` 与 `libssl`（常见发行版都自带，极简容器里需补装 `libdbus-1-3` / `libssl3`）。

## 三步开始

### 1️⃣ 连上你的平台

![平台连接](docs/screenshots/connections.jpg)

在「连接」页填平台地址与 API Key。

**以 RunningHub 为例，API Key 在哪**：登录 runninghub.cn（或 .ai）→ 打开「API」页面 → 进「控制台」→ 复制 API Key，粘回连接页。国内站与国际站是两套独立账号，各配一条连接即可。

**还没有账号？** 到 runninghub.cn（或 .ai）注册一个即可。

**密钥由 Rust daemon 写进系统凭据库（Windows Credential Manager），不进 SQLite、不进日志、不进错误响应。** 界面上也**只显示它的尾号**（如「已保存 · 尾号 a1b2」），不显示密钥本身——截图、录屏、剪贴板都拿不到它。

支持 RunningHub（国内 / 国际）、ComfyUI（本机或云端）、OpenAI 兼容的图像 / 模型 API：

- **ComfyUI 跑在 RunningHub 云端**（地址形如 `https://www.runninghub.cn/proxy`）时，用的**就是** RunningHub 那把 Key——所以界面不让你再填一遍，改成从下拉里**选一条 RunningHub 连接**：密钥不存第二份，那边轮换自动生效，删了这边立刻报错。
- **OpenAI 兼容的图像 API** 可以点「拉取可用模型」把平台声明的模型清单拉下来，勾选这条连接能跑的那几个；之后运行表单里的「模型」字段就是这几个选项，不用手打模型名。连接上还能设一个**默认模型**（运行和配方都没点名时用它；都没说就交给平台自己的默认——我们不替你编一个）。

### 2️⃣ 导入一个应用

在「应用库」页导入：

![从 RunningHub 添加](docs/screenshots/rh-import.jpg)

- **从 RunningHub 添加** —— 粘贴链接或 ID 就行，**AI 应用与工作流都能导入**：粘进去它会自己认出是哪种（`…/ai-detail/<id>` 是应用，`…/post/<id>` 是工作流），想指定也可以在「导入方式」里选。
- **从 ComfyUI API Prompt 添加** —— 粘贴 ComfyUI「保存(API 格式)」导出的 JSON；也可以粘 RunningHub 工作流链接或 ID 让平台拉取，再自己勾选要暴露成表单的字段。

### 3️⃣ 填参数、出图

![填参数](docs/screenshots/runner.jpg)

打开应用 → 填表单 → 运行 → 在「任务」页看进度 → 在产物里取图。界面顶部实时显示平台余额。

![任务历史](docs/screenshots/tasks.jpg)

每个任务下面那行「27 秒 · 6 币」是**平台自己报的**用量（耗时与币在平台的同一个响应里）——同一张图两次跑价钱可能不同，我们不替你估算。

## 关掉窗口之后

**点 X 不是退出，是收进托盘**——因为一次云端出图是分钟级，你多半会去干别的：

- 窗口关了，**任务照旧跑完**：daemon 留在后台对账、轮询，跑完时弹一条系统通知；
- 托盘图标（右下角，可能被 Windows 收进溢出区）右键 → **打开控制台** / **退出**；
  左键点它也是打开控制台；
- 托盘悬停会写"正在跑几个任务"；
- **真正退出**只有托盘菜单里的「退出」（或 `renderloft-cli daemon stop`）——那时 daemon 才停。
  就算在任务跑到一半时退出了也不丢：**平台侧会继续跑**，下次打开自动对账回来。
- 只想要一个窗口、不想让它常驻？任务跑完自己点托盘里的「退出」即可；本程序**不做开机自启**。

## 你的东西在哪（换机器 / 备份）

应用库、表单、备注名、任务历史、上传的素材、产物缓存，全都在**一个目录**里：

```text
%LOCALAPPDATA%\renderloft-workspace      一个 SQLite 库 + 一个 artifacts\ 目录
```

- **换机器**：退出应用 → 把这个目录整个拷过去 → 打开 → 在**「连接」页**重新填一次平台 API Key。
- **为什么密钥要重填**：API Key 存在 Windows 凭据管理器里，**设计上就不落库**——备份文件会被随手分享，密钥不该跟着走。
- 你从界面下载到**「下载目录」**的那些图是你自己的文件，跟工作区无关，想带就自己拷。
- **想省事**：`renderloft-cli workspace backup --to D:\backup` 会导出一份**一致的**快照。它比"自己拷目录"多做的只有一件事——用 SQLite 官方的方式导库，所以**不用先退出应用**，也不会拷出一个缺数据或打不开的库。（界面上暂时还没有这个按钮。）

> ⚠️ **别在应用运行时直接拷那个库**：它跑在 WAL 模式下，库和 `-wal` / `-shm` 是活的，拷出来可能缺数据甚至打不开。先退出应用（托盘菜单里的「退出」）再拷。

## 关键的一步：给字段写备注名

平台给的字段名是机器话：`874::text`、`871::batch_size`。

![一次策展](docs/screenshots/curation.jpg)

三格是同一件事的三步：

- **上**：还没整理过的一条应用（31 个字段）。`value`、`save_output` 各重名好几次，重名的只能靠括号里的节点号分开——那是**图里节点的编号**，是事实，不是我们起的名；
- **中**：在「编辑字段」里**写一次名字、挑一次控件**（单行文本 / 多行文本 / 数字 / 开关 / 下拉 / 文件）；
- **下**：整理过的应用，运行表单直接用上了这些名字。

之后：

- **人**看到的表单是「提示词」「每批张数」
- **AI** 通过 MCP 拿到的 schema 描述也是「提示词」「每批张数」

> **一次策展，两个观众。** 这是这个产品相对平台扁平目录的核心价值。

备注名**只能在界面上写**——改元数据属于「策展」，是人的判断，我们刻意不把它交给 AI。

## 用 AI 驱动它（可选）

RenderLoft 自带一个 MCP 服务器（stdio），把本机 daemon 的能力暴露给任何 MCP 客户端。**每个动词就是一个工具**（`renderloft_run_recipe`、`renderloft_get_operation`、`renderloft_cancel_operation`…），每个工具自带它的参数 schema 与**逐动作标注**（只读 / 不可逆 / 是否花钱）——所以客户端与审批层**不必先查一次**才知道某个动作能不能做、要不要花钱。另有一个只读的 `renderloft_catalog` 作按需查阅（给一个 verb id 就返回那个动词的完整语义；不给参数就返回索引）。

在你的 MCP 客户端配置里指向 `renderloft-mcp.exe` 即可——**它和界面是同一个安装包里的同一个目录**（安装版默认在 `%LOCALAPPDATA%\RenderLoft`），不需要单独下载；免安装 zip 与 Linux 便携包里就是解压目录里的那个（Linux 无 `.exe` 后缀）：

```json
{
  "mcpServers": {
    "renderloft": { "command": "C:\\Users\\<你>\\AppData\\Local\\RenderLoft\\renderloft-mcp.exe" }
  }
}
```

> `renderloft-mcp.exe` 是 **stdio 服务**：由 MCP 客户端按需拉起，不用常驻；自己编译的话路径是 `<仓库路径>\src-tauri\target\release\renderloft-mcp.exe`。它连的 daemon 如果没在跑，**它会自己把旁边的 `renderloft.exe` 拉起来**（CLI 也一样）——所以不必先打开界面。

然后就能直接说「用 Krea2-turbo 画一只在草地上的猫，等它完成，把图给我」——它背后走的是 `renderloft_list_recipes → renderloft_describe_recipe → renderloft_run_recipe → renderloft_get_operation`，**不需要你解释字段是什么意思**，因为备注名已经写在 schema 里了。

**刻意不给 AI 的动词不在工具清单里，也调不到**：写密钥、写备注名、建/删连接、关 daemon、往磁盘写文件——它们属于人的动作，改的是「谁能跑什么」这条授权边界本身。

> **带图提交、批量、回调、dry-run、分页与常见坑**：[`docs/agent-cookbook.md`](docs/agent-cookbook.md)。
> **这套工具面的完整推导**（公理 → 设计律 → 物理边界 → 可证伪的判据、常驻 token 账单，以及"为什么压平成原生工具"这个**已经定下来**的岔路）：[`docs/mcp-surface.md`](docs/mcp-surface.md)。

## 产物怎么拿到手

产物落在**下载目录**（「服务配置」里可改，默认是系统下载目录）；**文件名照抄平台给的**，往一个目录里收一批时重名按浏览器口径递增（`870.png` → `870 (1).png`），不覆盖你已有的文件；任务完成的通知上点「在文件夹中显示」，会打开下载目录并选中那个文件。

`byteSize` 为 0 的产物**字节还在平台上**（中转站定位，不落本地）——用这一条按需拉下来：`renderloft-cli artifact download --artifact 999002000 --to D:\pics\`（给目录，不存在会建）或 `--to D:\pics\cat.png`（指定文件）。

> **取过一次就落到本地了**：在界面里看一眼（或命令行下载一次）之后，那份字节会缓存在工作区的 `artifacts\` 目录里，之后不再回平台取——`byteSize` 也随之变成真实大小（0 的含义因此是"还没取过"）。任务被删/被 30 天清扫时，缓存文件跟着一起删。

> 撞名策略（`--on-conflict`）、`--name-template`、同 run 内重名、定时收图脚本与两个实测坑，都在 [`docs/agent-cookbook.md`](docs/agent-cookbook.md)。

## 用命令行（可选）

和界面、MCP 同一个安装包里的同一个目录。**安装时会把安装目录加进你的用户 PATH**（卸载时自动移除），所以装完开一个**新**终端就能直接敲 `renderloft-cli`：

```powershell
renderloft-cli recipe list                               # 有哪些应用
renderloft-cli recipe describe --recipe 1                # 它的字段（含备注名）
renderloft-cli operation run --recipe-revision 1 --input cat.json
renderloft-cli operation run --recipe-revision 1 --input inputs.json `
  --asset '475::image=person.png' --dry-run              # 带图：按字段键传资产；--dry-run 只预检不花钱
renderloft-cli operation wait --operation 70
renderloft-cli artifact list
```

- **退出码与输出流**：成功时信封走 **stdout**、退出码 **0**；失败时信封走 **stderr**、退出码 **1**——所以任务计划里 `if ($LASTEXITCODE -ne 0) { 报警 }` 是有效的。
- daemon 没在跑也没关系：CLI 会自己把旁边的 `renderloft.exe` 拉起来（**只启动，不拥有**）。
- **有几条动词只给 CLI、不给 AI**：写密钥（`connection set-key` / `callback set-secret`）、关 daemon、策展（`recipe remove` / `restore`）、往磁盘写文件（`artifact download`）。
- 免安装 zip 不改 PATH，用解压目录里的全路径即可；CLI 一次提交一个任务，**批量走 MCP 的 `runs`，或在运行页切到「批量运行」**。

> **完整旗标表、产物落盘规则、批量、回调、dry-run 与常见坑**，都在 [`docs/agent-cookbook.md`](docs/agent-cookbook.md)。

## 它是怎么工作的

```text
GUI（Vue + Tauri）/ CLI / MCP —— 都是客户端
                    │  HTTP + SSE（/api/v1，本地回环）
            Rust daemon —— 唯一业务宿主
                    │  调度 · 对账 · 重试 · 幂等 · 密钥
        SQLite + 产物目录 + 系统凭据库
                    │
      RunningHub / ComfyUI / 模型 API
```

**推理交给你的 AI（它会持续变强），能力、状态、信任留在本地（越用越厚）。**

## 卡住了看这里

**「连接测试失败」**：**先读那一行红字**——它就是为了省掉猜。缺 Key、平台回了 401（Key 不对或过期）、余额不足、连不上（地址写错、或代理没在跑），各自的原因都写在连接行里。改完点那一行的「测试」再验一次。

**明明能上网，应用却说连不上平台**：看环境里有没有 `HTTP_PROXY` / `HTTPS_PROXY` 指向一个**没在跑**的代理（常见于关掉代理软件之后）。这时"连不上平台 / Key 好像坏了"是假象——回环地址不走代理，所以本机 ComfyUI 反而是绿的。把变量清掉再启动应用即可。

**提交时被平台拒（如队列已满）**：平台的上限由平台说了算，被拒会被当成「等一会儿」**自动重试**（每 30 秒一次），不是失败。**但重试有终点**：到死线（默认 1 小时，跟连接上配的那个一样）还没成功就判失败，不会一直转下去——那时你可以 `operation retry`（被拒过的提交没有被受理，重试一般不会重复计费）。别反复点「开始生成」——那只会把并发占得更满。

**提交时平台回了 5xx**：这类**不自动重投**——500 可能是网关/内部错误发生在受理之后，**平台可能已经收下**并计费，盲投就可能扣两次。所以它落到「等确认」：连 ComfyUI 时编号是提交时我们自己定的，通常**几秒就查清**；连 RunningHub 时那边没有任何接口能回查，只能等到 1 小时死线，再你自己决定要不要重试（拿不准就先看平台后台）。

**提交时连不上平台**：分两种，**都按实际情况处理，不用你判断**——「连接被拒 / DNS 解析失败 / 握手就超时」这类**一个字节都没发出去**（平台什么都没收到），所以**30 秒后自动重试**，不会重复计费；「发出去之后才超时 / 连接被重置」这类**可能平台已经收下了**，所以它落到「等确认」，等到对账确认或 1 小时死线判失败——**绝不自动重投**（重投就是再花一次钱）。连 ComfyUI 时这一类还能**自己查回来**：任务的编号是提交时我们自己定的，所以响应丢了也知道该去问哪一个——通常几秒就确认，不必等满一小时。

**任务一直显示「进行中」，但它其实没在动**：排队、退避重试、提交结果不明这几种，状态**都是「进行中」**。
界面里任务卡片上会直接写出来（「它其实没在动 · 等确认 · 15:52 判失败」），点开详情还有一句解释；
命令行 / MCP 那侧用 `renderloft-cli operation list`（列表里就有）或 `operation get --operation <id>`
看那条 `attention`：它给出**原因码 + 一句人话 + 死线 + 到点会怎样 + 现在能做什么**；
`attention` 是 `null` 才代表真的在跑（MCP 那侧是同名的 `renderloft_list_operations` / `renderloft_get_operation`）。
要放弃重来就用 `operation retry`（界面上的「重新运行」也是它）——它是**新建一个任务**（会再计费一次，
所以界面会先问你一句）。**批量里只失败了几条时，别整批重跑**：那会把已经成功的那几条也再提交一遍。
命令行加 `--run <id>` 只重试一条；界面在**批次详情**里有「只重试失败的 N 条」，一次把失败的都重来
（每条各建一个新任务，所以它会告诉你"再计费 N 次"）。

**想改一个参数再跑一次**：别用「重新运行」（它参数不能改）。任务详情/批次详情里每条记录旁边有个
「**用这套参数再跑一次（可改）**」——它会打开那个应用的表单、把这次提交的参数（含素材）填好，
你改完自己按「开始生成」。**它不替你提交**，所以不会悄悄花钱。

**填了一半的表会不会丢**：不会——每个应用的**草稿会留着**（关掉应用、关掉界面再打开都还在）。
它按应用分开存、互不串；**关掉那个应用的工作标签页**、或点表单上的「清空表单」就会清掉。
提交过的任务不受影响：它的参数在任务里还能用「用这套参数再跑一次」取回来。

**截图/日志里能不能出现密钥**：不能。密钥只在系统凭据库里，界面只显示尾号，daemon 从不回传它。

## 如实说明

- **历史只到最近 100 条**（任务页一次拉 100、渲染 20，没有分页）。
- **产物的 `byteSize` 可能是 0**：含义是「字节还在平台上、**还没取过**」——RunningHub 只给 URL 不给字节。取过一次（界面预览 / `artifact download`）就有真实大小、字节也留在本地缓存里。界面不展示这个字段。
- **用量来自平台自己的报价**（RunningHub 的 `consumeCoins`）；平台没说就是没说，**不会替你估算**。单价是浮动的。
- **不做并发控制**：平台的上限由平台说了算。被拒（如队列已满）会被当成「等一会儿」自动重试，而不是失败——但**重试有终点**（到死线判失败，不会永远转下去）。
- 任务历史、产物、凭据都在**本机**；只有「生成」这一步会发到平台。

## 开发

需要 Node.js、pnpm、Rust，以及 Windows Tauri 所需的 C++ 构建工具。

```powershell
pnpm install
pnpm dev            # daemon(127.0.0.1:31337) + 控制台(http://127.0.0.1:5174)
pnpm build          # 前端类型检查 + 构建
pnpm verify         # 完整门禁：lint · 格式 · 分层 · 配色契约 · 版本号 · 前端构建 · clippy · Rust 测试
pnpm build:windows  # 打 Windows 安装包（NSIS）
pnpm build:linux    # 打 Linux 无头便携包（tar.gz：三件套 + dist/）
```

分层约束由 `scripts/check-layers.mjs` 守门：daemon 是唯一业务宿主，只有调度器能向平台提交，写端点必须是应用用例。

### 发布

版本号只有一个真相来源：根 `package.json`。其余 5 处（两个 `Cargo.toml`、`desktop/tauri.conf.json`、两个 `Cargo.lock`）由脚本同步，`pnpm verify` 与 CI 都会校验一致性——**不一致时编译、测试、打包全绿，只有将来的 updater 会静默判断错版本**。

```powershell
pnpm version:bump minor        # 0.1.0 → 0.2.0，6 处一起改
git commit -am "chore(release): v0.2.0"
git tag v0.2.0
git push --tags                # tag 触发 .github/workflows/release.yml
```

`release.yml` 先校验 **tag 与版本号一致**，再打包、生成免安装 zip、Linux 便携包与 `SHA256SUMS.txt`，发成 GitHub Release（notes 由 commit 自动生成）。

> CI 与 Release 都由 **GitHub Actions** 跑（`.github/workflows/`）。
>
> Gitee 那边是**只读代码镜像**：Gitee 仓库里配了 GitHub → Gitee 的 Pull 镜像，代码与 tag 自动同步，
> **不要再往 Gitee 推**（会被下一次同步覆盖），已发布的 tag 也别删或移动——发行版挂在 tag 上。
> 安装包以 GitHub Releases 为准；Gitee 发行版的附件由本机手动上传——镜像只搬代码，不搬附件。
>
> CI 里那个 `windows-installer` job 产出的是 **Actions artifact**（14 天过期、下载要登录 GitHub）——它只用于调试，**不是给用户的下载渠道**。

## 文档

- [产品愿景](docs/product-vision.md)
- [架构与技术选择](ARCHITECTURE.md)
- [当前状态](docs/status.md)
- [维护指南](docs/AGENTS.md)
- [Agent Cookbook：从零跑通一个任务](docs/agent-cookbook.md)
- [完成回调与验签](docs/callbacks.md)
- [MCP 工具面：第一性推导与判据](docs/mcp-surface.md)

## 许可证

[MIT](LICENSE) © 2026 inorizx
