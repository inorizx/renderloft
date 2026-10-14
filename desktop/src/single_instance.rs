//! 单实例：同一时刻只允许一个外壳进程跑着。
//!
//! 为什么必须有：托盘是"常驻"的可见形式——两个外壳就是**两个托盘图标 + 两份状态**，
//! 用户点第二次图标会以为"没反应"（其实开了个新的在一旁），关掉其中一个另一个还在。
//!
//! 为什么不用官方插件：`tauri-plugin-single-instance` 不在本机的离线依赖缓存里，而这件事
//! 用 Win32 的**命名互斥体**几十行就够——换掉一个新依赖。
//!
//! 拿不到互斥体（创建失败）时**放行**：宁可多开一个窗口，也不要让应用打不开。

/// 抢到"唯一实例"了吗。`false` = 已经有一个在跑（调用方应把那个窗口叫到前台，然后自己退出）。
#[cfg(windows)]
pub fn claim() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    // `Local\` = 每个登录会话一个：同一台机器换个用户登录，各开一个是合理的。
    let name: Vec<u16> = "Local\\RenderLoft-Shell\0".encode_utf16().collect();
    // SAFETY: 名字是 NUL 结尾的宽字符串，安全属性指针为 null = 默认属性。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return true;
    }
    // ⚠️ 句柄**故意不释放、也不 CloseHandle**：进程退出时由系统回收，
    // "句柄还活着"本身就是"我这一份还在跑"这个事实。手动释放等于把单实例关掉。
    // SAFETY: 紧挨着 CreateMutexW 调用，读的是它设置的最后错误码。
    unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
}

#[cfg(not(windows))]
pub fn claim() -> bool {
    true
}

/// 把已经在跑的那个窗口叫到前台。
///
/// 关进托盘之后窗口是**隐藏**的（`ShowWindow(SW_RESTORE)` 才会显出来），所以这里不是
/// "切个焦点"那么简单——不做这一步，用户点第二次图标就真的什么也看不到。
#[cfg(windows)]
pub fn focus_existing_window(title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    let title: Vec<u16> = format!("{title}\0").encode_utf16().collect();
    // SAFETY: 标题是 NUL 结尾的宽字符串；类名传 null = 不限定窗口类。
    unsafe {
        let window = FindWindowW(std::ptr::null(), title.as_ptr());
        if window.is_null() {
            // 窗口还没建出来（比如上一个实例正在启动）——那就什么也不做：
            // 用户看到的是"点了没反应"，比弹一个错误框好。
            return;
        }
        ShowWindow(window, SW_RESTORE);
        SetForegroundWindow(window);
    }
}

#[cfg(not(windows))]
pub fn focus_existing_window(_title: &str) {}
