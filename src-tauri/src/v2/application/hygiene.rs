use std::sync::Arc;

use crate::v2::domain::errors::DomainResult;
use crate::v2::domain::model::Timestamp;
use crate::v2::ports::repositories::{OperationRepository, SettingsRepository};

/// 陈旧未终态操作的清扫时限（操作级兜底，独立于 run 级 tracking_deadline）。
/// 单任务很少超过 1 小时，但重试/不确定提交可能跨时段，取 6 小时作为"明显已被中断"的判据。
pub const HYGIENE_STALE_AFTER_MILLIS: u64 = 6 * 60 * 60 * 1000;
/// 每轮清扫上限，避免一轮事务阻塞过久。
const HYGIENE_SWEEP_LIMIT: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HygieneResult {
    pub stale_failed: usize,
    pub purged: usize,
}

pub struct HygieneService {
    pub repositories: Arc<dyn OperationRepository>,
    /// 历史保留天数的来源。`0` = **永久保留**（不清理，见 `tick`）。
    pub settings: Arc<dyn SettingsRepository>,
}

impl HygieneService {
    /// 周期清理两步：
    /// ① 陈旧未终态操作置为 FAILED —— 修复 UI 永久"执行中"的孤儿操作（无 run 或
    ///    tracking_deadline 被轮询报错清空的 run 都不会被 reconcile 终结）；
    /// ② 超期已终态历史记录级联删除 —— 控制 DB 增长，避免无限膨胀。
    pub async fn tick(&self) -> DomainResult<HygieneResult> {
        let now = Timestamp::now_millis();

        let stale_cutoff = Timestamp::from_millis(now.saturating_sub(HYGIENE_STALE_AFTER_MILLIS));
        let stale = self
            .repositories
            .list_stale_operations(stale_cutoff, HYGIENE_SWEEP_LIMIT)
            .await?;
        let mut stale_failed = 0usize;
        for operation_id in stale {
            let changed = self
                .repositories
                .force_fail_operation(operation_id, "stale-execution")
                .await?;
            // 出事了：这是"UI 上那个永远转圈的任务"被收掉的时刻，值得留一行——
            // 否则用户只能看到它某天忽然变成了失败，不知道为什么。
            if changed > 0 {
                crate::v2::logging::log_failure(
                    "hygiene",
                    &format!("op={}", operation_id.0),
                    "STALE_EXECUTION",
                    "陈旧未终态操作被判失败（超过 6 小时）",
                );
            }
            stale_failed += changed;
        }

        // 保留期由**设置**决定（默认 30 天，`0` = 永久保留）：这是"我想留多久"的问题，
        // 属于用户策略，不该由代码里的常量替用户决定。读到设置失败就按默认走——
        // 清扫挂掉比多删一点更糟，而默认值与旧行为逐字一致。
        let retain_days = self
            .settings
            .load_settings()
            .await
            .map(|record| record.history_retain_days)
            .unwrap_or(crate::v2::ports::repositories::DEFAULT_HISTORY_RETAIN_DAYS);
        let purged = match purge_cutoff(now, retain_days) {
            // `0` = 永久保留：不清理，连查询都不发。
            None => 0,
            Some(retain_cutoff) => {
                let old_terminal = self
                    .repositories
                    .list_old_terminal_operations(retain_cutoff, HYGIENE_SWEEP_LIMIT)
                    .await?;
                self.repositories.delete_operations(&old_terminal).await?
            }
        };

        Ok(HygieneResult {
            stale_failed,
            purged,
        })
    }
}

/// 保留期 → 清理截止点。`0` = **永久保留** ⇒ `None`（一条都不清）。
///
/// 为什么抽成纯函数：真库里"30 天前的记录"在测试里造不出来（写入路径会把 `updated_at`
/// 刷成 now），所以"0 到底是不是不删"这条判据只能落在不需要数据的算术上——
/// 顺便也让"天 → 毫秒"的那种手滑（写成 24*60*60 而不是 *1000）无处可藏。
pub fn purge_cutoff(now_millis: u64, retain_days: u64) -> Option<Timestamp> {
    if retain_days == 0 {
        return None;
    }
    let retain_millis = retain_days.saturating_mul(24 * 60 * 60 * 1000);
    Some(Timestamp::from_millis(now_millis.saturating_sub(retain_millis)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, ActorRef};
    use crate::v2::domain::ids::{
        ClientId, ConnectionRevisionId, IdempotencyKey, OperationId, RecipeRevisionId,
    };
    use crate::v2::domain::operation::{Operation, OperationStatus, RunStatus};
    use crate::v2::infrastructure::fakes::InMemoryRepositories;
    use crate::v2::ports::repositories::{NewOperation, NewRun, SettingsRecord};

    async fn seed(
        repositories: &Arc<InMemoryRepositories>,
        idempotency_key: &str,
    ) -> OperationId {
        let now = Timestamp::now();
        let operation = Operation {
            id: OperationId(0),
            actor: ActorRef {
                id: "test".to_string(),
                kind: ActorKind::Cli,
                client_id: ClientId("test".to_string()),
                scopes: vec![],
                label: None,
            },
            origin: "test".to_string(),
            idempotency_key: IdempotencyKey(idempotency_key.to_string()),
            request_hash: "hash".to_string(),
            status: OperationStatus::Queued,
            metadata: Default::default(),
            callback: None,
            created_at: now.clone(),
            updated_at: now,
        };
        repositories
            .create_operation(NewOperation {
                operation,
                runs: vec![NewRun {
                    recipe_revision_id: RecipeRevisionId(1),
                    connection_revision_id: ConnectionRevisionId(1),
                    client_run_id: "run-1".to_string(),
                    inputs: Default::default(),
                    assets: vec![],
                }],
            })
            .await
            .expect("seed 应成功")
            .operation
            .id
    }

    fn service(repositories: Arc<InMemoryRepositories>) -> HygieneService {
        HygieneService {
            repositories: repositories.clone(),
            settings: repositories,
        }
    }

    async fn save_retention(repositories: &Arc<InMemoryRepositories>, days: u64) {
        repositories
            .save_settings(&SettingsRecord {
                download_dir: String::new(),
                history_retain_days: days,
            })
            .await
            .expect("保存设置应成功");
    }

    /// "保留多少天"到"截止点在哪"的算术：`0` 必须是 `None`（永久），30 天必须是 30×86400000ms。
    ///
    /// 为什么只测这个纯函数：真库里"30 天前的记录"造不出来（写入路径会把 `updated_at`
    /// 刷成 now），所以"0 到底删不删"这条判据只能落在这里。
    #[test]
    fn retention_zero_means_never_and_other_values_become_a_cutoff() {
        let now = 1_700_000_000_000u64;
        assert!(
            purge_cutoff(now, 0).is_none(),
            "0 = 永久保留，不该算出截止点"
        );
        assert_eq!(
            purge_cutoff(now, 30).unwrap().millis(),
            now - 30 * 86_400_000,
            "天 → 毫秒一旦写错（少乘 1000），保留期就从 30 天变成 43 分钟"
        );
    }

    #[tokio::test]
    async fn a_zero_retention_setting_purges_nothing() {
        let repositories = Arc::new(InMemoryRepositories::default());
        save_retention(&repositories, 0).await;
        let out = service(repositories).tick().await.unwrap();
        assert_eq!(out.purged, 0);
    }

    #[tokio::test]    async fn empty_repo_tick_is_noop() {
        let repositories = Arc::new(InMemoryRepositories::default());
        let out = service(repositories).tick().await.unwrap();
        assert_eq!(out, HygieneResult::default());
    }

    #[tokio::test]
    async fn fresh_non_terminal_operation_is_not_cleaned() {
        let repositories = Arc::new(InMemoryRepositories::default());
        let operation_id = seed(&repositories, "fresh-key").await;
        let out = service(repositories.clone()).tick().await.unwrap();
        assert_eq!(out.stale_failed, 0);
        assert_eq!(out.purged, 0);
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .expect("不应被清理");
        // 刚创建的 QUEUED 任务必须保持原样，绝不因清扫误杀。
        assert_eq!(persisted.operation.status, OperationStatus::Queued);
        assert_eq!(persisted.runs[0].status, RunStatus::Queued);
    }
}
