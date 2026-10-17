//! 动词表 —— **一份定义，两个客户端渲染**。
//!
//! 为什么有这个文件：CLI 与 MCP 此前各自手写一份动词清单（`cli.rs` 里一套 clap 子命令、
//! `mcp.rs` 里一套 `tools()`），于是**加一个能力要改两三处**，而"哪个通道支持哪些动词"
//! 只能靠人工比对两个文件才能发现——本仓库真的漂移过两次（花钱闸门只在 agent 层、
//! 领域知识只在 agent 描述里）。
//!
//! 现在：动词、参数（名字/类型/必填/哪些通道有）、只读与破坏性标注、两边的文案，
//! 全部写在下面这张表里。
//!
//! * CLI 用 clap builder **渲染**成子命令（`cli_command`），flag 名由参数名推导；
//! * MCP **渲染**成工具 schema（`mcp_tools`），键名由参数名推导；
//! * **分发**（动词 → client 调用）也只写一次（`execute`），两个通道共用。
//!
//! 加一个能力 = 在表里加一行。某个动词"MCP 不支持" = 它的 `mcp_name` 是 `None`
//! ——**写在表里**，不靠人比对两个文件。
//!
//! **不在表里的东西**（是决定，不是遗漏——写在这里免得下一个人重新论证一遍）：
//! * `connection` **只读可见，写入只从文件读**：`connection list` 给 MCP（Agent 要
//!   `connectionId`，库里一条 recipe 都没有时也有正路），`connection set-key` 只给 CLI
//!   且**值从文件读**——密钥不穿 MCP（参数是模型生成的），也不进命令行（history /
//!   进程列表 / CI 日志）。`settings` / `token` / `diagnostics` 整组不做动词：运维面，
//!   不是使用面。
//! * 各资源的 **`update` 也不做**：改元数据属于策展，与 `recipe.remove` / `agent.delete`
//!   同属"人的动作"（那三个是显式 `mcp_name: None`，这里是整组不出现）。
//! * `operation.events`（SSE）不做：那是界面实时刷新的通道，不是请求-响应式的动词。
//!
//! ⚠️ 表里写下的东西由测试钉住（见文件末尾）：
//! * 每个动词都有 clap 子命令、每个 MCP 工具都来自表（**双向**断言，不是人工比对）；
//! * 每个动词都能被分发（"表里有、实现里没有"会红）；
//! * 分发读的参数名**必须**在表里声明（`Args::param`），否则立刻报 `UNKNOWN_PARAM`
//!   ——于是"schema 说 A、实现读 B"这种静默漂移在结构上做不到。

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Map, Value};
use std::path::Path;

use super::client::{run_request, ClientError, V2Client};
use super::daemon_boot;

// ---------- 通道 ----------

/// 哪些通道暴露某个动词 / 某个参数。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Channels(u8);

impl Channels {
    pub const NONE: Channels = Channels(0);
    pub const CLI: Channels = Channels(1);
    pub const MCP: Channels = Channels(2);
    pub const BOTH: Channels = Channels(3);

    pub const fn has(self, other: Channels) -> bool {
        self.0 & other.0 != 0
    }
}

// ---------- 参数 ----------

/// 参数类型 —— 决定它在两个通道上各自的渲染，以及分发时怎么读。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// u64 标识。CLI：`--recipe <id>`（名字去掉 `Id` 后缀）；MCP：integer。
    Id,
    /// 计数 / 上限。CLI：`--limit <n>`；MCP：integer（可带 minimum/maximum）。
    Count,
    /// 一段文本。
    Text,
    /// 开关。CLI：`--purge`（不带值）；MCP：boolean。
    Flag,
    /// id 列表。CLI：`--recipe-allowlist 1,2`；MCP：整数数组。
    IdList,
    /// 自由 JSON 对象。CLI：传**文件路径**（读进来解析）；MCP：直接给对象。
    Json,
    /// JSON 数组（批量 runs）。CLI：传文件路径；MCP：直接给数组。
    JsonArray,
    /// 本机文件路径。两个通道都只传路径，**由分发自己读字节**
    /// （MCP 是本机 stdio 进程，读得到；让模型把 base64 塞进 JSON 会把 1MB 图变成 1.4MB token）。
    LocalFile,
    /// 媒体资产（字段键 → 资产）。
    ///
    /// * CLI：可重复的 `--asset '<字段键>=<本机文件路径>'`；分发负责**先上传**再把
    ///   `{assetId, checksum}` 组装成 keyed 对象（上传本身不计费）。
    /// * MCP：直接给**已上传**的 keyed 对象 `{ "<字段键>": {assetId, checksum} }`
    ///   （先 `asset.upload` 拿 assetId）——模型不给文件路径，避免把字节读进上下文。
    ///
    /// ⚠️ 位置数组在 MCP 形状里**不接受**：它只是 HTTP 的兼容编码，让模型按位置猜
    /// 字段正是错位事故的来源（字段键在 inputSchema 里，`recipe.describe` 就能看到）。
    KeyedAssets,
}

#[derive(Clone, Copy, Debug)]
pub struct Param {
    /// 规范名（camelCase）。**唯一权威**：MCP 的键、CLI 的 arg id、分发读取的键都用它。
    pub name: &'static str,
    pub kind: Kind,
    /// 哪些通道有这个参数。
    pub channels: Channels,
    /// 哪些通道上它必填（必须是 `channels` 的子集）。
    pub required: Channels,
    /// CLI flag 名覆盖（默认由 `name` 推导：去掉 `Id` 后缀再转 kebab-case）。
    pub cli_flag: Option<&'static str>,
    /// MCP 键名覆盖（默认就是 `name`）。
    pub mcp_key: Option<&'static str>,
    /// 缺省值（按 `kind` 解析）。CLI 交给 clap 渲染，MCP 由 `Args::from_mcp` 补。
    pub default: Option<&'static str>,
    /// 上下界（只影响 MCP schema 与文档）：`Count` → `minimum`/`maximum`；数组 → `minItems`/`maxItems`。
    ///
    /// ⚠️ 这不是"文档装饰"：**凡是服务端已经在强制的边界，都必须出现在协议地板上** ——
    /// 只写在散文里的边界，调用方只能靠自觉。
    pub min: Option<u64>,
    pub max: Option<u64>,
    /// CLI help（一行）。
    pub help: &'static str,
    /// MCP schema 里的 description。
    pub mcp: Option<&'static str>,
}

impl Param {
    /// CLI 上的 flag 名。
    pub fn cli_name(&self) -> String {
        match self.cli_flag {
            Some(flag) => flag.to_string(),
            None => kebab(self.name.strip_suffix("Id").unwrap_or(self.name)),
        }
    }

    /// MCP 上的键名。
    pub fn mcp_name(&self) -> &'static str {
        self.mcp_key.unwrap_or(self.name)
    }
}

/// camelCase → kebab-case（`recipeRevision` → `recipe-revision`）。
fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

// ---------- 动词 ----------

#[derive(Clone, Copy, Debug)]
pub struct Verb {
    /// 规范 id（`recipe.list`）。CLI 信封里的 `command`，也是分发的键。
    pub id: &'static str,
    pub group: &'static str,
    pub action: &'static str,
    /// CLI help（一行）。
    pub summary: &'static str,
    /// MCP 工具名。**`None` = 这个动词 MCP 不支持**（这就是"覆盖差异写在表里"）。
    pub mcp_name: Option<&'static str>,
    pub mcp_description: Option<&'static str>,
    pub read_only: bool,
    pub destructive: bool,
    /// MCP 的 `idempotentHint`：同样的参数重复调用不会产生第二次副作用。
    pub idempotent: bool,
    /// **这个动词会花用户自己的额度**（钱）。它与"不可逆"是两件事，必须分开报：
    /// `operation.cancel` 不可逆但不花钱，`operation.run` 花钱。
    ///
    /// ⚠️ **"花钱"与"幂等"也要分开报，而且这里有一个真实的坑**：`operation.run` 只在调用方
    /// **显式传同一个 `idempotencyKey`** 时才幂等（同键同请求重放，不再计费）。**不带键时，
    /// 客户端每次自己生成一个新 UUID 键**（见 `run_operation`）⇒ 两次"完全相同"的调用是
    /// **两次独立计费**。所以它的 `idempotent` 必须是 `false`：把它标成 `true`，等于在协议
    /// 地板上对一条会花钱的动作说假话——照着 `idempotentHint` 重试的调用方会多付一次钱。
    ///
    /// 为什么是每行一个字段，而不是像 `needs_daemon()` 那样按规则推导：**没有可推导的规则** ——
    /// 它既不跟 group 走（同组的 `get` / `wait` / `cancel` 都不花钱），也不跟 `destructive` 走。
    /// 而且它是**必填字段**：少写一行编译不过，所以不存在"某个动词偷偷不标花钱"（那正是
    /// 按规则推导才会有的失效模式）。
    ///
    /// 它是 MCP 地板上 `[COST]` 标记的唯一来源（有测试钉住"标记集合 == 表"）。
    pub spends_credits: bool,
    pub params: &'static [Param],
}

impl Verb {
    pub fn mcp_params(&self) -> impl Iterator<Item = &'static Param> {
        self.params
            .iter()
            .filter(|param| param.channels.has(Channels::MCP))
    }

    pub fn cli_params(&self) -> impl Iterator<Item = &'static Param> {
        self.params
            .iter()
            .filter(|param| param.channels.has(Channels::CLI))
    }

    /// 这个动词需不需要 daemon **已经**在跑？
    ///
    /// `daemon.*` 是"关于 daemon 本身"的动词，答案是不需要——而且**不能**自动拉起：
    /// 问"它在不在跑"却先把它启动起来是自相矛盾的，"让它停"更是如此。
    ///
    /// 判据按 group 推导（而不是每个动词加一个字段）：那会在 20 多个表项里各写一遍，
    /// 漏一个就是"某个动词偷偷不再自动拉起了"。有测试钉着这一组。
    pub fn needs_daemon(&self) -> bool {
        self.group != "daemon"
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub name: &'static str,
    pub about: &'static str,
}

pub static GROUPS: &[Group] = &[
    Group {
        name: "connection",
        about: "连接：查看平台连接；写入密钥只从文件读（不进命令行）",
    },
    Group {
        name: "daemon",
        about: "守护进程本身：在不在跑、日志在哪、让它停（不给 AI）",
    },
    Group {
        name: "callback",
        about: "完成回调：签名密钥只从文件读，且只回「配没配」（不给 AI）",
    },
    Group {
        name: "recipe",
        about: "应用（Recipe）：列出、查看、导入",
    },
    Group {
        name: "operation",
        about: "任务（Operation）：列出、提交、查询、等待、取消",
    },
    Group {
        name: "asset",
        about: "资产：把本机文件上传成可被应用引用的输入",
    },
    Group {
        name: "artifact",
        about: "产物：任务产出的文件元数据",
    },
];

/// 参数构造器：两个通道都有、可选、无缺省。
const fn opt(name: &'static str, kind: Kind) -> Param {
    Param {
        name,
        kind,
        channels: Channels::BOTH,
        required: Channels::NONE,
        cli_flag: None,
        mcp_key: None,
        default: None,
        min: None,
        max: None,
        help: "",
        mcp: None,
    }
}

/// 参数构造器：两个通道都有、都必填。
const fn req(name: &'static str, kind: Kind) -> Param {
    Param {
        required: Channels::BOTH,
        ..opt(name, kind)
    }
}

/// **动词表**。顺序即 CLI help 里的顺序。
pub static VERBS: &[Verb] = &[
    // ---------- connection ----------
    Verb {
        id: "connection.list",
        group: "connection",
        action: "list",
        summary: "列出已配置的平台连接（不含密钥）",
        mcp_name: Some("renderloft_list_connections"),
        mcp_description: Some(
            "List the platform connections configured in this workspace: id, display name, vendor, \
             protocol and status. No secret is ever returned. This is how you get a `connectionId` \
             for renderloft_import_recipe when the workspace has no recipes yet — otherwise you can \
             take one from any recipe's `defaultConnectionId` in renderloft_list_recipes.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[],
    },
    Verb {
        id: "connection.models",
        group: "connection",
        action: "models",
        summary: "列出这条连接上平台声明的可用模型",
        mcp_name: Some("renderloft_list_models"),
        mcp_description: Some(
            "List the model ids the platform behind a connection reports (OpenAI-compatible \
             `GET {baseUrl}/models`). Read-only and free. Use it to pick a `model` value instead \
             of guessing one: pass a connectionId from renderloft_list_connections. Providers that \
             have no model listing (RunningHub, ComfyUI) answer with MODELS_NOT_SUPPORTED.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "连接 id（见 connection list）",
            ..req("connectionId", Kind::Id)
        }],
    },
    Verb {
        id: "connection.set-key",
        group: "connection",
        action: "set-key",
        summary: "从文件设置连接的 API Key（值不进命令行）",
        // **刻意不给 MCP**：MCP 工具的参数是模型生成的，密钥进去就等于进了模型上下文与
        // 对话记录。CLI 这条路存在，是为了无头场景（没有界面可点）——但值必须从文件读。
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        // 覆盖一个正在用的密钥会让原本能跑的连接失败，所以它是破坏性的。
        destructive: true,
        // 同一个值重复写就是同一个结果。
        idempotent: true,
        spends_credits: false,
        params: &[
            Param {
                help: "连接 id（见 connection list）",
                ..req("connectionId", Kind::Id)
            },
            Param {
                help: "文件路径：文件内容就是密钥（首尾空白会被去掉）",
                ..req("fromFile", Kind::LocalFile)
            },
            Param {
                help: "覆盖已有的 API Key（不给就拒绝——覆盖会让原本能跑的连接失败，旧值不可恢复）",
                ..opt("force", Kind::Flag)
            },
        ],
    },
    // ---------- daemon ----------
    //
    // 这一组是**关于 daemon 本身**的（它在不在跑、让它停），所以：
    // - **不自动拉起 daemon**（见 `Verb::needs_daemon`）：问"它在不在"却先把它启动起来，
    //   是自相矛盾的；"停"更是如此。
    // - **刻意都不给 MCP**（`mcp_name: None`）：让 AI 能关掉 daemon 是错的授权方向——
    //   任务在后台跑、对账靠 daemon 常驻，进程生命周期是人的事。
    Verb {
        id: "daemon.status",
        group: "daemon",
        action: "status",
        summary: "daemon 在不在跑、版本、日志在哪、还有几条任务没结束",
        mcp_name: None,
        mcp_description: None,
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[],
    },
    Verb {
        id: "daemon.stop",
        group: "daemon",
        action: "stop",
        summary: "让 daemon 优雅退出（任务不会丢：平台侧继续跑，下次启动对账回来）",
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        destructive: true,
        idempotent: true,
        spends_credits: false,
        params: &[],
    },
    // ---------- callback ----------
    //
    // 这两条**刻意都不给 MCP**（`mcp_name: None`）：它们碰的是回调签名密钥。
    // MCP 的工具参数是模型生成的，密钥进去就等于进了模型上下文与对话记录；
    // 而"读状态"虽然只回 true/false，也没必要给 AI 一条探边界的手。
    // 与"写密钥不给 AI"是同一条边界（见 `connection.set-key`）。
    Verb {
        id: "callback.set-secret",
        group: "callback",
        action: "set-secret",
        summary: "从文件设置回调签名密钥（值不进命令行）",
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        // 覆盖密钥会让原本能验签的接收端验不过（旧值不可恢复），所以是破坏性的。
        destructive: true,
        idempotent: true,
        spends_credits: false,
        params: &[
            Param {
                help: "文件路径：文件内容就是密钥（首尾空白会被去掉）",
                ..req("fromFile", Kind::LocalFile)
            },
            Param {
                help: "覆盖已有的签名密钥（不给就拒绝——旧值不可恢复）",
                ..opt("force", Kind::Flag)
            },
        ],
    },
    Verb {
        id: "callback.status",
        group: "callback",
        action: "status",
        summary: "回调签名密钥配没配（只回 true/false，永远不回值）",
        mcp_name: None,
        mcp_description: None,
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[],
    },
    Verb {
        id: "callback.clear-secret",
        group: "callback",
        action: "clear-secret",
        summary: "清掉回调签名密钥（关掉签名；之后投递不带签名头）",
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        // 清掉之后接收端就验不了了——但它**可逆**（重新设一次即可），所以不加 --force。
        destructive: true,
        idempotent: true,
        spends_credits: false,
        params: &[],
    },
    // ---------- recipe ----------
    Verb {
        id: "recipe.list",
        group: "recipe",
        action: "list",
        summary: "列出应用（Recipe）",
        mcp_name: Some("renderloft_list_recipes"),
        mcp_description: Some(
            "List AIGC recipes available in the local RenderLoft workspace. Start here: this is the \
             app index (id, name, capability, favorite, and the `defaultConnectionId` that \
             renderloft_import_recipe needs). It deliberately carries NO field schemas — pick an app \
             here, call renderloft_describe_recipe for its fields, then renderloft_run_recipe to submit.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                default: Some("50"),
                min: Some(1),
                max: Some(200),
                help: "最多返回多少条（上限 200）",
                mcp: Some("How many recipes to return (1-200)."),
                ..opt("limit", Kind::Count)
            },
            Param {
                // CLI 独有：命令行要能看见回收站里有什么，否则"恢复"无从下手。
                // 刻意不给 MCP：外界 Agent 不需要（也不该）知道用户移除了什么。
                channels: Channels::CLI,
                help: "列出**已移除**的（回收站），而不是活着的那些",
                ..opt("removed", Kind::Flag)
            },
        ],
    },
    Verb {
        id: "recipe.describe",
        group: "recipe",
        action: "describe",
        summary: "看一个应用的输入/输出 schema",
        mcp_name: Some("renderloft_describe_recipe"),
        mcp_description: Some(
            "Get a recipe's input and output JSON Schemas — call this before renderloft_run_recipe: \
             `inputs` must match the input schema exactly. Field descriptions come from the upstream \
             platform and are often just the field name; a human may have added a clearer label.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "Recipe id（见 recipe list）",
            mcp: Some("Recipe id (see renderloft_list_recipes)."),
            ..req("recipeId", Kind::Id)
        }],
    },
    Verb {
        id: "recipe.import",
        group: "recipe",
        action: "import",
        summary: "从平台链接/应用 ID 导入一个应用",
        mcp_name: Some("renderloft_import_recipe"),
        mcp_description: Some(
            "Import an app (recipe) from a platform app link or app id into the local workspace, \
             so it becomes runnable and curatable locally. `connectionId` is required and you can \
             take one from any recipe's `defaultConnectionId` in renderloft_list_recipes (the \
             workspace is single-account in practice). Afterwards call renderloft_describe_recipe \
             to see the fields it needs. Importing the same platform app twice is refused with \
             WEBAPP_ALREADY_IMPORTED — remove the old copy first if you really want a refresh.",
        ),
        read_only: false,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                help: "平台连接 id（见 recipe list 里的 defaultConnectionId）",
                mcp: Some("Connection id to import through (see a recipe's defaultConnectionId)."),
                ..req("connectionId", Kind::Id)
            },
            Param {
                help: "平台应用链接或应用 ID",
                mcp: Some("Platform app link (URL) or bare app id."),
                ..req("source", Kind::Text)
            },
        ],
    },
    Verb {
        id: "recipe.import-workflow",
        group: "recipe",
        action: "import-workflow",
        summary: "从 RunningHub 工作流链接/ID 导入一个工作流应用",
        // 给 MCP：与 `recipe.import`（AI 应用）同一口径——导入**不是**策展，
        // 勾字段与写备注才是人的事（这里刻意一个字段都不挑，全暴露）。
        mcp_name: Some("renderloft_import_workflow"),
        mcp_description: Some(
            "Import a RunningHub workflow (the platform's `/post/<id>` link or bare workflow id) \
             as a runnable local app. `connectionId` is a RunningHub connection (see \
             renderloft_list_connections). The workflow itself stays on the platform — we store \
             the workflow id plus the discovered input fields, so the graph never drifts from \
             what the platform has. Every input field is exposed; call \
             renderloft_describe_recipe afterwards to see them. Importing the same workflow twice \
             is refused with WORKFLOW_ALREADY_IMPORTED.",
        ),
        read_only: false,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                help: "RunningHub 连接 id（见 connection list）",
                mcp: Some("RunningHub connection id to import through."),
                ..req("connectionId", Kind::Id)
            },
            Param {
                help: "平台工作流链接或工作流 ID",
                mcp: Some("Platform workflow link (URL) or bare workflow id."),
                ..req("source", Kind::Text)
            },
            Param {
                help: "导入后的名字（不给就用平台的默认名）",
                mcp: Some("Optional display name; defaults to a name built from the workflow id."),
                ..opt("name", Kind::Text)
            },
        ],
    },
    Verb {
        id: "recipe.remove",
        group: "recipe",
        action: "remove",
        summary: "从资源库移除一个应用（打墓碑，可恢复）",
        // **只给 CLI**：策展自己的应用库是**人**的动作，与 `agent create/delete` 同一个口径
        //（改的是"Agent 能跑什么"这条授权边界本身）。外界 Agent 不参与策展。
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        destructive: true,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "Recipe id（见 recipe list）",
            ..req("recipeId", Kind::Id)
        }],
    },
    Verb {
        id: "recipe.restore",
        group: "recipe",
        action: "restore",
        summary: "从「已移除」里恢复一个应用",
        // 同上：CLI 独有。
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "Recipe id（见 recipe list --removed）",
            ..req("recipeId", Kind::Id)
        }],
    },
    // ---------- operation ----------
    Verb {
        id: "operation.list",
        group: "operation",
        action: "list",
        summary: "列出最近的任务（新的在前）",
        mcp_name: Some("renderloft_list_operations"),
        mcp_description: Some(
            "List recent operations (tasks), newest first: id, status, run count and creation time. \
             Use this when an operation id has been lost — renderloft_run_recipe already returns one, \
             so you rarely need it. This carries summaries only: for the inputs a task was submitted \
             with, call renderloft_get_operation. To page further back, pass `after` a previous \
             `nextCursor`.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                default: Some("50"),
                min: Some(1),
                max: Some(200),
                help: "最多返回多少条（上限 200）",
                mcp: Some("How many operations to return (1-200)."),
                ..opt("limit", Kind::Count)
            },
            Param {
                help: "游标：只看比这个任务更早的（取上次结果里的 nextCursor）",
                mcp: Some("Cursor: only operations older than this id (a previous nextCursor)."),
                ..opt("after", Kind::Id)
            },
        ],
    },
    Verb {
        id: "operation.run",
        group: "operation",
        action: "run",
        summary: "提交任务（会花用户额度：CLI 一次一个，批量用 MCP 的 runs）",
        mcp_name: Some("renderloft_run_recipe"),
        mcp_description: Some(
            "Submit recipe execution(s). Most providers CHARGE THE USER'S OWN CREDITS per run \
             (every entry in `runs` is a separate charged run), and \
             real work takes tens of seconds to minutes — this call returns as soon as the work is \
             queued, not when it finishes. For several inputs, put them in ONE call's `runs` array \
             (up to 100) instead of making many calls: it is cheaper to track and the daemon caps \
             concurrent operations from autonomous callers. For media recipes, pass `assets` keyed \
             by the recipe's field keys ({fieldKey: {assetId, checksum}}; upload files with \
             renderloft_upload_asset first) — never positional arrays. `dryRun: true` pre-checks the \
             whole request (revision, keys, assets) without creating or charging anything. For a long \
             job, pass `callbackUrl`: the \
             daemon POSTs a terminal snapshot to that URL when the operation finishes, so you can \
             submit and end your turn instead of waiting inside this call — a tool call is \
             request/response, and a long wait occupies the client's request channel (its own \
             timeout may be shorter than yours). If you do wait, use renderloft_wait_operation with \
             a SHORT timeoutSeconds and call it again as needed. If a run fails, read its `error` \
             together with the `inputs` that were submitted, fix the offending parameter, and only \
             then resubmit — blindly retrying costs money again.",
        ),
        read_only: false,
        destructive: false,
        // ⚠️ **不是幂等**：幂等只在"显式传同一个 `idempotencyKey`"时成立，而不带键的调用每次
        // 拿一个新键（`run_operation` 里生成）⇒ 重试 = 第二次计费。标成 `true` 会让调用方
        // 照着 `idempotentHint` 放心重试，然后多花一次钱。
        idempotent: false,
        spends_credits: true,
        params: &[
            Param {
                help: "Recipe revision id（缺省用当前 revision）",
                mcp: Some("Recipe revision id (defaults to the recipe's current revision)."),
                ..req("recipeRevisionId", Kind::Id)
            },
            Param {
                required: Channels::CLI,
                cli_flag: Some("input"),
                help: "输入 JSON 文件（内容必须匹配该应用的 inputSchema）",
                mcp: Some(
                    "Single-run shorthand. Must match the recipe's inputSchema exactly — call \
                     renderloft_describe_recipe first. Field descriptions are whatever the upstream \
                     platform supplied, which is often just the field name; a human may have added a \
                     clearer label in the app.",
                ),
                ..opt("inputs", Kind::Json)
            },
            Param {
                cli_flag: Some("asset"),
                help: "媒体资产（可重复）：'<字段键>=<本机文件路径>'；字段键见 recipe 的 inputSchema（如 475::image）",
                mcp: Some(
                    "Media assets, keyed by field: {\"<field key>\": {\"assetId\": .., \"checksum\": \"..\"}}. \
                     Upload each file with renderloft_upload_asset first and use the RETURNED assetId+checksum here \
                     (do not put assetIds in `inputs`; schema media fields have format \"file\"). The field key \
                     (e.g. \"475::image\") is the address — array position is never the meaning.",
                ),
                ..opt("assets", Kind::KeyedAssets)
            },
            Param {
                help: "只预检不提交：校验 revision/字段键/资产后报告结论（不建任务、不调度、不花钱）",
                mcp: Some(
                    "Validate only (dry run): same checks as a real submission (revision, inputSchema, \
                     field keys, asset existence + checksum) but creates nothing and charges nothing. \
                     Use it to pre-flight a shape before spending credits.",
                ),
                ..opt("dryRun", Kind::Flag)
            },
            Param {
                // MCP 独有：批量的形状（CLI 目前没有对应 flag，这一点写在表里而不是靠人比对）。
                channels: Channels::MCP,
                help: "",
                // **把已经在强制的上限搬上协议地板**：daemon 侧 `create_operation.rs` 的
                // `MAX_RUNS_PER_OPERATION` 拒绝超过这个数的请求，而 schema 里此前只有一句
                // 散文说 "up to 100" —— 调用方看不见，就没法据以规划。
                // 两边同值由测试钉住（表与代码没有编译期联系）。
                max: Some(100),
                mcp: Some(
                    "Preferred for batches: one entry per execution, each with its own \
                     inputs/clientRunId. Every entry is a **separate charged run**; at most 100 \
                     per call.",
                ),
                ..opt("runs", Kind::JsonArray)
            },
            Param {
                help: "幂等键：同键同请求会重放原来的任务，而不是再花一次钱",
                mcp: Some(
                    "Same key + same request replays the original operation instead of charging again.",
                ),
                ..opt("idempotencyKey", Kind::Text)
            },
            Param {
                help: "完成回调地址：进终态时 daemon 会 POST 一份快照过去（失败会重试）",
                mcp: Some(
                    "Optional webhook. When the operation reaches a terminal state, the daemon POSTs \
                     a small snapshot ({operationId, status, runs:[{runId, status, clientRunId}]}) to \
                     this URL and retries on failure. Use it to \"submit and walk away\" instead of \
                     polling — an MCP tool call is request/response, so waiting inside it blocks the \
                     client. Must be https://, or http:// on a loopback host (127.0.0.1 / localhost \
                     / [::1]) when the receiver runs on this machine.",
                ),
                ..opt("callbackUrl", Kind::Text)
            },
        ],
    },
    Verb {
        id: "operation.get",
        group: "operation",
        action: "get",
        summary: "查一个任务及其全部 run",
        mcp_name: Some("renderloft_get_operation"),
        mcp_description: Some(
            "Get an operation with all its runs and current statuses. Each run carries the `inputs` \
             that were actually submitted alongside its `error` — read them together when diagnosing \
             a failure (the platform's message alone often does not say which parameter was wrong).",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "Operation id",
            mcp: Some("Operation id."),
            ..req("operationId", Kind::Id)
        }],
    },
    Verb {
        id: "operation.wait",
        group: "operation",
        action: "wait",
        summary: "等一个任务到终态",
        mcp_name: Some("renderloft_wait_operation"),
        mcp_description: Some(
            "Wait for an operation to reach a terminal state. The waiting happens INSIDE this \
             call, so it occupies the client's request channel — and the client's own request \
             timeout may be shorter than timeoutSeconds, in which case this call (and the ones \
             right after it) time out even though the work is fine. Use a SHORT timeoutSeconds and \
             call again if needed. For long jobs prefer `callbackUrl` on renderloft_run_recipe: \
             submit, end your turn, get notified. renderloft_get_operation is a cheap read that \
             never blocks — checking it between turns is fine.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                help: "Operation id",
                mcp: Some("Operation id."),
                ..req("operationId", Kind::Id)
            },
            Param {
                cli_flag: Some("interval"),
                default: Some("5"),
                min: Some(1),
                max: Some(60),
                help: "轮询间隔秒",
                mcp: Some("Seconds between polls inside this call."),
                ..opt("intervalSeconds", Kind::Count)
            },
            Param {
                cli_flag: Some("timeout"),
                default: Some("0"),
                min: Some(1),
                help: "最多等多少秒（0 = 一直等）",
                mcp: Some("Give up (and return complete:false) after this many seconds."),
                ..opt("timeoutSeconds", Kind::Count)
            },
        ],
    },
    Verb {
        id: "operation.cancel",
        group: "operation",
        action: "cancel",
        summary: "取消一个任务",
        mcp_name: Some("renderloft_cancel_operation"),
        mcp_description: Some(
            "Cancel a queued or running operation. Runs that cannot be cancelled remotely are \
             reported in remoteMayContinue.",
        ),
        read_only: false,
        destructive: true,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "Operation id",
            mcp: Some("Operation id."),
            ..req("operationId", Kind::Id)
        }],
    },
    // ---------- artifact ----------
    Verb {
        id: "artifact.list",
        group: "artifact",
        action: "list",
        summary: "列出产物",
        mcp_name: Some("renderloft_list_artifacts"),
        mcp_description: Some(
            "List generated artifacts produced by operations. Some providers store results as a \
             remote URL only (bytes stay on the platform), so a missing/zero byte size does NOT \
             mean the artifact is empty — fetch it by id if you need the bytes. Paged: pass `limit` \
             (default 50, max 200) and `after` an artifact id to walk backwards.",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                help: "最多返回多少条（1-200，缺省 50）",
                ..opt("limit", Kind::Count)
            },
            Param {
                help: "游标：只看这个产物 id 之前的（上一页最后一条的 id）",
                ..opt("after", Kind::Id)
            },
        ],
    },
    Verb {
        id: "artifact.get",
        group: "artifact",
        action: "get",
        summary: "看单个产物的元数据",
        mcp_name: Some("renderloft_get_artifact"),
        mcp_description: Some(
            "Get one artifact's metadata by id: filename, mimeType, byteSize and checksum. Use it \
             after renderloft_list_artifacts when you need the details of a specific result — and \
             remember byteSize 0 does not mean empty (the bytes may live on the platform).",
        ),
        read_only: true,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[Param {
            help: "Artifact id（见 artifact list）",
            mcp: Some("Artifact id (see renderloft_list_artifacts)."),
            ..req("artifactId", Kind::Id)
        }],
    },
    Verb {
        id: "artifact.download",
        group: "artifact",
        action: "download",
        summary: "把产物的字节下载到本地文件（byteSize 0 的产物会按需从平台拉取）",
        // **刻意不给 MCP**：它往本地磁盘写文件，落点由参数决定。让 AI 拿到"任意路径写文件"
        // 这个能力，与"密钥不穿 MCP"是同一种考虑——能力面要给得有意，不是顺手就给。
        // 脚本与界面本来就有这个需求（收图、归档），它们走 CLI。
        mcp_name: None,
        mcp_description: None,
        read_only: false,
        destructive: false,
        // ⚠️ **不是幂等的**（这里此前写成 true，是错的）：同一个产物重复下载到同一路径确实
        // 得到同样的字节，但**不同产物可以同名**（RunningHub 按输出节点命名，同一 recipe 的
        // 每个 run 都产出 `870.png`）⇒ 第二次下载会覆盖掉第一次的字节，落盘结果因此取决于
        // "下过几个、下到哪"。落点里含调用方参数，幂等判定不能只看"同 id 同结果"。
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                help: "Artifact id（见 artifact list）",
                ..req("artifactId", Kind::Id)
            },
            Param {
                help: "写到哪个文件；给目录（或以 \\ / 结尾、不存在也会创建）则用产物原名。缺省 = 当前目录 + 产物原名",
                ..opt("to", Kind::Text)
            },
            Param {
                // 默认 rename，与浏览器/yt-dlp 的 `name (1).ext` 同形（主流下载器的默认行为）。
                // 刻意不复用 --force：那在本项目里已经是"覆盖凭据/密钥"的既有语义。
                help: "目标已存在怎么办：rename（默认，加 (1)(2) 序号）/ skip（不动）/ overwrite（覆盖）",
                default: Some("rename"),
                ..opt("onConflict", Kind::Text)
            },
            Param {
                // "名字该叫什么"不该由我们替调用方决定，所以默认照抄平台名、把选择权做成参数
                // （yt-dlp 的 `-o` 就是这个位置）。占位符只有两个、且都来自产物元数据：
                // 刻意不做 {date} 这类，那会引入"同一条命令跑两次结果不同"的不确定性。
                help: "自己定文件名（与 --to 给目录同用）：{artifactId} / {filename}；可含子目录，如 {artifactId}/{filename}",
                ..opt("nameTemplate", Kind::Text)
            },
        ],
    },
    // ---------- asset ----------
    Verb {
        id: "asset.upload",
        group: "asset",
        action: "upload",
        summary: "把本机文件上传成资产",
        mcp_name: Some("renderloft_upload_asset"),
        mcp_description: Some(
            "Upload a local file as an asset and get its assetId. Use this for recipe media \
             inputs: the recipe's inputSchema media fields have \"format\": \"file\", and the \
             uploaded asset belongs in the run's `assets` object keyed by the recipe's field key \
             — NOT in `inputs` (putting assetId in inputs silently fails) and NOT by array \
             position. Shape: {\"<field key>\": {\"assetId\": <returned id>, \"checksum\": \
             \"<returned checksum>\"}} — the field key (e.g. \"475::image\") comes from \
             renderloft_describe_recipe's inputSchema. Example: renderloft_run_recipe with \
             assets {\"475::image\": {\"assetId\": 12, \"checksum\": \"..\"}}. \
             The `renderloft-cli` CLI does this end to end with: renderloft-cli operation run \
             --recipe-revision N --input inputs.json --asset '475::image=person.png'. \
             Pass an ABSOLUTE path to a file on THIS machine — the MCP server is a local \
             process, it reads the file itself and uploads the bytes.",
        ),
        read_only: false,
        destructive: false,
        idempotent: false,
        spends_credits: false,
        params: &[
            Param {
                cli_flag: Some("file"),
                help: "本机文件路径",
                mcp: Some("Absolute path to the local file to upload."),
                ..req("path", Kind::LocalFile)
            },
            Param {
                help: "存下来的文件名（缺省用文件自己的名字）",
                mcp: Some("Name to store it under. Defaults to the file's own name."),
                ..opt("filename", Kind::Text)
            },
            Param {
                help: "MIME 类型（缺省按扩展名推断）",
                mcp: Some(
                    "MIME type, e.g. image/png. Defaults to the type guessed from the file extension.",
                ),
                ..opt("mimeType", Kind::Text)
            },
        ],
    },
];

/// 旧的**动态 per-app 工具**名前缀（`renderloft_run_{recipeId}`）。
///
/// 这类工具已经**不再出现在工具列表里**（理由见 `mcp.rs` 文件头：每次 tools/list 是
/// N+1 次 HTTP、列表随应用目录变形、与"索引常驻、schema 按需"的决定相反），
/// 但**名字仍然认**——调用方可能缓存过那份列表。正式形态是
/// `renderloft_run_recipe` + `recipeRevisionId`。
pub const DYNAMIC_TOOL_PREFIX: &str = "renderloft_run_";

pub fn find_by_id(verb_id: &str) -> Option<&'static Verb> {
    VERBS.iter().find(|verb| verb.id == verb_id)
}

pub fn find_by_cli(group: &str, action: &str) -> Option<&'static Verb> {
    VERBS
        .iter()
        .find(|verb| verb.group == group && verb.action == action)
}

/// 工具名 → 动词。旧动态名字（`renderloft_run_{id}`）回落到 `operation.run`，
/// 并把名字里的 recipeId 一并带出来（**兼容路径**，见 `DYNAMIC_TOOL_PREFIX`）。
pub fn route_tool(name: &str) -> Option<(&'static Verb, Option<u64>)> {
    if let Some(verb) = VERBS.iter().find(|verb| verb.mcp_name == Some(name)) {
        return Some((verb, None));
    }
    let recipe_id = name
        .strip_prefix(DYNAMIC_TOOL_PREFIX)
        .and_then(|value| value.parse::<u64>().ok())?;
    let verb = find_by_id("operation.run")?;
    Some((verb, Some(recipe_id)))
}

/// **元层入口**：`renderloft_call` 的 `verb` 参数 → 动词。
///
/// ⚠️ 两个必须守住的东西：
/// 1. **只有 `mcp_name` 非空的动词能穿过这里**——否则元层就成了绕过"刻意不给 AI"的后门
///    （`callback set-secret` / `daemon stop` / `artifact download` / `connection set-key`
///    都是 CLI 专属，它们必须**调不到**，而不是"没列出来"）。有测试钉着。
/// 2. 旧工具名仍然**认**（缓存过 `tools/list` 的调用方不该突然失败），但它们**不再出现在
///    清单里**——所以"兼容"不花常驻成本。
pub fn route_call(verb_id: &str) -> Result<&'static Verb, String> {
    let Some(verb) = VERBS.iter().find(|verb| verb.id == verb_id) else {
        return Err(format!(
            "动词表里没有 {verb_id}（先调 renderloft_catalog 看索引）"
        ));
    };
    if verb.mcp_name.is_none() {
        return Err(format!(
            "{verb_id} 不对 MCP 开放（它属于人的动作：密钥 / 进程寿命 / 往磁盘写文件 / 建连接）。\
             用 CLI：{} {}",
            verb.group, verb.action
        ));
    }
    Ok(verb)
}

// ---------- CLI 渲染 ----------

/// MCP 的**身份句**：这个 server 是干嘛的、什么时候该用它。
///
/// 为什么它必须常驻：元层把"能力名"（`renderloft_run_recipe` 这种自带领域暗示的名字）换成了
/// 一个中立的调用口，于是**"这是什么"就没有别的地方可放了**——模型看到的常驻文本里如果
/// 不说它是干嘛的，能力就等于不存在（"不可见 = 不存在"）。这一句就是那个代价的对价。
pub const MCP_IDENTITY: &str =
    "RenderLoft — a local workbench that runs the user's own image/video \
generation apps on their own cloud platform accounts (RunningHub / ComfyUI), tracks the tasks in \
the background, and collects the resulting files. Use it when the user asks to generate, edit, \
restyle, upscale or batch images/videos — or to see which apps (recipes) they have, what a task \
cost, or where its outputs went. Runs spend the user's own credits: [COST] marks the verbs that \
charge their account, [IRREVERSIBLE] the ones that cannot be undone.";

/// MCP 的两个工具名。**渐进式披露的形状**：常驻只有"索引 + 一个调用口"。
pub const MCP_CATALOG_TOOL: &str = "renderloft_catalog";
pub const MCP_CALL_TOOL: &str = "renderloft_call";

/// 成本与不可逆的**机器可读标记**：行首、顺序固定、纯 ASCII、码点唯一。
///
/// 为什么不用 `⚠️` 那一套：它与裸 `⚠` 之间只差一个 U+FE0F（零宽变体选择符）—— 实测表里
/// 18 处带、6 处不带，于是"花钱"与"持续不可逆"两类**只靠一个看不见的字符区分**。
/// 人眼看不见，正则写错一个字符就漏。
///
/// > **零宽差异不是通道。** 判据是"一条正则筛出一组，且与表逐条对上"（有测试钉着）。
pub const MCP_COST_MARKER: &str = "[COST]";
pub const MCP_IRREVERSIBLE_MARKER: &str = "[IRREVERSIBLE]";

/// 一个动词的行首标记（顺序固定：先钱、后不可逆）。两类都不是时是空串。
///
/// 事实来源只有表（`spends_credits` / `destructive`）——**不许从 `summary` 里"看有没有
/// ⚠️"推断**：那是散文通道，改一个字就静默失效（这正是它此前的失效方式）。
pub fn mcp_markers(verb: &Verb) -> String {
    let mut markers: Vec<&str> = Vec::new();
    if verb.spends_credits {
        markers.push(MCP_COST_MARKER);
    }
    if verb.destructive {
        markers.push(MCP_IRREVERSIBLE_MARKER);
    }
    if markers.is_empty() {
        String::new()
    } else {
        format!("{} ", markers.join(" "))
    }
}

/// **常驻索引**：一行一个动词（MCP 能碰到的那些）。
///
/// 为什么长这样（从模型侧推，不是从省 token 推）：
/// - 模型没有跨轮记忆，每轮都要重新从文本里推出"我能做什么" ⇒ **索引必须常驻**（否则能力
///   等于不存在）；
/// - 但"怎么调"（完整描述 + schema）**用到才要** ⇒ 那一半必须按需取；
/// - 扁平列表把两件事混在一起：它把"怎么调"对**每一个**能力、**每一轮**都付一遍，
///   常驻成本随 **Σ(每个能力的复杂度)** 增长——**加一个参数，全世界每一轮都多付一点**。
///   这个形状让它随 **O(能力数量) × 一行** 增长：加一个能力只是多一行，与它多复杂无关。
///
/// 全部从 `VERBS` 生成，**一个字都不手写**：手写就会漂移（有测试钉着）。
pub fn mcp_catalog() -> String {
    let mut lines = Vec::new();
    for verb in VERBS.iter().filter(|verb| verb.mcp_name.is_some()) {
        let required: Vec<&str> = verb
            .mcp_params()
            .filter(|param| param.required.has(Channels::MCP))
            .map(|param| param.mcp_name())
            .collect();
        // 风险档进常驻索引：**"这个动词花钱"必须在模型看见目录时就看见**，
        // 不能等到它读了详情才知道。标记从表里的 `spends_credits` / `destructive` 来
        //（**不再从 `summary` 里"看有没有 ⚠️"推断**：那是散文通道，且与"破坏性"共用符号）。
        let required = if required.is_empty() {
            String::new()
        } else {
            format!(" · 必填: {}", required.join(", "))
        };
        lines.push(format!(
            "{}{} — {}{}",
            mcp_markers(verb),
            verb.id,
            verb.summary,
            required
        ));
    }
    lines.join("\n")
}

/// MCP 的工具清单：**只有两个**。
///
/// 这是刻意的形状，不是省事：常驻面越小，"没在干这活的时候"对判断与上下文的干扰就越小。
/// 加动词**不改这里**（改动词表即可，索引会自动多一行）。
pub fn mcp_tools() -> Vec<Value> {
    let catalog = mcp_catalog();
    vec![
        json!({
            "name": MCP_CATALOG_TOOL,
            "description": format!(
                "{MCP_IDENTITY}\n\n\
                 This tool is the index: every line below is one verb — `id — what it does · \
                 required params`, with `[COST]` / `[IRREVERSIBLE]` marking the ones that charge the \
                 user's own credits or cannot be undone. \
                 Call it with a `verb` id to get that verb's full input schema and semantics before \
                 calling it (worth doing the first time you use a verb, or whenever a call is \
                 rejected for a bad parameter).\n\n\
                 Available verbs:\n{catalog}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "description": "A verb id from the index (e.g. \"operation.run\"). Omit to get the index as structured data."
                    }
                },
                "required": []
            },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }),
        json!({
            "name": MCP_CALL_TOOL,
            "description": "Run one RenderLoft verb. RenderLoft is the user's local AIGC workbench \
                            (image/video generation apps on their own cloud accounts) — call \
                            renderloft_catalog first to see what it can do and what each verb needs.\n\n\
                            Pass `verb` (an id from that index) and `args` (an object keyed by the \
                            verb's parameter names).\n\n\
                            Most providers CHARGE THE USER'S OWN CREDITS per run ([COST] in the index \
                            marks which verbs do, and each entry in `runs` is a separate charged \
                            run), and real work \
                            takes tens of seconds to minutes — the call returns as soon as the work \
                            is queued, not when it finishes. For several inputs, put them in ONE \
                            call's `runs` array (up to 100) instead of making many calls: it is \
                            cheaper to track and the daemon caps concurrent operations from \
                            autonomous callers. For a long job, pass `callbackUrl`: the daemon POSTs \
                            a terminal snapshot to that URL when the operation finishes, so you can \
                            submit and end your turn instead of waiting inside this call — a tool \
                            call is request/response, and a long wait occupies the client's request \
                            channel (its own timeout may be shorter than yours). If you do wait, keep \
                            it SHORT and call again as needed. If a run fails, read its `error` \
                            together with the `inputs` that were submitted, fix the offending \
                            parameter, and only then resubmit — blindly retrying costs money again.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "description": "Verb id from the catalog index, e.g. \"operation.run\"."
                    },
                    "args": {
                        "type": "object",
                        "description": "Arguments keyed by the verb's parameter names (see renderloft_catalog for the exact shape)."
                    }
                },
                "required": ["verb"]
            },
            // 标注只能给一个：`call` 能碰的东西取决于 verb，所以按最保守的报
            //（不声称只读、不声称幂等）——宁可让客户端多问一句，也别让它以为这是只读的。
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": true
            }
        }),
    ]
}

/// 某个动词的**详情**（`catalog(<verb>)` 的返回）：完整 schema + 语义 + 标注 + CLI 等价写法。
///
/// 这里放的是"schema 说不出来的东西"：语义边界（`byteSize 0` 不是空文件）、契约（回调必须
/// https 或回环 http）、失败语义（队列满可重试、参数错不可）——**这些只能写在文字里**，
/// 而且现在**只在你要用这个动词时才付**。
pub fn mcp_verb_detail(verb: &Verb) -> Value {
    json!({
        "id": verb.id,
        "group": verb.group,
        "action": verb.action,
        "summary": verb.summary,
        // 标记挂在**首行**（固定位置、可正则）——详情与将来的原生工具描述共用这一条路，
        // 所以标记不会只在索引里有、进了详情反而丢（有测试钉住两侧都在）。
        "description": format!(
            "{}{}",
            mcp_markers(verb),
            verb.mcp_description.unwrap_or(verb.summary)
        ),
        "inputSchema": mcp_input_schema(verb),
        "annotations": mcp_annotations(verb),
        "cli": format!("renderloft-cli {} {}", verb.group, verb.action),
    })
}

/// 从表渲染出整棵 clap 命令树。
pub fn cli_command() -> Command {
    let mut root = Command::new("renderloft")
        .version(env!("CARGO_PKG_VERSION"))
        .about("RenderLoft daemon client")
        .subcommand_required(true)
        .arg_required_else_help(true);
    for group in GROUPS {
        let mut command = Command::new(group.name)
            .about(group.about)
            .subcommand_required(true);
        for verb in VERBS.iter().filter(|verb| verb.group == group.name) {
            let mut subcommand = Command::new(verb.action).about(verb.summary);
            for param in verb.cli_params() {
                subcommand = subcommand.arg(cli_arg(param));
            }
            command = command.subcommand(subcommand);
        }
        root = root.subcommand(command);
    }
    root
}

fn cli_arg(param: &'static Param) -> Arg {
    let mut arg = Arg::new(param.name).long(param.cli_name());
    if !param.help.is_empty() {
        arg = arg.help(param.help);
    }
    arg = match param.kind {
        Kind::Id => arg.value_name("ID").value_parser(clap::value_parser!(u64)),
        Kind::Count => arg.value_name("N").value_parser(clap::value_parser!(usize)),
        Kind::Text => arg
            .value_name("TEXT")
            .value_parser(clap::value_parser!(String)),
        Kind::Flag => arg.action(ArgAction::SetTrue),
        Kind::IdList => arg
            .value_name("LIST")
            .value_parser(clap::value_parser!(String)),
        // 可重复的 `--asset '<字段键>=<本机文件路径>'`：同一字段给两次在解析时报错
        // （见 `parse_asset_spec`），而不是让后一个悄悄覆盖前一个。
        Kind::KeyedAssets => arg
            .value_name("字段键=文件")
            .action(ArgAction::Append)
            .value_parser(clap::value_parser!(String)),
        Kind::Json | Kind::JsonArray | Kind::LocalFile => arg
            .value_name("FILE")
            .value_parser(clap::value_parser!(std::path::PathBuf)),
    };
    if param.kind != Kind::Flag {
        if let Some(default) = param.default {
            arg = arg.default_value(default);
        }
    }
    if param.required.has(Channels::CLI) {
        arg = arg.required(true);
    }
    arg
}

// ---------- MCP 渲染 ----------

/// 工具的标注（只读 / 破坏性 / 幂等 / 是否伸到外部世界）。
///
/// 抽出来是为了**动态 per-app 工具**也能用它：那些工具是同一批动词的另一种名字，
/// 各写一份标注必然漂移。
pub fn mcp_annotations(verb: &Verb) -> Value {
    json!({
        "readOnlyHint": verb.read_only,
        "destructiveHint": verb.destructive,
        "idempotentHint": verb.idempotent,
        // 这个 daemon 只在本机、只碰用户自己的平台账号；唯一"伸到外面去"的
        // 就是会改远端状态的破坏性动词，所以由 destructive 派生。
        "openWorldHint": verb.destructive
    })
}

fn mcp_input_schema(verb: &Verb) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for param in verb.mcp_params() {
        properties.insert(param.mcp_name().to_string(), mcp_property(param));
        if param.required.has(Channels::MCP) {
            required.push(json!(param.mcp_name()));
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn mcp_property(param: &Param) -> Value {
    let mut property = match param.kind {
        Kind::Id => json!({ "type": "integer" }),
        Kind::Count => {
            let mut schema = Map::new();
            schema.insert("type".to_string(), json!("integer"));
            if let Some(min) = param.min {
                schema.insert("minimum".to_string(), json!(min));
            }
            if let Some(max) = param.max {
                schema.insert("maximum".to_string(), json!(max));
            }
            Value::Object(schema)
        }
        Kind::Text | Kind::LocalFile => json!({ "type": "string" }),
        Kind::Flag => json!({ "type": "boolean" }),
        Kind::IdList => json!({ "type": "array", "items": { "type": "integer" } }),
        Kind::Json => json!({ "type": "object" }),
        Kind::KeyedAssets => json!({
            "type": "object",
            // 键是**数据空间的字段键**（recipe 的 inputSchema 给），所以只能
            // additionalProperties，不能静态列举——这才是"资产按字段键定位"在协议地板上的样子。
            "additionalProperties": {
                "type": "object",
                "required": ["assetId", "checksum"],
                "properties": {
                    "assetId": { "type": "integer" },
                    "checksum": { "type": "string" }
                }
            }
        }),
        Kind::JsonArray => {
            let mut schema = Map::new();
            schema.insert("type".to_string(), json!("array"));
            schema.insert("items".to_string(), json!({ "type": "object" }));
            // 批量上限必须**能校验**，不能只写在散文里：一次调用最多花多少次钱就在这里。
            if let Some(min) = param.min {
                schema.insert("minItems".to_string(), json!(min));
            }
            if let Some(max) = param.max {
                schema.insert("maxItems".to_string(), json!(max));
            }
            Value::Object(schema)
        }
    };
    if let Some(description) = param.mcp {
        if let Value::Object(map) = &mut property {
            map.insert("description".to_string(), json!(description));
        }
    }
    property
}

// ---------- 参数解析 ----------

/// 一个动词的**已解析参数**。
///
/// 两个通道都收敛到它：CLI 从 `ArgMatches` 来、MCP 从 JSON 来，
/// 之后**分发只认这张表里的名字**（`param()` 会拒绝没声明的名字）。
pub struct Args {
    verb: &'static Verb,
    values: Map<String, Value>,
    channel: Channels,
}

impl Args {
    /// 从 clap 解析结果构造（CLI 通道）。
    pub fn from_cli(verb: &'static Verb, matches: &ArgMatches) -> Result<Self, ClientError> {
        let mut values = Map::new();
        for param in verb.cli_params() {
            let value = match param.kind {
                Kind::Id => matches.get_one::<u64>(param.name).map(|value| json!(value)),
                Kind::Count => matches
                    .get_one::<usize>(param.name)
                    .map(|value| json!(value)),
                Kind::Text => matches
                    .get_one::<String>(param.name)
                    .map(|value| json!(value)),
                Kind::Flag => Some(json!(matches.get_flag(param.name))),
                Kind::IdList => matches
                    .get_one::<String>(param.name)
                    .map(|raw| json!(parse_id_list(raw))),
                // 输入形状：CLI 给的是**文件路径**，内容在这里读进来（读不到就是调用方的错，
                // 不是 daemon 的错——所以不进网络请求）。
                Kind::Json | Kind::JsonArray => {
                    match matches.get_one::<std::path::PathBuf>(param.name) {
                        Some(path) => Some(read_json_file(path)?),
                        None => None,
                    }
                }
                Kind::LocalFile => matches
                    .get_one::<std::path::PathBuf>(param.name)
                    .map(|path| json!(path.to_string_lossy())),
                // 可重复的 `--asset '字段键=文件'`：解析成 [{fieldKey, path}]，
                // **重名即报错**（同一字段两个文件是歧义，不许悄悄取后一个）。
                Kind::KeyedAssets => {
                    let mut specs: Vec<Value> = Vec::new();
                    let mut seen = std::collections::BTreeSet::new();
                    if let Some(values) = matches.get_many::<String>(param.name) {
                        for raw in values {
                            let (field_key, path) = parse_asset_spec(raw)?;
                            if !seen.insert(field_key.clone()) {
                                return Err(argument_error(
                                    "ASSET_FIELD_DUPLICATED",
                                    format!(
                                        "同一个字段键 `{field_key}` 给了多个文件。一个媒体字段一次只能绑一个资产；\
                                         去掉多余的 --asset（如果确实要换文件，只留想用的那一个）。"
                                    ),
                                ));
                            }
                            specs.push(json!({ "fieldKey": field_key, "path": path }));
                        }
                    }
                    // 一个 `--asset` 都没给就**不放这个键**：`run_request` 那边"没给"与"给了空对象"
                    // 是两件事（空对象等于显式声明"零个资产"，那不是同一个意思）。
                    if specs.is_empty() {
                        None
                    } else {
                        Some(json!(specs))
                    }
                }
            };
            if let Some(value) = value {
                values.insert(param.name.to_string(), value);
            }
        }
        Self::finish(verb, values, Channels::CLI)
    }

    /// 从工具参数构造（MCP 通道）。
    pub fn from_mcp(verb: &'static Verb, arguments: &Value) -> Result<Self, ClientError> {
        let object = match arguments {
            Value::Object(map) => map.clone(),
            Value::Null => Map::new(),
            other => {
                return Err(argument_error(
                    "ARGUMENT_TYPE",
                    format!("{} 的参数必须是一个对象，收到：{}", verb.id, brief(other)),
                ))
            }
        };
        // 未知键**明确报错**：静默忽略会让"模型按旧 schema 传参"变成一次无声的空转
        //（它以为参数生效了，实际被丢掉）——那正是最难查的一类故障。
        for key in object.keys() {
            if !verb.mcp_params().any(|param| param.mcp_name() == key) {
                let allowed = verb
                    .mcp_params()
                    .map(|param| param.mcp_name())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(argument_error(
                    "UNKNOWN_PARAM",
                    format!("{} 不认识参数 `{key}`；它接受：{allowed}", verb.id),
                ));
            }
        }
        let mut values = Map::new();
        for param in verb.mcp_params() {
            // 显式 `null` 当作"没给"：MCP 客户端常用 null 表达缺省。
            let raw = object
                .get(param.mcp_name())
                .filter(|value| !value.is_null());
            if let Some(raw) = raw {
                values.insert(param.name.to_string(), coerce_mcp(param, raw)?);
            }
        }
        Self::finish(verb, values, Channels::MCP)
    }

    fn finish(
        verb: &'static Verb,
        mut values: Map<String, Value>,
        channel: Channels,
    ) -> Result<Self, ClientError> {
        for param in verb
            .params
            .iter()
            .filter(|param| param.channels.has(channel))
        {
            if !values.contains_key(param.name) {
                if param.required.has(channel) {
                    return Err(argument_error(
                        "ARGUMENT_REQUIRED",
                        format!("缺少必填参数：{}", param.mcp_name()),
                    ));
                }
                if let Some(default) = param.default {
                    values.insert(param.name.to_string(), parse_default(param, default)?);
                }
            }
        }
        Ok(Self {
            verb,
            values,
            channel,
        })
    }

    /// 分发读某个参数前的**声明检查**：表里没有这个名字就是 bug（拼错、或表与实现漂移），
    /// 立刻报错而不是静默取默认值。
    fn param(&self, name: &str) -> Result<&'static Param, ClientError> {
        self.verb
            .params
            .iter()
            .find(|param| param.name == name)
            .ok_or_else(|| {
                argument_error(
                    "UNKNOWN_PARAM",
                    format!(
                        "{} 的实现读了一个没声明的参数 `{name}`（动词表与分发漂移了）",
                        self.verb.id
                    ),
                )
            })
    }

    pub fn u64(&self, name: &str) -> Result<u64, ClientError> {
        self.param(name)?;
        self.values
            .get(name)
            .and_then(coerce_u64)
            .ok_or_else(|| missing(name))
    }

    /// 取可选的 `Id` 参数（没给 = `None`，不是错误）。游标这类"可选 id"用它。
    pub fn opt_u64(&self, name: &str) -> Result<Option<u64>, ClientError> {
        self.param(name)?;
        Ok(self.values.get(name).and_then(coerce_u64))
    }

    /// 取 `Count` 参数（表里必须声明了缺省值，所以"没给"与"给 0"不会混淆）。
    pub fn count(&self, name: &str) -> Result<usize, ClientError> {
        self.param(name)?;
        self.values
            .get(name)
            .and_then(coerce_u64)
            .map(|value| value as usize)
            .ok_or_else(|| missing(name))
    }

    pub fn text(&self, name: &str) -> Result<String, ClientError> {
        self.param(name)?;
        self.values
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| missing(name))
    }

    pub fn opt_text(&self, name: &str) -> Result<Option<String>, ClientError> {
        self.param(name)?;
        Ok(self
            .values
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    pub fn flag(&self, name: &str) -> Result<bool, ClientError> {
        self.param(name)?;
        Ok(self
            .values
            .get(name)
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub fn json(&self, name: &str) -> Result<Value, ClientError> {
        self.param(name)?;
        self.values.get(name).cloned().ok_or_else(|| missing(name))
    }

    pub fn opt_json(&self, name: &str) -> Result<Option<Value>, ClientError> {
        self.param(name)?;
        Ok(self.values.get(name).cloned())
    }

    pub fn u64_list(&self, name: &str) -> Result<Vec<u64>, ClientError> {
        self.param(name)?;
        Ok(self
            .values
            .get(name)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(coerce_u64).collect())
            .unwrap_or_default())
    }

    /// 调用方没给幂等键时用的前缀（保持历史形状：`cli-…` / `mcp-…`）。
    pub fn idempotency_prefix(&self) -> &'static str {
        if self.channel.has(Channels::MCP) {
            "mcp"
        } else {
            "cli"
        }
    }
}

fn missing(name: &str) -> ClientError {
    argument_error(
        "ARGUMENT_REQUIRED",
        format!("缺少必填参数：{name}（解析阶段应当已经拦下）"),
    )
}

fn parse_default(param: &Param, default: &str) -> Result<Value, ClientError> {
    match param.kind {
        Kind::Count => default
            .parse::<u64>()
            .map(|value| json!(value))
            .map_err(|_| {
                argument_error(
                    "VERB_TABLE_INVALID",
                    format!("{} 的缺省值不是数字：{default}", param.name),
                )
            }),
        Kind::Flag => Ok(json!(default == "true")),
        Kind::IdList => Ok(json!(parse_id_list(default))),
        _ => Ok(json!(default)),
    }
}

fn parse_id_list(raw: &str) -> Vec<u64> {
    raw.split(',')
        .filter_map(|value| value.trim().parse::<u64>().ok())
        .collect()
}

fn read_json_file(path: &Path) -> Result<Value, ClientError> {
    let content = std::fs::read_to_string(path).map_err(|error| ClientError {
        status: 0,
        code: "INPUT_INVALID".to_string(),
        message: format!("无法读取输入文件：{}（{error}）", path.display()),
        retryable: false,
        details: Value::Null,
    })?;
    serde_json::from_str(&content).map_err(|error| ClientError {
        status: 0,
        code: "INPUT_INVALID".to_string(),
        message: format!("输入文件不是合法 JSON：{}（{error}）", path.display()),
        retryable: false,
        details: Value::Null,
    })
}

/// 解析一条 `--asset '<字段键>=<本机文件路径>'`。
///
/// 三条边界都在这里报清楚（而不是等到上传时才冒出个笼统错误）：
/// 没有 `=`、字段键为空、文件路径为空。**文件存在与否在分发时才查**——那一步的报错
/// 带路径（`ASSET_READ_FAILED`），比解析期的"形状不对"更说明问题。
fn parse_asset_spec(raw: &str) -> Result<(String, String), ClientError> {
    let Some((field_key, path)) = raw.split_once('=') else {
        return Err(argument_error(
            "ASSET_SPEC_INVALID",
            format!("`--asset {raw}` 缺少 `=`。形状是 '<字段键>=<本机文件路径>'，例如 --asset '475::image=person.png'。"),
        ));
    };
    let field_key = field_key.trim();
    let path = path.trim();
    if field_key.is_empty() {
        return Err(argument_error(
            "ASSET_SPEC_INVALID",
            format!("`--asset {raw}` 的字段键是空的。字段键就是 recipe 的 inputSchema 里的键（如 475::image），用 `recipe describe` 能看到。"),
        ));
    }
    if path.is_empty() {
        return Err(argument_error(
            "ASSET_SPEC_INVALID",
            format!("`--asset {raw}` 的文件路径是空的。"),
        ));
    }
    Ok((field_key.to_string(), path.to_string()))
}

fn coerce_mcp(param: &Param, value: &Value) -> Result<Value, ClientError> {
    let key = param.mcp_name();
    match param.kind {
        Kind::Id | Kind::Count => coerce_u64(value)
            .map(|number| json!(number))
            .ok_or_else(|| type_error(key, "整数", value)),
        Kind::Text | Kind::LocalFile => match value {
            Value::String(text) => Ok(json!(text)),
            _ => Err(type_error(key, "字符串", value)),
        },
        Kind::Flag => match value {
            Value::Bool(flag) => Ok(json!(flag)),
            _ => Err(type_error(key, "布尔值", value)),
        },
        Kind::IdList => {
            let ids = match value {
                Value::Array(items) => items.iter().map(coerce_u64).collect::<Option<Vec<u64>>>(),
                // 有的客户端会把列表压成 "1,2" —— 与 CLI 同一个解析器，收下。
                Value::String(text) => Some(parse_id_list(text)),
                _ => None,
            };
            ids.map(|ids| json!(ids))
                .ok_or_else(|| type_error(key, "整数数组", value))
        }
        Kind::Json => match value {
            Value::Object(_) => Ok(value.clone()),
            _ => Err(type_error(key, "JSON 对象", value)),
        },
        Kind::JsonArray => match value {
            Value::Array(_) => Ok(value.clone()),
            _ => Err(type_error(key, "JSON 数组", value)),
        },
        // keyed 资产对象：键 = 字段键，值 = {assetId, checksum}。
        //
        // ⚠️ 位置数组在这里被**明确拒绝**（不是"顺带不支持"）：数组编码里资产的语义是
        // 下标，而字段键才是语义。让模型按位置猜正是错位事故的根；错误信息给出替代做法。
        Kind::KeyedAssets => {
            let Value::Object(entries) = value else {
                return Err(argument_error(
                    "ASSET_SHAPE_INVALID",
                    format!(
                        "参数 {key} 必须是**对象**：{{\"<字段键>\": {{\"assetId\": 数字, \"checksum\": \"...\"}}}}，\
                         例如 {{\"475::image\": {{\"assetId\": 12}}}}。收到：{}。\
                         （位置数组只是 HTTP 兼容层；字段键在 recipe 的 inputSchema 里，用 recipe.describe 能拿到。）",
                        brief(value)
                    ),
                ));
            };
            let mut normalized = serde_json::Map::new();
            for (field_key, entry) in entries {
                if field_key.trim().is_empty() {
                    return Err(argument_error(
                        "ASSET_FIELD_UNKNOWN",
                        format!("参数 {key} 里有空字段键。键必须是 recipe inputSchema 里的字段键（如 \"475::image\"）。"),
                    ));
                }
                let Some(entry) = entry.as_object() else {
                    return Err(argument_error(
                        "ASSET_SHAPE_INVALID",
                        format!(
                            "参数 {key} 的字段 `{field_key}` 的值必须是对象 {{assetId, checksum}}，收到：{}。",
                            brief(entry)
                        ),
                    ));
                };
                for entry_key in entry.keys() {
                    if entry_key != "assetId" && entry_key != "checksum" {
                        return Err(argument_error(
                            "ASSET_SHAPE_INVALID",
                            format!(
                                "参数 {key} 的字段 `{field_key}` 里有不认识的键 `{entry_key}`。只收 {{assetId, checksum}}——\
                                 多余字段会让人以为它们起了作用，静默忽略比报错危险。"
                            ),
                        ));
                    }
                }
                let asset_id = entry.get("assetId").and_then(coerce_u64).ok_or_else(|| {
                    argument_error(
                        "ASSET_SHAPE_INVALID",
                        format!(
                            "参数 {key} 的字段 `{field_key}` 缺 assetId（先调 asset.upload 拿）。"
                        ),
                    )
                })?;
                let checksum = entry
                    .get("checksum")
                    .and_then(Value::as_str)
                    .filter(|checksum| !checksum.trim().is_empty())
                    .ok_or_else(|| {
                        argument_error(
                            "ASSET_SHAPE_INVALID",
                            format!("参数 {key} 的字段 `{field_key}` 缺 checksum（asset.upload 的返回里有）。"),
                        )
                    })?;
                normalized.insert(
                    field_key.clone(),
                    json!({ "assetId": asset_id, "checksum": checksum }),
                );
            }
            Ok(Value::Object(normalized))
        }
    }
}

fn coerce_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|float| float.fract() == 0.0 && *float >= 0.0)
                .map(|float| float as u64)
        }),
        // 数字写成字符串也收下（模型偶尔这么干）；下面 `type_error` 仍然兜住真正的类型错。
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn type_error(key: &str, expected: &str, value: &Value) -> ClientError {
    argument_error(
        "ARGUMENT_TYPE",
        format!("参数 {key} 必须是{expected}，收到：{}", brief(value)),
    )
}

fn brief(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 120 {
        format!("{}…", text.chars().take(120).collect::<String>())
    } else {
        text
    }
}

fn argument_error(code: &str, message: String) -> ClientError {
    ClientError {
        status: 0,
        code: code.to_string(),
        message,
        retryable: false,
        details: Value::Null,
    }
}

// ---------- 分发 ----------

/// 动词 → client 调用。**只写一次**，CLI 与 MCP 共用。
pub async fn execute(
    client: &V2Client,
    verb: &'static Verb,
    args: &Args,
) -> Result<Value, ClientError> {
    match verb.id {
        "connection.list" => client.list_connections().await,
        "connection.models" => {
            client
                .list_connection_models(args.u64("connectionId")?)
                .await
        }
        "connection.set-key" => set_connection_api_key(client, args).await,
        "callback.set-secret" => set_callback_secret(client, args).await,
        "callback.status" => client.callback_secret_status().await,
        "callback.clear-secret" => client.clear_callback_secret().await,
        "daemon.status" => daemon_status(client).await,
        "daemon.stop" => daemon_stop(client).await,
        "artifact.download" => download_artifact(client, args).await,
        "recipe.list" => {
            // `--removed` 走的是**另一个集合**（墓碑列表），不是"活着的里面筛一部分"。
            if args.flag("removed")? {
                client.list_removed_recipes(args.count("limit")?).await
            } else {
                client.list_recipes(args.count("limit")?).await
            }
        }
        "recipe.describe" => client.get_recipe(args.u64("recipeId")?).await,
        "recipe.remove" => client.remove_recipe(args.u64("recipeId")?).await,
        "recipe.restore" => client.restore_recipe(args.u64("recipeId")?).await,
        "recipe.import" => {
            client
                .import_recipe(args.u64("connectionId")?, &args.text("source")?)
                .await
        }
        "recipe.import-workflow" => {
            client
                .import_recipe_workflow(
                    args.u64("connectionId")?,
                    &args.text("source")?,
                    args.opt_text("name")?.as_deref(),
                )
                .await
        }
        "operation.list" => {
            client
                .list_operations(args.count("limit")?, args.opt_u64("after")?)
                .await
        }
        "operation.run" => run_operation(client, args).await,
        "operation.get" => client.get_operation(args.u64("operationId")?).await,
        "operation.wait" => {
            client
                .wait_operation(
                    args.u64("operationId")?,
                    args.count("intervalSeconds")? as u64,
                    args.count("timeoutSeconds")? as u64,
                )
                .await
        }
        "operation.cancel" => client.cancel_operation(args.u64("operationId")?).await,
        "artifact.list" => {
            client
                .list_artifacts(args.opt_u64("limit")?, args.opt_u64("after")?)
                .await
        }
        "artifact.get" => client.get_artifact(args.u64("artifactId")?).await,
        "asset.upload" => upload_asset(client, args).await,
        other => Err(argument_error(
            "UNKNOWN_VERB",
            format!("动词表里没有为 {other} 写分发（表与实现漂移了）"),
        )),
    }
}

/// 提交任务：`inputs`（单次）与 `runs`（批量）**至少给一个**。
///
/// 批量的价值不只是方便：一个 Operation 带 N 个 Run 是界面「批量运行」的同形，
/// 也是 daemon 对自主调用方推荐的形状——并发闸按 **Operation** 计数，
/// 铺开 N 个 Operation 更容易撞上限，而且跟踪起来更碎。
///
/// **资产（媒体）走 `assets`**：
/// - CLI：可重复的 `--asset '字段键=文件'`（解析成 `[{fieldKey, path}]`）——这里先
///   **上传**再组装 keyed 对象。缺 `--input`/读不到文件/重名都在解析或这里报清楚。
/// - MCP：直接给**已上传**的 keyed 对象（`asset.upload` 拿 assetId + checksum）。
/// - 批量 `runs[]` 里每一行也可以带自己的 `assets`（keyed 对象；只收对象，
///   位置数组只是 HTTP 兼容层，客户端层不给这条路）。
///
/// `dryRun=true`：形状与资产全部照常准备（含 CLI 先上传——上传不计费），但**不提交**，
/// 而是发到只读预检端点拿回同样判据的结论。预检不花钱，但仍要求 execute 权限。
async fn run_operation(client: &V2Client, args: &Args) -> Result<Value, ClientError> {
    let recipe_revision = args.u64("recipeRevisionId")?;
    let key = args.opt_text("idempotencyKey")?.unwrap_or_else(|| {
        format!(
            "{}-{}",
            args.idempotency_prefix(),
            uuid::Uuid::new_v4().simple()
        )
    });
    let callback = callback_body(args)?;
    let dry_run = args.flag("dryRun")?;
    let assets_arg = args.opt_json("assets")?;
    let runs_arg = args.opt_json("runs")?;
    // ⚠️ 顶层 `assets` 与 `runs` 同时给 = **歧义**，不是"每行都用这份"。
    //
    // 两种写法各自都合法，合在一起却有两种读法（每行的默认？还是只给单次用的？）。这里过去更糟：
    // 批量路径根本不看顶层 `assets`，于是每一行都没图却照常提交、**照常计费**，且零提示。
    // 工具的立场是：要么照做、要么说清楚，不许静默——尤其不许在花钱的事上静默。
    //
    // 为什么不选"当作每行的默认"：那是**不可逆**的——一旦有人依赖它，收紧就是破坏性变更；
    // 而报错 → 以后放宽是兼容的。不可逆的设计决策往保守那一侧站。
    //
    // 这道检查在 `prepare_assets` 之前：歧义请求**一个字节都不上传**。
    if assets_arg.is_some() && runs_arg.is_some() {
        return Err(argument_error(
            "ASSETS_NOT_ALLOWED_WITH_RUNS",
            "同时给了 `assets` 和 `runs`，这是两种写法，不能合用（顶层 `assets` 是给单次提交的）。\
             两种正确写法：① 单次：给 `inputs` + `assets`；\
             ② 批量：给 `runs`，资产写在该行自己的 `runs[i].assets` 里（keyed 对象，键 = 字段键）。"
                .to_string(),
        ));
    }
    let single_assets = match assets_arg {
        Some(value) => Some(prepare_assets(client, &value).await?),
        None => None,
    };
    if let Some(runs) = runs_arg {
        let runs = runs.as_array().cloned().unwrap_or_default();
        if runs.is_empty() {
            return Err(argument_error(
                "RUNS_EMPTY",
                "runs 不能是空数组（要么给 inputs 单次，要么给至少一个 run）".to_string(),
            ));
        }
        let mut requests = Vec::with_capacity(runs.len());
        for (index, run) in runs.iter().enumerate() {
            let inputs = run.get("inputs").cloned().ok_or_else(|| {
                argument_error("INPUT_REQUIRED", format!("runs[{index}] 缺少 inputs"))
            })?;
            let mut request = run_request(recipe_revision, inputs, None);
            // 调用方给了 clientRunId 就用它（幂等与追踪都靠它），否则按序号生成。
            if let Some(client_run_id) = run.get("clientRunId").and_then(Value::as_str) {
                if let Some(entry) = request.get_mut(0) {
                    entry["clientRunId"] = json!(client_run_id);
                }
            }
            // 批量行自己的资产：**真读真传**（过去 runs[] 里的 assets 被静默丢弃）。
            if let Some(assets) = run.get("assets") {
                let Value::Object(_) = assets else {
                    return Err(argument_error(
                        "ASSET_SHAPE_INVALID",
                        format!(
                            "runs[{index}].assets 必须是 keyed 对象 {{\"<字段键>\": {{\"assetId\", \"checksum\"}}}}，\
                             收到：{}。位置数组只是 HTTP 兼容层，不许按位置猜字段；字段键见 recipe 的 inputSchema。",
                            brief(assets)
                        ),
                    ));
                };
                if let Some(entry) = request.get_mut(0) {
                    entry["assets"] = assets.clone();
                }
            }
            requests.extend(request.as_array().cloned().unwrap_or_default());
        }
        let runs_value = Value::Array(requests);
        if dry_run {
            return client.validate_operation(runs_value).await;
        }
        return client.create_operation(&key, runs_value, callback).await;
    }
    let inputs = args.json("inputs").map_err(|_| {
        argument_error(
            "INPUT_REQUIRED",
            "缺少 inputs（单次）或 runs（批量），至少给一个".to_string(),
        )
    })?;
    let request = run_request(recipe_revision, inputs, single_assets);
    if dry_run {
        return client.validate_operation(request).await;
    }
    client.create_operation(&key, request, callback).await
}

/// 资产的两种输入形状，在**提交前**统一成 keyed 对象：
///
/// - **CLI**（`[{fieldKey, path}]`）：这里**先上传**。上传不计费；文件读不到给
///   `ASSET_READ_FAILED`（带路径与字段键）——比在上游平台上"图是空的"便宜太多。
/// - **MCP**（`{字段键: {assetId, checksum}}`）：已上传，直接透传（形状在 `coerce_mcp` 收口）。
/// - 其他形状（含位置数组）：拒绝。位置数组只是 HTTP 兼容层，客户端层不提供这条路。
async fn prepare_assets(client: &V2Client, value: &Value) -> Result<Value, ClientError> {
    match value {
        Value::Object(_) => Ok(value.clone()),
        Value::Array(specs) => {
            let mut keyed = serde_json::Map::new();
            for spec in specs {
                let field_key = spec
                    .get("fieldKey")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let path = spec
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if field_key.is_empty() || path.is_empty() {
                    return Err(argument_error(
                        "ASSET_SPEC_INVALID",
                        format!("资产项形状不对（要 fieldKey 与 path）：{spec}"),
                    ));
                }
                let bytes = std::fs::read(&path).map_err(|error| ClientError {
                    status: 0,
                    code: "ASSET_READ_FAILED".to_string(),
                    message: format!("无法读取资产文件 {path}（字段键 {field_key}）：{error}"),
                    retryable: false,
                    details: Value::Null,
                })?;
                let filename = Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("upload.bin")
                    .replace('\n', "");
                let mime_type = guess_mime_type(&filename).to_string();
                let uploaded = client.upload_asset(&filename, &mime_type, bytes).await?;
                let asset_id =
                    uploaded
                        .get("assetId")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| {
                            argument_error(
                                "ASSET_UPLOAD_INVALID",
                                format!("上传响应里没有 assetId：{uploaded}"),
                            )
                        })?;
                let checksum = uploaded
                    .get("checksum")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        argument_error(
                            "ASSET_UPLOAD_INVALID",
                            format!("上传响应里没有 checksum：{uploaded}"),
                        )
                    })?;
                keyed.insert(
                    field_key,
                    json!({ "assetId": asset_id, "checksum": checksum }),
                );
            }
            Ok(Value::Object(keyed))
        }
        _ => Err(argument_error(
            "ASSET_SHAPE_INVALID",
            format!(
                "assets 必须是 keyed 对象或 --asset 规格数组，收到：{}",
                brief(value)
            ),
        )),
    }
}

/// 回调地址 → 请求体里的 `callback` 对象；没给就 `None`（**不要**凭空塞字段：
/// 服务端对"没给"与"给了空值"的处理不同）。
///
/// 为什么把 `events` 写死成 `operation.terminal`：daemon 目前只在这个事件上投递。
/// 显式写出来比留空（语义是"全部事件"）更可预测——将来加了新事件，
/// 这条订阅的行为不会**悄悄**变。
fn callback_body(args: &Args) -> Result<Option<Value>, ClientError> {
    let Some(url) = args.opt_text("callbackUrl")? else {
        return Ok(None);
    };
    let url = url.trim();
    if url.is_empty() {
        return Err(argument_error(
            "CALLBACK_URL_EMPTY",
            "callbackUrl 不能是空字符串（要么别给，要么给一个地址）".to_string(),
        ));
    }
    Ok(Some(json!({
        "url": url,
        "events": ["operation.terminal"],
    })))
}

async fn upload_asset(client: &V2Client, args: &Args) -> Result<Value, ClientError> {
    let path = args.text("path")?;
    let bytes = std::fs::read(&path).map_err(|error| ClientError {
        status: 0,
        code: "ASSET_READ_FAILED".to_string(),
        message: format!("无法读取文件 {path}：{error}"),
        retryable: false,
        details: Value::Null,
    })?;
    let filename = args
        .opt_text("filename")?
        .unwrap_or_else(|| {
            Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("upload.bin")
                .to_string()
        })
        // 文件名里的换行会污染 multipart 头。
        .replace('\n', "");
    let mime_type = args
        .opt_text("mimeType")?
        .unwrap_or_else(|| guess_mime_type(&filename).to_string());
    client.upload_asset(&filename, &mime_type, bytes).await
}

/// 从文件读一个 API Key 写进连接。
///
/// 值**只从文件来**：命令行参数会同时留在 shell history、进程列表与 CI 日志里。
/// 读到的内容会 `trim()`（`echo "sk-..." > k` 会带一个换行，那是文件格式，不是密钥的一部分），
/// 多行内容直接拒绝——否则密钥里混进换行，认证失败会莫名其妙。
async fn set_connection_api_key(client: &V2Client, args: &Args) -> Result<Value, ClientError> {
    let connection_id = args.u64("connectionId")?;
    let value = read_secret_file(&args.text("fromFile")?)?;
    // 默认拒绝覆盖。**这条闸门是被一次真实事故换来的**：本机实测时把一条正在用的
    // connection 1 的 key 覆盖成了别的值，而旧值在任何地方都没有副本（凭据库没有版本），
    // 只能靠一个更早 service 名下的遗留条目侥幸救回。覆盖一个能用的密钥必须是有意的动作。
    if !args.flag("force")? && connection_has_api_key(client, connection_id).await? {
        return Err(ClientError {
            status: 0,
            code: "API_KEY_ALREADY_SET".to_string(),
            message: format!(
                "连接 {connection_id} 已经配了 API Key；确认要覆盖就加 --force\
                 （覆盖会让原本能跑的连接失败，而且旧值不可恢复）"
            ),
            retryable: false,
            details: Value::Null,
        });
    }
    client.set_connection_api_key(connection_id, &value).await
}

/// 从文件读一个密钥（连接 API Key 与回调签名密钥共用）。
///
/// 值**只从文件来**：命令行参数会同时留在 shell history、进程列表与 CI 日志里。
/// 读到的内容会 `trim()`（`echo "sk-..." > k` 会带一个换行，那是文件格式，不是密钥的一部分），
/// 多行内容直接拒绝——否则密钥里混进换行，认证失败会莫名其妙。
///
/// ⚠️ 抽成一个函数是因为两处**必须**用同一套判据：这些检查每一条都是踩出来的，
/// 复制一份迟早会漂移（而漂移的后果是"某个入口悄悄把坏值写进了凭据库"）。
fn read_secret_file(path: &str) -> Result<String, ClientError> {
    let raw = std::fs::read_to_string(path).map_err(|error| ClientError {
        status: 0,
        code: "SECRET_FILE_READ_FAILED".to_string(),
        message: format!("无法读取密钥文件 {path}：{error}"),
        retryable: false,
        details: Value::Null,
    })?;
    // 先剥 BOM 再 trim。**这不是洁癖**：Windows 上 `Set-Content` / 记事本写出的"空文件"
    // 其实带 3 字节 BOM，而 Rust 的 `trim()` **不把 U+FEFF 当空白**——于是"空文件"守卫
    // 会被绕过，一个 BOM 被当成密钥写进去。实测发生过：它覆盖掉了一个正在用的 key。
    let value = raw.trim_start_matches('\u{feff}').trim();
    if value.is_empty() {
        return Err(ClientError {
            status: 0,
            code: "SECRET_FILE_EMPTY".to_string(),
            message: format!("密钥文件 {path} 去掉空白后是空的"),
            retryable: false,
            details: Value::Null,
        });
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(ClientError {
            status: 0,
            code: "SECRET_FILE_MULTILINE".to_string(),
            message: format!("密钥文件 {path} 有多行内容；密钥应当只有一行"),
            retryable: false,
            details: Value::Null,
        });
    }
    Ok(value.to_string())
}

/// 下载产物字节到本地文件。
///
/// ⚠️ 这条补的是一个真缺口：`artifact list` 里 `byteSize` 为 0 的产物，字节**在平台上**
/// （中转站定位，不落本地），而 CLI 此前只能看元数据 ⇒ 脚本与 AI 都拿不到文件，只有界面能预览。
///
/// **文件名一律照抄平台给的**（不发明名字——那是主流下载器的做法：浏览器 `Content-Disposition`、
/// `curl -O`、`wget` 都不改名）。但"**往一个目录里放 N 个可能重名的文件**"是**这个动词知道的事实**：
/// RunningHub 按输出节点命名，同一 recipe 的每个 run 都产出 `870.png` ⇒ 收一批就会互相覆盖。
/// 所以目录落点带冲突策略（默认 `rename`，加 ` (1)` 序号，与浏览器/yt-dlp 同形）：
/// 静默留最后一个是最坏的一种（悄悄丢比丢更坏）。
async fn download_artifact(client: &V2Client, args: &Args) -> Result<Value, ClientError> {
    let artifact_id = args.u64("artifactId")?;
    // 先取元数据：文件名（决定默认落点）与它是不是"字节在平台上"都从这里看。
    let artifact = client.get_artifact(artifact_id).await?;
    let filename = artifact["filename"].as_str().unwrap_or("artifact.bin");
    let template = args.opt_text("nameTemplate")?;
    let template = template
        .as_deref()
        .map(str::trim)
        .filter(|it| !it.is_empty());
    // 落点解析分两条路，**互不叠加**：
    // - 给了模板 = 调用方接管命名 ⇒ `--to` 一律当目录（缺省当前目录），模板自己可能带子目录；
    // - 没给模板 = 照旧：`--to` 是文件就照写，是目录（或以分隔符结尾）就用产物原名。
    //   ⚠️ 这两条曾经叠在一起（先 join 原名、再 join 模板），于是模板变成
    //   `870.png/<模板>` 这种怪路径——**互斥的两种语义不要写成两段顺序执行的代码**。
    let mut target = match (args.opt_text("to")?, template) {
        (Some(path), _) if !path.trim().is_empty() => std::path::PathBuf::from(path),
        (_, Some(_)) => std::path::PathBuf::from("."),
        (_, None) => std::path::PathBuf::from(filename),
    };
    if let Some(template) = template {
        // `--to` 是显式给的目录时不用判"以分隔符结尾"；缺省那支已经在当前目录了。
        let explicit_directory = target.is_dir() || target.to_string_lossy().ends_with(['\\', '/']);
        if !explicit_directory {
            target = target.join(filename);
        }
        target = target.join(expand_name_template(template, artifact_id, filename)?);
    } else {
        // "下载到某个文件夹"是脚本里最自然的写法，而那个文件夹**常常还不存在**——
        // 只靠 `is_dir()` 判断会把 `--to C:\out\` 当成文件名，写出一个没有扩展名的怪文件
        //（实测踩到）。所以：**以路径分隔符结尾**也算目录，不存在就建。
        let wants_directory = target.is_dir() || target.to_string_lossy().ends_with(['\\', '/']);
        if wants_directory {
            target = target.join(filename);
        }
    }
    let on_conflict = args.opt_text("onConflict")?.unwrap_or_default();
    let on_conflict = match on_conflict.trim() {
        // 缺省与显式 `rename` 同路（表里 default 只是给 clap 渲染用的）。
        "" | "rename" => "rename",
        "skip" => "skip",
        "overwrite" => "overwrite",
        other => {
            return Err(ClientError {
                status: 0,
                code: "INVALID_ON_CONFLICT".to_string(),
                message: format!(
                    "on-conflict 只认 rename / skip / overwrite，收到 `{other}`\
                     （rename = 加 (1)(2) 序号，默认）"
                ),
                retryable: false,
                details: Value::Null,
            })
        }
    };
    let bytes = client.artifact_bytes(artifact_id).await?;
    // 落点冲突只在**那儿已经有一个文件**时才有意义：`--to` 指向还不存在的路径 = 无事发生。
    // 用 `is_file()` 而不是 `exists()`：目标是个目录（或其它东西）时不是"撞名"，让 `fs::write`
    // 去报它自己的错，别在这里猜。
    let collision = if target.is_file() {
        match on_conflict {
            // skip 是"这个我收过了"的语义，跳过就是成功（幂等），不是错误。
            "skip" => {
                return Ok(json!({
                    "artifact": artifact,
                    "path": target.display().to_string(),
                    "bytes": std::fs::metadata(&target).map(|meta| meta.len()).unwrap_or(0),
                    "skipped": true,
                    "reason": "目标已存在（on-conflict=skip）",
                }))
            }
            // 覆盖是**明确要求**的，照写；但把"覆盖了一个内容不同的文件"说出来——这类覆盖
            // 事后无从察觉（库里查不到，目录里只剩最后一个），而它常常就是丢图的原因。
            "overwrite" => {
                let differs = std::fs::read(&target)
                    .map(|existing| existing != bytes)
                    .unwrap_or(true);
                differs.then_some(format!(
                    "目标已存在且内容不同，按 on-conflict=overwrite 覆盖：{}",
                    target.display()
                ))
            }
            // rename：与浏览器 / yt-dlp 同形 —— `cat.png` → `cat (1).png`。
            _ => {
                let renamed = unique_sibling(&target);
                let note = format!("{} 已存在，改存为 {}", target.display(), renamed.display());
                target = renamed;
                Some(note)
            }
        }
    } else {
        None
    };
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| ClientError {
                status: 0,
                code: "ARTIFACT_WRITE_FAILED".to_string(),
                message: format!("无法创建目录 {}：{error}", parent.display()),
                retryable: false,
                details: Value::Null,
            })?;
        }
    }
    std::fs::write(&target, &bytes).map_err(|error| ClientError {
        status: 0,
        code: "ARTIFACT_WRITE_FAILED".to_string(),
        message: format!("无法写入 {}：{error}", target.display()),
        retryable: false,
        details: Value::Null,
    })?;
    Ok(json!({
        "artifact": artifact,
        "path": target.display().to_string(),
        "bytes": bytes.len(),
        // 撞名这件事**必须说出来**：它不报错，事后只能靠人翻目录发现。
        "note": collision,
    }))
}

/// 给一个落点消歧：**没被占用就原样返回**（一个字符都不动），被占用了才找同目录下第一个空位/// （`cat.png` → `cat (1).png` → `cat (2).png`）。
///
/// "没冲突就不改名"是这条的**主要行为**，不是提前返回的优化——它是"我们不发明名字"这条原则的
/// 落点：平台给 `870.png`，那就该是 `870.png`；只有真的撞上才轮到序号出场。
///
/// 序号位置刻意与浏览器 / yt-dlp 一致（插在扩展名前，而不是 `cat-1.png`）——同一件事在不同
/// 工具里长得不一样，用户就得重新学一遍。
fn unique_sibling(path: &std::path::Path) -> std::path::PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    // 没有父目录（`--to cat.png` 这种相对当前目录的写法）时显式用 `.`：`Path::new("").join(x)`
    // 得到的是 `x`，而 `exists()` 会落到**进程当前目录**——行为一样但意图不清楚。
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => std::path::Path::new("."),
    };
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_string());
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().into_owned());
    for index in 1..10_000 {
        let name = match &extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    // 一万个同名文件之后还撞——那就别再猜了，交给 `fs::write` 去覆盖（此路几乎不可达）。
    path.to_path_buf()
}

/// 展开 `--name-template`：`{artifactId}` / `{filename}` 两个占位符，**没有别的**。
///
/// 为什么只给两个、且都来自产物元数据：模板是"把已有的事实换个排法"，不是"生成新信息"。
/// `{date}` 这类会让同一条命令在不同时刻产生不同结果——**可复现比方便重要**（这条在
/// `docs/AGENTS.md` 里对提示词模板也是同一个口径）。
///
/// 返回的是**相对路径**：模板可以带子目录（`{artifactId}/{filename}`），但一旦它能跳出
/// `--to` 指的目录，"往哪写"就不再由调用方决定了——所以绝对路径与 `..` 一律当场拒绝。
fn expand_name_template(
    template: &str,
    artifact_id: u64,
    filename: &str,
) -> Result<std::path::PathBuf, ClientError> {
    let expanded = template
        .replace("{artifactId}", &artifact_id.to_string())
        .replace("{filename}", filename);
    // 未知占位符**当场报错**，不静默留着：留着会写出一堆名字里带 `{foo}` 的文件，
    // 而调用方以为自己指定了命名规则。
    if let Some(start) = expanded.find('{') {
        let rest = &expanded[start..];
        let end = rest.find('}').map(|index| start + index + 1);
        let unknown = end.map(|end| &expanded[start..end]).unwrap_or(rest);
        return Err(ClientError {
            status: 0,
            code: "INVALID_NAME_TEMPLATE".to_string(),
            message: format!(
                "name-template 里有不认识的占位符 `{unknown}`；只支持 {{artifactId}} 与 {{filename}}"
            ),
            retryable: false,
            details: Value::Null,
        });
    }
    let path = std::path::PathBuf::from(&expanded);
    // ⚠️ 逃逸判据**不能**交给 `Path` 的平台语义（`has_root()` / `components()`）：
    // 在 Linux 上 `\foo` 与 `C:/foo` 都不是 rooted、也不含 `..`，于是**判为合法**；
    // 而这两个形态在 Windows 上会让 `base.join(模板)` 把 base 整个丢掉。产品只在
    // Windows 上跑，判据就必须按 **Windows 规则**写，而不是"跑这段代码的那台机器的规则"——
    // 否则同一个模板在 CI 上合法、到用户机器上就跳出 `--to` 目录。
    // （这条是被 CI 的 Linux runner 抓出来的：本机 Windows 上全绿。）
    let escapes = {
        let bytes = expanded.as_bytes();
        // ① 盘符前缀（`C:...`，含 `C:evil.png` 这种 drive-relative 形态）。
        let drive_prefixed = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
        // ② 以分隔符开头：`base.join` 会**丢掉 base**。
        let rooted = expanded.starts_with('/') || expanded.starts_with('\\');
        // ③ 任一段是 `..`：两种分隔符都要认。
        let climbs_out = expanded.split(['/', '\\']).any(|part| part == "..");
        drive_prefixed || rooted || climbs_out
    };
    if escapes {
        return Err(ClientError {
            status: 0,
            code: "INVALID_NAME_TEMPLATE".to_string(),
            message: format!(
                "name-template 只能给出 `--to` 目录内的相对路径，不能是绝对路径、也不能含 `..`：`{expanded}`"
            ),
            retryable: false,
            details: Value::Null,
        });
    }
    if expanded.trim().is_empty() {
        return Err(ClientError {
            status: 0,
            code: "INVALID_NAME_TEMPLATE".to_string(),
            message: "name-template 展开后是空的——给不出落点".to_string(),
            retryable: false,
            details: Value::Null,
        });
    }
    Ok(path)
}

/// daemon 在不在跑、版本、日志在哪、还有几条任务没结束。
///
/// 它**不在跑时也要能回答**——这正是这个动词存在的理由（此前 CLI 用户没有任何办法知道
/// "那个进程是谁起的、怎么关掉"，只能去任务管理器）。
async fn daemon_status(client: &V2Client) -> Result<Value, ClientError> {
    let running = daemon_boot::is_running();
    let info = daemon_boot::endpoint_info();
    let workspace = crate::v2::daemon::v2_data_dir().ok();
    let log_file = daemon_boot::daemon_log_path().ok();
    let mut payload = json!({
        "running": running,
        // 只有真在跑时才报端点与版本：端点文件可能是上一次留下的残留（见 `endpoint_info`）。
        "endpoint": if running { info.as_ref().map(|(endpoint, _)| endpoint.clone()) } else { None },
        "version": if running { info.map(|(_, version)| version) } else { None },
        "workspace": workspace.map(|path| path.display().to_string()),
        "logFile": log_file.map(|path| path.display().to_string()),
    });
    if running {
        // "现在停安不安全"取决于这个数：任务在平台侧继续跑，但本地对账要等下次启动。
        payload["activeOperations"] = json!(count_active_operations(client).await?);
    }
    Ok(payload)
}

/// 让 daemon 优雅退出。**没在跑也算成功**（幂等）："关一个已经关着的东西"不是错误。
async fn daemon_stop(client: &V2Client) -> Result<Value, ClientError> {
    if !daemon_boot::is_running() {
        return Ok(json!({ "stopped": false, "reason": "daemon 没在跑" }));
    }
    let active = count_active_operations(client).await.unwrap_or(0);
    client.shutdown_daemon().await?;
    let mut payload = json!({ "stopped": true });
    if active > 0 {
        payload["activeOperations"] = json!(active);
        payload["note"] = json!(format!(
            "还有 {active} 条任务没结束：平台侧会继续跑，daemon 下次启动会把结果对账回来"
        ));
    }
    Ok(payload)
}

/// 最近 50 条任务里，还没进终态的有几条。
///
/// 用已有的列表接口数（不新增端点）：翻一页足够回答"现在停安不安全"。
async fn count_active_operations(client: &V2Client) -> Result<usize, ClientError> {
    let operations = client.list_operations(50, None).await?;
    Ok(operations["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    !matches!(
                        item["status"].as_str().unwrap_or_default(),
                        "SUCCEEDED" | "FAILED" | "CANCELLED" | "PARTIAL"
                    )
                })
                .count()
        })
        .unwrap_or(0))
}

/// 设置回调签名密钥（**只有 CLI 有这条路**，见动词表里的 `mcp_name: None`）。
///
/// 拒覆盖的理由与 API Key 相同，只是后果换了个方向：覆盖之后**接收端会验不过**，
/// 而现象是"回调突然都不带了/验签失败"，排查起来会先怀疑网络。
async fn set_callback_secret(client: &V2Client, args: &Args) -> Result<Value, ClientError> {
    let value = read_secret_file(&args.text("fromFile")?)?;
    if !args.flag("force")? && callback_secret_configured(client).await? {
        return Err(ClientError {
            status: 0,
            code: "CALLBACK_SECRET_ALREADY_SET".to_string(),
            message: "已经配了回调签名密钥；确认要覆盖就加 --force\
                      （覆盖之后接收端会验不过，而旧值不可恢复）"
                .to_string(),
            retryable: false,
            details: Value::Null,
        });
    }
    client.set_callback_secret(&value).await
}

/// 回调签名密钥配没配。只读**状态**，不读值（值一旦读出来就有泄漏面）。
async fn callback_secret_configured(client: &V2Client) -> Result<bool, ClientError> {
    let status = client.callback_secret_status().await?;
    Ok(status["configured"].as_bool().unwrap_or(false))
}

/// 这个连接是否已经配了 API Key。只读**槽位名**，不读值。
///
/// ⚠️ 连接详情的 `secretBindings` 是**有效**槽位（凭据引用借来的也算），所以对一条
/// "引用 RunningHub 密钥"的 ComfyUI 连接，这里会返回 true（它确实能认证）。真要给它
/// 自己填一把 Key 是另一回事：写入口会用 `CONNECTION_SOURCE_TAKES_PRECEDENCE` 拒绝
/// （引用会覆盖自己的绑定，那把 Key 永远用不上），所以 `--force` 也走不通。
///
/// ⚠️ `V2Client` 已经把信封拆了（`unwrap_envelope` 返回的是 `data`），所以这里**没有**
/// `["data"]` 这一层。实测踩过：多写一层会让这个守卫**静默失效**——编译过、clippy 干净、
/// 单测全绿，而它一个都没拦住。
async fn connection_has_api_key(
    client: &V2Client,
    connection_id: u64,
) -> Result<bool, ClientError> {
    let detail = client.get_connection(connection_id).await?;
    Ok(detail["secretBindings"]
        .as_array()
        .map(|bindings| {
            bindings.iter().any(|binding| {
                binding["slot"]
                    .as_str()
                    .is_some_and(|slot| slot.ends_with("-api_key"))
            })
        })
        .unwrap_or(false))
}

/// 按扩展名猜 MIME。猜不出来时给 `application/octet-stream`（不猜一个可能错的 `image/png`）。
fn guess_mime_type(filename: &str) -> &'static str {
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "txt" => "text/plain",
        "json" => "application/json",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_subcommand(group: &str, action: &str) -> Command {
        cli_command()
            .get_subcommands()
            .find(|command| command.get_name() == group)
            .expect("group 应当存在")
            .get_subcommands()
            .find(|command| command.get_name() == action)
            .expect("action 应当存在")
            .clone()
    }

    /// 表自身必须自洽：必填 ⊆ 有、名字唯一、group 存在、MCP 支持与描述成对出现。
    #[test]
    fn the_table_is_internally_consistent() {
        let mut seen_ids = Vec::new();
        for verb in VERBS {
            assert!(!seen_ids.contains(&verb.id), "动词 id 重复：{}", verb.id);
            seen_ids.push(verb.id);
            assert!(
                GROUPS.iter().any(|group| group.name == verb.group),
                "{} 的 group `{}` 不在 GROUPS 里（CLI 渲染会漏掉它）",
                verb.id,
                verb.group
            );
            assert!(
                verb.mcp_name.is_some() == verb.mcp_description.is_some(),
                "{}：MCP 支持与 MCP 描述必须成对（要么都给，要么都别给）",
                verb.id
            );
            if let Some(name) = verb.mcp_name {
                assert!(
                    VERBS
                        .iter()
                        .filter(|other| other.mcp_name == Some(name))
                        .count()
                        == 1,
                    "MCP 工具名重复：{name}"
                );
            }
            let mut seen_params = Vec::new();
            for param in verb.params {
                assert!(
                    !seen_params.contains(&param.name),
                    "{} 的参数名重复：{}",
                    verb.id,
                    param.name
                );
                seen_params.push(param.name);
                assert!(
                    !param.required.has(Channels::CLI) || param.channels.has(Channels::CLI),
                    "{} 的参数 {} 声明了 CLI 必填，却不在 CLI 通道上",
                    verb.id,
                    param.name
                );
                assert!(
                    !param.required.has(Channels::MCP) || param.channels.has(Channels::MCP),
                    "{} 的参数 {} 声明了 MCP 必填，却不在 MCP 通道上",
                    verb.id,
                    param.name
                );
                assert!(
                    param.channels.has(Channels::CLI) || param.channels.has(Channels::MCP),
                    "{} 的参数 {} 两个通道都没有，谁也传不进来",
                    verb.id,
                    param.name
                );
                if let Some(default) = param.default {
                    assert!(
                        parse_default(param, default).is_ok(),
                        "{} 的参数 {} 缺省值无法解析：{default}",
                        verb.id,
                        param.name
                    );
                }
            }
        }
    }

    /// CLI 覆盖：表里的每个动词都要在 clap 树里，且**声明的参数一个不少、多一个不多**。
    #[test]
    fn every_verb_renders_a_cli_subcommand_with_exactly_its_declared_flags() {
        let root = cli_command();
        let groups = root
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            groups,
            GROUPS
                .iter()
                .map(|group| group.name.to_string())
                .collect::<Vec<_>>(),
            "clap 的组与 GROUPS 必须一一对应"
        );

        for verb in VERBS {
            let subcommand = cli_subcommand(verb.group, verb.action);
            let declared = verb
                .cli_params()
                .map(|param| param.cli_name())
                .collect::<Vec<_>>();
            let rendered = subcommand
                .get_arguments()
                .filter_map(|arg| arg.get_long().map(str::to_owned))
                .collect::<Vec<_>>();
            assert_eq!(rendered, declared, "{} 的 CLI flag 与表不一致", verb.id);
            for param in verb.cli_params() {
                let arg = subcommand
                    .get_arguments()
                    .find(|arg| arg.get_id() == param.name)
                    .expect("arg id 应当是规范名");
                assert_eq!(
                    arg.is_required_set(),
                    param.required.has(Channels::CLI),
                    "{} 的参数 {} 必填标注与表不一致",
                    verb.id,
                    param.name
                );
            }
        }
    }

    /// 历史 CLI flag 名不许被"推导规则"悄悄改掉（用户与脚本按它们调用）。
    #[test]
    fn historical_cli_flag_names_are_preserved() {
        let expected: &[(&str, &[&str])] = &[
            ("connection.list", &[]),
            // 后加的：模型发现（读平台声明的模型清单，只读、不花钱）。
            ("connection.models", &["connection"]),
            ("connection.set-key", &["connection", "from-file", "force"]),
            // 回调签名密钥：与 connection.set-key 同形（值只从文件读、默认拒覆盖）。
            // 这两条**只给 CLI**（`mcp_name: None`）——密钥不进模型上下文。
            ("callback.set-secret", &["from-file", "force"]),
            ("callback.status", &[]),
            ("callback.clear-secret", &[]),
            // daemon.*：关于 daemon 本身（不自动拉起、不给 AI）。
            ("daemon.status", &[]),
            ("daemon.stop", &[]),
            ("recipe.list", &["limit", "removed"]),
            ("recipe.describe", &["recipe"]),
            ("recipe.import", &["connection", "source"]),
            // 后加的：RH 工作流那一半此前只有界面够得着。
            ("recipe.import-workflow", &["connection", "source", "name"]),
            ("recipe.remove", &["recipe"]),
            ("recipe.restore", &["recipe"]),
            ("operation.list", &["limit", "after"]),
            (
                "operation.run",
                // `callback-url` 是后加的（完成回调）。加 flag 是**有意的**契约变更：
                // 只加不改，老脚本照跑；但这份清单必须同步，否则"悄悄改掉一个 flag"没人发现。
                &[
                    "recipe-revision",
                    "input",
                    // 媒体提交闭环：`--asset '字段键=文件'`（可重复）与只读预检 `--dry-run`。
                    // 加 flag 是**有意的**契约变更：只加不改，老脚本照跑。
                    "asset",
                    "dry-run",
                    "idempotency-key",
                    "callback-url",
                ],
            ),
            ("operation.get", &["operation"]),
            ("operation.wait", &["operation", "interval", "timeout"]),
            ("operation.cancel", &["operation"]),
            ("artifact.list", &["limit", "after"]),
            ("artifact.get", &["artifact"]),
            // 后加的：把产物字节下载到本地文件（CLI 专属，不给 AI）。
            (
                "artifact.download",
                &["artifact", "to", "on-conflict", "name-template"],
            ),
            ("asset.upload", &["file", "filename", "mime-type"]),
        ];
        assert_eq!(
            expected.len(),
            VERBS.len(),
            "新增/删除动词时这张清单也要跟着改——它就是 CLI 的用户可见契约"
        );
        for (verb_id, flags) in expected {
            let verb = find_by_id(verb_id).unwrap_or_else(|| panic!("{verb_id} 应当在表里"));
            let rendered = verb
                .cli_params()
                .map(|param| param.cli_name())
                .collect::<Vec<_>>();
            assert_eq!(&rendered, flags, "{verb_id} 的 CLI flag 变了");
        }
    }

    /// **索引与详情都必须与表一致**（渐进式披露之后，"不漂移"要在这两处都钉住）。
    ///
    /// 形状变了，判据跟着变（意图没变：表是唯一真相）：
    /// - **索引**（常驻那几行）：每个 MCP 动词恰好一行，CLI 专属的一个字都不出现；
    /// - **详情**（`catalog(<verb>)` 的返回）：键、必填、标注、描述与表逐字一致。
    ///
    /// ⚠️ 这里只钉"表 → 渲染"这一层；"**服务端真的端出什么**"由 `mcp.rs` 的
    /// `the_index_and_the_table_agree_in_both_directions` 钉住——那一条才抓得住
    /// "有人又手写了一份清单"。
    #[test]
    fn rendered_mcp_index_and_detail_match_the_table() {
        let index = mcp_catalog();
        let exposed = VERBS
            .iter()
            .filter(|verb| verb.mcp_name.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            index.lines().count(),
            exposed.len(),
            "索引必须一个动词一行（多一行少一行都是漂移）：\n{index}"
        );
        assert!(
            VERBS.iter().any(|verb| verb.mcp_name.is_none()),
            "至少要有动词是 MCP 不支持的，否则 `mcp_name: None` 这条路没人走"
        );
        for verb in &exposed {
            assert!(index.contains(verb.id), "索引里少了 {}：\n{index}", verb.id);
        }
        for verb in VERBS.iter().filter(|verb| verb.mcp_name.is_none()) {
            assert!(
                !index.contains(verb.id),
                "{} 是 CLI 专属，不该出现在常驻索引里（那等于把它暴露给 AI）",
                verb.id
            );
        }

        // 详情：键 / 必填 / 标注 / 描述，与表逐字一致。
        for verb in &exposed {
            let detail = mcp_verb_detail(verb);
            let description = detail["description"].as_str().unwrap_or_default();
            assert!(
                description.len() > 20,
                "{} 的详情太短，等于没给模型任何领域知识：{description}",
                verb.id
            );
            let mut expected_properties = verb
                .mcp_params()
                .map(|param| param.mcp_name().to_string())
                .collect::<Vec<_>>();
            expected_properties.sort();
            let mut actual_properties = detail["inputSchema"]["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            actual_properties.sort();
            assert_eq!(
                actual_properties, expected_properties,
                "{} 的 schema 键与表不一致",
                verb.id
            );
            let expected_required = verb
                .mcp_params()
                .filter(|param| param.required.has(Channels::MCP))
                .map(|param| json!(param.mcp_name()))
                .collect::<Vec<_>>();
            assert_eq!(
                detail["inputSchema"]["required"],
                Value::Array(expected_required),
                "{} 的 required 与表不一致",
                verb.id
            );
            assert_eq!(detail["annotations"]["readOnlyHint"], json!(verb.read_only));
            assert_eq!(
                detail["annotations"]["destructiveHint"],
                json!(verb.destructive)
            );
            assert_eq!(
                detail["annotations"]["idempotentHint"],
                json!(verb.idempotent)
            );
        }
    }

    /// **元层不是后门**：`renderloft_call` 只能碰 `mcp_name` 非空的动词。
    ///
    /// 这条比"没列出来"强得多：CLI 专属的四个动词（写密钥 / 关 daemon / 往磁盘写文件 /
    /// 建连接）此前是"清单里没有"，现在多了一个统一入口——**入口必须自己拒绝**，
    /// 否则"刻意不给 AI"就退化成"藏起来"。
    ///
    /// 负对照：把 `route_call` 里 `mcp_name.is_none()` 那个判断删掉 ⇒ 这条红。
    #[test]
    fn the_meta_layer_refuses_verbs_that_are_cli_only() {
        for verb_id in [
            "callback.set-secret",
            "callback.clear-secret",
            "daemon.stop",
            "artifact.download",
            "connection.set-key",
        ] {
            let verb = find_by_id(verb_id).unwrap_or_else(|| panic!("{verb_id} 应当存在"));
            assert!(verb.mcp_name.is_none(), "{verb_id} 应当是 CLI 专属");
            let error = route_call(verb_id).expect_err("元层必须拒绝 CLI 专属动词");
            assert!(
                error.contains("不对 MCP 开放"),
                "拒绝的理由要说清是「人的动作」：{error}"
            );
        }
        // 反过来：开放的动词必须能穿过。
        assert_eq!(
            route_call("operation.run").expect("开放").id,
            "operation.run"
        );
    }

    /// 详情里承诺的形状，实现必须真的支持（这是给模型的契约，没有编译期守卫）。
    ///
    /// 判别点：`operation.run` 的 schema 声明了 `runs`（批量形状），
    /// 而实现一度只认 `inputs`——那样描述就是**假话**：外界 Agent 按描述传 `runs`，
    /// 会拿到一个"缺少 inputs"的错，而它明明照着说明做的。
    ///
    /// 形状变了（详情现在按需取），判据跟着走：从**详情**里读 schema，
    /// 从 `renderloft_call` 的**常驻描述**里读那些跨动词的规矩（付费 / 长任务）。
    #[test]
    fn run_recipe_schema_and_implementation_agree() {
        let run_verb = find_by_id("operation.run").expect("operation.run");
        let run_tool = mcp_verb_detail(run_verb);
        let properties = run_tool["inputSchema"]["properties"]
            .as_object()
            .expect("properties");
        for promised in ["inputs", "runs", "callbackUrl"] {
            assert!(
                properties.contains_key(promised),
                "描述里承诺了 `{promised}`，schema 就必须声明它：{run_tool}"
            );
        }
        // 参数级的说明落在 property 的 description 上（不是详情级 description）。
        assert!(
            properties["callbackUrl"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("webhook"),
            "callbackUrl 得说清它是干什么的（webhook）：{run_tool}"
        );
        // inputs/runs 二选一，schema 表达不了"二选一"，所以靠实现给出清晰错误
        //（CLI 上 inputs 是必填，那由 clap 管，与 MCP 的 required 不是一回事）。
        assert_eq!(
            run_tool["inputSchema"]["required"],
            json!(["recipeRevisionId"]),
            "MCP 上只有 recipeRevisionId 必填：{run_tool}"
        );
        // 跨动词的规矩住在**常驻**的那一份描述里（`renderloft_call`）：付费警告与
        // "长任务用回调、要等就用短超时"必须**每一轮都在**——它们不能等到模型读了详情才知道。
        let call_tool = mcp_tools()
            .into_iter()
            .find(|tool| tool["name"] == MCP_CALL_TOOL)
            .expect("call tool");
        let description = call_tool["description"].as_str().expect("description");
        assert!(
            description.contains("CHARGE"),
            "必须说清这是付费的：{description}"
        );
        assert!(
            description.contains("callbackUrl") && description.contains("SHORT"),
            "必须告诉模型：长任务用 callbackUrl，要等就用短超时：{description}"
        );
        // 索引里也要看得见"这个动词花钱"（模型看目录时就该看见，而不是读了详情才知道）。
        // 判据必须**指到那一行、那个标记**：整份索引里有某个记号是不够的（`operation.cancel`
        // 也带一个），而"花钱"恰恰不是 destructive —— 它曾经因此漏掉过标记。
        let run_line = mcp_catalog()
            .lines()
            .find(|line| line.contains("operation.run"))
            .expect("operation.run 应当在索引里")
            .to_string();
        assert!(
            run_line.starts_with(&format!("{MCP_COST_MARKER} operation.run")),
            "花钱的动词必须以 [COST] 开头（行首、固定位置）出现在常驻索引里：{run_line}"
        );
        assert!(
            !run_line.contains(MCP_IRREVERSIBLE_MARKER),
            "`operation.run` 可以重试（幂等键），不该被标成不可逆：{run_line}"
        );
    }

    /// **成本与不可逆的标记必须是机械通道**：一条正则筛出一组，且与表逐条对上。
    ///
    /// 为什么盯这个：标记此前长在 `summary` 的散文里（`⚠️花钱`），"破坏性"则由渲染器补一段
    /// 后缀 —— 两者只差一个 U+FE0F（零宽变体选择符）。**零宽差异不是通道**：人眼看不见，
    /// 正则写错一个字符就漏，而且"花钱"与"不可逆"混在同一个符号里，谁也筛不干净。
    ///
    /// 负对照：把某个动词的 `spends_credits` 改成 `false` ⇒ 第一条红；
    /// 把 `summary` 里的 `[COST]` 换回 `⚠️`（回到散文通道）⇒ 末两条红。
    #[test]
    fn cost_and_irreversibility_marks_are_mechanical_and_match_the_table() {
        let index = mcp_catalog();
        // 行首标记之后、第一个词就是动词 id（可能同时带两个标记，所以两个都要剥）。
        let marked = |marker: &str| {
            index
                .lines()
                .filter(|line| line.starts_with(marker))
                .map(|line| {
                    let mut rest = line;
                    for prefix in [MCP_COST_MARKER, MCP_IRREVERSIBLE_MARKER] {
                        if let Some(stripped) = rest.strip_prefix(prefix) {
                            rest = stripped.trim_start();
                        }
                    }
                    rest.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        let expected = |pick: fn(&Verb) -> bool| {
            VERBS
                .iter()
                .filter(|verb| verb.mcp_name.is_some())
                .filter(|verb| pick(verb))
                .map(|verb| verb.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            marked(MCP_COST_MARKER),
            expected(|verb| verb.spends_credits),
            "`[COST]` 筛出来的集合必须与表里的 `spends_credits` 逐条相同：\n{index}"
        );
        assert_eq!(
            marked(MCP_IRREVERSIBLE_MARKER),
            expected(|verb| verb.destructive),
            "`[IRREVERSIBLE]` 筛出来的集合必须与表里的 `destructive` 逐条相同：\n{index}"
        );
        assert!(
            !expected(|verb| verb.spends_credits).is_empty(),
            "至少要有一个动词花钱，否则这条测试是空转的"
        );

        // 标记还要在**详情首行**（模型读 schema 时看的就是它），不能只在索引里有。
        for verb in VERBS.iter().filter(|verb| verb.mcp_name.is_some()) {
            let detail = mcp_verb_detail(verb);
            let description = detail["description"].as_str().unwrap_or_default();
            assert!(
                description.starts_with(&mcp_markers(verb)),
                "{} 的详情描述首行必须与索引用同一套标记：{description}",
                verb.id
            );
        }

        // 被服务的 MCP 文本里**不许再出现第二套通道**：`⚠`（U+26A0）或它的变体选择符
        //（U+FE0F）。它出现就说明"人眼看不见的差异"又回来了。
        let mut served = index.clone();
        served.push_str(
            &mcp_tools()
                .iter()
                .map(|tool| tool.to_string())
                .collect::<String>(),
        );
        for verb in VERBS.iter().filter(|verb| verb.mcp_name.is_some()) {
            served.push_str(&mcp_verb_detail(verb).to_string());
        }
        assert!(
            !served.contains('\u{26A0}') && !served.contains('\u{FE0F}'),
            "被服务的 MCP 文本里还有 ⚠ / U+FE0F：风险标记只能走 [COST] / [IRREVERSIBLE] 这条 ASCII 通道"
        );
    }

    /// 回调参数：给了就变成 `operation.terminal` 订阅；没给就**一个字段都不加**。
    ///
    /// 负对照：把 `callback_body` 里的 `"events"` 去掉（留空的语义是"全部事件"）⇒
    /// 第一条断言会红——那样将来加了新事件，这条订阅的行为会**悄悄**变。
    #[test]
    fn callback_url_becomes_a_terminal_subscription() {
        let verb = VERBS
            .iter()
            .find(|verb| verb.id == "operation.run")
            .expect("operation.run");

        let with = Args::from_mcp(
            verb,
            &json!({ "recipeRevisionId": 1, "callbackUrl": "http://127.0.0.1:8787/hook" }),
        )
        .expect("表里声明了 callbackUrl，就该收得下");
        assert_eq!(
            callback_body(&with).expect("应当构造出回调").unwrap(),
            json!({ "url": "http://127.0.0.1:8787/hook", "events": ["operation.terminal"] })
        );

        let without =
            Args::from_mcp(verb, &json!({ "recipeRevisionId": 1 })).expect("不给回调也该收得下");
        assert!(
            callback_body(&without).expect("没给就该是 None").is_none(),
            "没给回调时不能凭空塞一个 callback 字段"
        );

        // 空字符串：当场报错，别把一个空地址发到服务端再被拒
        let empty = Args::from_mcp(
            verb,
            &json!({ "recipeRevisionId": 1, "callbackUrl": "   " }),
        )
        .expect("参数形状本身是合法的");
        assert_eq!(
            callback_body(&empty).expect_err("空地址必须报错").code,
            "CALLBACK_URL_EMPTY"
        );
    }

    /// 旧的 per-app 工具名仍要能回到表里的动词（`renderloft_run_{recipeId}` → `operation.run`）
    /// ——**兼容路径**：这些名字不再出现在工具列表里，但缓存过列表的调用方不该突然失败。
    #[test]
    fn dynamic_tool_names_route_back_to_the_run_verb() {
        let (verb, recipe_id) = route_tool("renderloft_run_42").expect("动态名字应当被认出");
        assert_eq!(verb.id, "operation.run");
        assert_eq!(recipe_id, Some(42));
        let (verb, recipe_id) = route_tool("renderloft_list_recipes").expect("静态工具");
        assert_eq!(verb.id, "recipe.list");
        assert_eq!(recipe_id, None);
        assert!(route_tool("renderloft_run_abc").is_none(), "不是 id 就不认");
        assert!(route_tool("nope").is_none());
    }

    /// **只有 `daemon` 组**能在没有 daemon 时回答，而且它不给 AI。
    ///
    /// 两个方向都要钉：
    /// - daemon 组若要求 daemon 在跑 ⇒ "它到底在不在"就只能靠"启动它试试"来回答（荒谬），
    ///   而且 `daemon stop` 会先把 daemon 启动起来再关掉；
    /// - 其余动词若不要求 ⇒ "客户端先起来"那个场景又退回死局（CLI/MCP 报"无法连接 daemon"）。
    ///
    /// 负对照：把 `Verb::needs_daemon` 改成恒 `true` ⇒ 第一条断言红；恒 `false` ⇒ 第二条红。
    #[test]
    fn only_the_daemon_group_can_answer_without_a_daemon() {
        let mut daemon_verbs = 0;
        for verb in VERBS {
            if verb.group == "daemon" {
                daemon_verbs += 1;
                assert!(!verb.needs_daemon(), "{} 不该要求 daemon 已经在跑", verb.id);
                assert!(
                    verb.mcp_name.is_none(),
                    "{} 不该给 AI：任务在后台跑、对账靠 daemon 常驻，进程生命周期是人的事",
                    verb.id
                );
            } else {
                assert!(verb.needs_daemon(), "{} 应当要求 daemon 在跑", verb.id);
            }
        }
        assert!(
            daemon_verbs >= 2,
            "daemon 组至少有 status 与 stop（实际 {daemon_verbs} 个）"
        );
    }

    /// **每个动词都要真的走到实现**（表里有、实现里没有 ⇒ 这条会红）。
    ///
    /// 做法：参数按表补全（必填与可选都给，走到哪条分支都算"进了实现"），
    /// client 指向一个**没人监听的端口**，于是"走到网络"的动词必然拿到
    /// `CLIENT_NETWORK_ERROR`。若某个动词没写分发，拿到的是 `UNKNOWN_VERB`；
    /// 若分发读了一个没声明的参数名，拿到的是 `UNKNOWN_PARAM`——两种都让这条测试失败。
    ///
    /// `daemon.*` 是例外，判据换成"答案的形状对"：那一组**按设计要能离线回答**
    /// （问"它在不在跑"却先把它启动起来是荒谬的，见 `Verb::needs_daemon`）。
    #[tokio::test]
    async fn every_verb_reaches_its_client_call() {
        let client = V2Client::new("http://127.0.0.1:9", "rh-token-test");
        for verb in VERBS {
            let mut arguments = Map::new();
            for param in verb.mcp_params() {
                arguments.insert(param.mcp_name().to_string(), dummy(param));
            }
            // `operation.run` 的两种写法（单次 `inputs` + `assets` / 批量 `runs`）**不能同时给**
            // ——按表补全会把两个都塞进去，那是歧义（以前被静默忽略，现在明确报错）。
            // 这里按单次那种走，动词照样会走到实现。
            if verb.id == "operation.run" {
                arguments.remove("runs");
            }
            let args = Args::from_mcp(verb, &Value::Object(arguments))
                .unwrap_or_else(|error| panic!("{} 的参数应当能补全：{error}", verb.id));
            let outcome = execute(&client, verb, &args).await;
            if !verb.needs_daemon() {
                // ⚠️ 这一组的结果**取决于本机 daemon 是否在跑**（在跑时它会去问一次真 daemon，
                // 于是拿到网络错；不在跑时直接离线回答）。所以判据是"**进了实现**"，
                // 而不是"必须成功"——否则这条守卫会随环境漂移（实测踩到）。
                // 漏了分发会拿到 UNKNOWN_VERB / UNKNOWN_PARAM，那才是它要抓的。
                match outcome {
                    Ok(data) => {
                        let expected_key = if verb.id == "daemon.status" {
                            "running"
                        } else {
                            "stopped"
                        };
                        assert!(
                            data.get(expected_key).is_some(),
                            "{} 的答案里应当有 `{expected_key}`（没有多半是分发漏了这一行）：{data}",
                            verb.id
                        );
                    }
                    Err(error) => assert!(
                        matches!(error.code.as_str(), "CLIENT_NETWORK_ERROR"),
                        "{} 只允许「本机 daemon 在跑」这一种失败，拿到的是 {}：{}",
                        verb.id,
                        error.code,
                        error.message
                    ),
                }
                continue;
            }
            let error = outcome.expect_err("没有 daemon 时不该成功");
            assert!(
                matches!(
                    error.code.as_str(),
                    // 走到网络、走到本机文件读取，都算"进了实现"。
                    "CLIENT_NETWORK_ERROR" | "ASSET_READ_FAILED" | "SECRET_FILE_READ_FAILED"
                ),
                "{} 没有走到 client 调用（拿到 {}：{}）——多半是分发漏了这一行",
                verb.id,
                error.code,
                error.message
            );
        }
    }

    fn dummy(param: &Param) -> Value {
        match param.kind {
            Kind::Id | Kind::Count => json!(1),
            Kind::Text => json!("x"),
            Kind::Flag => json!(true),
            Kind::IdList => json!([1]),
            Kind::Json => json!({}),
            Kind::JsonArray => json!([{ "inputs": {} }]),
            Kind::LocalFile => json!(std::env::temp_dir()
                .join("renderloft-verb-table-missing.bin")
                .to_string_lossy()),
            // keyed 资产：给一个形状合法但指向不存在资产的对象——分发要能把它**读出来并
            // 传下去**（真不真由 daemon 判）。过去这类输入会被静默丢弃，这条就抓那个。
            Kind::KeyedAssets => {
                json!({ "475::image": { "assetId": 999_999, "checksum": "nope" } })
            }
        }
    }

    /// 未知参数要**报错并列出可用的**，不能静默丢掉。
    #[test]
    fn unknown_arguments_are_rejected_with_the_allowed_list() {
        let verb = find_by_id("operation.get").expect("verb");
        let error = Args::from_mcp(verb, &json!({ "operationId": 1, "recipeId": 2 }))
            .err()
            .expect("未知参数应当报错");
        assert_eq!(error.code, "UNKNOWN_PARAM");
        assert!(
            error.message.contains("operationId"),
            "要列出可用的：{}",
            error.message
        );
    }

    /// 必填缺失要**指名**报错（模型据此知道该补什么）。
    #[test]
    fn missing_required_argument_names_the_parameter() {
        let verb = find_by_id("operation.get").expect("verb");
        let error = Args::from_mcp(verb, &json!({}))
            .err()
            .expect("缺必填应当报错");
        assert_eq!(error.code, "ARGUMENT_REQUIRED");
        assert!(error.message.contains("operationId"), "{}", error.message);
        // 显式 null 与"没给"同义（MCP 客户端常用 null 表达缺省）。
        assert!(Args::from_mcp(verb, &json!({ "operationId": null })).is_err());
    }

    /// 类型错要给**可执行的**错误，而不是一句"缺少必填参数"。
    #[test]
    fn wrong_types_are_reported_as_type_errors() {
        let verb = find_by_id("operation.get").expect("verb");
        let error = Args::from_mcp(verb, &json!({ "operationId": { "nope": 1 } }))
            .err()
            .expect("类型错应当报错");
        assert_eq!(error.code, "ARGUMENT_TYPE");
        // 数字写成字符串收下（模型偶尔这么干），而不是报错。
        let args =
            Args::from_mcp(verb, &json!({ "operationId": "7" })).expect("数字字符串应当收下");
        assert_eq!(args.u64("operationId").expect("读得到"), 7);
    }

    /// 缺省值：表里声明了就必须在"没给"时补上（CLI 与 MCP 拿到同一个值）。
    #[test]
    fn defaults_come_from_the_table() {
        let verb = find_by_id("recipe.list").expect("verb");
        let args = Args::from_mcp(verb, &json!({})).expect("缺省应当补上");
        assert_eq!(args.count("limit").expect("limit"), 50);

        let verb = find_by_id("operation.wait").expect("verb");
        let args = Args::from_mcp(verb, &json!({ "operationId": 1 })).expect("缺省应当补上");
        assert_eq!(args.count("intervalSeconds").expect("interval"), 5);
        assert_eq!(args.count("timeoutSeconds").expect("timeout"), 0);
    }

    /// CLI 侧：flag 渲染、`--input` 读文件、id 列表解析——都在这一条里钉住。
    #[test]
    fn cli_args_render_and_parse_into_the_same_shape() {
        // 文件不存在：**解析阶段**不管（clap 不碰文件系统），读文件在 `Args::from_cli` 里，
        // 于是错误码是 INPUT_INVALID 而不是 clap 的用法错误——调用方要能分辨这两种。
        let matches = cli_command()
            .try_get_matches_from([
                "renderloft",
                "operation",
                "run",
                "--recipe-revision",
                "7",
                "--input",
                "does-not-matter.json",
            ])
            .expect("解析本身应当成功（文件存在与否是下一步的事）");
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let verb = find_by_id("operation.run").expect("verb");
        let error = Args::from_cli(verb, verb_matches)
            .err()
            .expect("读不到输入文件应当报错");
        assert_eq!(error.code, "INPUT_INVALID");
        assert!(
            error.message.contains("does-not-matter.json"),
            "{}",
            error.message
        );

        // 缺必填 ⇒ clap 直接报错（CLI 的必填由 clap 管，MCP 的由 Args 管）。
        assert!(cli_command()
            .try_get_matches_from(["renderloft", "operation", "get"])
            .is_err());
    }

    /// 从**真实文件**读 inputs（CLI 的 `--input` 形状），而不是只测路径解析。
    #[test]
    fn cli_input_file_is_read_as_json() {
        let path = std::env::temp_dir().join(format!(
            "renderloft-verb-table-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, r#"{"prompt":"a cat"}"#).expect("写临时文件");
        let matches = cli_command()
            .try_get_matches_from([
                "renderloft",
                "operation",
                "run",
                "--recipe-revision",
                "7",
                "--input",
                &path.to_string_lossy(),
            ])
            .expect("应当解析成功");
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let verb = find_by_id("operation.run").expect("verb");
        let args = Args::from_cli(verb, verb_matches).expect("解析应当成功");
        assert_eq!(args.json("inputs").expect("inputs")["prompt"], "a cat");
        let _ = std::fs::remove_file(&path);
    }

    /// CLI 的媒体提交闭环：`--asset '字段键=文件'`（可重复）解析成字段键规格表，
    /// 三个错误边界（缺 `=` / 重名 / 空路径）各报各的码——不许静默丢掉或取后一个。
    #[test]
    fn cli_asset_flags_parse_into_field_key_specs() {
        let input_path = std::env::temp_dir().join(format!(
            "renderloft-asset-input-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&input_path, "{}").expect("写临时文件");
        let run_matches = |assets: &[&str]| {
            let mut argv = vec![
                "renderloft",
                "operation",
                "run",
                "--recipe-revision",
                "7",
                "--input",
            ];
            let input = input_path.to_string_lossy().to_string();
            argv.push(&input);
            for asset in assets {
                argv.push("--asset");
                argv.push(asset);
            }
            cli_command()
                .try_get_matches_from(argv)
                .expect("clap 解析应当成功")
        };
        let verb = find_by_id("operation.run").expect("verb");

        // 正常：可重复、顺序无关（字段键才是地址）。
        let matches = run_matches(&["475::image=person.png", "520::image=cloth.png"]);
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let args = Args::from_cli(verb, verb_matches).expect("解析应当成功");
        assert_eq!(
            args.json("assets").expect("assets"),
            json!([
                { "fieldKey": "475::image", "path": "person.png" },
                { "fieldKey": "520::image", "path": "cloth.png" },
            ])
        );

        // 缺 `=`。
        let matches = run_matches(&["475::image"]);
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let error = Args::from_cli(verb, verb_matches)
            .err()
            .expect("缺 = 应当报错");
        assert_eq!(error.code, "ASSET_SPEC_INVALID");

        // 同一字段两个文件：歧义 ⇒ 报错（而不是取后一个）。
        let matches = run_matches(&["475::image=a.png", "475::image=b.png"]);
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let error = Args::from_cli(verb, verb_matches)
            .err()
            .expect("重名应当报错");
        assert_eq!(error.code, "ASSET_FIELD_DUPLICATED");
        assert!(error.message.contains("475::image"), "{}", error.message);

        // 空路径。
        let matches = run_matches(&["475::image="]);
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let error = Args::from_cli(verb, verb_matches)
            .err()
            .expect("空路径应当报错");
        assert_eq!(error.code, "ASSET_SPEC_INVALID");
        let _ = std::fs::remove_file(&input_path);
    }

    /// MCP 的 `assets`：keyed 对象被收下并**规范化**；位置数组、缺字段、多余键都**报错**
    /// ——静默丢弃正是事故的开端。
    #[test]
    fn mcp_assets_must_be_keyed_objects() {
        let verb = find_by_id("operation.run").expect("verb");
        let args = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 7,
                "inputs": {},
                "assets": { "475::image": { "assetId": 12, "checksum": "sha1-x" } },
            }),
        )
        .expect("keyed 对象应当合法");
        assert_eq!(
            args.json("assets").expect("assets"),
            json!({ "475::image": { "assetId": 12, "checksum": "sha1-x" } })
        );

        // 位置数组 ⇒ 明确拒绝，并给出替代做法。
        let error = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 7,
                "inputs": {},
                "assets": [{ "assetId": 12, "checksum": "sha1-x" }],
            }),
        )
        .err()
        .expect("位置数组必须被拒");
        assert_eq!(error.code, "ASSET_SHAPE_INVALID");
        assert!(
            error.message.contains("475::image"),
            "错误要告诉调用方键长什么样：{}",
            error.message
        );

        // 缺 checksum。
        let error = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 7,
                "inputs": {},
                "assets": { "475::image": { "assetId": 12 } },
            }),
        )
        .err()
        .expect("缺 checksum 必须报错");
        assert_eq!(error.code, "ASSET_SHAPE_INVALID");

        // 字段里多塞键：报错而不是忽略（忽略会让人以为它起了作用）。
        let error = Args::from_mcp(
            verb,
            &json!({
                "recipeRevisionId": 7,
                "inputs": {},
                "assets": { "475::image": { "assetId": 12, "checksum": "x", "note": "hi" } },
            }),
        )
        .err()
        .expect("多余键必须报错");
        assert_eq!(error.code, "ASSET_SHAPE_INVALID");

        // `dryRun` 是布尔，读取路径与其它 flag 一致。
        let args = Args::from_mcp(
            verb,
            &json!({ "recipeRevisionId": 7, "inputs": {}, "dryRun": true }),
        )
        .expect("dryRun 应当合法");
        assert!(args.flag("dryRun").expect("dryRun"));
    }

    /// 上传：文件名与 MIME 都从**表 + 派生**来，不再各通道各写一个默认值。
    #[test]
    fn upload_defaults_are_derived_not_duplicated() {
        assert_eq!(guess_mime_type("a.PNG"), "image/png");
        assert_eq!(guess_mime_type("a.jpeg"), "image/jpeg");
        assert_eq!(guess_mime_type("a.unknown"), "application/octet-stream");
        let verb = find_by_id("asset.upload").expect("verb");
        let args = Args::from_mcp(verb, &json!({ "path": "/tmp/a.webp" })).expect("解析");
        assert_eq!(args.text("path").expect("path"), "/tmp/a.webp");
        assert!(args.opt_text("mimeType").expect("mime").is_none());
    }

    /// `recipe.import` 与 `artifact.get` 同批补上；`recipe remove/restore` 与
    /// `connection.set-key` **故意不给 MCP**，而这件事写在表里（不是靠人比对两个文件）。
    #[test]
    fn channel_coverage_is_written_in_the_table() {
        for verb_id in [
            "recipe.import",
            "artifact.get",
            "operation.list",
            "connection.list",
        ] {
            let verb = find_by_id(verb_id).expect("verb");
            assert!(verb.mcp_name.is_some(), "{verb_id} 应当对 MCP 开放");
        }
        for verb_id in ["recipe.remove", "recipe.restore", "connection.set-key"] {
            let verb = find_by_id(verb_id).expect("verb");
            assert!(
                verb.mcp_name.is_none(),
                "{verb_id} 是「不该给外界 Agent」的动作（改授权边界 / 策展自己的库 / 写密钥）"
            );
            // 但它们仍然要从 CLI 可用——这就是"通道覆盖差异写在表里"的意义。
            let _ = cli_subcommand(verb.group, verb.action);
        }
        // MCP 支持的动词数量就是表里 `mcp_name` 非空的行数：CLI 独有的（`mcp_name: None`）
        // 不算，写密钥/关 daemon/删配方那些属于人的动作也算不进去（见上一条测试）。
        // 这个数字变了要在这里显式改一次——它是"MCP 对外承诺的工具数"，改小等于**删能力**，
        // 不该悄悄发生。
        assert_eq!(
            VERBS.iter().filter(|verb| verb.mcp_name.is_some()).count(),
            14
        );
    }

    /// 落点冲突：**文件名叫什么我们不管，但"撞上了"必须由动词兜底**。
    ///
    /// 背景（实测）：RunningHub 按输出节点命名，同一 recipe 的每个 run 都产出 `870.png`，
    /// 19 个产物收进一个目录 ⇒ 此前 `fs::write` **静默覆盖，只剩最后一个**。脚本只能自己加
    /// `runId-` 前缀绕过（那是绕过，不是解决）。现在动词给序号，形状与浏览器 / yt-dlp 一致。
    #[test]
    fn artifact_download_resolves_name_collisions_like_a_mainstream_downloader() {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-collision-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let target = dir.join("870.png");
        // 一个空目录里没有冲突，名字**一个字符都不动**（不发明名字）。
        assert_eq!(unique_sibling(&target), target);

        std::fs::write(&target, b"first").expect("占位");
        // `cat.png` → `cat (1).png`：序号插在扩展名前，不是 `870-1.png`、更不是 `runId-870.png`。
        assert_eq!(
            unique_sibling(&target),
            dir.join("870 (1).png"),
            "撞名后缀的形状必须与浏览器/yt-dlp 同形"
        );
        std::fs::write(dir.join("870 (1).png"), b"second").expect("占位");
        assert_eq!(unique_sibling(&target), dir.join("870 (2).png"));

        // 没有扩展名的产物也要能消歧（`artifact.bin` 这类兜底名）。
        let bare = dir.join("noext");
        std::fs::write(&bare, b"x").expect("占位");
        assert_eq!(unique_sibling(&bare), dir.join("noext (1)"));

        // ⚠️ 目标**目录还不存在**时（`--to D:\还没建的目录\`）任何路径的 `exists()` 都是 false，
        // 于是消歧会退回原名——这不是 bug，但形状得钉住：下载自己会建目录，第一次落盘本来
        // 就没有冲突可言。
        let missing = dir.join("还没有的目录").join("870.png");
        assert_eq!(unique_sibling(&missing), missing);

        // 无父目录的相对写法（`--to 870.png`）也走同一条路，不 panic。
        assert_eq!(
            unique_sibling(std::path::Path::new("renderloft-不可能存在的名字.png")),
            std::path::Path::new("renderloft-不可能存在的名字.png")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 冲突策略写在**表**里（默认 rename），不是散在实现里；且 `--on-conflict` 有缺省值。
    #[test]
    fn artifact_download_conflict_policy_is_declared_in_the_table() {
        let verb = find_by_id("artifact.download").expect("verb");
        let param = verb
            .params
            .iter()
            .find(|param| param.name == "onConflict")
            .expect("冲突策略必须在表里声明");
        assert_eq!(param.default, Some("rename"), "缺省策略是 rename");
        assert!(
            param.help.contains("rename")
                && param.help.contains("skip")
                && param.help.contains("overwrite"),
            "三个取值都要写在 help 里：{}",
            param.help
        );
        // 幂等声明必须是 false：不同产物可以同名，落盘结果取决于"下过几个、下到哪"。
        assert!(
            !verb.idempotent,
            "artifact.download 不是幂等的——同名产物会互相覆盖"
        );

        // 表 → clap 这条渲染链真的把 flag 端出来了（`onConflict` → `--on-conflict`），
        // 且值能原样回到 Args 里。**不给 `--on-conflict` 时 clap 补表里的缺省**——
        // 实现里那行 `unwrap_or_default()` 走的是"空 = rename"，两条路必须同解。
        let matches = cli_command()
            .try_get_matches_from([
                "renderloft",
                "artifact",
                "download",
                "--artifact",
                "7",
                "--on-conflict",
                "skip",
            ])
            .expect("--on-conflict 应当被 clap 认出来");
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let args = Args::from_cli(verb, verb_matches).expect("解析应当成功");
        assert_eq!(
            args.opt_text("onConflict").expect("onConflict").as_deref(),
            Some("skip")
        );

        let matches = cli_command()
            .try_get_matches_from(["renderloft", "artifact", "download", "--artifact", "7"])
            .expect("不给 --on-conflict 也应当成功");
        let (_, group) = matches.subcommand().expect("group");
        let (_, verb_matches) = group.subcommand().expect("action");
        let args = Args::from_cli(verb, verb_matches).expect("解析应当成功");
        assert_eq!(
            args.opt_text("onConflict").expect("onConflict").as_deref(),
            Some("rename"),
            "缺省值来自表"
        );
    }

    /// `--name-template`：**默认不发明名字，但把命名权交出去**（yt-dlp 的 `-o` 那个位置）。
    ///
    /// 只支持 `{artifactId}` / `{filename}` 两个占位符，且展开结果必须是 `--to` 目录内的
    /// 相对路径——模板一旦能跳出那个目录，"往哪写"就不再由调用方决定了。
    #[test]
    fn name_template_expands_two_placeholders_and_cannot_escape_the_target() {
        let expanded = |template: &str| {
            expand_name_template(template, 999002000, "870.png")
                .map(|path| path.to_string_lossy().replace('\\', "/"))
        };
        assert_eq!(
            expanded("{artifactId}-{filename}").unwrap(),
            "999002000-870.png"
        );
        assert_eq!(expanded("{filename}").unwrap(), "870.png");
        // 子目录是允许的——批量场景最干净的排法，而且它**不污染文件名**。
        assert_eq!(
            expanded("{artifactId}/{filename}").unwrap(),
            "999002000/870.png"
        );

        // 未知占位符当场报错，不静默留着（留着会写出一堆名字里带 `{foo}` 的文件）。
        let unknown = expanded("{date}/{filename}").expect_err("未知占位符必须报错");
        assert_eq!(unknown.code, "INVALID_NAME_TEMPLATE");
        assert!(unknown.message.contains("{date}"), "{}", unknown.message);

        // ⚠️ Windows 上 `\foo` 是 rooted 但**不是** absolute，而 `base.join("\\foo")` 会把 base
        // 整个丢掉——所以必须用 `has_root()` 判，不能用 `is_absolute()`。
        for escaping in [
            "\\evil.png",
            "/evil.png",
            "../evil.png",
            "a/../../evil.png",
            "C:/evil.png",
        ] {
            let error = expanded(escaping).expect_err("不能跳出 --to 目录");
            assert_eq!(error.code, "INVALID_NAME_TEMPLATE", "{escaping}");
        }
    }
}
