# AGENTS.md — RenderLoft 维护指南（AI / 新贡献者入口）

> 先读本文件，再按需深入 `product-vision.md`、根目录 `ARCHITECTURE.md` 或 `status.md`。
> 本文件是项目唯一的维护快速入口，保持精简；实现细节以代码和测试为准。

## 项目是什么

本地优先（local-first）的 **AI 应用化运行工作区**（Rust 守护进程 + Vue 控制台 +
Tauri 壳 + CLI + MCP）。它使用用户自己的平台连接和 API Key，把 RunningHub 创作者
开放的应用与工作流、ComfyUI API Prompt、模型 API 包装成可复用 Recipe，提供参数化、
单次与批量执行、任务恢复、对账和产物管理。界面、CLI、MCP 都是同一个 daemon 的客户端；
自动化入口是 **CLI 与 MCP 的原生工具**（每个动词一个工具，逐动作的只读/破坏性/花钱标注都在
`tools/list` 里 —— 审批层读的就是那一格）。MCP 另留一个只读的 `renderloft_catalog` 作**按需查阅**
（索引 + 单个动词的完整 schema 与语义），它不是入口，也不需要先调。

## 三条架构铁律（违反即回退评审）

1. **Rust 守护进程是唯一应用宿主**。Vue UI / CLI / MCP / 外部 Agent 都是
   `/api/v1`（HTTP+SSE）的客户端，不是独立业务实现。
2. **只有 scheduler 能向 provider（RunningHub 等）提交任务**。提交后超时/断连
   → `SUBMISSION_UNCERTAIN`，必须先 reconcile 再重试，禁止盲目重投。
3. **写端点必须是 application 用例**。`transports/` 不得直接编排 repositories/
   secrets；`application/` 与 `transports/` 生产代码不得 `use
   crate::v2::infrastructure`（CI 有守门，见下）。

## 目录地图

```text
src/                          Vue 前端（仅 UI + 缓存读模型）
  features/                  views 与 feature 局部组件（feature 间零互引）
  shared/api/v2.ts           唯一 API 客户端（camelCase DTO、信封解析）
  shared/lib/                runtime/download 等纯工具（无 DOM 混入 api 层）
  components/ui/             展示原语（自建，仅依赖 vue 与项目内路径）
src-tauri/src/
  main.rs / bin/             renderloft / renderloft-cli / renderloft-mcp
  v2/domain/                 纯净领域（实体/值对象/状态机，无 IO）
  v2/application/            use cases（create_operation、scheduler、reconcile、
                             credential_reference、connection、recipe、asset、settings …）
  v2/ports/                  接口（repositories、provider、stores、safety、token、
                             policy、schema、callback）
  v2/infrastructure/         SQLite、keyring、provider drivers、fakes
  v2/transports/             HTTP/SSE（唯一的 API 面）
scripts/                     dev / build / check-layers.mjs（分层守门）
```

## 文档地图（按需读，不整读）

稳定入口只有四份：

| 文档 | 用途 |
|---|---|
| `../README.md` | 项目简介、产品闭环、开发入口 |
| `product-vision.md` | 项目目的、用户价值、能力来源和产品边界 |
| `../ARCHITECTURE.md` | 长期架构不变量与技术选择 |
| `status.md` | 当前已完成、进行中和暂缓事项 |

另有一份**专题设计文档**（不是入口，改 MCP 工具面前必读）：

| 文档 | 用途 |
|---|---|
| `mcp-surface.md` | MCP 工具面的设计律与判据：公理、推导、物理边界、可证伪的验收。**改工具面之前先读它**，否则很容易把"调用方需要的事实"留在服务端 |

## 常用命令

```bash
pnpm dev
pnpm build
pnpm verify
node scripts/check-layers.mjs
cargo test --offline --manifest-path src-tauri/Cargo.toml --lib
cargo clippy --offline --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

## 编码约定

- **契约**：公开边界用显式 camelCase DTO；避免无界 `Value`（自由载荷如
  recipe schema / provider config / metadata 除外，需注释说明）。
  ⚠️ **加字段必须同批确认 `#[serde(rename_all = "camelCase")]`**——实测后端给一个 DTO
  加了 `asset_ids` 却漏了 `rename_all`，而前端发的是 `assetIds`，
  serde 只认 snake_case ⇒ **字段被静默丢弃：编译过、运行不报错、图片就是不见了**。
  这类"静默失效"与具体功能无关，只能靠前后端字段名对照发现，所以新写 DTO 一律带 `rename_all`。
  ⚠️ **请求体的键名一律 camelCase，包括 Rust 侧客户端**：`clients/` 里的 CLI/MCP 是**手写
  `json!`** 组装请求体，与 DTO **没有编译期联系**。实测过一次：请求体写成 snake_case
  （`system_prompt`/`executing_agent_id`），而服务端 DTO 是 `rename_all = "camelCase"` +
  `deny_unknown_fields` ⇒ 必填字段"缺失"，编译期完全发现不了。改请求体后请对照 DTO 字段名
  （现在的 `run_request`、上传、回调都是这类手写体）。
  ⚠️ **加能力改动词表，不要改客户端**：CLI 与 MCP 的动词、参数、通道覆盖、文案都定义在
  `v2/clients/verbs.rs` 的**一张表**里——CLI 由它渲染 clap 子命令、MCP 由它渲染工具 schema、
  分发（动词 → client 调用）也只写一次。某个动词"MCP 不支持"写在 `mcp_name: None`，
  不靠人比对两个文件。在 `cli.rs`/`mcp.rs` 里手写动词会被
  `the_listed_tools_are_the_table_s_verbs_plus_the_reference_tool`、
  `every_listed_verb_reports_the_table_s_own_annotations` 与 CLI 覆盖测试打回
  （工具数按表算，**不写死数字**；清单里每个动词的标注必须与表逐条相等）。
  ⚠️ **客户端的路径与方法必须打真实路由**：`clients/client.rs` 里的路径是手写字符串，
  与 axum Router **没有编译期联系**。实测有一次把"取消"写成 GET 而服务端注册的是 POST
  ⇒ 那条动词**一直是坏的**（拿到的是 405，而不是任何有意义的错误），而单元测试全绿。
  新增/改动词后跑 `every_verb_hits_a_real_route_with_the_right_method`
  （它把真实 router 起在真 socket 上，用不存在的 id 跑一遍整张动词表）。
  ⚠️ **加/改路由必须同步 `transports/http/handlers.rs::openapi()` 里那张声明表**：
  它手工维护（axum Router 无法自省），但**现在有机械守卫**——
  `transports::http::tests::openapi_table_matches_registered_routes` 会扫源码逐条核对。
  这道守卫是后来补的：此前实测曾漏报 16/44 条路径（含 `/api/v1/settings`），
  使对外发布的 `openapi.json` 少报 1/3 的 API。
- **错误**：稳定 `code` 是机器契约，`message` 可演进；`retryable` 由 Rust 核心
  决定；错误信封 `{ contractVersion, ok, error: { kind, code, message,
  retryable, details } }`。
- **密钥**：用户输入的密钥可以通过经认证的本地 GUI 管理请求传给 Daemon，之后只由
  Rust 通过系统凭据库（Windows Credential Manager / keyring）保存和解析；绝不落 SQLite，
  不出现在 CLI/MCP/diagnostics/logs/errors。
- **提交**：Conventional Commits；每个有实质意义的改动一个 commit；提交前
  `cargo test --lib` 必须绿。
- **提交后验证**：每个 commit 后用 `git worktree add <tmp> HEAD` 在干净 HEAD
  上跑 `cargo test --offline --lib`——工作区全绿不代表提交内容自洽。
- **界面文案（i18n）**：所有用户可见文案必须走 `t()`，包括 `aria-label`/`placeholder`/`title`、
  `window.confirm`、`Notification` 以及 **API 层抛出的错误 message**（`useLocale().t` 读的是模块级
  `locale` ref，setup 之外也能调）。表在 `shared/composables/useLocale.ts`：
  ① **中英两段必须同步加** —— `MessageKey` 派生自中文段，**en 段漏 key 不会编译报错**，只在运行时回退成 key 名；
  ② **加 key 前先 `git grep "'<key>'"` 查同名** —— 同名 key 后定义者生效，会**静默改掉原引用语义**；
  ③ 改表后跑**配对自检**（应「空输出」）：
  `grep -oE "^    '[a-zA-Z0-9_.]+':" src/shared/composables/useLocale.ts | sed "s/[ ':]//g" | sort | uniq -c | awk '$1!=2 {print "不配对:", $0}'`；
  ④ 插入 key 块时**最后一项一律带尾逗号**（JS 允许 trailing comma）—— 实测漏逗号是语法错误，
  且 **`vue-tsc` 会放过它，只有 `vite build` 报**；
  ⑤ **删 key 前必须查动态拼接**（`git grep 't(\`'`）—— 实测 3 个"死 key"候选全是
  `t(\`connection.${...}\`)` 这类动态引用的**活文案**，盲删必出事。
  ⑥ **注释里不要写 `t('<key>')` 这种字面形态** —— 集合差扫描是按文本匹配的，
  注释里写一次就会**永久多出一个幽灵键**（实测：`15e5512` 的注释里写了该形态，
  下一次扫描立刻在"被引用但不在表内"一栏冒出一个 `...`）。要举例就写成 `t(...)`（不带引号）。
- **清理纪律：批量改造之前先扫零引用**（组件 / 类型 / 依赖 / key）。
  本项目的真实教训：先做了 i18n 改造，事后才发现其中 **5 个组件是零引用死代码**，等于白改。
  扫组件：按**组件名**全 `src` 搜引用；扫依赖：**三方一致**才算数（精确 `import` + **构建产物
  `dist/assets` 里是否出现** + 该目录的 import 来源）；
  扫 key：取「表内 key」与「被引用 key」的**集合差**，并**跑反向差集自检**
  （若出现"引用了不存在的 key"，说明搜索模式太宽 —— 用 `\bt\(` 而不是 `t\(`，后者会误匹配 `emit(`）。
- **破坏性命令失败后，别只看 `git status`**：源码干净**不代表环境完好**。
  实测 `pnpm remove` 被删除守门拦后**中断在删文件阶段**，`node_modules/vite`、`node_modules/vue`
  都被删掉，而 `git status` 完全干净（直到 build 报 `Cannot find module` 才暴露）。
  ⇒ 失败后要**抽查它实际碰过的目录**再判断"零副作用"。

## 测试要求

- 改动按边界测：schema→仓库测试；scheduler/reconcile→幂等/租约/恢复测试；
  HTTP/CLI 契约→DTO+错误信封契约测试；密钥→所有传输层的非泄露测试；
  Vue→typecheck + 构建。
- 测试绝不创建真实 RunningHub 任务（除非显式专用账号 + 执行标志）。
- **新增守卫/校验要有负对照**：临时停用该守卫，对应测试**必须失败**，否则说明测试
  钉住的是别的路径（"绿了"只是假设）。实测：多模态"读完按实际字节复核"停用后，
  测试直接暴露出 **5MB 图片被读入并 base64** ⇒ 才算证明这条测试钉住的是这道复核。
- 测试替身 `infrastructure/fakes.rs` 必须与 sqlite 行为对齐（如 id 分配），
  发现偏差即修。**另**：fakes 是"原样存"语义（如 `create_asset` 不重算 `byte_size`），
  这正好用来**低成本构造边界场景**（元数据与内容不一致）而不用另写替身。

## 禁忌

- 不得把密钥/Authorization/原始上传体写进日志、错误、测试输出。
- 不得让前端/CLI/MCP 直接读写 SQLite 或 keyring。
- 不得在 transports 里编排多步持久化（写端点用例化）。
- 不得绕过 scheduler 直投 provider。
- **不得裸建 reqwest 客户端**：出网的一律经
  `infrastructure::providers::proxy::configure()`（环境变量与 Windows 系统代理都在那里
  汇成同一份判据），刻意直连的必须写 `.no_proxy()` 并在旁边说明理由 ——
  `scripts/check-layers.mjs` 有机器守门，例外名单要写清原因。
- 不要删除工作区里**未被跟踪**的本地目录（维护者的笔记与跨会话上下文，不在版本库里）。

## 当前状态

当前实现状态、已完成能力和待验证事项统一记录在 [`status.md`](status.md)。
维护指南不复制带日期的进度、分支或工作区状态，避免这份入口文档快速过期。
