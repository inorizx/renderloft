//! 让 CLI / MCP 自己把 daemon 拉起来：**启动，但不拥有**。
//!
//! ## 为什么需要
//!
//! daemon 是常驻的那一个（调度、对账、回调投递、工作区），CLI 与 MCP 都只是它的瘦客户端。
//! 但客户端比 daemon 先起来的场景很常见：MCP 会话是客户端（如 DSH）按需拉起的，可能早于
//! 任何人打开界面；CLI 更常见于脚本与任务计划里。此前两者都只是"读端点文件 → 连 HTTP"，
//! daemon 不在就报 `CLIENT_NETWORK_ERROR 无法连接 daemon` —— 一个**本可以自愈**的死局。
//!
//! ## 边界（这些是刻意的，不是没做）
//!
//! - **启动，但不拥有**：spawn 完就撒手，不 `wait`、不持有句柄、不负责关它。daemon 的生死
//!   由"有没有活任务"决定，不由某个客户端会话决定——否则 MCP 会话一结束，正在平台侧跑的
//!   任务就没人对账了（这正是"后台"必须挂在 daemon 而不是 MCP 上的原因）。
//! - **只在健康检查失败时才启动**，所以正常路径上不会重复启动；真撞上并发启动，daemon 自己
//!   的端口单例会让后一个实例退出，两边都还能用。
//! - **只找"自己旁边"的那个可执行文件**：不搜 PATH、不搜全盘。安装布局里 daemon / CLI / MCP
//!   同目录，找不到就是安装不完整，报错比乱找强。
//! - **不改环境**：不替用户清 `HTTP_PROXY` 之类（那是用户的决定），只原样继承。
//!   ⚠️ 因此"谁启动它，它就继承谁的环境"——从带死代理的终端启动，它也会连不上平台。
//! - **输出必须落到文件**，绝不接管道：实测过（见 `v2::logging`），给 daemon 一个会断的管道
//!   能把对账循环写死，任务永远停在 `ACCEPTED`。
//!
//! 桌面外壳里那套"启动 + 等端口"是同一件事的另一份实现（它在另一个 crate 里，没法直接复用），
//! 两边都以 `v2_data_dir()` 为准。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// daemon 的可执行文件名。与桌面外壳里的那处一致（安装布局里同目录）。
pub const DAEMON_BINARY: &str = if cfg!(windows) {
    "renderloft.exe"
} else {
    "renderloft"
};

/// daemon 的默认监听地址，与 `daemon.rs` 里 `RENDERLOFT_BIND` 的默认值一致。
pub const DEFAULT_BIND: &str = "127.0.0.1:31337";

/// 探测超时。
///
/// ⚠️ 为什么是 150ms 而不是 700ms（实测，2026-10-10）：这台机器上对**没人听的端口**发 SYN
/// 会被**丢掉**，不是立刻拒绝——`TcpClient.ConnectAsync` 实测要 **2000ms** 才返回。
/// 于是"探一次"会等满整个超时，而拉起 daemon 之前要探一次、等待循环里还要再探一次
/// ⇒ 冷调用实测 **2052ms**，其中 **1515ms 全花在探测超时上**（daemon 自己 240ms 就绪）。
/// 回环上活着的 daemon 应答是亚毫秒级的，150ms 绰绰有余。
const PROBE_TIMEOUT: Duration = Duration::from_millis(150);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
/// 等待循环的探测间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(150);
/// 刚 spawn 完先等这么久再探第一次。
///
/// 为什么：立刻探必然失败，而失败一次要等满 `PROBE_TIMEOUT`（见上）。实测 daemon 约 240ms
/// 就绪，所以先睡一小段比"探一次白等 150ms"划算。
const FIRST_PROBE_DELAY: Duration = Duration::from_millis(120);

#[derive(Debug, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// 已经在跑，什么都没做。
    AlreadyRunning,
    /// 我们刚把它拉起来的。
    Started,
}

/// 确保 daemon 在跑：不在就拉起来，然后等它健康。
///
/// 调用方可以忽略返回值——它只是给日志/诊断用的；真正的失败让随后的 API 调用去报。
pub fn ensure_running() -> Result<EnsureOutcome, String> {
    let log_hint = daemon_log_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "（日志路径不可用）".to_string());
    ensure_with(health_ok, spawn_daemon, wait_until_healthy, &log_hint)
}

/// 决策部分单独拎出来，好让测试用假的"健康检查 / 启动 / 等待"把每种分支都走一遍。
fn ensure_with<H, S, W>(
    health: H,
    spawn: S,
    wait: W,
    log_hint: &str,
) -> Result<EnsureOutcome, String>
where
    H: Fn() -> bool,
    S: Fn() -> Result<(), String>,
    W: Fn() -> bool,
{
    if health() {
        return Ok(EnsureOutcome::AlreadyRunning);
    }
    spawn()?;
    if wait() {
        return Ok(EnsureOutcome::Started);
    }
    Err(format!(
        "已尝试启动 daemon，但 {} 秒内健康检查仍未通过。日志：{log_hint}",
        STARTUP_TIMEOUT.as_secs()
    ))
}

/// 调用失败时，如果"自动启动 daemon"也失败了，就把原因接在错误后面。
///
/// 为什么不干脆在启动失败时直接返回错误：健康检查偶发失败、而 daemon 其实活着时，
/// 直接返回就**制造了一个假失败**。所以策略是"**调用照常走，失败才解释**"。
///
/// 为什么还要挑错误类型：只有**网络类**（`status == 0`）才可能是"连不上 daemon"。
/// 一个 404 / 409 是业务错误，把它归咎于"daemon 没起来"会误导排查方向。
pub fn explain_failure(
    error: crate::v2::clients::client::ClientError,
    boot: &Result<EnsureOutcome, String>,
) -> crate::v2::clients::client::ClientError {
    let Err(reason) = boot else {
        return error;
    };
    if error.status != 0 {
        return error;
    }
    crate::v2::clients::client::ClientError {
        message: format!("{}（自动启动 daemon 也失败了：{reason}）", error.message),
        ..error
    }
}

/// daemon 应该在哪：**本程序旁边**（安装布局里它们同目录）。
pub fn daemon_binary_path(current_exe: &Path) -> PathBuf {
    current_exe
        .parent()
        .map(|dir| dir.join(DAEMON_BINARY))
        .unwrap_or_else(|| PathBuf::from(DAEMON_BINARY))
}

/// daemon 的日志文件（与桌面外壳同一个）：`<工作区>/daemon.log`。
pub fn daemon_log_path() -> Result<PathBuf, String> {
    Ok(crate::v2::daemon::v2_data_dir()?.join("daemon.log"))
}

/// 日志轮转阈值：超过它就把当前那份挪成 `daemon.log.1`。
///
/// ⚠️ **与桌面外壳同值**（`desktop/src/main.rs` 的 `daemon_log_file`）。两份实现都在
/// （外壳在另一个 crate 里，见模块头），所以这个数字由 `scripts/check-layers.mjs` 跨文件守门
/// ——**别只改一处**：改了这边，从外壳启动的 daemon 与从 CLI / MCP 启动的 daemon 就会用两个
/// 阈值，而"日志多长"取决于谁先把它拉起来，没人能预期。
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// 打开 daemon 日志：超过阈值先把当前那份挪成 `daemon.log.1`（**保留上一份**——崩溃现场
/// 往往正好在被轮转掉的那份里），再追加。
///
/// 为什么必须有它（实测）：这条路径此前**只 append、从不轮转**，于是"谁拉起 daemon"决定了
/// 日志会不会无限长——外壳拉起的那份到 5 MB 就轮转，CLI / MCP 拉起的那份永远长下去。
/// 这不是磁盘空间问题：日志是排查的唯一线索，而一份几百 MB 的文件连打开都费劲。
/// 而 CLI / MCP 恰好是最常拉起 daemon 的那两条路（MCP 会话、任务计划）。
///
/// 轮转失败**不影响启动**（挪不动就继续往旧文件里追加）：宁可日志长一点，也不能因为
/// "日志挪不动"让 daemon 起不来。
fn open_daemon_log(path: &Path) -> Result<std::fs::File, String> {
    if std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) > LOG_ROTATE_BYTES {
        // `daemon.log` → `daemon.log.1`（`with_extension` 替换最后一个点之后的部分）。
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("无法打开日志 {}：{error}", path.display()))
}

/// daemon 现在在跑吗——**只回答，不做任何启动动作**。
///
/// `daemon status` 用它：那个动词必须在 daemon 没跑时也能回答"没在跑"。
pub fn is_running() -> bool {
    health_ok()
}

/// 端点文件里的 `(endpoint, version)`。
///
/// ⚠️ 文件可能是**上一次**留下的（daemon 崩了不会删它），所以这个结果**必须**与
/// `is_running()` 一起用——单看它会把"已经死了但残留着端点文件"读成"在跑"。
pub fn endpoint_info() -> Option<(String, String)> {
    let path = crate::v2::daemon::v2_data_dir()
        .ok()?
        .join("daemon-endpoint.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let endpoint = value.get("endpoint")?.as_str()?.to_string();
    let version = value
        .get("version")
        .and_then(|version| version.as_str())
        .unwrap_or("unknown")
        .to_string();
    Some((endpoint, version))
}

/// daemon 活着吗：往 `/health` 打一发最小 HTTP 请求。
///
/// 为什么手写而不是用 reqwest：调用方既有同步的（MCP 的 stdio 主循环），也有异步的（CLI），
/// 而 reqwest 的 blocking 客户端在 async 上下文里会 panic。这里只要"回环 + 一个 GET"，
/// 不值得为它引两套客户端。`/health` 是唯一不需要鉴权的路由（见 `daemon.rs` 的路由表）。
fn health_ok() -> bool {
    probe_health().unwrap_or(false)
}

fn probe_health() -> Option<bool> {
    let bind = bind_candidates();
    for candidate in bind {
        let Ok(mut stream) = TcpStream::connect_timeout(&candidate, PROBE_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
        let request = format!(
            "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            candidate.ip()
        );
        if stream.write_all(request.as_bytes()).is_err() {
            continue;
        }
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        return Some(
            response.starts_with("HTTP/1.1 200")
                || response.starts_with("HTTP/1.0 200")
                || response.contains("\"ok\":true"),
        );
    }
    None
}

/// 该往哪儿探：端点文件里写的地址优先（daemon 上次报的），否则 `RENDERLOFT_BIND`，否则默认。
fn bind_candidates() -> Vec<std::net::SocketAddr> {
    let mut raw = Vec::new();
    if let Ok(endpoint) = read_endpoint_endpoint() {
        raw.push(endpoint);
    }
    if let Ok(bind) = std::env::var("RENDERLOFT_BIND") {
        raw.push(format!("http://{bind}"));
    }
    raw.push(format!("http://{DEFAULT_BIND}"));
    raw.into_iter().filter_map(|url| parse_bind(&url)).collect()
}

/// 从 `http://127.0.0.1:31337` 这样的地址里取出可连接的 socket 地址。
fn parse_bind(url: &str) -> Option<std::net::SocketAddr> {
    let host_port = url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    host_port.to_socket_addrs().ok()?.next()
}

/// 读端点文件里的 `endpoint` 字段；文件不存在或读不动都当作"不知道"。
fn read_endpoint_endpoint() -> Result<String, String> {
    let path = crate::v2::daemon::v2_data_dir()?.join("daemon-endpoint.json");
    let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|error| error.to_string())?;
    value
        .get("endpoint")
        .and_then(|endpoint| endpoint.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "端点文件里没有 endpoint 字段".to_string())
}

fn wait_until_healthy() -> bool {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    // 先给 daemon 一点启动时间再探第一次（理由见 `FIRST_PROBE_DELAY`）。
    std::thread::sleep(FIRST_PROBE_DELAY);
    while Instant::now() < deadline {
        if health_ok() {
            return true;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    health_ok()
}

/// 启动 daemon：**脱离、输出落文件、不持有**。
fn spawn_daemon() -> Result<(), String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let daemon = daemon_binary_path(&current);
    if !daemon.is_file() {
        return Err(format!(
            "找不到 daemon 可执行文件 {}（它应当与本程序同目录；请手动启动 RenderLoft 或检查安装是否完整）",
            daemon.display()
        ));
    }
    let log = daemon_log_path()?;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let stdout = open_daemon_log(&log)?;
    let stderr = stdout
        .try_clone()
        .map_err(|error| format!("无法复制日志句柄：{error}"))?;

    let mut command = Command::new(&daemon);
    // 输出必须是**文件**：给管道的话，调用方一结束管道就断，对账循环会被日志写死。
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：MCP 没有控制台，别凭空弹一个黑框。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    spawn_detached(&mut command)
        .map_err(|error| format!("启动 {} 失败：{error}", daemon.display()))?;
    // 到这里就撒手：不 wait、不持有句柄（见模块头的"启动，但不拥有"）。
    Ok(())
}

/// 起 daemon：**先把本进程标准句柄的"可继承"位关掉**，spawn 完再恢复。
///
/// ⚠️ 为什么必须这么做（实测，不是理论）：Windows 上 `Command::spawn` 以
/// `bInheritHandles = TRUE` 创建进程，于是**本进程的标准句柄会被子进程一并继承**——
/// 哪怕我们已经给子进程指定了别的 stdout/stderr。后果是"自动拉起 daemon"这个功能会
/// **毒掉调用方的管道**：
///
/// - CLI 被脚本用管道调用时（`renderloft-cli … | Out-Null`、`$x = renderloft-cli …`、
///   `… | jq`），它拉起的 daemon 会攥着那条管道不放，脚本要等到 **daemon 退出**才返回。
///   实测：`renderloft-cli connection list | Out-Null` 挂死十分钟以上，而 CLI 进程早就退了；
///   把 daemon 杀掉，挂着的 shell **立刻**结束——这就是判据。
/// - MCP 那条路更隐蔽：daemon 会攥住 MCP 客户端的 stdio 管道。
///
/// `cmd /c start /b` 也躲不过（实测同样泄，管道等后台进程活完才返回），所以只能自己关。
/// 这个仓库此前没有 `unsafe`；这里两处调用是 Win32 里最稳的两个，且**只动继承位、不动别的**，
/// 失败也不影响 spawn 本身（返回值刻意忽略）。
#[cfg(windows)]
fn spawn_detached(command: &mut Command) -> std::io::Result<std::process::Child> {
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    let handles = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .filter_map(|kind| {
            let handle = unsafe { GetStdHandle(kind) };
            // INVALID_HANDLE_VALUE(-1) 与 NULL 都不是可用句柄。
            (!handle.is_null() && handle as isize != -1).then_some(handle)
        })
        .collect::<Vec<_>>();
    for handle in &handles {
        unsafe { SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, 0) };
    }
    let spawned = command.spawn();
    for handle in &handles {
        unsafe { SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    spawned
}

#[cfg(not(windows))]
fn spawn_detached(command: &mut Command) -> std::io::Result<std::process::Child> {
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// 负对照：把 `ensure_with` 里的 `if health() { return Ok(AlreadyRunning) }` 删掉
    /// ⇒ 这条会红（已经在跑的时候就不该再启动一个）。
    #[test]
    fn a_healthy_daemon_is_never_started_again() {
        let spawned = Cell::new(0);
        let outcome = ensure_with(
            || true,
            || {
                spawned.set(spawned.get() + 1);
                Ok(())
            },
            || true,
            "log",
        )
        .expect("已经在跑就该成功");
        assert_eq!(outcome, EnsureOutcome::AlreadyRunning);
        assert_eq!(spawned.get(), 0, "健康检查通过时**一次都不该**启动");
    }

    #[test]
    fn a_missing_daemon_is_started_once_and_waited_for() {
        let spawned = Cell::new(0);
        let waited = Cell::new(0);
        let outcome = ensure_with(
            || false,
            || {
                spawned.set(spawned.get() + 1);
                Ok(())
            },
            || {
                waited.set(waited.get() + 1);
                true
            },
            "log",
        )
        .expect("启动并等到健康就该成功");
        assert_eq!(outcome, EnsureOutcome::Started);
        assert_eq!(spawned.get(), 1);
        assert_eq!(waited.get(), 1);
    }

    /// 负对照：把 `spawn()?` 改成 `let _ = spawn();` ⇒ 这条会红
    /// （启动失败必须如实报出来，不能假装成功然后让用户去看一个看不懂的网络错误）。
    #[test]
    fn a_failed_spawn_is_reported_verbatim() {
        let error = ensure_with(
            || false,
            || Err("找不到 daemon 可执行文件".to_string()),
            || true,
            "log",
        )
        .expect_err("启动失败必须报错");
        assert!(error.contains("找不到 daemon"), "实际：{error}");
    }

    /// 起来了但一直不健康（例如端口被别的程序占着）：必须报错，并**指向日志**——
    /// 否则用户只看到一个超时，不知道该去哪儿看原因。
    #[test]
    fn a_timeout_points_at_the_daemon_log() {
        let error = ensure_with(|| false, || Ok(()), || false, r"C:\x\daemon.log")
            .expect_err("超时必须报错");
        assert!(error.contains(r"C:\x\daemon.log"), "实际：{error}");
    }

    /// 负对照：把 `daemon_binary_path` 改成 `PathBuf::from(DAEMON_BINARY)`（只给文件名）
    /// ⇒ 这条会红。钉住的是"安装布局里就在旁边"，不是"去 PATH 里找"。
    #[test]
    fn the_daemon_is_the_binary_next_to_us() {
        // ⚠️ 用 `join` 拼，**不要写字面量路径**：`Path` 的分隔符是平台相关的，
        // 写死 `C:\Program Files\...` 在 Linux runner 上会被当成"一个文件名"
        // （反斜杠不是分隔符）⇒ `parent()` 拿到空串、断言必挂。CI 实测过。
        let install = PathBuf::from("install");
        let here = install.join("renderloft-cli.exe");
        let expected = install.join(DAEMON_BINARY);
        assert_eq!(daemon_binary_path(&here), expected);
    }

    #[test]
    fn bind_addresses_are_parsed_from_endpoint_urls() {
        assert_eq!(
            parse_bind("http://127.0.0.1:31337").map(|addr| addr.to_string()),
            Some("127.0.0.1:31337".to_string())
        );
        assert_eq!(
            parse_bind("http://127.0.0.1:4000/").map(|addr| addr.to_string()),
            Some("127.0.0.1:4000".to_string())
        );
        assert!(parse_bind("不是地址").is_none());
    }

    fn network_error() -> crate::v2::clients::client::ClientError {
        crate::v2::clients::client::ClientError::network("无法连接 daemon")
    }

    /// daemon 日志**超过阈值要轮转**，且**保留上一份**。
    ///
    /// 为什么必须有它（实测）：CLI / MCP 这条启动路径此前只 append、从不轮转，而桌面外壳那条
    /// 到 5 MB 就轮转 ⇒ "日志会不会无限长"取决于**谁先把 daemon 拉起来**，没人能预期。
    /// 而 CLI / MCP 恰好是最常拉起 daemon 的那两条路。
    ///
    /// 判别点四条，缺一条都不算做对：
    /// 1. 没到阈值：**不**轮转（否则每次启动都把上一份顶掉，等于只留最后一段日志）；
    /// 2. 超过阈值：轮转成 `daemon.log.1`，且**上一份一个字节都不少**（崩溃现场常在那份里）；
    /// 3. 轮转后新文件从零开始（不是接着往旧的里写）；
    /// 4. 只留一份 `.1`：再来一次就覆盖它（不是无限堆 `.2` `.3`）。
    ///
    /// 负对照：把 `open_daemon_log` 里那段轮转改成 `if false && …`（能编译）⇒ 第 2、3 条红。
    #[test]
    fn the_daemon_log_rotates_once_and_keeps_the_previous_file() {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-log-rotate-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
        let path = dir.join("daemon.log");
        let rotated = dir.join("daemon.log.1");

        // 1) 没到阈值：原样追加，不轮转。
        std::fs::write(&path, b"first run\n").expect("写日志");
        drop(open_daemon_log(&path).expect("打开日志"));
        assert!(!rotated.exists(), "没到阈值不该轮转：{rotated:?}");

        // 2) 超过阈值：轮转，且上一份完整保留。
        //    用 `set_len` 造一个稀疏的大文件——不必真的写 5 MB。
        let big = std::fs::File::create(&path).expect("造大文件");
        big.set_len(LOG_ROTATE_BYTES + 1).expect("设长度");
        drop(big);
        drop(open_daemon_log(&path).expect("打开日志"));
        assert_eq!(
            std::fs::metadata(&rotated)
                .expect("轮转后的文件应当存在")
                .len(),
            LOG_ROTATE_BYTES + 1,
            "上一份必须一个字节都不少（崩溃现场常在被轮转掉的那份里）"
        );
        // 3) 新文件从零开始。
        assert_eq!(
            std::fs::metadata(&path).expect("新日志").len(),
            0,
            "轮转后应当从零开始写"
        );

        // 4) 只留一份 `.1`。
        let big = std::fs::File::create(&path).expect("再造大文件");
        big.set_len(LOG_ROTATE_BYTES + 1).expect("设长度");
        drop(big);
        drop(open_daemon_log(&path).expect("打开日志"));
        assert!(
            !dir.join("daemon.log.2").exists(),
            "只保留上一份，不该堆出 .2"
        );
        assert_eq!(
            std::fs::metadata(&rotated).expect("上一份仍在").len(),
            LOG_ROTATE_BYTES + 1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 启动失败 + 连不上 ⇒ 必须把启动失败的原因说出来。
    ///
    /// 负对照：把 `explain_failure` 里的 `if error.status != 0 { return error }` 删掉
    /// ⇒ 下面第二条断言会红（业务错误不该被归咎于"daemon 没起来"）。
    #[test]
    fn a_boot_failure_explains_a_network_failure() {
        let boot = Err("找不到 daemon 可执行文件 C:\\x\\renderloft.exe".to_string());
        let explained = explain_failure(network_error(), &boot);
        assert!(
            explained.message.contains("找不到 daemon 可执行文件"),
            "实际：{}",
            explained.message
        );

        let business = crate::v2::clients::client::ClientError {
            status: 404,
            code: "RECIPE_NOT_FOUND".to_string(),
            message: "应用不存在".to_string(),
            retryable: false,
            details: serde_json::Value::Null,
        };
        let untouched = explain_failure(business, &boot);
        assert_eq!(
            untouched.message, "应用不存在",
            "业务错误不该被改写成启动问题"
        );
    }

    /// 启动成功（或已经在跑）时，错误原样透传——不给自己加戏。
    #[test]
    fn a_successful_boot_leaves_errors_alone() {
        let explained = explain_failure(network_error(), &Ok(EnsureOutcome::AlreadyRunning));
        assert_eq!(explained.message, "无法连接 daemon");
    }
}
