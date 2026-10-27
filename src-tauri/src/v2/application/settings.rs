//! 本地工作区设置与 RunningHub 账号/队列状态。
//!
//! 设置只保存非机密偏好（下载目录）。**平台凭据一律由 Connection 管理**
//! （密钥进系统凭据库），此处没有任何密钥入口；密钥绝不落库、绝不出现
//! 在日志或错误响应。

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(test)]
use async_trait::async_trait;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::ConnectionId;
use crate::v2::domain::model::{is_api_key_slot, ConnectionRevision};
use crate::v2::ports::provider::{driver_key, DriverRegistry};
use crate::v2::ports::repositories::{ConnectionRepository, SettingsRecord, SettingsRepository};
use crate::v2::ports::stores::SecretStore;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub download_dir: String,
    /// 历史保留天数：终态操作超过它就整批清理；`0` = **永久保留**。
    pub history_retain_days: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    #[serde(default)]
    pub download_dir: String,
    /// `None` = 这次不动它（PUT 是部分更新，没给的键保持原值）。
    #[serde(default)]
    pub history_retain_days: Option<u64>,
}

pub struct SettingsService {
    settings: Arc<dyn SettingsRepository>,
}

impl SettingsService {
    pub fn new(settings: Arc<dyn SettingsRepository>) -> Self {
        Self { settings }
    }

    pub async fn view(&self) -> DomainResult<SettingsView> {
        let record = self.settings.load_settings().await?;
        Ok(SettingsView {
            download_dir: record.download_dir,
            history_retain_days: record.history_retain_days,
        })
    }

    pub async fn save(&self, input: SettingsInput) -> DomainResult<SettingsView> {
        let current = self.settings.load_settings().await?;
        let download_dir = if input.download_dir.trim().is_empty() {
            default_download_dir()
        } else {
            input.download_dir.trim().to_string()
        };
        // 没给就保持原值：设置是**部分更新**，不能让"只想改下载目录"的人顺手把保留期改回默认。
        // 上限 100 年只是防手滑（一个 20 位数会算出一个荒谬的 cutoff，行为与 0 相同但难读）。
        let history_retain_days = match input.history_retain_days {
            Some(days) => days.min(36_500),
            None => current.history_retain_days,
        };
        let record = SettingsRecord {
            download_dir,
            history_retain_days,
        };
        self.settings.save_settings(&record).await?;
        self.view().await
    }

    /// 打开下载目录；`reveal` 给了一个**目录内、且真实存在**的文件路径时，顺带把它选中。
    ///
    /// 选中那个文件是主流做法（浏览器、VS Code 的"在文件夹中显示"都是选中而不是只开目录）。
    ///
    /// ⚠️ Windows 上**不能**拼 `explorer.exe /select,<路径>`。实测（用户复现，2026-10-16）：
    /// **这个文件夹已经开着一个窗口时**，那条老命令行认不出参数，退回 Explorer 的默认目录
    /// ——也就是「文档」。改用 Shell API `SHOpenFolderAndSelectItems`：同样的语义，但它复用
    /// 已有窗口，也不经过命令行解析，所以没有这个失败模式。
    ///
    /// ⚠️ 还有一层：**这个动作最好由外壳做，而不是 daemon**。daemon 是后台进程，Windows 的
    /// 前台锁不允许它把新窗口带到前面——用户看到的是"任务栏图标在闪，但窗口没弹出来"（实测）。
    /// 外壳是前台进程（用户点的就是它），它调用时窗口才会真的到前面来。所以这里先写一张
    /// "请帮我打开"的请求单交给外壳，等一小会儿看它有没有接走；没接走（浏览器里开发、外壳
    /// 没在跑）才自己来——那时"窗口在后面"是可接受的，总比什么都不开要好。
    pub async fn open_download_dir(&self, reveal: Option<&str>) -> DomainResult<String> {
        let record = self.settings.load_settings().await?;
        let directory = if record.download_dir.trim().is_empty() {
            default_download_dir()
        } else {
            record.download_dir
        };
        std::fs::create_dir_all(&directory).map_err(|_| SafeError::storage("无法创建下载目录"))?;
        let target = reveal_target(&directory, reveal);
        if hand_off_to_shell(&directory, target.as_deref()).await {
            return Ok(directory);
        }
        open_in_file_manager(target.as_deref(), std::path::Path::new(&directory))
            .map_err(|_| SafeError::storage("无法打开下载目录"))?;
        Ok(directory)
    }
}

/// 外壳最多用多久来接走那张请求单（超过就自己开）。
///
/// 外壳的轮询间隔是 250ms（见 `desktop/src/reveal.rs`），所以 600ms 足够它看到；
/// 而用户点一下按钮等 0.6 秒是可以接受的——比自己开一个"在后面"的窗口要好。
const REVEAL_HANDOFF_MS: u64 = 600;

/// 请求单放在工作区目录里，与 `daemon-endpoint.json` 同处——外壳本来就在读那个目录，
/// 不引入第二套约定。
fn reveal_request_path() -> Option<std::path::PathBuf> {
    Some(
        crate::v2::daemon::v2_data_dir()
            .ok()?
            .join("reveal-request.json"),
    )
}

/// 把"打开并选中"交给外壳去办。返回 `true` 表示**它接走了**（这次不用自己开）。
async fn hand_off_to_shell(directory: &str, target: Option<&std::path::Path>) -> bool {
    let Some(request_path) = reveal_request_path() else {
        return false;
    };
    let payload = serde_json::json!({
        "directory": directory,
        "reveal": target.map(|path| path.to_string_lossy().to_string()),
    });
    if std::fs::write(&request_path, payload.to_string()).is_err() {
        return false;
    }
    tokio::time::sleep(std::time::Duration::from_millis(REVEAL_HANDOFF_MS)).await;
    // 外壳"认领"的方式是把它改名搬走，所以文件不在了就说明它接走了。
    if !request_path.exists() {
        return true;
    }
    let _ = std::fs::remove_file(&request_path);
    false
}

/// 界面传来的 `reveal` 值可信吗：必须**在下载目录里**、且**真的存在**。
///
/// 两道都要，各有各的理由：
/// - 「在下载目录里」：这个参数来自界面，不该让它变成"打开任意路径"的开关；
/// - 「真的存在」：不存在的路径交给文件管理器，它会**自作主张**开到默认目录（Windows 上是
///   文档），用户看到的就是"点了一下，跳到一个莫名其妙的地方"。宁可不选中，也不要跳错。
fn reveal_target(directory: &str, reveal: Option<&str>) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(reveal?.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    if !path.starts_with(std::path::Path::new(directory)) {
        return None;
    }
    if !path.exists() {
        return None;
    }
    Some(path.to_path_buf())
}

/// 打开文件管理器：给了 `target` 就选中它，否则打开 `directory`。
#[cfg(target_os = "windows")]
fn open_in_file_manager(
    target: Option<&std::path::Path>,
    directory: &std::path::Path,
) -> std::io::Result<()> {
    // 起一个**独立线程**：COM 是按线程初始化的，而这里是 tokio 的工作线程（共享的）。
    // 在专用线程里初始化、用完让线程自然结束——既不打扰运行时的线程，也不阻塞异步任务。
    // 调用方 fire-and-forget（Shell 没有"打开成功"的回执），所以这是它唯一能报错的地方。
    let path = target.unwrap_or(directory).to_path_buf();
    std::thread::Builder::new()
        .name("shell-reveal".to_string())
        .spawn(move || {
            if let Err(error) = shell_open_path(&path) {
                eprintln!("[renderloft] 打开文件管理器失败：{error}");
            }
        })
        .map(|_| ())
}

/// 打开路径：**文件**就选中它，**目录**就打开它。
///
/// ⚠️ 两种情形必须分开走，这是实测踩出来的（2026-10-16）：`SHOpenFolderAndSelectItems` 拿到的
/// PIDL 如果是**目录**，它打开的是**父目录**并把这个目录选中——用户看到的是"跳到了上一层、
/// 高亮了下载文件夹"，而不是"打开下载文件夹"。打开目录要用 `ShellExecuteW("open")`。
#[cfg(target_os = "windows")]
fn shell_open_path(path: &std::path::Path) -> Result<(), String> {
    if path.is_dir() {
        return shell_open_directory(path);
    }
    shell_open_and_select(path)
}

/// 打开一个目录（`ShellExecuteW` 的 "open" 动作，也就是资源管理器里双击一个文件夹的行为）。
#[cfg(target_os = "windows")]
fn shell_open_directory(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let verb = wide("open");
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // `ShellExecuteW` 的返回值是个古怪的约定：> 32 才算成功，<= 32 是错误码。
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err(format!("ShellExecuteW 失败：{:?}", result as isize));
    }
    Ok(())
}

/// `SHOpenFolderAndSelectItems`：打开文件所在的目录并选中它。
///
/// 这几处 `unsafe` 是必要的（Shell 只有 FFI）：`CoInitializeEx` 只初始化**本线程**的 COM、
/// `ILCreateFromPathW` 分配一个 PIDL 并由 `SHFree` 立刻还回去、`SHOpenFolderAndSelectItems`
/// 只做"打开 + 选中"这一件事。都只动自己的入参，失败以 HRESULT 返回，不 panic。
#[cfg(target_os = "windows")]
fn shell_open_and_select(path: &std::path::Path) -> Result<(), String> {
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

#[cfg(target_os = "macos")]
fn open_in_file_manager(
    target: Option<&std::path::Path>,
    directory: &std::path::Path,
) -> std::io::Result<()> {
    let mut command = std::process::Command::new("open");
    match target {
        // `-R` 就是 macOS 的"在访达中显示"。
        Some(path) => command.arg("-R").arg(path),
        None => command.arg(directory),
    };
    command.spawn().map(|_| ())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn open_in_file_manager(
    target: Option<&std::path::Path>,
    directory: &std::path::Path,
) -> std::io::Result<()> {
    // Linux 的文件管理器没有统一的"选中"约定，退回打开它所在的目录。
    let wanted = match target {
        Some(path) => path.parent().unwrap_or(directory),
        None => directory,
    };
    std::process::Command::new("xdg-open")
        .arg(wanted)
        .spawn()
        .map(|_| ())
}

pub fn mask_secret(value: &str) -> Option<String> {
    let characters = value.trim().chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return None;
    }
    if characters.len() <= 8 {
        return Some("*".repeat(characters.len()));
    }
    Some(format!(
        "{}****{}",
        characters[..4].iter().collect::<String>(),
        characters[characters.len() - 4..]
            .iter()
            .collect::<String>()
    ))
}

pub fn default_download_dir() -> String {
    dirs::download_dir()
        .or_else(dirs::document_dir)
        .or_else(dirs::home_dir)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

// ---------- RunningHub 账号 / 队列状态（基于 Connection） ----------

/// 状态查询用到的 RunningHub 连接摘要（供诊断展示）。
#[derive(Debug, Clone)]
pub struct RunningHubBrief {
    pub base_url: String,
    pub api_key_configured: bool,
    pub region: Option<String>,
    pub account: Option<String>,
}

pub struct RunningHubStatusService {
    connections: Arc<dyn ConnectionRepository>,
    drivers: Arc<DriverRegistry>,
    secrets: Arc<dyn SecretStore>,
}

impl RunningHubStatusService {
    pub fn new(
        connections: Arc<dyn ConnectionRepository>,
        drivers: Arc<DriverRegistry>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            connections,
            drivers,
            secrets,
        }
    }

    async fn runninghub_revision(
        &self,
        requested: Option<ConnectionId>,
    ) -> DomainResult<ConnectionRevision> {
        let connection = if let Some(id) = requested {
            let connection =
                self.connections.find_connection(id).await?.ok_or_else(|| {
                    SafeError::not_found("CONNECTION_NOT_FOUND", "Connection 不存在")
                })?;
            if connection.vendor != "runninghub" || connection.protocol != "runninghub" {
                return Err(SafeError::validation(
                    "NOT_RUNNINGHUB_CONNECTION",
                    "该 Connection 不是 RunningHub",
                ));
            }
            connection
        } else {
            let connections = self.connections.list_connections().await?;
            connections
                .into_iter()
                .find(|connection| {
                    connection.vendor == "runninghub" || connection.protocol == "runninghub"
                })
                .ok_or_else(|| {
                    SafeError::validation(
                        "RUNNINGHUB_CONNECTION_MISSING",
                        "尚未配置 RunningHub Connection，请到「连接」页添加",
                    )
                })?
        };
        self.connections
            .find_revision(connection.current_revision_id)
            .await?
            .ok_or_else(|| {
                SafeError::not_found(
                    "CONNECTION_REVISION_NOT_FOUND",
                    "Connection revision 不存在",
                )
            })
    }

    pub async fn account_status(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> DomainResult<serde_json::Value> {
        let revision = self.runninghub_revision(connection_id).await?;
        let driver = self.drivers.get(&driver_key("runninghub", "runninghub"))?;
        driver.fetch_account_status(&revision).await
    }

    pub async fn queue_status(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> DomainResult<serde_json::Value> {
        let revision = self.runninghub_revision(connection_id).await?;
        let driver = self.drivers.get(&driver_key("runninghub", "runninghub"))?;
        driver.fetch_queue_status(&revision).await
    }

    /// 首个 RunningHub 连接的摘要；未配置时返回 None（用于诊断，不报错）。
    pub async fn runninghub_brief(&self) -> DomainResult<Option<RunningHubBrief>> {
        let connections = self.connections.list_connections().await?;
        let Some(connection) = connections.into_iter().find(|connection| {
            connection.vendor == "runninghub" && connection.protocol == "runninghub"
        }) else {
            return Ok(None);
        };
        let Some(revision) = self
            .connections
            .find_revision(connection.current_revision_id)
            .await?
        else {
            return Ok(None);
        };
        let api_key_configured = if let Some(binding) = revision
            .secret_bindings
            .iter()
            .find(|binding| is_api_key_slot(&binding.slot))
        {
            self.secrets
                .resolve(binding)
                .await?
                .is_some_and(|value| !value.secret.trim().is_empty())
        } else {
            false
        };
        Ok(Some(RunningHubBrief {
            base_url: revision
                .config
                .get("baseUrl")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            api_key_configured,
            region: revision
                .config
                .get("region")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            account: revision
                .config
                .get("account")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::model::SecretBinding;
    use crate::v2::infrastructure::fakes::InMemoryRepositories;

    struct InMemorySecrets(std::sync::Mutex<std::collections::HashMap<String, String>>);

    #[async_trait]
    impl SecretStore for InMemorySecrets {
        async fn resolve(
            &self,
            binding: &SecretBinding,
        ) -> DomainResult<Option<crate::v2::ports::stores::SecretValue>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&binding.slot)
                .cloned()
                .map(|secret| crate::v2::ports::stores::SecretValue { secret }))
        }
        async fn set(&self, slot: &str, value: &str) -> DomainResult<()> {
            self.0
                .lock()
                .unwrap()
                .insert(slot.to_string(), value.to_string());
            Ok(())
        }
        async fn delete(&self, slot: &str) -> DomainResult<()> {
            self.0.lock().unwrap().remove(slot);
            Ok(())
        }
    }

    fn service() -> SettingsService {
        let repos = Arc::new(InMemoryRepositories::new());
        SettingsService::new(repos)
    }

    #[tokio::test]
    async fn save_then_view_roundtrip_download_dir() {
        let service = service();
        let view = service
            .save(SettingsInput {
                download_dir: "D:/tmp/downloads".to_string(),
                history_retain_days: None,
            })
            .await
            .unwrap();
        assert_eq!(view.download_dir, "D:/tmp/downloads");
    }

    #[tokio::test]
    async fn empty_download_dir_falls_back_to_default() {
        let service = service();
        let view = service
            .save(SettingsInput {
                download_dir: "  ".to_string(),
                history_retain_days: None,
            })
            .await
            .unwrap();
        assert!(!view.download_dir.is_empty());
    }

    /// 历史保留期：默认 30 天（= 旧行为）、`0` = 永久、**PUT 是部分更新**（没给就不动）。
    #[tokio::test]
    async fn history_retention_is_part_of_settings_and_is_a_partial_update() {
        let service = service();
        assert_eq!(
            service.view().await.unwrap().history_retain_days,
            30,
            "没改过的人看到的必须是旧行为（30 天）"
        );
        let view = service
            .save(SettingsInput {
                download_dir: "D:/a".to_string(),
                history_retain_days: Some(0),
            })
            .await
            .unwrap();
        assert_eq!(view.history_retain_days, 0, "0 = 永久保留");
        // 只想改下载目录的人，不该顺手把保留期改回默认。
        let view = service
            .save(SettingsInput {
                download_dir: "D:/b".to_string(),
                history_retain_days: None,
            })
            .await
            .unwrap();
        assert_eq!(view.history_retain_days, 0, "没给就保持原值");
        // 上限只是防手滑，不改变语义（超过 100 年按 100 年算）。
        let view = service
            .save(SettingsInput {
                download_dir: "D:/b".to_string(),
                history_retain_days: Some(u64::MAX),
            })
            .await
            .unwrap();
        assert_eq!(view.history_retain_days, 36_500);
    }

    #[test]
    fn masked_secret_formats() {
        assert_eq!(mask_secret("abcdef"), Some("******".to_string()));
        assert_eq!(
            mask_secret("abcdefghijklmnop"),
            Some("abcd****mnop".to_string())
        );
        assert_eq!(mask_secret(""), None);
    }

    #[tokio::test]
    async fn status_without_runninghub_connection_is_clear() {
        let repositories = Arc::new(InMemoryRepositories::new());
        let service = RunningHubStatusService::new(
            repositories.clone(),
            Arc::new(DriverRegistry::new()),
            Arc::new(InMemorySecrets(std::sync::Mutex::new(Default::default()))),
        );
        let error = service.account_status(None).await.unwrap_err();
        assert_eq!(error.code, "RUNNINGHUB_CONNECTION_MISSING");
        assert!(service.runninghub_brief().await.unwrap().is_none());
    }

    /// 造一个临时下载目录，里面放一个真实文件。
    fn temp_download_dir() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("renderloft-reveal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let file = dir.join("870.png");
        std::fs::write(&file, b"x").expect("写临时文件");
        (dir, file)
    }

    #[test]
    fn an_existing_file_inside_the_download_dir_is_revealed() {
        let (dir, file) = temp_download_dir();
        let directory = dir.to_string_lossy().to_string();
        let revealed = reveal_target(&directory, Some(&file.to_string_lossy()));
        assert_eq!(revealed.as_deref(), Some(file.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 负对照：**目录外**的路径不认 —— 这个参数来自界面，不能变成"打开任意路径"的开关。
    #[test]
    fn a_path_outside_the_download_dir_is_refused() {
        let (dir, _) = temp_download_dir();
        let outside = std::env::temp_dir().join("renderloft-outside.png");
        std::fs::write(&outside, b"x").expect("写目录外的文件");
        let directory = dir.to_string_lossy().to_string();
        assert_eq!(
            reveal_target(&directory, Some(&outside.to_string_lossy())),
            None
        );
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 负对照：**不存在的**路径不认。
    ///
    /// 这条正是用户那个 bug 的成因：不存在的路径交给文件管理器，它会自作主张开到默认目录
    /// （Windows 上是「文档」）——用户看到的就是"点了一下，跳到一个莫名其妙的地方"。
    #[test]
    fn a_missing_path_is_refused() {
        let (dir, file) = temp_download_dir();
        std::fs::remove_file(&file).expect("先删掉它，制造一个不存在的路径");
        let directory = dir.to_string_lossy().to_string();
        assert_eq!(
            reveal_target(&directory, Some(&file.to_string_lossy())),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blank_or_absent_reveal_is_refused() {
        let (dir, _) = temp_download_dir();
        let directory = dir.to_string_lossy().to_string();
        assert_eq!(reveal_target(&directory, None), None);
        assert_eq!(reveal_target(&directory, Some("")), None);
        assert_eq!(reveal_target(&directory, Some("   ")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
