//! 替 daemon 打开下载目录（并选中文件）。
//!
//! **为什么不由 daemon 直接开**：打开窗口的是 daemon（后台进程），而 Windows 的前台锁不允许
//! 后台进程把新窗口带到前面——实测的表现是"任务栏图标在闪，但窗口没弹出来"。前台进程是
//! **外壳**（用户点的就是它），由它调用 Shell API，窗口才会真的到前面来。
//!
//! 交接方式：daemon 在工作区目录里写一张 `reveal-request.json`（与它写 `daemon-endpoint.json`
//! 同一处，不引入第二套约定），这里每 250ms 看一眼；看到就**改名认领**（原子操作，避免和
//! daemon 的兜底逻辑各开一次），再调用 `SHOpenFolderAndSelectItems`。

use std::path::{Path, PathBuf};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// daemon 写下的请求单。
#[derive(serde::Deserialize)]
struct RevealRequest {
    directory: String,
    /// 要选中的文件；可能为空（只开目录），也可能指向一个已经被删掉的文件。
    reveal: Option<String>,
}

pub fn spawn() {
    std::thread::spawn(|| loop {
        handle_once();
        std::thread::sleep(POLL_INTERVAL);
    });
}

/// 看一眼请求单；没有就立刻返回（一次 `exists()`，成本可以忽略）。
fn handle_once() {
    let Some(path) = request_path() else { return };
    if !path.exists() {
        return;
    }
    // **认领**：改名是原子的。daemon 那边等 600ms 再看文件还在不在——被搬走就说明我们接走了。
    let claimed = path.with_extension("claimed");
    if std::fs::rename(&path, &claimed).is_err() {
        return;
    }
    let payload = std::fs::read_to_string(&claimed);
    let _ = std::fs::remove_file(&claimed);
    let Ok(text) = payload else { return };
    let Ok(request) = serde_json::from_str::<RevealRequest>(&text) else {
        crate::shell_log("打开下载目录：请求单读不动，忽略");
        return;
    };
    let target = target_of(&request);
    if let Err(error) = shell_open_and_select(&target) {
        crate::shell_log(&format!("打开下载目录失败：{error}"));
    }
}

/// 真正交给 Shell 的路径：`reveal` 还在就用它（打开它所在的目录并选中它），否则用它所在的目录。
///
/// `reveal` 指向的文件可能已经被删了（用户或别的程序动过）——那时**退回打开目录**，
/// 而不是把一个不存在的路径交给 Shell：不存在的路径会让文件管理器自作主张开到默认目录
/// （Windows 上是「文档」），用户看到的就是"点了一下，跳到一个莫名其妙的地方"。
fn target_of(request: &RevealRequest) -> PathBuf {
    request
        .reveal
        .as_deref()
        .map(Path::new)
        .filter(|path| path.exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&request.directory))
}

fn request_path() -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join("renderloft-workspace")
            .join("reveal-request.json"),
    )
}

/// `SHOpenFolderAndSelectItems`：打开并选中；传目录就是打开它。
///
/// 与 `src-tauri/src/v2/application/settings.rs` 里那份是同一段逻辑。两个 crate 之间没有共享库
/// （`desktop` 是独立 crate），所以这是一份**刻意的重复**——改动其一请同步另一份。
#[cfg(target_os = "windows")]
fn shell_open_and_select(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    // 注意：C 头文件里的 `ILFree` 宏，在 windows-sys 里暴露为 `SHFree`（同一个 shell32 导出）。
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, SHFree, SHOpenFolderAndSelectItems};

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        // S_OK / S_FALSE 都表示"这个线程现在是初始化好的"；只有它们才配对 CoUninitialize。
        let initialized = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0;
        let pidl = ILCreateFromPathW(wide.as_ptr());
        if pidl.is_null() {
            if initialized {
                CoUninitialize();
            }
            return Err(format!("拿不到 PIDL：{}", path.display()));
        }
        // cidl = 0 且 apidl = NULL：`pidl` 指向谁，就打开谁所在的目录并选中它。
        let hr = SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0);
        SHFree(pidl as *const core::ffi::c_void);
        if initialized {
            CoUninitialize();
        }
        if hr < 0 {
            return Err(format!("SHOpenFolderAndSelectItems 失败：0x{hr:08X}"));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn shell_open_and_select(path: &Path) -> Result<(), String> {
    // 前台锁是 Windows 的事，别的平台不需要这层交接；保底打开它所在的目录。
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    std::process::Command::new("open")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(directory: &str, reveal: Option<&str>) -> RevealRequest {
        RevealRequest {
            directory: directory.to_string(),
            reveal: reveal.map(str::to_string),
        }
    }

    /// 每个用例一个独立子目录（同一个进程里测试是并行的，别互相踩）。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("renderloft-reveal-{tag}"));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn an_existing_file_wins_over_its_directory() {
        let dir = temp_dir("existing");
        let file = dir.join("870.png");
        std::fs::write(&file, b"x").expect("写临时文件");
        let target = target_of(&request(
            &dir.to_string_lossy(),
            Some(&file.to_string_lossy()),
        ));
        assert_eq!(target, file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 负对照：`reveal` 指向的文件已经不在了 ⇒ **退回打开目录**，
    /// 不能把不存在的路径交给 Shell（它会自作主张开到「文档」）。
    #[test]
    fn a_vanished_file_falls_back_to_the_directory() {
        let dir = temp_dir("vanished");
        let gone = dir.join("gone.png");
        let target = target_of(&request(
            &dir.to_string_lossy(),
            Some(&gone.to_string_lossy()),
        ));
        assert_eq!(target, dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_reveal_means_the_directory() {
        assert_eq!(
            target_of(&request("C:\\downloads", None)),
            PathBuf::from("C:\\downloads")
        );
    }
}
