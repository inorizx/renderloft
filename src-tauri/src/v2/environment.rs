//! 进程的**环境事实**：从环境变量与可执行文件位置推导出来的那些值。
//!
//! 为什么单独一个模块：这些值有**两个**消费者，而它们必须给出同一个答案——真正装配服务的
//! 那一方（`daemon.rs`）与**诊断接口**（`transports/http/shell.rs`）。诊断说"静态目录是 X"
//! 而服务实际用的是 Y，比不报更糟："我明明重铺了 dist / 我明明设了那个变量"这类问题，就是
//! 靠这里能一眼看出来。
//!
//! ⚠️ **分层**：`transports` 不得 import `daemon`（`scripts/check-layers.mjs`），所以这些
//! 不能留在 `daemon.rs` 里。同类的先例是 `outbound.rs`（出网代理）——它被 shell.rs 与
//! driver 共用，理由完全一样。
//!
//! ⚠️ **数据目录刻意不在这里**：它的所有者是 `daemon.rs` 的 `v2_data_dir()`，而
//! `check-layers.mjs` 的数据目录守门正是按"daemon.rs 里只有一种 `path.join("<名>")`"来判的。
//! 要把它也搬过来，就得同步改那个守门的所有者名单——那是另一件事，别顺手做。

/// daemon 的默认监听地址（`RENDERLOFT_BIND` 覆盖它）。
pub const DEFAULT_BIND: &str = "127.0.0.1:31337";

/// daemon 绑定的地址。
pub fn bind_address() -> String {
    std::env::var("RENDERLOFT_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string())
}

/// Host 白名单（`RENDERLOFT_ALLOW_HOST`，逗号分隔）。空 = 只认回环。
pub fn allowed_hosts() -> Vec<String> {
    std::env::var("RENDERLOFT_ALLOW_HOST")
        .map(|raw| parse_hosts(&raw))
        .unwrap_or_default()
}

/// 白名单的解析（纯函数，可测）：空串与多余逗号都不产出空条目——
/// 空条目在判据里等于"谁都放行"，那不是用户写 `A,,B` 时的意思。
fn parse_hosts(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// 界面来源（`RENDERLOFT_UI_ORIGIN`）。
///
/// 设了它，`/bootstrap` 就 302 到那个 origin（开发时指向 vite），否则 302 到 daemon 自己
/// 供的那份 `dist/`。**这是"外壳窗口里为什么是旧界面 / 为什么是 dev server"的答案**——
/// 同一个 exe，两种落点，界面上本来没有任何线索。
pub fn ui_origin() -> Option<String> {
    std::env::var("RENDERLOFT_UI_ORIGIN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 控制台静态资源根目录：`RENDERLOFT_DIST` 优先，否则**可执行文件旁边**的 `dist/`，
/// 最后退回构建期的源码树路径。
///
/// 判据的顺序不是随意的：安装到别人的机器上时，编译期路径（`CARGO_MANIFEST_DIR`）
/// **不存在**，只有"按 exe 位置找"能在那种机器上成立。而开发机上永远复现不了
/// （那个路径正好存在），所以这条只能靠这个顺序保证。
pub fn console_dist_dir() -> std::path::PathBuf {
    console_dist_root(
        std::env::var_os("RENDERLOFT_DIST").map(std::path::PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf)),
    )
}

/// `console_dist_dir` 的纯函数部分（可测：不读环境、不碰当前进程）。
pub fn console_dist_root(
    explicit: Option<std::path::PathBuf>,
    exe_dir: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(dir) = exe_dir
        .map(|dir| dir.join("dist"))
        .filter(|dir| dir.is_dir())
    {
        return dir;
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("dist")
}

/// 目录里的一个日志文件（给诊断接口报的"日志在这儿、现在多大、最后什么时候写的"）。
///
/// ⚠️ `rename_all = "camelCase"`：这个项目对外的一律是 camelCase（见 `transports/http/dto.rs`）。
/// 少了它字段会以 `modified_at_millis` 出去，而前端读 `modifiedAtMillis` ⇒ **静默变成
/// "写入时间未知"**（实测踩过：大小显示正常、时间不显示，因为 `bytes` 一个词两种写法一样）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFile {
    pub name: String,
    pub bytes: u64,
    /// 最后写入时间（毫秒时间戳）。**这是"日志到底有没有在写"的唯一判据**——
    /// daemon 是别人启动的（外壳 / CLI / 手工），输出可能被重定向到 `NUL`，
    /// 那时目录里会留着一个**陈旧**的 `daemon.log`，看时间戳才知道。
    pub modified_at_millis: u64,
}

/// 日志所在的目录。
///
/// 与数据目录**同一个**（`daemon.log` / `shell.log` 都在里面），所有者是
/// `daemon.rs` 的 `v2_data_dir()`——这里只是**转述**：调它而不是自己拼一遍目录名，
/// 免得两个名字漂掉（数据目录守门只认 daemon.rs 里那一处 `path.join`）。
pub fn log_directory() -> Option<std::path::PathBuf> {
    crate::v2::daemon::v2_data_dir().ok()
}

/// 目录里现存的日志文件（按名字排序；目录不存在就是空）。
pub fn log_files() -> Vec<LogFile> {
    let Some(dir) = log_directory() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<LogFile> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("daemon.log") && !name.starts_with("shell.log") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            let modified_at_millis = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64)
                .unwrap_or(0);
            Some(LogFile {
                name,
                bytes: meta.len(),
                modified_at_millis,
            })
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "renderloft-env-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
        dir
    }

    /// 负对照：把 `console_dist_root` 里"可执行文件旁"那一段删掉 ⇒ 这条会红。
    ///
    /// 钉住的是"安装到别的机器还能打开界面"这件事：安装包里 `dist/` 就在 daemon 旁边，
    /// 而编译期源码路径在别人的机器上不存在。
    #[test]
    fn console_dist_prefers_the_copy_next_to_the_daemon_binary() {
        let root = temp_dir("dist-root");
        let install = root.join("resources");
        std::fs::create_dir_all(install.join("dist")).expect("造一个安装目录布局应成功");
        assert_eq!(
            console_dist_root(None, Some(install.clone())),
            install.join("dist"),
            "daemon 旁边就有 dist 时必须用它，否则装到别人机器上界面打不开"
        );
        // 显式覆盖优先于一切（开发与 CI 用它指到别处）
        let explicit = root.join("explicit");
        assert_eq!(
            console_dist_root(Some(explicit.clone()), Some(install)),
            explicit
        );
        // exe 旁边没有 dist（开发机的 target/debug）⇒ 退回源码树
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).expect("创建空目录应成功");
        assert!(
            console_dist_root(None, Some(bare)).ends_with("dist"),
            "退回源码树的 dist"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 白名单解析：空串与多余逗号都不该产出空条目（空条目等于"谁都放行"，
    /// 那不是用户写 `A,,B` 时的意思）。
    #[test]
    fn parse_hosts_ignores_empty_entries() {
        assert_eq!(
            parse_hosts("a,, b ,"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(parse_hosts("  ").is_empty());
        assert_eq!(parse_hosts("127.0.0.1"), vec!["127.0.0.1".to_string()]);
    }
}
