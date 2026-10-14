use std::sync::Arc;

use crate::v2::domain::errors::DomainResult;
use crate::v2::domain::model::Timestamp;
use crate::v2::ports::repositories::OperationRepository;

/// 陈旧未终态操作的清扫时限（操作级兜底，独立于 run 级 tracking_deadline）。
/// 单任务很少超过 1 小时，但重试/不确定提交可能跨时段，取 6 小时作为"明显已被中断"的判据。
pub const HYGIENE_STALE_AFTER_MILLIS: u64 = 6 * 60 * 60 * 1000;
/// 已终态历史操作的保留期限（超期清理，含产物/事件，防止 DB 无限膨胀）。
pub const HYGIENE_RETAIN_MILLIS: u64 = 30 * 24 * 60 * 60 * 1000;
/// 每轮清扫上限，避免一轮事务阻塞过久。
const HYGIENE_SWEEP_LIMIT: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HygieneResult {
    pub stale_failed: usize,
    pub purged: usize,
}

pub struct HygieneService {
    pub repositories: Arc<dyn OperationRepository>,
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

        let retain_cutoff = Timestamp::from_millis(now.saturating_sub(HYGIENE_RETAIN_MILLIS));
        let old_terminal = self
            .repositories
            .list_old_terminal_operations(retain_cutoff, HYGIENE_SWEEP_LIMIT)
            .await?;
        let purged = self.repositories.delete_operations(&old_terminal).await?;

        Ok(HygieneResult {
            stale_failed,
            purged,
        })
    }
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
    use crate::v2::ports::repositories::{NewOperation, NewRun};

    async fn seed(
        repositories: &Arc<dyn OperationRepository>,
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

    fn service(repositories: Arc<dyn OperationRepository>) -> HygieneService {
        HygieneService { repositories }
    }

    #[tokio::test]
    async fn empty_repo_tick_is_noop() {
        let repositories: Arc<dyn OperationRepository> = Arc::new(InMemoryRepositories::default());
        let out = service(repositories).tick().await.unwrap();
        assert_eq!(out, HygieneResult::default());
    }

    #[tokio::test]
    async fn fresh_non_terminal_operation_is_not_cleaned() {
        let repositories: Arc<dyn OperationRepository> = Arc::new(InMemoryRepositories::default());
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
