//! MCP 客户端 —— **工具 schema 从动词表渲染**（见 `verbs.rs`）。
//!
//! 这个文件里**不该再出现工具清单**：名字、描述、参数 schema、只读/破坏性标注
//! 都由那张表决定。加能力请改表。
//!
//! ⚠️ **工具列表不再随应用目录变形**（此前每个应用会生成一个 `renderloft_run_{id}`，
//! 并把该应用的完整 inputSchema 塞进工具列表）。三条理由：
//!
//! 1. **每次 `tools/list` 是 N+1 次 HTTP 请求**（N = 应用数）——而工具列表本该是
//!    本地常量：现在它连 daemon 都不需要（见 `handle_request` 的 `tools/list` 分支）；
//! 2. **会变形状的接口是 bug 来源**：调用方（包括各种客户端）会缓存工具列表，
//!    应用一增一减列表就变，"工具不见了"这类故障极难查；
//! 3. **与内置 agent 侧已经做过的决定相反**：`recipe_catalog` 的注释写着"索引常驻、
//!    schema 按需……把 schema 留在提示里，等于每轮都为'可能用得上'付钱"。
//!    动态工具做的正是那件事，只不过付的是调用方的上下文。
//!
//! 现在"挑应用"是**三步常驻流程**：`renderloft_list_recipes`（索引）→
//! `renderloft_describe_recipe`（按需取字段）→ `renderloft_run_recipe`（提交）。

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

use super::client::{ClientError, V2Client};
use super::verbs::{self, Args};
// ⚠️ 这里 import 的是**强执行方的那几个数字**，不是另抄一份：`instructions` 里报的上限
// 必须与代码真正强制的上限是同一个值 —— 抄一份就迟早会说谎（"清单说 5、代码拦在 4"）。
use crate::v2::application::create_operation::{
    MAX_AUTONOMOUS_CONCURRENT_OPERATIONS, MAX_RUNS_PER_OPERATION,
};

/// 工具列表 —— **表渲染**（这就是"加一个能力只加一行"的那一处）。
///
/// 它是**纯函数**：不看应用目录、不连 daemon、不发请求。
pub fn tools() -> Vec<Value> {
    verbs::mcp_tools()
}

/// `initialize` / `server/discover` 的 `instructions`：身份句 ＋ **跨动词的规矩** ＋ 已经在强制的边界。
///
/// ## 为什么这里放"流程"，而工具描述只放"这一个动词"（§D）
///
/// 工具面压平之后（每个动词一个原生工具），**每一轮都要付 N 条描述**。把"典型流程 / 付费 /
/// 长任务 / 失败怎么查"这类**跨动词**的话写进每一条描述里，等于同一句话付 N 遍 —— 而且改一次
/// 要改 N 处（漏一处就是"清单说一件事、另一个工具说另一件"）。所以分工是死的：
///
/// * **`instructions` 说流程**（这一份，只付一次）；
/// * **工具 `description` 说这一个动词**（那句里不许出现别的工具名）。
///
/// 判据（有测试钉着）：流程、付费警告与并发上限的数字都能在 `instructions` 里读到；
/// 且没有任何一条工具描述在教"先调哪个工具"。
///
/// ## 为什么边界必须在这里
///
/// 调用方要能**据以规划**，而不是撞墙才知道。"按目录铺开 5 个任务"被并发闸拒掉，浪费的不只是
/// 一轮 —— 还会被误判成故障。这些边界在代码里**早就存在**，缺的只是把它们搬到协议地板上
/// （设计律：**凡是服务端已经在强制的边界，都必须出现在协议地板上**；它不新增权力，只是把已经
/// 存在的边界公开）。
///
/// ⚠️ 数字全部来自强执行方（`create_operation` 与本文件的 `MCP_RESULT_*`），**不手抄**：
/// 手抄的那些迟早与代码分叉。
fn instructions() -> String {
    format!(
        "{identity}\n\n\
         Typical flow: list the apps (recipes) → read one app's input schema → submit a run → \
         follow it up with the cheap read-only `operation.get` until it reaches a terminal state. \
         Submitting returns as soon as the work is QUEUED, not when it finishes; `operation.wait` \
         and `callbackUrl` are optional conveniences, never required — a tool call is \
         request/response, and a long wait inside one occupies the client's request channel (whose \
         own timeout may be shorter than the wait).\n\n\
         Money: runs spend the USER'S OWN platform credits. Each entry in `runs` is a separate \
         charged run, and retrying an operation submits (and charges) again — so before resubmitting \
         after a failure, read the run's `inputs` together with its `error`, fix the offending \
         parameter, and only then retry.\n\n\
         Do not read `status: RUNNING` as \"work is progressing\": a run may be queued behind \
         another, waiting to retry, or waiting for confirmation of a submission whose outcome is \
         unknown. Every run that is waiting for something carries an `attention` object saying why, \
         until when, and what can be done — read it before telling the user how long a task will \
         take.\n\n\
         Limits this server already enforces — plan with them instead of discovering them by \
         failing: one operation carries at most {runs} runs, and **every run is a separate charged \
         run**; an autonomous caller such as this one may have at most {concurrency} operations in \
         flight at once (wait for one to finish before submitting the next); list results are \
         trimmed to {items} items / {kilobytes} KB, with an explicit marker in the result when that \
         happens — an honest truncation, never a silent one.",
        identity = verbs::MCP_IDENTITY,
        runs = MAX_RUNS_PER_OPERATION,
        concurrency = MAX_AUTONOMOUS_CONCURRENT_OPERATIONS,
        items = MCP_RESULT_MAX_ITEMS,
        kilobytes = MCP_RESULT_MAX_BYTES / 1024,
    )
}

pub async fn call_tool(name: &str, arguments: &Value) -> Value {
    // ⚠️ **自报 `mcp`**（见 `mcp_client`）：daemon 侧据此对这个调用方设花钱闸门。
    // MCP 与 GUI 走同一个端点（origin 都是 `http`），不自报就分不出
    // "外界 Agent 在自动提交"与"人在点界面"——而前者必须有闸。
    // daemon 不在就自己拉起来（**启动，但不拥有**，见 `daemon_boot`）；探测结果往下传，
    // 好让"连不上"的报错里带上"为什么没能把 daemon 拉起来"。
    let boot = super::daemon_boot::ensure_running();
    let client = match mcp_client(&boot) {
        Ok(client) => client,
        Err(error) => {
            return error_as_mcp(&ClientError::network(format!("无法连接 daemon：{error}")))
        }
    };
    match invoke(&client, name, arguments).await {
        Ok(data) => response_as_mcp(data),
        Err(error) => error_as_mcp(&super::daemon_boot::explain_failure(error, &boot)),
    }
}

/// 工具名 → 动词 → 参数 → 分发。**三个通道里唯一的一条路**（CLI 那条在 `cli.rs`）。
///
/// 对外是**每个动词一个原生工具**（见 `verbs::mcp_tools`），外加一个只读的参考工具
/// `renderloft_catalog`（索引 + 单个动词的完整语义，**纯本地，不问 daemon**）。
///
/// 旧工具名**仍然认**：调用方可能缓存过旧的 `tools/list`（包括已从清单里撤掉的
/// `renderloft_call`，以及更早的 `renderloft_list_recipes` 这些），让它因为我们的形状改变而
/// 突然失败是不礼貌的——而兼容不花常驻成本，因为它们**不再出现在清单里**。
async fn invoke(client: &V2Client, name: &str, arguments: &Value) -> Result<Value, ClientError> {
    // 索引与详情：本地就能答，不用打扰 daemon。
    if name == verbs::MCP_CATALOG_TOOL {
        let requested = arguments.get("verb").and_then(Value::as_str);
        return match requested {
            Some(verb_id) => verbs::route_call(verb_id)
                .map(verbs::mcp_verb_detail)
                .map_err(argument_error_of),
            None => {
                let verbs: Vec<Value> = verbs::VERBS
                    .iter()
                    .filter(|verb| verb.mcp_name.is_some())
                    .map(verbs::mcp_verb_detail)
                    .collect();
                Ok(json!({ "verbs": verbs, "count": verbs.len() }))
            }
        };
    }
    // 元层调用：把 `verb` 解出来，再走与旧工具名**完全相同**的那条路。
    let (name, arguments) = if name == verbs::MCP_CALL_TOOL {
        let Some(verb_id) = arguments.get("verb").and_then(Value::as_str) else {
            return Err(argument_error_of("VERB_REQUIRED".to_string()));
        };
        let verb = verbs::route_call(verb_id).map_err(argument_error_of)?;
        let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
        (verb.mcp_name.unwrap_or(verb_id), args)
    } else {
        (name, arguments.clone())
    };

    let Some((verb, recipe_id)) = verbs::route_tool(name) else {
        return Err(ClientError {
            status: 0,
            code: "UNKNOWN_TOOL".to_string(),
            message: format!("未知工具：{name}"),
            retryable: false,
            details: Value::Null,
        });
    };
    let mut arguments = arguments;
    if let Some(recipe_id) = recipe_id {
        // **兼容路径**：`renderloft_run_{recipeId}` 是旧的动态工具名，已经不再出现在
        // `tools/list` 里（理由见文件头）。但**名字仍然认**——调用方可能缓存过那份工具列表，
        // 让它因为我们的决定而突然失败是不礼貌的，而且这条路的成本只有一次
        // "问 daemon 要当前 revision"。正式形态是 `renderloft_run_recipe` + `recipeRevisionId`。
        if arguments
            .get("recipeRevisionId")
            .and_then(Value::as_u64)
            .is_none()
        {
            let revision = current_revision(client, recipe_id).await?;
            if let Value::Object(map) = &mut arguments {
                map.insert("recipeRevisionId".to_string(), json!(revision));
            }
        }
    }
    let args = Args::from_mcp(verb, &arguments)?;
    verbs::execute(client, verb, &args).await
}

/// 参数级错误（`route_call` 那类"你说得不对"）→ `ClientError`，让报错与别的路径同形。
fn argument_error_of(message: String) -> ClientError {
    ClientError {
        status: 0,
        code: "UNKNOWN_PARAM".to_string(),
        message,
        retryable: false,
        details: Value::Null,
    }
}

/// 取某个 Recipe 的当前 revision（旧的动态工具名没显式给 `recipeRevisionId` 时用）。
async fn current_revision(client: &V2Client, recipe_id: u64) -> Result<u64, ClientError> {
    let recipe = client.get_recipe(recipe_id).await?;
    recipe
        .get("recipe")
        .and_then(|recipe| recipe.get("currentRevisionId"))
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError {
            status: 0,
            code: "RECIPE_REVISION_REQUIRED".to_string(),
            message: "无法确定 recipeRevisionId（未提供且无法解析当前 revision）".to_string(),
            retryable: false,
            details: Value::Null,
        })
}

/// 回执里**最多保留多少条**列表项（渐进式披露的**输出侧**那一档）。
///
/// 输入侧（索引/详情）解决"模型每轮要付多少"；输出侧解决"一次调用能灌进多少"。
/// 20 条：够看清形状与挑出目标，又不至于把窗口里真正干活的地方挤掉。
pub const MCP_RESULT_MAX_ITEMS: usize = 20;
/// 回执的字节兜底：列表之外，**单个对象也可能很大**（例如一次 100 个 run 的任务详情）。
pub const MCP_RESULT_MAX_BYTES: usize = 64 * 1024;

/// **有界回执**：列表截断时**必须显式说明**，绝不悄悄丢。
///
/// ⚠️ 悄悄截断比不截断更坏：模型会以为"我看到的就是全部"，于是漏掉剩下的还理直气壮。
/// 所以截断时给三样东西：`truncated`（有截断）、`totalItems`（原本多少）、以及**怎么拿更多**
/// （用 `limit`/`after` 分页）。同时**去掉 `nextCursor`**——它指向的是 daemon 那一页的末尾，
/// 在截断之后继续用它会把中间那些**跳过**（那是错误的分页语义）。
pub fn bound_result(data: Value) -> Value {
    let mut payload = data;
    if let Some(items) = payload.get("items").and_then(Value::as_array) {
        let total = items.len();
        if total > MCP_RESULT_MAX_ITEMS {
            let kept = items[..MCP_RESULT_MAX_ITEMS].to_vec();
            if let Value::Object(map) = &mut payload {
                map.insert("items".to_string(), Value::Array(kept));
                map.insert("truncated".to_string(), json!(true));
                map.insert("returnedItems".to_string(), json!(MCP_RESULT_MAX_ITEMS));
                map.insert("totalItems".to_string(), json!(total));
                map.remove("nextCursor");
                map.insert(
                    "hint".to_string(),
                    json!(format!(
                        "只返回了前 {MCP_RESULT_MAX_ITEMS} 条（共 {total} 条）。要更多：用更小的 \
                         limit 分页，或把最后一条的 id 作为 after 再查一次。"
                    )),
                );
            }
        }
    }
    let size = serde_json::to_string(&payload)
        .map(|text| text.len())
        .unwrap_or(0);
    if size > MCP_RESULT_MAX_BYTES {
        return json!({
            "truncated": true,
            "bytes": size,
            "hint": format!(
                "结果太大（{size} 字节，上限 {MCP_RESULT_MAX_BYTES}），没有内联。\
                 用更小的 limit 分页取，或先取元数据再按 id 查细节。"
            )
        });
    }
    payload
}

/// 把 daemon 的响应包成 MCP 工具结果。
///
/// ⚠️ `structuredContent` **必须是对象**（MCP 规范），而 daemon 有个别端点直接返回数组
/// （`/api/v1/connections` 就是）。实测（2026-10-10，DSH 接进来后第一次真调）：
/// 数组会让**严格的 MCP 客户端直接拒收整个结果**——
/// `Invalid result for tools/call: expected record, received array` ——
/// 于是那个工具在客户端侧**完全不可用**，而当时的单测全绿（它们只看 `content` 文本）。
/// 所以非对象一律包成 `{ "items": ... }`（与 daemon 其它列表端点的形状一致）；
/// **文本与结构化两边都包**，免得同一个结果出现两种形状。
pub fn response_as_mcp(data: Value) -> Value {
    let payload = if data.is_object() {
        data
    } else {
        json!({ "items": data })
    };
    // 有界：**所有**回执都过这一道（列表截断 + 字节兜底），别指望每个动词自己记得。
    let payload = bound_result(payload);
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&payload).unwrap_or_default()
        }],
        "structuredContent": payload,
        "isError": false
    })
}

pub fn error_as_mcp(error: &ClientError) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&json!({
                "code": error.code,
                "message": error.message,
                "retryable": error.retryable,
            })).unwrap_or_default()
        }],
        "structuredContent": json!({
            "error": {
                "status": error.status,
                "code": error.code,
                "message": error.message,
                "retryable": error.retryable,
                "details": error.details,
            }
        }),
        "isError": true
    })
}

/// MCP 专用的 client：**永远自报 `mcp`**。
///
/// ⚠️ 抽成一个函数，是为了让"忘记自报"这件事**做不到**：这个文件里原本有**两处**
/// `V2Client::from_endpoint_file()`，我加 driver 时只加了一处、漏了另一处
/// （`tools/list` 那条）。那次没造成后果——它只读不写——但形状是危险的：
/// **漏掉自报的那条路就是绕过花钱闸门的路**。现在 `tools/list` 已经不需要 client 了，
/// 但这个函数仍然是**唯一**的取 client 入口。
///
/// `boot` 由调用方传入（`call_tool` 已经 ensure 过一次），这里不再重复探测。
fn mcp_client(
    boot: &Result<super::daemon_boot::EnsureOutcome, String>,
) -> Result<V2Client, String> {
    match V2Client::from_endpoint_file() {
        Ok(client) => Ok(client.with_driver("mcp")),
        Err(error) => match boot {
            Err(boot_error) => Err(format!("{error}（自动启动 daemon 也失败了：{boot_error}）")),
            Ok(_) => Err(error),
        },
    }
}

/// 我们**明确支持**的协议版本。
///
/// * `2026-07-28`：**现代**协议——没有握手，版本/身份/能力走每次请求的 `_meta`，
///   结果必须带 `resultType`，列表结果要带缓存提示（`ttlMs`/`cacheScope`）。
/// * `2025-03-26`：**legacy** 握手版本（我们原本实现的），继续认——客户端可能还在用。
///
/// 两者都支持即规范里说的 **dual-era**。判断依据见
/// <https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning>。
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = ["2026-07-28", "2025-03-26"];

pub fn handle_request(request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method")?.as_str()?;
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    // 现代协议把版本放在**每次请求**的 `_meta` 里（没有握手）。不认识的版本要按规范回
    // `UnsupportedProtocolVersionError`（-32022）并列出支持的版本，客户端据此重挑一个再来。
    // 老客户端不带这个字段 ⇒ 照旧走 legacy 握手（dual-era）。
    if let Some(version) = params
        .get("_meta")
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
    {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version",
                    "data": { "supported": SUPPORTED_PROTOCOL_VERSIONS, "requested": version }
                }
            }));
        }
    }
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-03-26"),
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "renderloft", "version": env!("CARGO_PKG_VERSION") },
            // MCP 里**专门放"这个 server 是干嘛的"**的字段（客户端可以把它给模型看）。
            // 元层把自带领域暗示的工具名换成了中立的调用口，所以身份必须在这里也说一遍——
            // 这是"不可见 = 不存在"那条约束的对价。工具描述里也有一份（那条是**保证**会被看到
            // 的通道；这条取决于客户端是否转达）。上限也在这里报（见 `instructions()`）。
            "instructions": instructions()
        })),
        // 工具列表是**本地常量**：不连 daemon、不发请求、不随应用目录变形
        //（此前每个应用要一次 HTTP，见文件头）。daemon 没起来时它照样可用——
        // 这很重要：客户端要能先看到工具，才知道怎么把 daemon 用起来。
        //
        // `ttlMs`/`cacheScope` 是 2026-07-28 起对列表结果的硬要求（CacheableResult）：
        // 工具表是编译期常量、与用户无关 ⇒ public，给一个长 TTL 让客户端少问几次。
        "tools/list" => Ok(json!({
            "tools": tools(),
            "ttlMs": 3_600_000,
            "cacheScope": "public"
        })),
        // 规范里 `server/discover` 是 **MUST**。客户端可以不调（直接发别的 RPC、遇错再挑版本也行），
        // 但它在 STDIO 上还兼着"向后兼容探针"的角色，也是客户端一次问清版本/能力/身份的正路。
        //
        // 字段名照官方 schema 抄，**不猜**（2026-07-28 的 DiscoverResult）：
        // `supportedVersions` + `capabilities` + 身份放 `_meta["io.modelcontextprotocol/serverInfo"]`
        // + 可选 `instructions` + 缓存提示。`resultType` 由下面的统一出口补。
        "server/discover" => Ok(json!({
            "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
            "capabilities": { "tools": { "listChanged": true } },
            "_meta": {
                "io.modelcontextprotocol/serverInfo": {
                    "name": "renderloft",
                    "version": env!("CARGO_PKG_VERSION")
                }
            },
            "instructions": instructions(),
            "ttlMs": 3_600_000,
            "cacheScope": "public"
        })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let runtime = tokio::runtime::Runtime::new().expect("unable to create MCP runtime");
            Ok(runtime.block_on(call_tool(name, &arguments)))
        }
        _ => Err(json!({
            "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": format!("unsupported MCP method: {method}") }
        })),
    };
    Some(match result {
        // 2026-07-28 起**每个结果都必须带 `resultType`**（`"complete"` / `"input_required"`）。
        // 在这里统一补：三个分支（initialize / tools/list / tools/call）共用一条出口，
        // 免得以后加方法时漏掉一个。规范要求新客户端把**缺这个字段的老结果**当作 `"complete"`，
        // 所以补上它对老客户端也无害。
        Ok(value) => {
            let mut value = value;
            if let Some(object) = value.as_object_mut() {
                object
                    .entry("resultType")
                    .or_insert_with(|| json!("complete"));
            }
            json!({ "jsonrpc": "2.0", "id": id, "result": value })
        }
        Err(value) => value,
    })
}

pub fn run_stdio_loop() {
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(response) = handle_request(request) {
            println!("{response}");
            let _ = io::stdout().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 给"连 daemon **之前**就该失败"的分支用的 client：**不读端点文件**。
    ///
    /// ⚠️ 为什么不能用 `mcp_client()`：它读的是**本机正在跑的 daemon** 写出的端点文件。
    /// 实测（2026-10-09，数据目录改名那一次）：本机 daemon 还在写旧路径，这两条测试直接以
    /// `os error 3`（系统找不到指定的路径）失败——而它们要验的 `ASSET_READ_FAILED` /
    /// `UNKNOWN_TOOL` 根本不碰网络。**测试依赖开发机的运行时状态，等于环境一变就红**：
    /// CI 里从来没有 daemon（这份工作流第一次真跑就会是红的）。
    fn offline_client() -> V2Client {
        V2Client::new("http://127.0.0.1:31337", "offline-test-token").with_driver("mcp")
    }

    /// 每个工具的形状自检：有名字、有 schema、描述足够长；参考工具是只读的；
    /// 两个"最容易被写错"的标注（会花钱的 `operation.run` 不许声称幂等、不可逆的
    /// `operation.cancel` 必须声称破坏性）逐条钉住。
    ///
    /// ⚠️ 这条**不再**断言"只有两个工具"：那是压平前的形状。工具数由
    /// `the_listed_tools_are_the_table_s_verbs_plus_the_reference_tool` 按表钉住（不写死数字）。
    #[test]
    fn the_meta_tools_are_annotated_conservatively() {
        let tools = tools();
        assert_eq!(tools.len(), verbs::mcp_tools().len());
        for tool in &tools {
            assert!(tool.get("name").is_some());
            assert!(tool.get("inputSchema").is_some());
            assert!(
                tool["description"].as_str().unwrap_or_default().len() > 20,
                "描述太短等于没给模型任何领域知识"
            );
        }
        // 参考工具（索引 + 详情）是只读的，而且不伸到外部世界。
        let catalog = tools
            .iter()
            .find(|tool| tool["name"] == verbs::MCP_CATALOG_TOOL)
            .expect("catalog");
        assert_eq!(catalog["annotations"]["readOnlyHint"], true);
        assert_eq!(catalog["annotations"]["openWorldHint"], false);

        // ⚠️ `operation.run` 是**花钱且不幂等**的：幂等只在调用方显式传同一个 `idempotencyKey`
        // 时成立，而不带键的调用每次拿一个新键（`verbs::run_operation` 里生成）⇒ 重试就是
        // 第二次计费。所以这里必须断言 `false`——把它写成 `true`，等于在协议地板上对一条会
        // 花钱的动作说假话，照着 `idempotentHint` 重试的调用方会多付一次钱。
        let run = verbs::mcp_verb_detail(
            verbs::VERBS
                .iter()
                .find(|verb| verb.id == "operation.run")
                .expect("operation.run"),
        );
        assert_eq!(
            run["annotations"]["idempotentHint"], false,
            "operation.run 花钱且不带键时不幂等，不能声称幂等"
        );
        assert_eq!(run["annotations"]["destructiveHint"], false);
        let cancel = verbs::mcp_verb_detail(
            verbs::VERBS
                .iter()
                .find(|verb| verb.id == "operation.cancel")
                .expect("operation.cancel"),
        );
        assert_eq!(cancel["annotations"]["destructiveHint"], true);
    }

    /// **跨动词的规矩住在 `instructions`（只付一次），不在每条工具描述里（付 N 遍）。**
    ///
    /// 判据（§D）：`instructions` 里能读到完整流程、付费警告**与并发上限的数字**；
    /// 且**没有任何一条工具描述**在教"先调哪个工具"。
    ///
    /// 负对照：把 `instructions` 里的流程段删掉 ⇒ 前两条红；
    /// 在任意一条工具描述里写回 "call `renderloft_x` first" ⇒ 末一条红。
    #[test]
    fn instructions_carry_the_cross_verb_rules() {
        let text = instructions();
        // 流程：提交之后用廉价的只读查询跟进（而不是在这条调用里干等）。
        for needle in ["operation.get", "queued"] {
            assert!(
                text.contains(needle),
                "instructions 里读不到流程（缺 {needle}）：{text}"
            );
        }
        // 钱：花的是用户自己的额度，且重投会再花一次。
        assert!(
            text.contains("USER'S OWN platform credits") && text.contains("charged"),
            "instructions 必须说清钱：{text}"
        );
        // 上限的数字必须**真的在里面**（数字来自强执行方，不手抄）。
        assert!(
            text.contains(&MAX_RUNS_PER_OPERATION.to_string()),
            "instructions 里没有单次 runs 上限的数字：{text}"
        );
        assert!(
            text.contains(&MAX_AUTONOMOUS_CONCURRENT_OPERATIONS.to_string()),
            "instructions 里没有自主调用方并发上限的数字：{text}"
        );
        // 别把 `attention` 这件事漏了：`RUNNING` 不等于在动。
        assert!(
            text.contains("attention"),
            "instructions 必须提醒 attention（RUNNING 不等于在动）：{text}"
        );
        // 工具描述里**不许**再教"先调哪个工具"（那正是压平要拆掉的形状）。
        for tool in tools() {
            let description = tool["description"].as_str().unwrap_or_default();
            assert!(
                !description.contains("first") || !description.contains("renderloft_"),
                "{} 的描述里还在教先调别的工具：{description}",
                tool["name"]
            );
        }
    }

    /// **已从清单里撤掉的调用口仍然认得**（弃用窗口），但**不许**再出现在 `tools/list` 里。
    ///
    /// 两件事必须同时成立，少一件都是坏的：
    /// * 只撤不认 ⇒ 缓存过旧清单的调用方突然失败（不礼貌，而且是我们的形状变了）；
    /// * 只认不撤 ⇒ 常驻里永远多一个"什么都能干"的工具，逐动作标注也就白压平了。
    #[test]
    fn the_deprecated_call_tool_is_still_accepted_but_no_longer_listed() {
        assert!(
            !tools()
                .iter()
                .any(|tool| tool["name"] == verbs::MCP_CALL_TOOL),
            "`renderloft_call` 必须从 tools/list 里撤掉（这正是 B 那一步）"
        );
        // 名字仍然路由得到（兼容路径在 `invoke` 里，这里钉住路由本身不报错）。
        let verb = verbs::route_call("operation.get").expect("动词应当仍然可达");
        assert_eq!(verb.mcp_name, Some("renderloft_get_operation"));
    }

    /// **常驻预算**：`tools/list` 序列化后不许超过这个字符数。
    ///
    /// 为什么是一个写死的常数，而不是"看着还行"：压平之后**每一轮都要付这 N 条描述**，
    /// 而"再补一句"是每次改动里最容易做、最难察觉的事。上限写在这里，越线就是红的。
    ///
    /// 口径：`tools/list` 的 JSON 序列化长度（名字 + 描述 + schema + 标注）。
    /// **不含**索引 —— 索引是 `renderloft_catalog` 的返回，按需取，不进常驻。
    ///
    /// ⚠️ 设计文档（`docs/mcp-surface.md` §5.A）写的是"≤8000 字符"，那是**压平前的估算**；
    /// 实测（2026-10-18，压平后）是 **12 534**（本测试量的口径 = `tools/list` 的 JSON 序列化；
    /// `node scripts/mcp-cost.mjs` 另给拆分：描述 4 142 + schema 5 422 + 名字与标注 463）。
    /// 上限按实测留约 5% 余量定在 13 200 —— 它要抓的是"又悄悄长了"，不是一个好看的整数。
    ///
    /// 为什么不再往下压：文档的判据是"**超预算就裁 description，不许动 schema**"，而描述已经被
    /// 压到"只留调用它之前必须知道的事实"（16 条共 3 191 字符，平均 ~200）；再压就只剩同义反复，
    /// 等于把逐动作的信息从常驻地板上撤掉——那正是这次压平要修的东西。
    /// **要真正变小，唯一的杠杆是 schema（= 动词的参数面），那是另一件事。**
    const TOOLS_LIST_BUDGET_CHARS: usize = 13_200;

    /// 工具清单 = **表里开放的那些动词**（每个一个原生工具）＋ 一个按需查阅的参考工具。
    ///
    /// 判据里**不许写死数字**：写死的数字会过期（这份文档自己就漂过两次：13 → 14 → 16）。
    /// 等于"表里 `mcp_name` 非空的行数 + 1"。
    ///
    /// 为什么压平（而不是留两个元工具）：审批层读的是 `tools/list` 里的**逐动作标注**，
    /// 而一个工具只有一格 `annotations`，装 N 个动作时物理上只能报最保守值 ——
    /// 于是"只读的"和"会花钱、不可逆的"在审批层长得一模一样。
    ///
    /// 负对照：把 `mcp_tools` 改回"索引 + 调用口"两个工具 ⇒ 第一条红。
    #[test]
    fn the_listed_tools_are_the_table_s_verbs_plus_the_reference_tool() {
        let served = tools()
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_string())
            .collect::<Vec<_>>();
        let exposed = verbs::VERBS
            .iter()
            .filter_map(|verb| verb.mcp_name)
            .collect::<Vec<_>>();
        assert_eq!(
            served.len(),
            exposed.len() + 1,
            "清单应当恰好是「表里开放的动词」+ 一个参考工具：{served:?}"
        );
        for name in &exposed {
            assert!(served.contains(&name.to_string()), "清单里少了 {name}");
        }
        assert!(
            served.contains(&verbs::MCP_CATALOG_TOOL.to_string()),
            "参考工具（索引/详情）应当保留"
        );
        // 每个动词的工具必须带 schema 与标注（缺了就等于把门控信息又藏起来）。
        for tool in tools() {
            assert!(
                tool.get("inputSchema").is_some(),
                "{} 没有 schema",
                tool["name"]
            );
            assert!(
                tool.get("annotations").is_some(),
                "{} 没有标注",
                tool["name"]
            );
            assert!(
                tool["description"].as_str().unwrap_or_default().len() > 20,
                "{} 的描述太短，等于没给调用方任何信息",
                tool["name"]
            );
        }
        // 索引仍然与表双向一致（有人手写清单就会红）。
        let index = verbs::mcp_catalog();
        for verb_id in verbs::VERBS
            .iter()
            .filter(|verb| verb.mcp_name.is_some())
            .map(|verb| verb.id)
        {
            assert!(index.contains(verb_id), "索引里少了 {verb_id}");
        }
        assert_eq!(
            index.lines().count(),
            exposed.len(),
            "索引行数与表不一致（有人手写了一份清单？）"
        );
    }

    /// **门（gate）**：清单里每个动词的 `annotations` 与表**逐条相等**，风险标记的集合也与表相等。
    ///
    /// 这是压平工具面**唯一**的理由：审批层读的就是这一格。所以"清单里的标注 == 表里的字段"
    /// 必须机器钉住 —— 渲染器与表同源只保证今天一致，保证不了"有人又在清单里手写一条工具"。
    ///
    /// 负对照：把 `mcp_annotations` 换回"按最保守的报"（压平前 `call` 的写法）⇒ 这条红。
    #[test]
    fn every_listed_verb_reports_the_table_s_own_annotations() {
        let listed = tools();
        for verb in verbs::VERBS.iter().filter(|verb| verb.mcp_name.is_some()) {
            let name = verb.mcp_name.expect("mcp_name");
            let tool = listed
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("清单里少了 {name}"));
            assert_eq!(
                tool["annotations"]["readOnlyHint"], verb.read_only,
                "{name} 的 readOnlyHint 与表不一致"
            );
            assert_eq!(
                tool["annotations"]["destructiveHint"], verb.destructive,
                "{name} 的 destructiveHint 与表不一致"
            );
            assert_eq!(
                tool["annotations"]["idempotentHint"], verb.idempotent,
                "{name} 的 idempotentHint 与表不一致"
            );
            // 标记必须在描述**首行**（位置固定、可正则）——§C 的判据。
            let description = tool["description"].as_str().unwrap_or_default();
            assert!(
                description.starts_with(&verbs::mcp_markers(verb)),
                "{name} 的描述没有以风险标记开头（标记只在索引里有 = 门控信息又藏起来了）：{description}"
            );
        }
        // "只读"与"不可逆"必须**能分辨**（一个工具报最保守值时这一条必然废）。
        assert!(
            listed
                .iter()
                .any(|tool| tool["annotations"]["readOnlyHint"] == true),
            "清单里应当有只读动词"
        );
        assert!(
            listed
                .iter()
                .any(|tool| tool["annotations"]["destructiveHint"] == true),
            "清单里应当有不可逆动词"
        );
        // 集合相等：`[COST]` 筛出的那些 == 表里会花钱的那些（逐条对上，不比总数）。
        let marked = listed
            .iter()
            .filter(|tool| {
                tool["description"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with(verbs::MCP_COST_MARKER)
            })
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        let expected = verbs::VERBS
            .iter()
            .filter(|verb| verb.spends_credits)
            .filter_map(|verb| verb.mcp_name)
            .collect::<Vec<_>>();
        for name in &expected {
            assert!(marked.contains(name), "[COST] 集合里少了 {name}");
        }
        assert_eq!(
            marked.len(),
            expected.len(),
            "被标 [COST] 的比表里会花钱的多：{marked:?}"
        );
    }

    /// **常驻预算不许越线**，而且它与应用数量无关。
    ///
    /// 负对照：把任意一条 description 复制三遍 ⇒ 这条红（实测过）。
    #[test]
    fn the_resident_tool_list_stays_within_its_budget() {
        let listed = tools();
        let serialized = serde_json::to_string(&listed).expect("序列化应成功");
        // 失败时要说清**是哪一半长了**（描述还是 schema）——否则下次只能靠猜。
        let descriptions: usize = listed
            .iter()
            .map(|tool| tool["description"].as_str().unwrap_or_default().len())
            .sum();
        let schemas: usize = listed
            .iter()
            .map(|tool| tool["inputSchema"].to_string().len())
            .sum();
        assert!(
            serialized.len() <= TOOLS_LIST_BUDGET_CHARS,
            "常驻工具清单 {} 字符，超过预算 {}（超了要裁描述，不许动 schema）——\
             其中描述 {}、schema {}、其余（名字/标注/JSON 骨架）{}",
            serialized.len(),
            TOOLS_LIST_BUDGET_CHARS,
            descriptions,
            schemas,
            serialized.len() - descriptions - schemas
        );
        assert!(
            serialized.len() > 5_000,
            "清单只有 {} 字符，不像渲染出了每个动词一个工具（是不是漏了？）",
            serialized.len()
        );
    }

    #[test]
    fn error_as_mcp_marks_is_error_with_structured_code() {
        let error = ClientError {
            status: 409,
            code: "IDEMPOTENCY_CONFLICT".to_string(),
            message: "冲突".to_string(),
            retryable: false,
            details: Value::Null,
        };
        let result = error_as_mcp(&error);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "IDEMPOTENCY_CONFLICT"
        );
        assert_eq!(result["structuredContent"]["error"]["status"], 409);
    }

    /// MCP 规范要求 `structuredContent` 是**对象**；数组会被严格客户端整条拒收。
    ///
    /// 实测（2026-10-10，DSH 第一次真调 `renderloft_list_connections`）：
    /// `Invalid result for tools/call: ... expected record, received array` ——
    /// 那个工具在客户端侧完全不可用，而当时的单测全绿（只看 `content` 文本）。
    ///
    /// 负对照：把 `response_as_mcp` 改回 `"structuredContent": data` ⇒ 第一条断言红。
    #[test]
    fn array_payloads_are_wrapped_so_strict_clients_accept_them() {
        let wrapped = response_as_mcp(json!(["a", "b"]));
        assert!(
            wrapped["structuredContent"].is_object(),
            "structuredContent 必须是对象，数组会被严格客户端拒收：{wrapped}"
        );
        assert_eq!(wrapped["structuredContent"]["items"], json!(["a", "b"]));
        assert_eq!(
            wrapped["content"][0]["text"], r#"{"items":["a","b"]}"#,
            "文本与结构化两边形状要一致"
        );

        // 对象原样透传，不要多包一层
        let passthrough = response_as_mcp(json!({ "items": [], "hasMore": false }));
        assert_eq!(
            passthrough["structuredContent"],
            json!({ "items": [], "hasMore": false })
        );
        assert_eq!(
            passthrough["content"][0]["text"], r#"{"hasMore":false,"items":[]}"#,
            "对象不该被包成 {{\"items\": {{...}}}}"
        );
    }

    #[test]
    fn handle_request_responds_to_initialize() {
        let response = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .unwrap();
        assert_eq!(response["id"], 1);
        assert!(response["result"]["capabilities"]["tools"].is_object());
        // **身份必须能被看见**：元层换掉了自带领域暗示的工具名，所以"这是干嘛的"要有地方说。
        // 两条通道都要有：`instructions`（协议字段，客户端可转达）与工具描述（保证被看到）。
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("initialize 必须带 instructions");
        assert!(
            instructions.contains("RenderLoft") && instructions.contains("image"),
            "instructions 要说清这是什么：{instructions}"
        );
    }

    /// 2026-07-28 起的三条硬要求：**每个结果带 `resultType`**、列表结果带**缓存提示**、
    /// 版本走每次请求的 `_meta` 且不认识的版本要回 `-32022`（并列出支持的版本）。
    ///
    /// 同时钉住 **dual-era**：不带 `_meta` 的老请求照旧能用（legacy 握手），
    /// 否则我们会在升级规范的同时把还在用老协议的客户端一脚踢开。
    #[test]
    fn modern_protocol_requirements_are_met_without_breaking_legacy() {
        // ① 现代请求：版本放 _meta，结果必须有 resultType
        let modern = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } }
        }))
        .unwrap();
        assert_eq!(modern["result"]["resultType"], "complete");
        assert_eq!(modern["result"]["cacheScope"], "public");
        assert!(
            modern["result"]["ttlMs"].as_u64().unwrap_or(0) > 0,
            "列表结果要带 ttlMs 缓存提示"
        );

        // ② 不认识的版本：-32022，且必须**列出我们支持的版本**让客户端重挑
        let unknown = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "1900-01-01" } }
        }))
        .unwrap();
        assert_eq!(unknown["error"]["code"], -32022);
        let supported = unknown["error"]["data"]["supported"].as_array().unwrap();
        assert!(
            supported.iter().any(|v| v == "2026-07-28"),
            "要自报支持的版本：{supported:?}"
        );
        assert_eq!(unknown["error"]["data"]["requested"], "1900-01-01");

        // ③ legacy（没有 _meta）照旧：握手可用、结果也有 resultType（老客户端会忽略它）
        let legacy = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "initialize",
            "params": { "protocolVersion": "2025-03-26" }
        }))
        .unwrap();
        assert_eq!(legacy["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(legacy["result"]["resultType"], "complete");

        // ④ `server/discover`（规范 MUST）：一次问清版本/能力/身份。
        //    身份放 `_meta` 里（不是顶层 `serverInfo`）——按官方 DiscoverResult 抄的字段名。
        let discover = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "server/discover",
            "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } }
        }))
        .unwrap();
        assert_eq!(discover["result"]["resultType"], "complete");
        let versions = discover["result"]["supportedVersions"].as_array().unwrap();
        assert!(
            versions.iter().any(|version| version == "2026-07-28")
                && versions.iter().any(|version| version == "2025-03-26"),
            "要自报两个版本（dual-era）：{versions:?}"
        );
        assert_eq!(
            discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "renderloft"
        );
        assert!(discover["result"]["capabilities"]["tools"].is_object());
        assert_eq!(discover["result"]["cacheScope"], "public");
    }

    /// **"这是什么"必须常驻**：模型看到的常驻文本里如果不说它是干嘛的，能力就等于不存在。
    ///
    /// 负对照：把 `MCP_IDENTITY` 从 catalog 的描述里去掉 ⇒ 这条红。
    #[test]
    fn the_always_on_text_says_what_this_is() {
        let tools = tools();
        let catalog = tools
            .iter()
            .find(|tool| tool["name"] == verbs::MCP_CATALOG_TOOL)
            .expect("catalog");
        let description = catalog["description"].as_str().expect("description");
        assert!(
            description.contains("RenderLoft") && description.contains("Use it when"),
            "常驻描述必须同时说清「这是什么」与「什么时候该用它」：{description}"
        );
        assert!(
            description.contains("image"),
            "要说清领域（图/视频），否则模型不知道这跟画图有关：{description}"
        );
        // 身份句必须**常驻**（模型没有跨轮记忆）：它现在有两个落点，两个都要在——
        // `initialize.instructions`（协议专门放"这个 server 是干嘛的"字段）与参考工具的描述
        // （模型只看 tools/list 时也能读到）。
        let always_on = instructions();
        assert!(
            always_on.contains("RenderLoft") && always_on.contains("Use it when"),
            "instructions 必须同时说清「这是什么」与「什么时候该用它」：{always_on}"
        );
        assert!(
            always_on.contains("image"),
            "要说清领域（图/视频），否则模型不知道这跟画图有关：{always_on}"
        );
    }

    /// **有界回执**：列表太长就截断，而且**必须说实话**（渐进式披露的输出侧那一档）。
    ///
    /// 悄悄截断比不截断更坏：模型会以为"我看到的就是全部"，于是漏掉剩下的还理直气壮。
    /// 负对照：把 `bound_result` 里的截断整段删掉 ⇒ 前三条断言红。
    #[test]
    fn a_long_list_is_truncated_with_an_honest_marker() {
        let long: Vec<Value> = (1..=300).map(|id| json!({ "artifactId": id })).collect();
        let bounded = bound_result(json!({ "items": long, "nextCursor": 1, "hasMore": true }));
        assert_eq!(bounded["truncated"], true);
        assert_eq!(bounded["returnedItems"], MCP_RESULT_MAX_ITEMS);
        assert_eq!(bounded["totalItems"], 300);
        assert_eq!(
            bounded["items"].as_array().unwrap().len(),
            MCP_RESULT_MAX_ITEMS
        );
        assert!(
            bounded["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("after"),
            "必须告诉模型怎么拿更多：{bounded}"
        );
        // ⚠️ `nextCursor` 指向的是 daemon 那一页的末尾；截断之后继续用它会把中间那些**跳过**。
        assert!(
            bounded.get("nextCursor").is_none(),
            "截断之后不能留着会跳页的游标：{bounded}"
        );

        // 不长的列表**原样通过**（不加任何标记，免得模型以为总有问题）。
        let short = bound_result(json!({ "items": [1, 2, 3], "hasMore": false }));
        assert_eq!(short["items"].as_array().unwrap().len(), 3);
        assert!(short.get("truncated").is_none());
    }

    /// 单个对象也可能很大（一次 100 个 run 的任务详情）⇒ 字节兜底：**不内联**，只说清多大、
    /// 怎么办。判据同样是"说实话"。
    #[test]
    fn an_oversized_single_object_is_not_inlined() {
        let huge = json!({ "runs": vec!["x".repeat(1000); 100] });
        let bounded = bound_result(huge);
        assert_eq!(bounded["truncated"], true);
        assert!(
            bounded["bytes"].as_u64().unwrap_or(0) > MCP_RESULT_MAX_BYTES as u64,
            "要报出实际多大：{bounded}"
        );
        assert!(bounded["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("limit"));
        assert!(bounded.get("runs").is_none(), "太大的东西不内联");
    }

    #[test]
    fn handle_request_lists_tools() {
        let response = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }))
        .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        // 数量**钉死**：形状是刻意的（渐进式披露），改它必须是自觉的决定，不能是漂移。
        // 工具数按**表**算，不写死数字（写死的会过期：13 → 14 → 16 已经漂过三次）。
        let expected = verbs::VERBS
            .iter()
            .filter(|verb| verb.mcp_name.is_some())
            .count()
            + 1; // + 参考工具
        assert_eq!(
            tools.len(),
            expected,
            "对外是「表里开放的动词」+ 一个参考工具"
        );
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default())
            .collect();
        assert!(names.contains(&verbs::MCP_CATALOG_TOOL));
        assert!(
            !names.contains(&verbs::MCP_CALL_TOOL),
            "旧调用口已经从清单里撤掉（仍然认得，但不再列出）"
        );
        assert!(
            names.contains(&"renderloft_run_recipe"),
            "每个动词一个原生工具"
        );
    }

    /// **工具列表是本地常量**：与表逐字相等，且**不含任何 per-app 工具**。
    ///
    /// ⚠️ 判据的强度取决于环境：daemon 在跑时（开发机就是），旧实现会在这里多出
    /// N 个 `renderloft_run_{id}` ⇒ 相等断言必然失败；daemon 没跑时两边都只有静态工具，
    /// 所以这条还额外断言"名字里没有 per-app 形态"——那一条与环境无关。
    #[test]
    fn tools_list_is_a_local_constant_without_per_app_tools() {
        let response = handle_request(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list"
        }))
        .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(
            Value::Array(tools.clone()),
            Value::Array(verbs::mcp_tools()),
            "tools/list 必须**逐字**等于动词表渲染出来的那份（不随应用目录变形）"
        );
        for tool in tools {
            let name = tool["name"].as_str().unwrap_or_default();
            assert!(
                !name
                    .strip_prefix(verbs::DYNAMIC_TOOL_PREFIX)
                    .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())),
                "per-app 工具（{name}）不该再出现在工具列表里：它让 tools/list 变成 N+1 次 \
                 HTTP，而且列表会随应用增减变形"
            );
        }
    }

    /// 文本里的工具名（`renderloft_*`）：只吃 `[a-z_]` 那一段，不引正则依赖。
    /// 文本里出现的**动词 id**（`operation.get` / `recipe.list-workflow` 这种）。
    ///
    /// 为什么压平之后需要它：跨动词的流程搬进了 `instructions`，而那些句子是**手写的散文**，
    /// 里面写错一个动词 id（`operation.gett`）没有任何编译期检查 —— 调用方照着做就是"查一个
    /// 不存在的动词"。所以按"表里有的 id 全扫一遍"来判：**扫到的必须都在表里**。
    fn verb_ids_in(text: &str) -> Vec<String> {
        let table_ids = verbs::VERBS.iter().map(|verb| verb.id).collect::<Vec<_>>();
        let mut found = Vec::new();
        for id in table_ids {
            if text.contains(id) {
                found.push(id.to_string());
            }
        }
        // 表里没有的"像动词 id 的东西"也要能被抓住：按 `group.action` 的形状扫一遍，
        // 再看它在不在表里（否则拼错的那个永远扫不到 —— 上面那圈只认表里已有的）。
        let bytes = text.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            let is_start = bytes[index].is_ascii_lowercase()
                && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric());
            if is_start {
                let mut end = index;
                while end < bytes.len()
                    && (bytes[end].is_ascii_lowercase()
                        || bytes[end] == b'.'
                        || bytes[end] == b'-'
                        || bytes[end].is_ascii_digit())
                {
                    end += 1;
                }
                let candidate = &text[index..end];
                if candidate.contains('.')
                    && !candidate.ends_with('.')
                    && candidate.split('.').count() == 2
                {
                    let (group, action) = candidate.split_once('.').expect("split");
                    if !group.is_empty()
                        && !action.is_empty()
                        && !found.iter().any(|id| id == candidate)
                    {
                        found.push(candidate.to_string());
                    }
                }
                index = end.max(index + 1);
            } else {
                index += 1;
            }
        }
        found
    }

    fn tool_names_in(text: &str) -> Vec<String> {
        const PREFIX: &str = "renderloft_";
        let mut names = Vec::new();
        let mut cursor = 0usize;
        while let Some(offset) = text[cursor..].find(PREFIX) {
            let start = cursor + offset;
            let mut end = start + PREFIX.len();
            for (index, character) in text[end..].char_indices() {
                if character.is_ascii_lowercase() || character == '_' {
                    end = start + PREFIX.len() + index + character.len_utf8();
                } else {
                    break;
                }
            }
            names.push(text[start..end].to_string());
            cursor = end;
        }
        names.sort();
        names.dedup();
        names
    }

    /// **已经在强制的边界必须出现在协议地板上**：这些上限在代码里早就存在，缺的只是
    /// "调用方看得见" —— 看不见的边界只能靠撞墙发现。
    ///
    /// 为什么这条值得一条测试：这些上限**在代码里早就存在**，缺的只是"调用方看得见"。
    /// 看不见的边界只能靠撞墙发现 —— 撞出来的那一轮会被误判成故障，而在付费 server 上
    /// 撞墙还可能顺手花掉钱。
    ///
    /// 负对照：把 `instructions()` 里那段 Limits 删掉 ⇒ 前四条红；
    /// 把 `runs` 参数的 `max` 从表里删掉 ⇒ 最后一条红（schema 里就没有 `maxItems` 了）。
    #[test]
    fn the_enforced_limits_are_on_the_protocol_floor() {
        let text = instructions();
        for (needle, what) in [
            (
                format!("at most {MAX_RUNS_PER_OPERATION} runs"),
                "单次 Run 上限",
            ),
            (
                format!("at most {MAX_AUTONOMOUS_CONCURRENT_OPERATIONS} operations"),
                "自主调用方并发上限",
            ),
            (format!("{MCP_RESULT_MAX_ITEMS} items"), "列表截断条数"),
            (
                format!("{} KB", MCP_RESULT_MAX_BYTES / 1024),
                "列表截断字节",
            ),
        ] {
            assert!(
                text.contains(&needle),
                "instructions 里读不到{what}（找不到 `{needle}`）：\n{text}"
            );
        }

        // schema 那一侧：`runs` 的上限必须**能机械校验**，且与强执行方同值
        //（表与代码没有编译期联系，所以只能在这里钉）。
        let run = verbs::VERBS
            .iter()
            .find(|verb| verb.id == "operation.run")
            .expect("operation.run");
        assert_eq!(
            verbs::mcp_verb_detail(run)["inputSchema"]["properties"]["runs"]["maxItems"],
            json!(MAX_RUNS_PER_OPERATION),
            "`runs` 的上限必须在 schema 里（`maxItems`），不能只在散文里说 up to 100"
        );
    }

    /// **不可见 = 不存在**的可执行形式：被服务的文本里点名的东西，必须真的够得到。
    ///
    /// 压平之后"被服务的文本"多了两处，判据也跟着扩：
    /// * `tools/list` 的每条描述 + `instructions`：里面出现的 `renderloft_*` 名字必须**在清单里**
    ///   （压平后每条动词描述都**不再点名别的工具**，所以这一半现在是"零命中即绿"——
    ///   它拦的是"有人又把跨动词指示写回某条描述里"）；
    /// * 同一份文本里出现的**动词 id**（`operation.get` 这种）必须在**表里真实存在** ——
    ///   这是压平后新增的风险面：流程写进了 `instructions`，写错一个动词 id 就没有任何编译期检查；
    /// * `catalog(<verb>)` 的**详情**（模型真正读的那一份）：里面的工具名必须是表里存在的名字。
    ///
    /// 负对照：① 把 `instructions` 里的 `operation.get` 改成 `operation.gett` ⇒ 第二条红；
    /// ② 在任意一条工具描述里写一句 "call `renderloft_typo` first" ⇒ 第一条红。
    #[test]
    fn every_tool_named_in_served_text_is_listed() {
        let served = tools()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        let text = tools()
            .iter()
            .map(|tool| tool["description"].as_str().unwrap_or_default().to_string())
            .chain(std::iter::once(instructions()))
            .collect::<Vec<_>>()
            .join("\n");
        for name in tool_names_in(&text) {
            assert!(
                served.contains(&name),
                "被服务的文本里点名了 `{name}`，但它不在 tools/list 里（清单：{served:?}）—— \
                 守规矩的调用方只会调被列出的工具，于是这一步要么被跳过、要么让人困惑"
            );
        }

        // 动词 id：文本里提到的必须在表里存在（拼错一个字母 = 教调用方去调一个不存在的东西）。
        let table_ids = verbs::VERBS.iter().map(|verb| verb.id).collect::<Vec<_>>();
        let mentioned = verb_ids_in(&text);
        assert!(
            mentioned.contains(&"operation.get".to_string()),
            "一个动词 id 都没扫到 —— 这条测试在空转（instructions 里应当提到主路径）"
        );
        for id in &mentioned {
            assert!(
                table_ids.contains(&id.as_str()),
                "被服务的文本里提到了动词 `{id}`，而表里没有这个 id（表里现有：{table_ids:?}）"
            );
        }

        let table_names = verbs::VERBS
            .iter()
            .filter_map(|verb| verb.mcp_name)
            .collect::<Vec<_>>();
        for verb in verbs::VERBS.iter().filter(|verb| verb.mcp_name.is_some()) {
            let detail = verbs::mcp_verb_detail(verb).to_string();
            for name in tool_names_in(&detail) {
                assert!(
                    table_names.contains(&name.as_str()),
                    "{} 的详情点名了 `{name}`，而表里没有这个名字：{detail}",
                    verb.id
                );
            }
        }
    }

    /// 补「起了对话却查不到」这条**断头路**。
    ///
    /// 判别点四条：
    /// 1. 三个工具都在列表里，`deliveryId` 必填（模型不该被允许不带 id 调用）；
    /// `renderloft_upload_asset` 必须**真读本机文件**，读不到时给清楚的错误码。
    ///
    /// 判别点三条：
    /// 1. 工具在列表里，且 `path` 是必填（模型不该被允许"不带路径"调用）；
    /// 2. 路径不存在 ⇒ `ASSET_READ_FAILED`，而**不是**笼统的 UNKNOWN_TOOL / 500
    ///    ——外界 Agent 要靠错误码判断"是我路径写错了"还是"服务出问题了"；
    /// 3. 缺 `path` ⇒ `ARGUMENT_REQUIRED`（与 2 分开，两条都是可执行的下一步）。
    ///
    /// ⚠️ 为什么需要这个工具：没有它，**图生图 / 需要参考图的一整类应用从 MCP 完全够不着**
    /// （实测用户的应用「二次元-anima-换装-洗图」第一个字段就是 `17::image`）。
    #[tokio::test]
    async fn upload_asset_reads_a_local_file_and_reports_clear_errors() {
        // 1：详情里 `path` 必填（模型不该被允许不带路径调用）。
        let upload = verbs::mcp_verb_detail(
            verbs::VERBS
                .iter()
                .find(|verb| verb.id == "asset.upload")
                .expect("asset.upload"),
        );
        assert_eq!(
            upload["inputSchema"]["required"],
            json!(["path"]),
            "path 必填——模型不该被允许不带路径调用"
        );

        // 2：路径不存在 ⇒ 明确的 ASSET_READ_FAILED（不消耗任何网络请求）。
        // 直接调 `invoke`（`call_tool` 会先连 daemon，而这条路径在连之前就该失败）。
        let client = offline_client();
        let missing = std::env::temp_dir().join("renderloft-definitely-missing-asset.png");
        let error = invoke(
            &client,
            "renderloft_upload_asset",
            &json!({ "path": missing.to_string_lossy() }),
        )
        .await
        .expect_err("不存在的路径应当失败");
        assert_eq!(error.code, "ASSET_READ_FAILED");
        assert!(
            error.message.contains("无法读取文件"),
            "要说清是读文件失败：{}",
            error.message
        );

        // 3：缺 path ⇒ 必填错误（与 2 分开，两条都是可执行的下一步）。
        let error = invoke(&client, "renderloft_upload_asset", &json!({}))
            .await
            .expect_err("缺 path 应当失败");
        assert_eq!(error.code, "ARGUMENT_REQUIRED");
        assert!(error.message.contains("path"), "{}", error.message);
    }

    /// 未知工具名仍然是**明确的** UNKNOWN_TOOL（而不是 500 或空结果）。
    #[tokio::test]
    async fn unknown_tool_is_reported_by_name() {
        let client = offline_client();
        let error = invoke(&client, "renderloft_nope", &json!({}))
            .await
            .expect_err("未知工具应当失败");
        assert_eq!(error.code, "UNKNOWN_TOOL");
        assert!(
            error.message.contains("renderloft_nope"),
            "{}",
            error.message
        );
    }
}
