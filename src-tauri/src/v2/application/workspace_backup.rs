//! 生成一份**一致的**工作区快照（迁移 / 备份用）。
//!
//! 为什么需要它：工作区就是一堆普通文件（一个 SQLite 库 + 一个 `artifacts\` 目录），所以
//! "换机器＝拷目录"这个直觉本身是对的——**但运行中直接拷那个库会坏**：库跑在 WAL 模式下，
//! 库文件和 `-wal` / `-shm` 是活的，拷出来可能缺最近的事务、甚至打不开，而且**不会报错**。
//! 这里用 `VACUUM INTO` 导一份自洽的库，再把产物缓存整份拷过去。
//!
//! **恢复不需要对应的功能**：`daemon stop` → 用备份替换工作区目录 → 任意一条 CLI 命令会把
//! daemon 拉起来（"启动但不拥有"）。密钥永远不在这份快照里——它们在系统凭据库，换机器必须
//! **由人重填**，这是设计，不是缺口。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::v2::domain::errors::{DomainResult, SafeError};
// 库文件名从端口层取（它是工作区契约的一部分）：application 不该为了一个常量去依赖组合根，
// 分层守门也会拦。daemon 那边的 V2_DB_FILE 现在只是这个常量的别名。
use crate::v2::ports::repositories::{WorkspaceRepository, WORKSPACE_DB_FILE};

/// 备份：把工作区导到调用方指定的目录。
pub struct WorkspaceBackupService {
    workspace: Arc<dyn WorkspaceRepository>,
}

/// 备份结果——把调用方需要的事实交给它（备份到哪了、里面有多少东西），别让它自己去数。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    /// 快照所在目录。
    pub path: String,
    /// 库文件大小（字节）。
    pub database_bytes: u64,
    /// 拷过去的产物缓存文件数。
    pub artifact_count: usize,
    /// 拷过去的产物缓存总字节数。
    pub artifact_bytes: u64,
}

impl WorkspaceBackupService {
    pub fn new(workspace: Arc<dyn WorkspaceRepository>) -> Self {
        Self { workspace }
    }

    /// 把工作区导到 `target` 目录。
    ///
    /// 四道校验都是**拒绝**而不是"纠正"：备份是数据安全动作，猜一个路径比报错危险得多。
    pub async fn backup(&self, target: &str) -> DomainResult<BackupReport> {
        let target = target.trim();
        if target.is_empty() {
            return Err(SafeError::validation(
                "BACKUP_TARGET_REQUIRED",
                "请给出备份目录（CLI 是 `--to <目录>`）。",
            ));
        }
        let target = PathBuf::from(target);

        // 不许把快照写进工作区自己：那等于往"正在被快照的东西"里写东西，而且会自我包含。
        if let Some(workspace_dir) = self
            .workspace
            .database_path()
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        {
            if target.starts_with(&workspace_dir) {
                return Err(SafeError::validation(
                    "BACKUP_TARGET_INSIDE_WORKSPACE",
                    format!(
                        "备份目录不能在工作区里（{}）。换一个目录——写进工作区会让快照包含它自己。",
                        workspace_dir.display()
                    ),
                ));
            }
        }

        if target.exists() && !target.is_dir() {
            return Err(SafeError::validation(
                "BACKUP_TARGET_NOT_A_DIRECTORY",
                format!("{} 已经存在，而且不是一个目录。", target.display()),
            ));
        }
        std::fs::create_dir_all(&target)
            .map_err(|error| SafeError::storage(format!("无法创建备份目录：{error}")))?;

        // 已有一份快照就**报错**，不覆盖：备份是"再存一份"，不是"改掉上一份"。
        let snapshot_database = target.join(WORKSPACE_DB_FILE);
        if snapshot_database.exists() {
            return Err(SafeError::conflict(
                "BACKUP_TARGET_NOT_EMPTY",
                format!(
                    "{} 里已经有一份备份了。换一个目录，或者先把旧的挪走——这里不覆盖已有备份。",
                    target.display()
                ),
            ));
        }

        self.workspace.snapshot_database(&snapshot_database).await?;
        let (artifact_count, artifact_bytes) =
            copy_artifacts(self.workspace.artifact_dir().as_deref(), &target)?;
        let database_bytes = std::fs::metadata(&snapshot_database)
            .map(|meta| meta.len())
            .unwrap_or(0);

        Ok(BackupReport {
            path: target.to_string_lossy().to_string(),
            database_bytes,
            artifact_count,
            artifact_bytes,
        })
    }
}

/// 把产物缓存目录整份拷到备份目录里（没有缓存目录就跳过，不是错误）。
///
/// 递归是**防御性**的：今天的实现把产物平铺成 `<id>` 文件，但缓存目录的形状是实现细节，
/// 备份不该假设它永远平铺。
fn copy_artifacts(source: Option<&Path>, target: &Path) -> DomainResult<(usize, u64)> {
    let Some(source) = source else {
        return Ok((0, 0));
    };
    if !source.is_dir() {
        return Ok((0, 0));
    }
    let destination = target.join("artifacts");
    let mut count = 0;
    let mut bytes = 0;
    copy_tree(source, &destination, &mut count, &mut bytes)?;
    Ok((count, bytes))
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    count: &mut usize,
    bytes: &mut u64,
) -> DomainResult<()> {
    std::fs::create_dir_all(destination)
        .map_err(|error| SafeError::storage(format!("无法创建产物备份目录：{error}")))?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| SafeError::storage(format!("无法读取产物缓存目录：{error}")))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| SafeError::storage(format!("无法读取产物缓存条目：{error}")))?;
        let path = entry.path();
        let name = entry.file_name();
        let to = destination.join(&name);
        if path.is_dir() {
            copy_tree(&path, &to, count, bytes)?;
            continue;
        }
        let copied = std::fs::copy(&path, &to).map_err(|error| {
            SafeError::storage(format!("无法拷贝产物 {}：{error}", path.display()))
        })?;
        *count += 1;
        *bytes += copied;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::infrastructure::sqlite::SqliteRepositories;
    use crate::v2::ports::repositories::{SettingsRecord, SettingsRepository};

    /// 每个用例一个独立目录（同一个进程里测试是并行的，别互相踩）。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("renderloft-backup-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    /// 真实文件库 + 服务。**不用替身**：这条路径的全部价值就在"真库能不能被一致地导出"。
    fn service(workspace: &Path) -> (WorkspaceBackupService, Arc<SqliteRepositories>) {
        let repositories =
            Arc::new(SqliteRepositories::open(&workspace.join(WORKSPACE_DB_FILE)).expect("开库"));
        let service =
            WorkspaceBackupService::new(repositories.clone() as Arc<dyn WorkspaceRepository>);
        (service, repositories)
    }

    /// **验收判据**：备份出来的库能被**重新打开**，而且数据与源一致；产物缓存一起过去。
    #[tokio::test]
    async fn a_backup_reopens_and_carries_the_data() {
        let workspace = temp_dir("happy");
        let (service, repositories) = service(&workspace);
        repositories
            .save_settings(&SettingsRecord {
                download_dir: "D:/pics".to_string(),
                history_retain_days: 30,
            })
            .await
            .expect("写设置");
        std::fs::write(workspace.join("artifacts").join("5000"), b"png-bytes").expect("放个产物");

        let target = temp_dir("happy-out");
        let report = service
            .backup(&target.to_string_lossy())
            .await
            .expect("备份应当成功");
        assert_eq!(report.artifact_count, 1);
        assert_eq!(report.artifact_bytes, 9);
        assert!(report.database_bytes > 0, "库文件不该是空的：{report:?}");

        // 用**全新的连接**打开备份里的库——这才是"备份可用"的证明。
        let reopened = SqliteRepositories::open(&target.join(WORKSPACE_DB_FILE))
            .expect("备份库应当能重新打开");
        assert_eq!(
            reopened.load_settings().await.expect("读设置").download_dir,
            "D:/pics",
            "备份里的数据必须与源一致"
        );
        assert_eq!(
            std::fs::read(target.join("artifacts").join("5000")).expect("产物也应当被拷过去"),
            b"png-bytes"
        );

        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&target);
    }

    /// 负对照：备份目录在**工作区里** ⇒ 拒绝（写进工作区会让快照包含它自己）。
    #[tokio::test]
    async fn a_target_inside_the_workspace_is_refused() {
        let workspace = temp_dir("inside");
        let (service, _) = service(&workspace);
        let error = service
            .backup(&workspace.join("backup").to_string_lossy())
            .await
            .expect_err("工作区内的备份目录必须被拒");
        assert_eq!(error.code, "BACKUP_TARGET_INSIDE_WORKSPACE");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    /// 负对照：目标里**已经有一份备份** ⇒ 拒绝，不覆盖（备份是"再存一份"，不是"改掉上一份"）。
    #[tokio::test]
    async fn an_existing_backup_is_not_overwritten() {
        let workspace = temp_dir("existing");
        let (service, _) = service(&workspace);
        let target = temp_dir("existing-out");
        std::fs::write(target.join(WORKSPACE_DB_FILE), b"previous-backup").expect("占位");
        let error = service
            .backup(&target.to_string_lossy())
            .await
            .expect_err("已有备份必须被拒");
        assert_eq!(error.code, "BACKUP_TARGET_NOT_EMPTY");
        assert_eq!(
            std::fs::read(target.join(WORKSPACE_DB_FILE)).expect("旧备份应当原样"),
            b"previous-backup",
            "不许动已有的那份"
        );
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&target);
    }

    /// 负对照：目标是**已存在的文件** ⇒ 拒绝（不能把一个文件当目录用）。
    #[tokio::test]
    async fn a_target_that_is_a_file_is_refused() {
        let workspace = temp_dir("file");
        let (service, _) = service(&workspace);
        let target = workspace.parent().expect("父目录").join(format!(
            "renderloft-backup-file-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&target, b"x").expect("写个文件");
        let error = service
            .backup(&target.to_string_lossy())
            .await
            .expect_err("文件当备份目录必须被拒");
        assert_eq!(error.code, "BACKUP_TARGET_NOT_A_DIRECTORY");
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    /// 负对照：没给目录 ⇒ 拒绝（不猜一个默认位置）。
    #[tokio::test]
    async fn a_blank_target_is_refused() {
        let workspace = temp_dir("blank");
        let (service, _) = service(&workspace);
        let error = service.backup("   ").await.expect_err("空目标必须被拒");
        assert_eq!(error.code, "BACKUP_TARGET_REQUIRED");
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
