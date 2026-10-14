// ⚠️ 这一行必须在任何 item 之前（crate 级属性）。
//
// Windows 上它是"启动时那个黑色控制台窗口"的根因：默认的控制台子系统会让 GUI 应用
// 也被分配一个控制台，双击打开就凭空多一个黑框，而且**会一直挂到应用退出**。
// 用 `not(debug_assertions)` 是有意的：`pnpm dev` 那种 debug 构建里留着控制台，
// 报错和打印看得见；发布版不要窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod single_instance;
mod status_poller;
mod tray;

use serde::Deserialize;
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{Manager, WebviewWindow};
use tauri_plugin_notification::NotificationExt;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EndpointFile {
    endpoint: String,
    token: String,
    bootstrap_url: String,
}

fn main() {
    // 单实例：抢不到就说明已经有一个在跑（它可能正收在托盘里，窗口是隐藏的）——
    // 把那个窗口叫到前台，然后自己退出。两个外壳 = 两个托盘图标 + 两份状态。
    if !single_instance::claim() {
        shell_log("第二次启动：把已有窗口叫到前台，然后退出");
        single_instance::focus_existing_window("RenderLoft");
        return;
    }
    let daemon = Arc::new(Mutex::new(None::<Child>));
    // 托盘建成功了吗。**没成功就不能把"关窗"改成"收进托盘"**——那会得到一个关不掉的窗口，
    // 而唯一的出口（托盘菜单）恰恰没建出来。
    let tray_ready = Arc::new(AtomicBool::new(false));
    let tray_ready_for_setup = tray_ready.clone();
    let tray_ready_for_window = tray_ready.clone();
    let daemon_for_setup = daemon.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            *daemon_for_setup
                .lock()
                .expect("daemon child lock unavailable") = start_daemon(app.handle());
            let window = app.get_webview_window("main").expect("missing main window");
            navigate_to_daemon(&window);
            // 托盘 + 状态轮询。托盘失败**不挡应用**：退回"关窗即退出"（旧行为），并留一行日志。
            match tray::build(app.handle()) {
                Ok(()) => {
                    tray_ready_for_setup.store(true, Ordering::Relaxed);
                    shell_log("托盘已就绪，启动状态轮询");
                    status_poller::spawn(app.handle().clone());
                }
                Err(error) => shell_log(&format!("托盘创建失败，关窗将直接退出：{error}")),
            }
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if !tray_ready_for_window.load(Ordering::Relaxed) {
                    // 没有托盘 → 老实退出（旧行为）。这条要留痕：它是"关窗之后什么都没了"的唯一解释。
                    shell_log("close: 没有托盘，按退出处理");
                    return;
                }
                // 关窗 = 收进托盘：任务还在平台上跑，daemon 还要留着对账与轮询。
                // 真正的退出在托盘菜单里（那里才停 daemon）——这是"常驻"的全部代价与含义。
                api.prevent_close();
                // ⚠️ 别把这一行的错误吞掉：`hide()` 失败的症状是"点了 X 但窗口还在"，
                // 而除了这里没有任何地方能解释它。
                match window.hide() {
                    Ok(()) => shell_log("close: 收进托盘（daemon 继续跑）"),
                    Err(error) => shell_log(&format!("close: 隐藏窗口失败：{error}")),
                }
                hint_kept_running(window.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("RenderLoft desktop shell failed");
    app.run(move |app, event| {
        if let tauri::RunEvent::ExitRequested { code, .. } = event {
            if let Some(mut child) = daemon.lock().expect("daemon child lock unavailable").take() {
                shell_log("退出：停 daemon");
                request_shutdown();
                thread::sleep(Duration::from_millis(500));
                let _ = child.kill();
            }
            // 显式收尾并退出：**不能依赖事件循环自己走到 `RunEvent::Exit`**。
            //
            // 实测（2026-10-07，逐事件打点复现）：只要有 WM_CLOSE 打到 tao 的内部消息窗口
            // （`Tao Thread Event Target`——一个可见、无属主、无标题的顶层窗口），tao 的
            // 事件循环就再也送不出 `Exit`。此后即使正常关闭主窗口，序列也停在
            // `CloseRequested → Destroyed → ExitRequested` 之后：窗口已销毁、daemon 已回收，
            // 而进程永久留在任务管理器里（无窗口、无 CPU），并使覆盖安装失败。
            //
            // 那个内部窗口会被任何「枚举顶层窗口找主窗口」的机制选中，例如
            // `taskkill /IM renderloft-desktop.exe`（不带 `/F`）、任务管理器的「结束任务」、
            // 以及 .NET 的 `Process.CloseMainWindow`。用户点 X 走的是另一条路（直接对主窗口
            // 发 WM_CLOSE），实测正常；但既然代价只有一次显式退出，就不要把「能不能退出」
            // 押在框架内部窗口的运气上。
            app.cleanup_before_exit();
            std::process::exit(code.unwrap_or(0));
        }
    });
}

fn start_daemon(app: &tauri::AppHandle) -> Option<Child> {
    let daemon = if cfg!(debug_assertions) {
        let Ok(current) = std::env::current_exe() else {
            return None;
        };
        current.with_file_name(if cfg!(windows) {
            "renderloft.exe"
        } else {
            "renderloft"
        })
    } else {
        let Ok(resources) = app.path().resource_dir() else {
            return None;
        };
        resources.join(if cfg!(windows) {
            "renderloft.exe"
        } else {
            "renderloft"
        })
    };
    // daemon 的日志落文件（追加），而不是丢进 `NUL`。
    //
    // **不落文件就等于没有日志。** 实测（2026-10-08）：daemon 的对账循环死掉之后，因为它
    // 的 stdout/stderr 全被丢弃，无法判断是 panic 还是卡住，也看不到是哪一轮出事——只能靠
    // 重启试。发布版里这一条等于"用户报 bug 时你什么都看不到"。
    //
    // 放在 daemon 的工作区目录里（`%LOCALAPPDATA%\renderloft-workspace\daemon.log`），找日志不需要
    // 第二个约定。**这个目录必须与安装目录分开**（见 daemon 侧 `v2_data_dir()` 的注释：
    // 安装包默认装到 `%LOCALAPPDATA%\RenderLoft`，与数据目录同名会撞在一起）。
    // 第二个约定。超过 5 MB 轮转一次并**保留上一份**：崩溃现场往往正好在被轮转掉的那份里。
    let log = daemon_log_file();
    let stdout = log.as_ref().and_then(|file| file.try_clone().ok());
    let mut command = Command::new(daemon);
    command
        .stdin(Stdio::null())
        .stdout(stdout.map(Stdio::from).unwrap_or_else(Stdio::null))
        .stderr(log.map(Stdio::from).unwrap_or_else(Stdio::null));
    // ⚠️ Windows：外壳现在是 GUI 子系统（没有控制台），而被拉起的 daemon 是**控制台子系统**
    // 程序 —— 不给这个标志，Windows 会为它新开一个控制台窗口，用户看到的还是同一个黑框，
    // 只是换了个宿主。CLI / MCP 那条启动路径早就加了同一个标志（`clients/daemon_boot.rs`），
    // 外壳这条一直漏着；上面把外壳改成 GUI 之后，这里就成了唯一的出口。
    // 看不见窗口 ≠ 看不见日志：daemon 的 stdout/stderr 照旧落 `daemon.log`（见上）。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().ok()
}

/// daemon 日志文件；拿不到就返回 `None`（外层退回 `NUL`，不影响启动）。
fn daemon_log_file() -> Option<fs::File> {
    const LIMIT: u64 = 5 * 1024 * 1024;
    let dir = dirs::data_local_dir()?.join("renderloft-workspace");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("daemon.log");
    if fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > LIMIT {
        let _ = fs::rename(&path, dir.join("daemon.log.1"));
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn navigate_to_daemon(window: &WebviewWindow) {
    let address: SocketAddr = "127.0.0.1:31337".parse().expect("invalid daemon address");
    for _ in 0..50 {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            if let Some(endpoint) = read_endpoint() {
                if let Ok(url) = console_url(&endpoint).parse() {
                    let _ = window.navigate(url);
                }
            }
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// 控制台入口：**优先向已经坐在端口上的那个 daemon 换一张新门票**。
///
/// `daemon-endpoint.json` 里的 `bootstrapUrl` 只在"端口上这个 daemon 就是我刚拉起来的那个"
/// 时才有效——nonce 是一次性的。而端口上坐着的可能是**上一个进程留下的 daemon**：外壳被强杀
/// （任务管理器结束任务、崩溃）时 daemon 不会跟着走；用户又点了一次图标时，新 daemon 也会
/// 因端口被占而退出。那两种情况下端点文件里的 nonce 早被上一轮用掉，照原样打开只会得到
/// 401 的 JSON 糊在窗口里。换不到就退回端点文件里的 URL（老行为，至少不会更差）。
fn console_url(endpoint: &EndpointFile) -> String {
    match request_fresh_nonce(endpoint) {
        Some(nonce) => format!("{}/bootstrap?nonce={nonce}", endpoint.endpoint),
        None => endpoint.bootstrap_url.clone(),
    }
}

/// 用端点文件里的 token 调 `POST /api/daemon/bootstrap`，换一个刚签发的一次性 nonce。
///
/// 手写一次 HTTP/1.1 而不是为这一行需求引入 HTTP 客户端：连接是本机回环、请求体为空、
/// 响应是一个几十字节的 JSON。超时必须给：端口上那个 daemon 万一不回，别把界面卡在启动上。
fn request_fresh_nonce(endpoint: &EndpointFile) -> Option<String> {
    let address = endpoint
        .endpoint
        .strip_prefix("http://")
        .unwrap_or(&endpoint.endpoint);
    let mut stream = TcpStream::connect(address).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .ok()?;
    let request = format!(
        "POST /api/daemon/bootstrap HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        endpoint.token
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let body = response.split("\r\n\r\n").nth(1)?;
    let parsed: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    parsed.get("nonce")?.as_str().map(str::to_string)
}

pub(crate) fn read_endpoint() -> Option<EndpointFile> {
    let path = dirs::data_local_dir()?
        .join("renderloft-workspace")
        .join("daemon-endpoint.json");
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// 外壳自己的一行日志（追加到 `%LOCALAPPDATA%\renderloft-workspace\shell.log`）。
///
/// 为什么需要：外壳是 **GUI 子系统**程序——它没有控制台，`eprintln!` 在用户机器上等于丢进
/// 虚空。而它现在有了生命周期（常驻托盘、拥有 daemon、关窗不退出），"点了 X 但窗口还在"、
/// "任务跑完没通知我"这类问题**没有任何地方能解释**。与 daemon 的日志同目录、同一套
/// 5 MB 轮转约定，找日志不需要第二个约定。
///
/// 只记**生命周期事件**（启动/单实例/托盘/关窗/退出），不记状态轮询——那是每秒都在发生的事，
/// 写进日志只会把有用的那几行淹掉。
fn shell_log(line: &str) {
    let Some(dir) = dirs::data_local_dir().map(|dir| dir.join("renderloft-workspace")) else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("shell.log");
    const LIMIT: u64 = 1024 * 1024;
    if fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > LIMIT {
        let _ = fs::rename(&path, dir.join("shell.log.1"));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{stamp}] {line}");
    }
}

/// 第一次把窗口收进托盘时说一句。
///
/// 不说的话，"点 X 之后进程还在"对用户是**无声的意外**——而它偏偏又是必须的行为
/// （任务在平台上跑着、daemon 要对账）。一次进程只说一次：每次关窗都弹就成了骚扰。
fn hint_kept_running(app: &tauri::AppHandle) {
    static HINTED: AtomicBool = AtomicBool::new(false);
    if HINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("RenderLoft 还在后台")
        .body("任务会继续跑，跑完会通知你。要退出请右键托盘图标 →「退出」。")
        .show();
}

fn request_shutdown() {
    let Some(endpoint) = read_endpoint() else {
        return;
    };
    let address = endpoint
        .endpoint
        .strip_prefix("http://")
        .unwrap_or(&endpoint.endpoint);
    let Ok(mut stream) = TcpStream::connect(address) else {
        return;
    };
    let request = format!("POST /api/daemon/shutdown HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", endpoint.token);
    let _ = stream.write_all(request.as_bytes());
}
