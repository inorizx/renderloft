use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::v2::application::credential_reference;
use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::execution::{RecoveryProof, SubmitOutcome};
use crate::v2::domain::ids::ArtifactId;
use crate::v2::domain::model::Timestamp;
use crate::v2::domain::operation::Transition;
use crate::v2::ports::provider::{driver_key, DriverRegistry, PrepareRequest, ProviderDriver, QueueCapacity};
use crate::v2::ports::repositories::{
    AssetRepository, AttemptResolution, ConnectionRepository, EventSpec, LeaseStore,
    OperationRepository,
};
use crate::v2::ports::stores::ArtifactStore;

pub const LEASE_TTL_SECONDS: u64 = 30;
// 「默认跟踪死线」那个数字的所有者是 `domain::connection_options`（这里原来也有一份，
// 而且从来没被用过——真正用它的是 `Run::apply_tracking` 与两处恢复路径）。
/// 可重试错误（限流/5xx）进入 RETRY_WAIT 后的退避间隔。
pub const RETRY_BACKOFF_MILLIS: u64 = 30_000;

#[derive(Debug, Clone, Default)]
pub struct SchedulerTickResult {
    pub claimed: usize,
    pub submitted: usize,
    pub succeeded: usize,
    pub accepted: usize,
    pub uncertain: usize,
    pub retry_wait: usize,
    pub failed: usize,
    pub recovered: usize,
    /// 本轮从别的连接"叫过来"的、从未尝试过的排队 run 数（账号池的再平衡）。
    pub adopted: usize,
    pub busy: bool,
}

pub struct SchedulerService {
    pub owner: String,
    pub repositories: Arc<dyn OperationRepository>,
    pub leases: Arc<dyn LeaseStore>,
    pub connections: Arc<dyn ConnectionRepository>,
    pub assets: Arc<dyn AssetRepository>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub drivers: Arc<DriverRegistry>,
    pub capacity: CapacityCache,
    pub paused: Arc<PausedAccounts>,
}

/// 并发容量是**平台说的事实**，但不必每个 tick 都问一次。
///
/// tick 每 2 秒一轮：100 条连接直接问 = 50 req/s，平台会先以"请求太快"回敬我们。
/// 30 秒够用——容量是订阅档位，不是秒级变化；真变了（被平台拒）我们会**立刻**丢掉缓存重问，
/// 所以这里的"旧"最多只让一次提交白撞，而那次拒绝不花钱。
const CAPACITY_TTL: Duration = Duration::from_secs(30);

/// 账号级冻结：某个成员的 key 失效 / 余额不足时，暂时不让它接新单。
///
/// **刻意不做"熔断器"**：没有阈值、没有半开状态、没有探测请求。只有一条规则——
/// 是谁的错就停谁；`PAUSE_TTL` 到点自动放行一次，让修好的 key 重新进来
/// （修不好就再冻 10 分钟，一条 run 的代价）。
const PAUSE_TTL: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct PausedAccounts {
    entries: std::sync::Mutex<HashMap<u64, (Instant, String)>>,
}

impl PausedAccounts {
    /// 被冻结则返回原因；过期的顺手清掉。
    fn reason(&self, revision_id: u64) -> Option<String> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (until, reason) = entries.get(&revision_id)?;
        if Instant::now() < *until {
            return Some(reason.clone());
        }
        entries.remove(&revision_id);
        None
    }

    fn pause(&self, revision_id: u64, reason: String) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(revision_id, (Instant::now() + PAUSE_TTL, reason));
    }

    /// 诊断用：当前被冻结的连接 revision 与原因。
    pub fn snapshot(&self) -> Vec<(u64, String)> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.retain(|_, (until, _)| now < *until);
        entries
            .iter()
            .map(|(id, (_, reason))| (*id, reason.clone()))
            .collect()
    }
}

#[derive(Default)]
pub struct CapacityCache {
    entries: std::sync::Mutex<HashMap<u64, (Instant, Option<QueueCapacity>)>>,
}

impl CapacityCache {
    /// `Some(...)` = 缓存有效（内层 `None` 表示"平台没有这个说法/查不到"）。
    fn get(&self, revision_id: u64) -> Option<Option<QueueCapacity>> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (at, capacity) = entries.get(&revision_id)?;
        (at.elapsed() < CAPACITY_TTL).then_some(*capacity)
    }

    fn put(&self, revision_id: u64, capacity: Option<QueueCapacity>) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(revision_id, (Instant::now(), capacity));
    }

    fn invalidate(&self, revision_id: u64) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&revision_id);
    }
}

impl SchedulerService {
    /// 完成 attempt 并推进 run 状态。若 attempt 已被 cancel fencing
    /// （complete_attempt 返回 None），run 已取消，跳过 transition，
    /// 避免 ILLEGAL_RUN_TRANSITION 噪音。
    async fn complete_and_transition(
        &self,
        run_id: crate::v2::domain::ids::RunId,
        fencing_token: &str,
        resolution: AttemptResolution,
        transition: Transition,
        event: EventSpec,
    ) -> DomainResult<()> {
        let completed = self
            .repositories
            .complete_attempt(run_id, fencing_token, resolution)
            .await?;
        if completed.is_none() {
            return Ok(());
        }
        self.repositories
            .transition_run(run_id, transition, vec![event])
            .await?;
        Ok(())
    }

    pub async fn tick(&self) -> DomainResult<SchedulerTickResult> {
        let token = format!("sch-{}", uuid::Uuid::new_v4().simple());
        let mut result = SchedulerTickResult::default();
        if self
            .leases
            .acquire(&self.owner, &token, LEASE_TTL_SECONDS)
            .await
            .is_err()
        {
            result.busy = true;
            return Ok(result);
        }
        let outcome = self.tick_claimed(&token).await;
        let _ = self.leases.release(&self.owner, &token).await;
        let mut result = outcome?;
        result.recovered = self.repositories.recover_expired_submissions().await?;
        Ok(result)
    }

    /// 这条连接此刻的并发容量（平台声明）。查不到就 `None`，由调用方退回静态槽位。
    async fn capacity_for(
        &self,
        revision: &crate::v2::domain::model::ConnectionRevision,
        driver: &dyn ProviderDriver,
    ) -> Option<QueueCapacity> {
        if let Some(cached) = self.capacity.get(revision.id.0) {
            return cached;
        }
        // 查询失败不使 tick 失败：退回静态槽位（保守），30 秒后再试。
        let capacity = match driver.fetch_queue_status(revision).await {
            Ok(status) => driver.queue_capacity(&status),
            Err(_) => None,
        };
        self.capacity.put(revision.id.0, capacity);
        capacity
    }

    async fn tick_claimed(&self, token: &str) -> DomainResult<SchedulerTickResult> {
        let mut result = SchedulerTickResult::default();
        let now = Timestamp::now();
        let revisions = self.connections.list_revisions().await?;

        /// 一条连接这一轮还吃得下几条。
        struct Plan {
            revision: crate::v2::domain::model::ConnectionRevision,
            driver: Arc<dyn ProviderDriver>,
            remaining: usize,
            /// 这条连接上还没提交的 run 数（含别的批次）。
            queued: usize,
        }

        // 第一遍：先把每条连接的剩余容量算清楚。**必须分两遍**——"叫号"得先知道谁排满了。
        let mut plans = Vec::new();
        for (vendor, protocol, revision) in revisions {
            let key = driver_key(&vendor, &protocol);
            let Ok(driver) = self.drivers.get(&key) else {
                continue;
            };
            // 账号级冻结（key 失效 / 余额不足）：这一轮完全不碰它，也不往里叫号。
            if self.paused.reason(revision.id.0).is_some() {
                continue;
            }
            // 「这个 provider 总共允许几个并发」的静态值，只在平台不报容量时兜底。
            let base_slots = driver.available_slots().max(1);
            // ⚠️ 光看静态槽位不够：tick 2 会在 tick 1 的任务还在跑时继续提交，被平台拒。
            // 实测（并发上限为 1 的 RunningHub 账号）：同时提交 2 个任务，第 2 个必然以
            // `RUNNINGHUB_REJECTED / TASK_QUEUE_MAXED` 失败，而这个**我们自己造成的**失败
            // 会被当成"任务失败"报给用户。所以先扣掉已经在跑的那几个。
            let in_flight = self
                .repositories
                .count_active_runs_for_connection(revision.id)
                .await?;
            // 平台报的 `runningCount` 含**所有来源**（网页控制台、别的工具），比本地账本更准；
            // 但两边都可能滞后，所以取大的那个：宁可少领一条，也不要撞一次拒绝。
            let capacity = self.capacity_for(&revision, driver.as_ref()).await;
            let used = match capacity {
                Some(capacity) => capacity.running.max(in_flight),
                None => in_flight,
            };
            let limit = capacity.map(|capacity| capacity.limit).unwrap_or(base_slots);
            let queued = self
                .repositories
                .count_queued_runs_for_connection(revision.id)
                .await?;
            plans.push(Plan {
                remaining: limit.saturating_sub(used),
                queued,
                revision,
                driver,
            });
        }

        // 第二遍：把"别的连接多出来的活"叫到有空位的连接上。
        //
        // 判据两条，缺一条就会走向反面：
        // * **来源**的"多出来" = 排队数 − 剩余容量 > 0（它本轮吃不下）；
        // * **目标**的"吃得下" = 剩余容量 − 已有排队 > 0（别把自己撑爆）。
        //
        // 第一版只判"目标有空位"，于是 3+1 的池子上 4 条会全堆到 3 个位那边、1 个位那边空转；
        // 只判"来源排满"又会让连续提交的小批次都停在第一条连接上（它没满就不叫号）。
        let mut surplus: Vec<(crate::v2::domain::ids::ConnectionRevisionId, usize)> = plans
            .iter()
            .map(|plan| (plan.revision.id, plan.queued.saturating_sub(plan.remaining)))
            .filter(|(_, extra)| *extra > 0)
            .collect();
        if !surplus.is_empty() {
            for plan in plans.iter_mut() {
                let mut budget = plan.remaining.saturating_sub(plan.queued);
                if budget == 0 {
                    continue;
                }
                for (source, extra) in surplus.iter_mut() {
                    if budget == 0 {
                        break;
                    }
                    if *source == plan.revision.id || *extra == 0 {
                        continue;
                    }
                    let taken = self
                        .repositories
                        .adopt_queued_runs(
                            plan.revision.id,
                            &[*source],
                            budget.min(*extra),
                        )
                        .await?;
                    result.adopted += taken;
                    *extra = extra.saturating_sub(taken);
                    budget = budget.saturating_sub(taken);
                }
            }
        }

        // 第三遍：领单、提交。一个 tick 内仍是顺序提交（平台只要求"同时在跑几个"，
        // 不要求我们并发发起请求）。
        for plan in plans {
            for _ in 0..plan.remaining {
                let Some(claimed) = self
                    .repositories
                    .claim_next_run(&self.owner, token, plan.revision.id, now.clone())
                    .await?
                else {
                    break;
                };
                result.claimed += 1;
                match self.execute_claimed(&claimed, plan.driver.clone()).await {
                    Ok(outcome) => match outcome {
                        ExecuteOutcome::Succeeded => result.succeeded += 1,
                        ExecuteOutcome::Accepted => result.accepted += 1,
                        ExecuteOutcome::Uncertain => result.uncertain += 1,
                        ExecuteOutcome::RetryWait {
                            capacity_exhausted,
                        } => {
                            result.retry_wait += 1;
                            if capacity_exhausted {
                                // 平台刚说了"满了"：本轮**不再领**（接着领只会连撞三次，
                                // 让三条 run 一起进退避），并丢掉缓存让下个 tick 重新问。
                                self.capacity.invalidate(plan.revision.id.0);
                                break;
                            }
                        }
                        ExecuteOutcome::Failed => result.failed += 1,
                        ExecuteOutcome::Cancelled => {}
                    },
                    Err(_) => {
                        // 调度错误不使整个 tick 失败；Run 已通过 claim 被标记，留在 SUBMITTING
                        // 由 recover_expired_submissions 在下一轮转为 UNCERTAIN。
                        result.uncertain += 1;
                    }
                }
                result.submitted += 1;
            }
        }
        Ok(result)
    }

    async fn execute_claimed(
        &self,
        claimed: &crate::v2::ports::repositories::ClaimedRun,
        driver: Arc<dyn crate::v2::ports::provider::ProviderDriver>,
    ) -> DomainResult<ExecuteOutcome> {
        let run_id = claimed.run.id;
        // ⚠️ **退避重试有终点**（2026-10-18）：认领到一条带着**已过期**死线的 run，说明它上一轮
        // 失败进了 `RETRY_WAIT`、而重试窗口已经用完 ⇒ 判失败，**不再提交**。
        //
        // 为什么判在"下一次重试本该发出去的那一刻"，而不是另起一个定时清扫：
        // ① 不需要新查询、新列、新循环 —— 这条路径本来就会在 `next_poll_at` 到点时被认领；
        // ② 它天然容忍"daemon 当时没在跑"：开机后第一次 tick 就会认领并判掉，不会漏；
        // ③ 时机与语义完全重合：**最后一次机会用完了**。
        //
        // 判据用 `tracking_deadline` 区分"这条是从 `RETRY_WAIT` 来的"：刚建出来的 run（`QUEUED`）
        // 没有死线；而被 scheduler 认领的只可能是 `QUEUED` / `RETRY_WAIT` 两种（`claim_next_run`
        // 的 SQL 就是这两个状态），`ACCEPTED` / `RUNNING` / 不确定态都由 reconcile 管，走不到这里。
        if claimed
            .run
            .tracking_deadline
            .as_ref()
            .is_some_and(|deadline| deadline.is_expired())
        {
            // 这条 run 不会再有下一次尝试：判**终态失败**（`retryable: false`，否则下一次
            // `resolve_submit_error` 又会把它排回 `RETRY_WAIT`，死循环）。
            let error = SafeError {
                code: "RETRY_EXHAUSTED".to_string(),
                kind: crate::v2::domain::errors::ErrorKind::Provider,
                message: "退避重试到期仍未成功：平台一直没有接受这次提交（上一次失败见该 attempt 的 error）。\
                     要再来一次用 operation retry —— 那是**新建一次提交**（是否重复计费取决于上次失败的性质：\
                     被平台明确拒绝 / 连接被拒这类没有受理过，重试不会重复计费；5xx 这类平台内部错误要先确认）。"
                    .to_string(),
                retryable: false,
                details: Default::default(),
            };
            crate::v2::logging::log_failure(
                "run",
                &format!("run={}", run_id.0),
                &error.code,
                &error.message,
            );
            // 与另一条终态失败的路一样：**run 上也要留原因**（`operation get` 读的是它；
            // 只写进 attempt 的话，调用方在 run 级看到的是一个没有原因的失败）。
            self.repositories
                .set_run_error(run_id, Some(error.clone()))
                .await?;
            self.complete_and_transition(
                run_id,
                &claimed.attempt.fencing_token,
                AttemptResolution::Failed {
                    error: error.clone(),
                    retryable: false,
                },
                Transition::ToFailed,
                EventSpec {
                    event_type: "run.failed".to_string(),
                    run_id: Some(run_id),
                    safe_payload: json!({ "reason": error.message, "error": error }),
                },
            )
            .await?;
            return Ok(ExecuteOutcome::Failed);
        }
        let fingerprint = format!("run-{}:attempt-{}", run_id.0, claimed.attempt.number);
        // 凭据引用：提交前把"借来的密钥绑定"装到 revision 上（见 credential_reference）。
        // ⚠️ 失败要走 `resolve_submit_error`（与 prepare 失败同一条路）：那条路会按错误的
        // 可重试性把 run 判死或排重试。若让它从 `execute_claimed` 直接冒泡出去，调用方只会
        // 记一个 `uncertain`，run 留在 SUBMITTING 里等对账兜底——用户看到的是"卡住"，
        // 而不是"引用的连接已被删除"。
        let connection_revision = match credential_reference::resolve(
            self.connections.as_ref(),
            claimed.connection_revision.clone(),
        )
        .await
        {
            Ok(revision) => revision,
            Err(error) => {
                self.resolve_submit_error(claimed, &error).await?;
                return Ok(error_outcome(&error, false));
            }
        };
        let mut resolved_assets = Vec::with_capacity(claimed.run.assets.len());
        for asset_ref in &claimed.run.assets {
            let bytes = self
                .assets
                .read_bytes(asset_ref.asset_id)
                .await?
                .unwrap_or_default();
            resolved_assets.push(crate::v2::ports::provider::ResolvedAsset {
                asset_ref: asset_ref.clone(),
                bytes,
            });
        }
        let prepared = driver
            .prepare(PrepareRequest {
                connection_revision: connection_revision.clone(),
                recipe_binding: claimed.recipe_revision.binding.clone(),
                inputs: claimed.run.inputs.clone(),
                assets: resolved_assets,
                request_fingerprint: fingerprint,
            })
            .await;

        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.resolve_submit_error(claimed, &error).await?;
                return Ok(error_outcome(&error, false));
            }
        };

        let outcome = driver.submit(prepared).await;
        match outcome {
            Ok(SubmitOutcome::Completed(mut artifacts)) => {
                // 落库前保证同一 run 内文件名互不相同（**所有 driver 的统一保证**，不靠各 driver
                // 自己记得）：库里两条不同 id 的记录叫同一个名字，人就没法认哪张是哪张。
                // 名字由应用名 + artifactId 组成时本来就不会撞，这条只在"查不到应用名"的
                // 兜底路径上还有活干。
                crate::v2::domain::execution::ArtifactDraft::make_filenames_unique(&mut artifacts);
                for (index, artifact) in artifacts.iter().enumerate() {
                    let id = ArtifactId(run_id.0 * 1000 + index as u64);
                    // ⚠️ 名字在**落库前**定下来，不是下载时才定：这样界面、CLI、API 看到的是
                    // 同一个名字，而且它进了库——改名规则要变时只动这一处，不用回头修数据。
                    let filename = crate::v2::domain::execution::ArtifactDraft::compose_filename(
                        &claimed.recipe_name,
                        id,
                        &artifact.filename,
                    );
                    let (url, bytes) = match &artifact.data {
                        crate::v2::domain::execution::ArtifactData::Bytes(bytes) => {
                            (None, bytes.clone())
                        }
                        crate::v2::domain::execution::ArtifactData::Url { url, .. } => {
                            (Some(url.clone()), Vec::new())
                        }
                    };
                    self.artifacts
                        .store(crate::v2::ports::stores::ArtifactObject {
                            id,
                            run_id: Some(run_id.0),
                            filename,
                            mime_type: artifact.mime_type.clone(),
                            byte_size: artifact.byte_size,
                            checksum: artifact.checksum.clone(),
                            storage_key: format!("{id}"),
                            url,
                            bytes,
                        })
                        .await?;
                }
                self.complete_and_transition(
                    run_id,
                    &claimed.attempt.fencing_token,
                    AttemptResolution::Succeeded {
                        remote_handle: None,
                    },
                    Transition::ToSucceeded,
                    EventSpec {
                        event_type: "run.succeeded".to_string(),
                        run_id: Some(run_id),
                        safe_payload: json!({ "artifactCount": artifacts.len() }),
                    },
                )
                .await?;
                Ok(ExecuteOutcome::Succeeded)
            }
            Ok(SubmitOutcome::Accepted(handle)) => {
                // transition_run(ToAccepted) 通过 apply_tracking 会把 tracking 设为 Active，
                // 并补一条**默认**的 tracking_deadline，从而进入 reconcile 的 active 对账队列。
                self.complete_and_transition(
                    run_id,
                    &claimed.attempt.fencing_token,
                    AttemptResolution::Succeeded {
                        remote_handle: Some(handle),
                    },
                    Transition::ToAccepted,
                    EventSpec {
                        event_type: "run.accepted".to_string(),
                        run_id: Some(run_id),
                        safe_payload: json!({}),
                    },
                )
                .await?;
                // 这条连接**显式配了**"跟多久"就现在写定：死线是"这一次提交最多跟多久"，
                // 而只有用户知道他的工作流要跑 90 分钟还是 10 秒（写死 1 小时会把长任务
                // 本地判失败，见 domain::connection_options 的文件头）。
                //
                // 为什么在这里写、而不是让仓库层去读配置：仓库（infrastructure）拿不到
                // 连接 revision，而**策略属于 application**。`next_poll_at` 传 None =
                // "现在就到期"（`list_active_runs` 的判据里有 `OR next_poll_at IS NULL`），
                // reconcile 下一拍就会接手，并原样保留这条死线。
                if let Some(deadline) =
                    crate::v2::domain::connection_options::configured_tracking_deadline_millis(
                        &connection_revision.config,
                    )
                {
                    self.repositories
                        .set_tracking(
                            run_id,
                            crate::v2::domain::operation::TrackingState::Active,
                            None,
                            Some(crate::v2::domain::model::Timestamp::from_millis(
                                crate::v2::domain::model::Timestamp::now_millis() + deadline,
                            )),
                        )
                        .await?;
                }
                Ok(ExecuteOutcome::Accepted)
            }
            Ok(SubmitOutcome::Uncertain(submission)) => {
                let remote_handle = match &submission.recovery {
                    RecoveryProof::LookupByRequestId(request_id) => {
                        Some(crate::v2::domain::execution::RemoteHandle {
                            provider: driver.descriptor().protocol_id.clone(),
                            remote_id: request_id.clone(),
                        })
                    }
                    _ => None,
                };
                let completed = self
                    .repositories
                    .complete_attempt(
                        run_id,
                        &claimed.attempt.fencing_token,
                        AttemptResolution::Uncertain {
                            remote_handle,
                            // 驱动自己报的"不确定"没有 `SafeError`（它给的是 `submission.reason`），
                            // 那句原因进事件载荷。网络失败那条路**有**结构化错误，见
                            // `resolve_submit_error`——证据必须落到 attempt 上。
                            error: None,
                        },
                    )
                    .await?;
                // 已被 cancel fencing：放弃后续状态推进。
                if completed.is_none() {
                    return Ok(ExecuteOutcome::Cancelled);
                }
                match submission.recovery {
                    RecoveryProof::Idempotent => {
                        self.repositories
                            .transition_run(
                                run_id,
                                Transition::ToRetryWait,
                                vec![EventSpec {
                                    event_type: "run.retry_wait".to_string(),
                                    run_id: Some(run_id),
                                    safe_payload: json!({ "reason": "uncertain-idempotent" }),
                                }],
                            )
                            .await?;
                        self.repositories
                            .set_tracking(
                                run_id,
                                crate::v2::domain::operation::TrackingState::Inactive,
                                Some(Timestamp::from_millis(
                                    Timestamp::now_millis() + RETRY_BACKOFF_MILLIS,
                                )),
                                None,
                            )
                            .await?;
                        Ok(ExecuteOutcome::RetryWait { capacity_exhausted: false })
                    }
                    _ => {
                        self.repositories
                            .transition_run(
                                run_id,
                                Transition::ToSubmissionUncertain,
                                vec![EventSpec {
                                    event_type: "run.submission_uncertain".to_string(),
                                    run_id: Some(run_id),
                                    safe_payload: json!({ "reason": submission.reason }),
                                }],
                            )
                            .await?;
                        Ok(ExecuteOutcome::Uncertain)
                    }
                }
            }
            Err(error) => {
                self.resolve_submit_error(claimed, &error).await?;
                // 只有"账号的错"才冻结这个成员：容量/平台/请求的错换谁都一样，
                // 冻它只会让池子越跑越小。
                if driver.fault_scope(&error) == crate::v2::domain::errors::FaultScope::Account {
                    let reason = format!("{}：{}", error.code, error.message);
                    self.paused.pause(claimed.connection_revision.id.0, reason.clone());
                    crate::v2::logging::log_line(&format!(
                        "[renderloft-scheduler] 连接 revision {} 暂停接单（{}）",
                        claimed.connection_revision.id.0, reason
                    ));
                }
                Ok(error_outcome(
                    &error,
                    driver.is_capacity_rejection(&error),
                ))
            }
        }
    }

    async fn resolve_submit_error(
        &self,
        claimed: &crate::v2::ports::repositories::ClaimedRun,
        error: &SafeError,
    ) -> DomainResult<()> {
        let run_id = claimed.run.id;
        // 判据只有一处（`SafeError::submission_is_unknown`，领域层）：落状态与报计数、
        // 以及驱动侧"要不要报可按 id 回查的不确定提交"，必须给出同一个答案。
        let unknown = error.submission_is_unknown();
        if unknown {
            // ⚠️ run 上也要留原因（2026-10-18 补）：只写进 attempt 的话，调用方在 run 级看到的是
            // `error: null` —— 而它正处在"要不要再花一次钱"的判断里，最需要知道的是**为什么不确定**
            // （连接断了？还是平台 500？后者意味着平台**可能已经收下**）。
            // cookbook §11 那句"读 run 的 `inputs` 与 `error` 一起读"也依赖这一列真的非空。
            self.repositories
                .set_run_error(run_id, Some(error.clone()))
                .await?;
            self.repositories
                .complete_attempt(
                    run_id,
                    &claimed.attempt.fencing_token,
                    AttemptResolution::Uncertain {
                        remote_handle: None,
                        // ⚠️ 这是**唯一**会把"字节到底出没出去"的证据留下来的一步：
                        // 事件载荷只有一句 message（不含原因链），日志那一行要等到死线判失败才写。
                        // 落库之后，「连接被拒」（`details.nothingSent = true`）与
                        // 「发出去后超时」（`false`）才分得开——而这两类能不能重投是相反的回答。
                        error: Some(error.clone()),
                    },
                )
                .await?;
            self.repositories
                .transition_run(
                    run_id,
                    Transition::ToSubmissionUncertain,
                    vec![EventSpec {
                        event_type: "run.submission_uncertain".to_string(),
                        run_id: Some(run_id),
                        safe_payload: json!({ "reason": error.message }),
                    }],
                )
                .await?;
        } else if error.retryable {
            self.complete_and_transition(
                run_id,
                &claimed.attempt.fencing_token,
                AttemptResolution::Failed {
                    error: error.clone(),
                    retryable: true,
                },
                Transition::ToRetryWait,
                EventSpec {
                    event_type: "run.retry_wait".to_string(),
                    run_id: Some(run_id),
                    safe_payload: json!({ "reason": error.message }),
                },
            )
            .await?;
            // 退避：设置下次可认领时间，防止 scheduler 每 2 秒无限快速重试，
            // 对限流/5xx 错误加重 Provider 压力。
            //
            // ⚠️ 同时写下**这一档的终点**（2026-10-18）：`RETRY_WAIT` 此前**没有死线**，于是
            // "平台一直拒"就每 30 秒重试一次、**永远不结束** —— 用户看到的是一个永远「进行中」
            // 的任务，而 `attention` 也只能含糊地说"正在退避重试"（说"到点判失败"会是假话，
            // 上一批正因为这个把那半句删掉了）。现在用连接上**早就存在**的那个旋钮
            // （`trackingDeadlineSeconds`，默认 1 小时），与「等确认」同一条语义：
            // **等这么久还没成，就判失败，让人来决定下一步。**
            //
            // ⚠️ 死线**只在第一次进入这一档时写**，之后每次重试都沿用同一个（认领时它就在
            // `claimed.run` 上）：每失败一次就往后推一小时 = 等于没有死线，只是藏得更深。
            let deadline = claimed.run.tracking_deadline.clone().unwrap_or_else(|| {
                Timestamp::from_millis(
                    Timestamp::now_millis()
                        + crate::v2::domain::connection_options::configured_tracking_deadline_millis(
                            &claimed.connection_revision.config,
                        )
                        .unwrap_or(
                            crate::v2::domain::connection_options::DEFAULT_TRACKING_DEADLINE_MILLIS,
                        ),
                )
            });
            self.repositories
                .set_tracking(
                    run_id,
                    crate::v2::domain::operation::TrackingState::Inactive,
                    Some(Timestamp::from_millis(
                        Timestamp::now_millis() + RETRY_BACKOFF_MILLIS,
                    )),
                    Some(deadline),
                )
                .await?;
        } else {
            // 出事了：写一行固定形状的（用户在日志里能查到"这条为什么失败"）。
            // 只记不可重试的终态失败——可重试的那些会自己再试，记下来只会把日志灌满。
            crate::v2::logging::log_failure(
                "run",
                &format!("run={}", run_id.0),
                &error.code,
                &error.message,
            );
            self.repositories
                .set_run_error(run_id, Some(error.clone()))
                .await?;
            self.complete_and_transition(
                run_id,
                &claimed.attempt.fencing_token,
                AttemptResolution::Failed {
                    error: error.clone(),
                    retryable: false,
                },
                Transition::ToFailed,
                EventSpec {
                    event_type: "run.failed".to_string(),
                    run_id: Some(run_id),
                    safe_payload: json!({ "reason": error.message, "error": error }),
                },
            )
            .await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecuteOutcome {
    Succeeded,
    Accepted,
    Uncertain,
    /// 可重试的失败。`capacity_exhausted` = 平台明确说"执行位满了"——
    /// 本轮不该再领（接着领只会连撞，把更多 run 一起推进退避）。
    RetryWait { capacity_exhausted: bool },
    Failed,
    /// attempt 已被 cancel fencing：run 已取消，跳过一切状态推进。
    Cancelled,
}

/// 这次提交失败算哪一类结局？**只在这里判一次。**
///
/// 为什么必须单源（实测踩到）：落状态（`resolve_submit_error`）与报计数（`execute_claimed` 的
/// `Err` 分支返回 `error_outcome`）此前**各写了一份**判据。加"字节没出去 ⇒ 直接重试"这条时
/// 只改了落状态那一处 ⇒ run 进了 `RETRY_WAIT`，而 tick 记的是 `uncertain`：
/// **库里的状态与计数各说各话**，而两边单独看都"没错"。这类分歧不会报错，只会让人看着
/// 日志排查一个不存在的问题。
///
/// 判据本身在 `SafeError::submission_is_unknown`（领域层）：驱动也要用它决定"报可按 id 回查的
/// 不确定提交"还是"把错误交回去重试"，所以它不能只活在这一层。
fn error_outcome(error: &SafeError, capacity_exhausted: bool) -> ExecuteOutcome {
    if error.submission_is_unknown() {
        ExecuteOutcome::Uncertain
    } else if error.retryable {
        ExecuteOutcome::RetryWait { capacity_exhausted }
    } else {
        ExecuteOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::actor::{ActorKind, ActorRef, Scope};
    use crate::v2::domain::execution::{
        ProviderDescriptor, RecoveryProof, RemoteHandle, SubmitOutcome, UncertainSubmission,
    };
    use crate::v2::domain::ids::{
        ClientId, ConnectionId, ConnectionRevisionId, OperationId, RecipeId, RecipeRevisionId,
    };
    use crate::v2::domain::model::{
        Connection, ConnectionOverride, ConnectionRevision, LifecycleFeatures, Recipe,
        RecipeBinding, RecipeRevision, RecipeSource, RecipeVisibility,
    };
    use crate::v2::domain::operation::{AttemptStatus, RunStatus};
    use crate::v2::infrastructure::fakes::{
        completed_artifact_draft, FakeProviderDriver, InMemoryRepositories,
    };
    use crate::v2::ports::provider::driver_key;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const EXECUTE_SCOPE: Scope = Scope::OperationsExecute;

    fn actor() -> ActorRef {
        ActorRef {
            id: "actor-1".to_string(),
            kind: ActorKind::Mcp,
            client_id: ClientId("client-1".to_string()),
            scopes: vec![EXECUTE_SCOPE],
            label: None,
        }
    }

    /// 凭据引用的两个夹具：一条 RunningHub"密钥宿主"连接，以及把某条连接指向它。
    ///
    /// 复用 `seed_connection` 的覆盖语义（同 id 再种一次即替换），所以不必重建整个环境。
    fn seed_runninghub_source(repositories: &InMemoryRepositories, id: u64) {
        repositories.seed_connection(
            Connection {
                id: ConnectionId(id),
                vendor: "runninghub".to_string(),
                protocol: "runninghub".to_string(),
                display_name: "RunningHub".to_string(),
                current_revision_id: ConnectionRevisionId(id * 100),
                status: crate::v2::domain::model::ConnectionStatus::Active,
            },
            vec![ConnectionRevision {
                id: ConnectionRevisionId(id * 100),
                connection_id: ConnectionId(id),
                config: BTreeMap::from([(
                    "baseUrl".to_string(),
                    json!("https://www.runninghub.cn"),
                )]),
                secret_bindings: vec![crate::v2::domain::model::SecretBinding {
                    slot: format!("connection-{id}-api_key"),
                    resolution: crate::v2::domain::model::SecretResolution::Current,
                }],
                effective_capabilities: vec!["workflow-execution".to_string()],
                lifecycle_features: LifecycleFeatures::default(),
                created_at: Timestamp::now(),
            }],
        );
    }

    fn point_connection_at(
        repositories: &InMemoryRepositories,
        connection_id: u64,
        source_id: u64,
    ) {
        let revision = ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(connection_id),
            config: BTreeMap::from([(
                credential_reference::SOURCE_CONNECTION_ID.to_string(),
                json!(source_id),
            )]),
            secret_bindings: vec![],
            effective_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures {
                synchronous_result: true,
                remote_reconciliation: true,
                ..Default::default()
            },
            created_at: Timestamp::now(),
        };
        repositories.seed_connection(
            Connection {
                id: ConnectionId(connection_id),
                vendor: "openai".to_string(),
                protocol: "openai-images".to_string(),
                display_name: "OpenAI Images".to_string(),
                current_revision_id: ConnectionRevisionId(1),
                status: crate::v2::domain::model::ConnectionStatus::Active,
            },
            vec![revision],
        );
    }

    fn seed_environment(repositories: &InMemoryRepositories) {
        seed_environment_with_config(repositories, Default::default());
    }

    /// 与 `seed_environment` 同一套环境，但**连接的 `config` 可指定**——
    /// 用来验"连接上配的可选参数真的被用上了"（见下面的跟踪上限那条测试）。
    fn seed_environment_with_config(
        repositories: &InMemoryRepositories,
        config: BTreeMap<String, Value>,
    ) {
        let connection = Connection {
            id: ConnectionId(1),
            vendor: "openai".to_string(),
            protocol: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            current_revision_id: ConnectionRevisionId(1),
            status: crate::v2::domain::model::ConnectionStatus::Active,
        };
        let connection_revision = ConnectionRevision {
            id: ConnectionRevisionId(1),
            connection_id: ConnectionId(1),
            config,
            secret_bindings: vec![],
            effective_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures {
                synchronous_result: true,
                remote_reconciliation: true,
                ..Default::default()
            },
            created_at: Timestamp::now(),
        };
        repositories.seed_connection(connection, vec![connection_revision]);

        let recipe = Recipe {
            id: RecipeId(1),
            name: "Image Recipe".to_string(),
            description: "text to image".to_string(),
            labels: vec![],
            capability: "text-to-image".to_string(),
            current_revision_id: RecipeRevisionId(1),
            visibility: RecipeVisibility::Private,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            favorite: false,
            group: None,
            note: None,
            last_opened_at: None,
            field_labels: Default::default(),
            removed_at: None,
        };
        let revision = RecipeRevision {
            id: RecipeRevisionId(1),
            recipe_id: RecipeId(1),
            revision: 1,
            input_schema: json!({
                "type": "object",
                "required": ["prompt"],
                "properties": { "prompt": { "type": "string" } }
            }),
            output_schema: json!({ "type": "object" }),
            binding: RecipeBinding {
                driver_key: "openai-images".to_string(),
                provider_config: Default::default(),
                default_connection_id: ConnectionId(1),
                allowed_connection_ids: vec![ConnectionId(1)],
                connection_override: ConnectionOverride::Denied,
            },
            defaults: Default::default(),
            source: RecipeSource::Embedded,
            created_at: Timestamp::now(),
        };
        repositories.seed_recipe(recipe, vec![revision]);
    }

    /// 两条连接 + 一张"池子"配方（可用连接 [1, 2]）。run 归属按序号轮转。
    fn seed_pool(repositories: &InMemoryRepositories) {
        seed_environment(repositories);
        repositories.seed_connection(
            Connection {
                id: ConnectionId(2),
                vendor: "openai".to_string(),
                protocol: "openai-images".to_string(),
                display_name: "OpenAI Images 2".to_string(),
                current_revision_id: ConnectionRevisionId(2),
                status: crate::v2::domain::model::ConnectionStatus::Active,
            },
            vec![ConnectionRevision {
                id: ConnectionRevisionId(2),
                connection_id: ConnectionId(2),
                config: Default::default(),
                secret_bindings: vec![],
                effective_capabilities: vec!["text-to-image".to_string()],
                lifecycle_features: LifecycleFeatures {
                    synchronous_result: true,
                    remote_reconciliation: true,
                    ..Default::default()
                },
                created_at: Timestamp::now(),
            }],
        );
        // 同一张配方、同一个 revision id，绑定换成"两只连接都可用"。
        repositories.seed_recipe(
            Recipe {
                id: RecipeId(1),
                name: "Image Recipe".to_string(),
                description: "text to image".to_string(),
                labels: vec![],
                capability: "text-to-image".to_string(),
                current_revision_id: RecipeRevisionId(1),
                visibility: RecipeVisibility::Private,
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
                favorite: false,
                group: None,
                note: None,
                last_opened_at: None,
                field_labels: Default::default(),
                removed_at: None,
            },
            vec![RecipeRevision {
                id: RecipeRevisionId(1),
                recipe_id: RecipeId(1),
                revision: 1,
                input_schema: json!({
                    "type": "object",
                    "required": ["prompt"],
                    "properties": { "prompt": { "type": "string" } }
                }),
                output_schema: json!({ "type": "object" }),
                binding: RecipeBinding {
                    driver_key: "openai-images".to_string(),
                    provider_config: Default::default(),
                    default_connection_id: ConnectionId(1),
                    allowed_connection_ids: vec![ConnectionId(1), ConnectionId(2)],
                    connection_override: ConnectionOverride::Denied,
                },
                defaults: Default::default(),
                source: RecipeSource::Embedded,
                created_at: Timestamp::now(),
            }],
        );
    }

    async fn create_operation(
        repositories: &InMemoryRepositories,
    ) -> (OperationId, crate::v2::domain::ids::RunId) {
        create_operation_with_key(repositories, "scheduler-test-key").await
    }

    async fn create_operation_with_key(
        repositories: &InMemoryRepositories,
        key: &str,
    ) -> (OperationId, crate::v2::domain::ids::RunId) {
        create_operation_with_runs(repositories, key, 1).await
    }

    /// 一次提交 N 条 run（一条 Operation）——账号池的分发只在**一个批次内**轮转。
    async fn create_operation_with_runs(
        repositories: &InMemoryRepositories,
        key: &str,
        count: usize,
    ) -> (OperationId, crate::v2::domain::ids::RunId) {
        use crate::v2::application::create_operation::{
            CreateOperation, CreateOperationRequest, Repositories, RunRequest,
        };
        use crate::v2::domain::ids::IdempotencyKey;
        use crate::v2::infrastructure::schema::BasicSchemaValidator;
        let use_case = CreateOperation::new(
            Repositories {
                recipes: Box::new(repositories.clone()),
                connections: Box::new(repositories.clone()),
                operations: Box::new(repositories.clone()),
                assets: Box::new(repositories.clone()),
            },
            Box::new(crate::v2::infrastructure::fakes::PermitAllAuthorizationPolicy),
            Box::new(BasicSchemaValidator),
        );
        let persisted = use_case
            .execute(CreateOperationRequest {
                actor: actor(),
                origin: "mcp".to_string(),
                idempotency_key: IdempotencyKey(key.to_string()),
                runs: (1..=count)
                    .map(|index| RunRequest {
                        client_run_id: format!("image-{index}"),
                        recipe_revision_id: RecipeRevisionId(1),
                        connection_id: None,
                        inputs: BTreeMap::from([(
                            "prompt".to_string(),
                            json!("a red house"),
                        )]),
                        assets: vec![],
                    })
                    .collect(),
                metadata: Default::default(),
                callback: None,
            })
            .await
            .expect("创建 Operation 应成功");
        (persisted.operation.id, persisted.runs[0].id)
    }

    /// 连接上配了「跟踪上限」⇒ 提交成功那一刻写下的死线就是它（而不是默认 1 小时）。
    ///
    /// 这条钉的是**跨层的那一步**：策略在 application（调度器读连接 config），而"写死线"的
    /// 能力在仓库层（`Run::apply_tracking` / `set_tracking`）——两边的单测各自绿的时候，
    /// 中间那条线断了也没人知道。而它断掉的后果是**用户配的上限不生效**：
    /// 跑 90 分钟的视频工作流会在 1 小时被本地判失败（`TRACKING_EXHAUSTED`）。
    ///
    /// 反向控制：**不配**的连接必须拿到默认 1 小时。少了这半条，一个"永远写死一个数"的
    /// 实现也能过前半条。
    #[tokio::test]
    async fn accepted_run_takes_its_tracking_deadline_from_the_connection() {
        use crate::v2::domain::connection_options;
        use crate::v2::domain::model::Timestamp;

        /// 受理一个任务，返回"死线还剩多少毫秒"。
        async fn remaining_deadline_millis(config: BTreeMap<String, Value>) -> u64 {
            let repositories = InMemoryRepositories::new();
            seed_environment_with_config(&repositories, config);
            create_operation(&repositories).await;
            let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
                provider: "openai-images".to_string(),
                remote_id: "task-1".to_string(),
            }));
            let service = scheduler(repositories.clone(), driver);
            let tick = service.tick().await.expect("tick 应成功");
            assert_eq!(tick.accepted, 1, "应当被受理");
            let operation = repositories
                .find_operation(OperationId(1))
                .await
                .expect("find")
                .expect("operation");
            let run = &operation.runs[0];
            assert_eq!(run.status, RunStatus::Accepted);
            run.tracking_deadline
                .as_ref()
                .expect("受理后必须有死线")
                .millis()
                .saturating_sub(Timestamp::now_millis())
        }

        // ① 配了 120 秒 ⇒ 死线约 120 秒后（与默认的 3600 秒差得很远，不会看错）。
        let configured = remaining_deadline_millis(BTreeMap::from([(
            connection_options::TRACKING_DEADLINE_KEY.to_string(),
            json!(120),
        )]))
        .await;
        assert!(
            (100_000..=130_000).contains(&configured),
            "连接上配了 120 秒，死线就该是约 120 秒，实际剩 {configured} ms"
        );

        // ② 不配 ⇒ 默认 1 小时。
        let by_default = remaining_deadline_millis(Default::default()).await;
        assert!(
            (3_500_000..=3_700_000).contains(&by_default),
            "没配就该是默认 1 小时，实际剩 {by_default} ms"
        );
    }

    /// **我们一次都不该被平台拒绝**——这条断言落在用户看到的事实上，
    /// 而不是调度器的内部计数。
    ///
    /// ⚠️ 这是"用假平台复现真平台"的第一条：真平台（并发上限 1）在忙时**拒绝**新提交，
    /// 实测原文 `RUNNINGHUB_REJECTED / TASK_QUEUE_MAXED`。旧替身没有"在跑几个"的状态，
    /// 所以这个场景**根本写不出来**——调度器抢执行位的 bug 就是这样从 309 个测试里
    /// 活到真跑的。
    ///
    /// 判别点两条：
    /// 1. 提交 2 个任务、上限 1 ⇒ **零拒绝**；
    /// 2. 执行位释放后，第二个**必须**被提交（否则任务永远不动）。
    #[tokio::test]
    async fn never_gets_refused_by_a_single_slot_provider() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        create_operation_with_key(&repositories, "second-key").await;

        let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "task-1".to_string(),
        }));
        // ⚠️ 一次只压**一个**结果：`push_outcome` 是 `Vec::push` 而 `submit` 用 `pop()`
        // ——取的是**最后压进去的**。一次压两个会让顺序反过来（踩过：release 了一个
        // 从没进过在跑集合的 id，表现为"释放后仍不提交"）。
        let service = scheduler(repositories.clone(), driver.clone());

        // tick 1：受理 task-1，占住唯一的执行位。
        let first = service.tick().await.expect("tick 应成功");
        assert_eq!(first.accepted, 1, "第一个应当被受理");

        // tick 2：执行位还占着。**这里就是那个 bug 的位置**——
        // 修复前它会照样提交第二个，被平台拒。
        let second = service.tick().await.expect("tick 应成功");
        assert_eq!(
            driver.refusal_count(),
            0,
            "执行位占着时不得再提交——被平台拒就是我们自己造成的失败，\
             而用户看到的是「任务失败」"
        );
        assert_eq!(second.accepted, 0, "没有执行位就不该受理第二个");

        // 2) 执行位释放后，第二个**必须**被提交，否则任务永远不动。
        //
        // ⚠️ "执行位"是**两侧**的事，测试要把两侧都推到位（这是踩出来的）：
        // * **平台侧**：`release` 告诉模拟器"task-1 跑完了"——否则模拟器仍会拒绝；
        // * **我们账本侧**：把 run 标终态——调度器判"有没有空位"看的是**自己的账本**
        //   （`count_active_runs_for_connection`），只 release 平台侧的话它仍认为满着。
        // 两侧都推到位，才是真平台上"任务完成 → 对账标终态 → 可以提交下一个"的完整过程。
        driver.release("task-1");
        // 下一个提交要用的脚本结果（一次只压一个，见上面的顺序说明）。
        driver.push_outcome(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "task-2".to_string(),
        }));
        {
            let persisted = repositories
                .find_operation(OperationId(1))
                .await
                .expect("find")
                .expect("operation");
            let run_id = persisted.runs[0].id;
            for transition in [crate::v2::domain::operation::Transition::ToSucceeded] {
                repositories
                    .transition_run(run_id, transition, Vec::new())
                    .await
                    .expect("transition");
            }
            repositories
                .set_tracking(
                    run_id,
                    crate::v2::domain::operation::TrackingState::Inactive,
                    None,
                    None,
                )
                .await
                .expect("set tracking");
        }
        let third = service.tick().await.expect("tick 应成功");
        assert_eq!(third.accepted, 1, "执行位释放后必须接着提交下一个");
        // 两个 run 都到达终态/受理（不是被拒、也不是卡在 QUEUED）。
        let first_run = repositories
            .find_operation(OperationId(1))
            .await
            .expect("find")
            .expect("operation");
        assert_eq!(first_run.runs[0].status, RunStatus::Succeeded);
        let second_run = repositories
            .find_operation(OperationId(2))
            .await
            .expect("find")
            .expect("operation");
        assert_eq!(
            second_run.runs[0].status,
            RunStatus::Accepted,
            "第二个应当被受理（而不是被平台拒）"
        );
    }

    /// 连接上**已有 run 在跑**时，调度器不得再提交下一个。
    ///
    /// 起因是一次真跑：并发上限为 1 的 RunningHub 账号上同时提交 2 个任务，
    /// 第 2 个必然以 `RUNNINGHUB_REJECTED / TASK_QUEUE_MAXED` 失败——
    /// **那是我们自己造成的**：`available_slots()` 是"这个 provider 总共允许几个并发"
    /// （静态值 1），而调度器只把它当"每个 tick 最多领几个"，于是 tick 2 在
    /// tick 1 的任务还占着执行位时继续提交。用户看到的是"两张图里有一张失败了"。
    ///
    /// 判别点两条，缺一条都不算做对：
    /// 1. 已有活跃 run ⇒ 这一个 tick **一个都不领**（`claimed == 0`）；
    /// 2. 没有活跃 run 时照常领——反向控制。少了它，一个"永远不领"的实现也能过第 1 条。
    #[tokio::test]
    async fn does_not_submit_when_a_run_is_already_in_flight() {
        use crate::v2::domain::operation::{TrackingState, Transition};

        // 1) 有一个 run 正占着 provider 执行位 ⇒ 这个 tick 一个都不领。
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let (_, in_flight_run) = create_operation(&repositories).await;
        // 走**端口方法**把它推到"已受理且在跟踪"——与生产路径同一套转换，
        // 而不是戳替身内部状态（那样测的就不是真实可达的状态组合了）。
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToAccepted,
        ] {
            repositories
                .transition_run(in_flight_run, transition, Vec::new())
                .await
                .expect("transition");
        }
        repositories
            .set_tracking(in_flight_run, TrackingState::Active, None, None)
            .await
            .expect("set tracking");
        // 再排一个待跑的：它**不该**被领走。
        create_operation_with_key(&repositories, "second-key").await;

        let service = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Accepted(RemoteHandle {
                provider: "openai-images".to_string(),
                remote_id: "task-1".to_string(),
            })),
        );
        let tick = service.tick().await.expect("tick 应成功");
        assert_eq!(
            tick.claimed, 0,
            "已有 run 在跑时不得再领新 run——否则那一个就是被平台拒的（TASK_QUEUE_MAXED）"
        );

        // 2) 反向控制：没有活跃 run 时必须照常领，否则任务永远不动。
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let service = scheduler(
            repositories,
            driver_with(SubmitOutcome::Accepted(RemoteHandle {
                provider: "openai-images".to_string(),
                remote_id: "task-1".to_string(),
            })),
        );
        let tick = service.tick().await.expect("tick 应成功");
        assert_eq!(tick.claimed, 1, "没有活跃 run 时必须照常提交");
    }

    fn driver_with(outcome: SubmitOutcome) -> Arc<FakeProviderDriver> {
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures {
                synchronous_result: true,
                remote_reconciliation: true,
                ..Default::default()
            },
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_outcome(outcome);
        driver
    }

    fn scheduler(
        repositories: InMemoryRepositories,
        driver: Arc<FakeProviderDriver>,
    ) -> SchedulerService {
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        SchedulerService {
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
            capacity: Default::default(),
            paused: Default::default(),
        }
    }

    /// 一个"平台会报容量"的替身：接受 `limit` 条，`running` 是**平台侧**已有占用。
    fn capacity_driver(limit: usize, running: usize) -> Arc<FakeProviderDriver> {
        let driver = Arc::new(
            FakeProviderDriver::new(ProviderDescriptor {
                vendor_id: "openai".to_string(),
                protocol_id: "openai-images".to_string(),
                display_name: "OpenAI Images".to_string(),
                business_capabilities: vec!["text-to-image".to_string()],
                lifecycle_features: LifecycleFeatures {
                    synchronous_result: true,
                    remote_reconciliation: true,
                    ..Default::default()
                },
                security_requirements: Default::default(),
                connection_schema: json!({}),
                binding_schema: json!({}),
            })
            .with_concurrency_limit(limit)
            .with_queue_capacity(limit, running),
        );
        for index in 0..limit {
            driver.push_outcome(SubmitOutcome::Accepted(RemoteHandle {
                provider: "openai-images".to_string(),
                remote_id: format!("task-{index}"),
            }));
        }
        driver
    }

    /// **平台说有几个位，就用几个**——不是恒 1。
    ///
    /// 判别点：一轮 tick 领满 3 条（而不是 1 条），第二轮一个都不领（本地账本已经 3 条在跑）。
    /// 负对照：把 `capacity_for` 改成恒 `None` ⇒ 第一轮只领 1 条，第一条断言就红。
    #[tokio::test]
    async fn uses_the_platform_declared_capacity() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        for index in 0..3 {
            create_operation_with_key(&repositories, &format!("capacity-{index}")).await;
        }
        let driver = capacity_driver(3, 0);
        let service = scheduler(repositories.clone(), driver.clone());

        let first = service.tick().await.expect("tick 应成功");
        assert_eq!(first.accepted, 3, "平台报 3 个位，一轮就该受理 3 条");

        let second = service.tick().await.expect("tick 应成功");
        assert_eq!(second.claimed, 0, "3 条都在跑时不该再领");
        assert_eq!(driver.refusal_count(), 0, "一条都不该被平台拒");
    }

    /// **平台报的占用优先于本地账本**：网页控制台跑着 2 条，我们只剩 1 个位。
    ///
    /// 这是"多个入口共用一把 key"的现实：本地账本只知道自己的，平台知道全部。
    #[tokio::test]
    async fn platform_running_count_is_subtracted_from_the_capacity() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        for index in 0..3 {
            create_operation_with_key(&repositories, &format!("occupied-{index}")).await;
        }
        let driver = capacity_driver(3, 2);
        let service = scheduler(repositories.clone(), driver.clone());

        let tick = service.tick().await.expect("tick 应成功");
        assert_eq!(
            tick.accepted, 1,
            "平台已经有 2 条在跑（不是我们提交的），只该再领 1 条"
        );
        assert_eq!(driver.refusal_count(), 0, "宁可少领，也不要撞一次拒绝");
    }

    /// **被平台拒一次就停手**：本轮不再领，并丢掉容量缓存（下个 tick 重新问）。
    ///
    /// 现场：平台缓存说"3 个位都空"，但实际被别的入口占满了。修之前 tick 会把 3 条全领出来
    /// 挨个撞，三条 run 一起进退避；现在只撞一条。
    #[tokio::test]
    async fn a_capacity_rejection_stops_the_rest_of_the_tick() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        for index in 0..3 {
            create_operation_with_key(&repositories, &format!("reject-{index}")).await;
        }
        let driver = capacity_driver(3, 0);
        // 平台侧实际被占满，但缓存里的"空位"是旧的。
        for index in 0..3 {
            driver.occupy_external(&format!("external-{index}"));
        }
        let service = scheduler(repositories.clone(), driver.clone());

        let first = service.tick().await.expect("tick 应成功");
        assert_eq!(first.claimed, 1, "拒一次就该停手，不该把队列里的全领出来撞一遍");
        assert_eq!(first.retry_wait, 1);
        assert_eq!(driver.refusal_count(), 1, "只该撞一次");
        assert_eq!(
            service.capacity.entries.lock().unwrap().len(),
            0,
            "被拒之后容量缓存必须失效，下个 tick 重新问平台"
        );

        // 下个 tick 会重新问一次（而不是拿旧缓存接着撞）。
        let before = driver.queue_status_calls();
        let _ = service.tick().await.expect("tick 应成功");
        assert_eq!(
            driver.queue_status_calls(),
            before + 1,
            "失效之后必须重新查询"
        );
    }

    /// 容量查询有缓存：两个 tick 只问平台一次。
    #[tokio::test]
    async fn capacity_is_cached_between_ticks() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = capacity_driver(3, 0);
        let service = scheduler(repositories.clone(), driver.clone());

        let _ = service.tick().await.expect("tick 应成功");
        let _ = service.tick().await.expect("tick 应成功");
        assert_eq!(
            driver.queue_status_calls(),
            1,
            "2 秒一轮不能每轮都问平台；30 秒缓存内只查一次"
        );
    }


    /// **账号的错只停那个账号**：key 失效 ⇒ 判失败 + 冻结这条连接，下一轮不再领新单。
    ///
    /// 反面对照在下面那条：容量满（`TASK_QUEUE_MAXED`）**不许**冻结——那是"忙"，不是"坏"。
    #[tokio::test]
    async fn an_account_fault_pauses_that_connection_until_the_key_is_fixed() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "never".to_string(),
        }));
        driver.push_submit_error(SafeError::provider("APIKEY_INVALID", "key 已失效"));
        let service = scheduler(repositories.clone(), driver);

        let first = service.tick().await.expect("tick 应成功");
        assert_eq!(first.failed, 1, "鉴权错是非可重试的，应当判失败而不是退避");
        assert!(
            service.paused.snapshot().iter().any(|(id, _)| *id == 1),
            "账号的错必须冻结这条连接：{:?}",
            service.paused.snapshot()
        );

        create_operation_with_key(&repositories, "after-pause").await;
        let second = service.tick().await.expect("tick 应成功");
        assert_eq!(second.claimed, 0, "冻结期间不该再领新单");
    }

    /// **容量满不是账号的错**：不许冻结，只退避。
    #[tokio::test]
    async fn a_capacity_rejection_does_not_pause_the_connection() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "never".to_string(),
        }));
        driver.push_submit_error(SafeError {
            retryable: true,
            ..SafeError::provider("RUNNINGHUB_REJECTED", "TASK_QUEUE_MAXED")
        });
        let service = scheduler(repositories, driver);

        let tick = service.tick().await.expect("tick 应成功");
        assert_eq!(tick.retry_wait, 1, "队列满应当退避重试");
        assert!(
            service.paused.snapshot().is_empty(),
            "忙不是坏：不该冻结连接"
        );
    }

    /// **调度侧再平衡**：把"没试过"的排队 run 从别的池成员叫到有空位的连接上。
    ///
    /// 现场就是这个：3+1 的池子上 4 条被轮转成 2/2，连接 1 还有 1 个空位、
    /// 连接 2 却排着一条——改派把它挪过去，一轮跑完。
    ///
    /// 边界：一次最多 `max` 条；挪走的不会重复计数；已经不在原连接上的不再动。
    #[tokio::test]
    async fn adoption_rebalances_a_pool_without_touching_anything_else() {
        let repositories = InMemoryRepositories::new();
        seed_pool(&repositories);
        create_operation_with_runs(&repositories, "adopt-pool", 4).await;
        let conn1 = crate::v2::domain::ids::ConnectionRevisionId(1);
        let conn2 = crate::v2::domain::ids::ConnectionRevisionId(2);

        // 轮转后 2/2：连接 1 有 2 条、连接 2 有 2 条。
        // 连接 1 还有 1 个空位 ⇒ 从连接 2 叫一条过来。
        assert_eq!(
            repositories.adopt_queued_runs(conn1, &[conn2], 1).await.unwrap(),
            1,
            "只该叫 max 条"
        );
        // 再叫：连接 2 只剩 1 条，叫过来之后它自己空了。
        assert_eq!(repositories.adopt_queued_runs(conn1, &[conn2], 1).await.unwrap(), 1);
        // 连接 2 已经空了 ⇒ 没有可叫的。
        assert_eq!(repositories.adopt_queued_runs(conn1, &[conn2], 1).await.unwrap(), 0);
        assert_eq!(
            repositories.adopt_queued_runs(conn2, &[conn1], 1).await.unwrap(),
            0,
            "连接 1 那 4 条已经不在排队区了（这里只验不会再被反向搬走）"
        );
    }

    /// 凭据引用：引用方的 run 提交时，驱动拿到的必须是**被引用连接**的槽位。
    ///
    /// 这条断言落在"驱动收到了什么"上（替身记下了每次 `prepare` 的槽位），而不是
    /// "我们调过解析函数"——中间任何一环漏掉，用户看到的就是 rh-proxy 地址里没有 key。
    #[tokio::test]
    async fn a_referencing_connection_submits_with_the_borrowed_slot() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        seed_runninghub_source(&repositories, 2);
        point_connection_at(&repositories, 1, 2);
        create_operation(&repositories).await;

        let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "task-1".to_string(),
        }));
        let service = scheduler(repositories.clone(), driver.clone());
        let result = service.tick().await.expect("调度应成功");

        assert_eq!(result.accepted, 1);
        assert_eq!(
            driver.prepare_slots(),
            vec![vec!["connection-2-api_key".to_string()]],
            "驱动必须拿到**借来的**槽位（自己那条连接上没有任何绑定）"
        );
    }

    /// 引用坏了（目标被删）⇒ run **当场失败**，而不是留在 SUBMITTING 等对账兜底。
    ///
    /// 后者是这条测试要防的具体事故：`execute_claimed` 的错误若直接冒泡，调用方只记一个
    /// `uncertain`，run 会被 recover 成 UNCERTAIN 再重试，用户看到的是"卡住/反复重试"，
    /// 而真正的原因（引用的连接已被删除）一句话就能说清。
    #[tokio::test]
    async fn a_broken_reference_fails_the_run_instead_of_leaving_it_uncertain() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        point_connection_at(&repositories, 1, 99); // 指向不存在的连接
        let (operation_id, _) = create_operation(&repositories).await;

        let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "task-1".to_string(),
        }));
        let service = scheduler(repositories.clone(), driver.clone());
        let result = service.tick().await.expect("调度应成功");

        assert_eq!(result.failed, 1, "引用坏掉应当判失败");
        assert_eq!(result.uncertain, 0, "不能落到 uncertain——那会变成「卡住」");
        assert!(
            driver.prepare_slots().is_empty(),
            "解析失败时**一次都不该**去调驱动"
        );
        let persisted = repositories
            .find_operation(operation_id)
            .await
            .unwrap()
            .unwrap();
        let error = persisted.runs[0].error.clone().expect("run 应当带错误");
        assert_eq!(error.code, "CONNECTION_SOURCE_NOT_FOUND");
        assert!(
            error.message.contains("#99"),
            "错误信息要指到具体是哪条连接：{}",
            error.message
        );
    }

    #[tokio::test]
    async fn completed_outcome_marks_run_succeeded_and_stores_artifact() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let (_, run_id) = create_operation(&repositories).await;
        let scheduler = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Completed(vec![completed_artifact_draft(
                "out.png",
            )])),
        );
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.succeeded, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].id, run_id);
        assert_eq!(persisted.runs[0].status, RunStatus::Succeeded);
        assert_eq!(
            persisted.operation.status,
            crate::v2::domain::operation::OperationStatus::Succeeded
        );
        assert_eq!(repositories.artifacts().len(), 1);
        let attempt = repositories.attempts();
        assert_eq!(
            attempt[0].status,
            crate::v2::domain::operation::AttemptStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn accepted_outcome_marks_run_accepted_with_handle() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let scheduler = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Accepted(RemoteHandle {
                provider: "openai-images".to_string(),
                remote_id: "task-1".to_string(),
            })),
        );
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.accepted, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Accepted);
        let attempt = repositories.attempts()[0].clone();
        assert_eq!(attempt.remote_handle.unwrap().remote_id, "task-1");
        // Accepted 后进入 active 对账队列（apply_tracking 已处理），
        // reconcile 可据此轮询远端。
        assert_eq!(
            persisted.runs[0].tracking_state,
            crate::v2::domain::operation::TrackingState::Active
        );
    }

    #[tokio::test]
    async fn uncertain_without_proof_marks_run_submission_uncertain() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let scheduler = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Uncertain(UncertainSubmission {
                reason: "响应丢失".to_string(),
                recovery: RecoveryProof::None,
            })),
        );
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.uncertain, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::SubmissionUncertain);
    }

    #[tokio::test]
    async fn uncertain_with_idempotent_proof_marks_run_retry_wait() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let scheduler = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Uncertain(UncertainSubmission {
                reason: "超时但请求幂等".to_string(),
                recovery: RecoveryProof::Idempotent,
            })),
        );
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.retry_wait, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::RetryWait);
    }

    #[tokio::test]
    async fn retry_wait_run_is_not_claimed_before_backoff() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_submit_error(SafeError::retryable("RATE_LIMITED", "临时限流"));
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver.clone());
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let result = scheduler.tick().await.expect("第一轮应成功");
        assert_eq!(result.retry_wait, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::RetryWait);
        assert!(
            persisted.runs[0]
                .next_poll_at
                .as_ref()
                .is_some_and(|next| next.millis() > Timestamp::now_millis()),
            "RETRY_WAIT 必须设置退避时间"
        );

        // 退避期内再次调度：run 不应被认领（driver 没有新的 submit 错误可用，
        // 若被认领会因错误栈为空而 panic 或产生额外提交）。
        let result2 = scheduler.tick().await.expect("第二轮应成功");
        assert_eq!(result2.claimed, 0, "退避期内不得认领 RETRY_WAIT run");
    }

    #[tokio::test]
    async fn retryable_error_marks_run_retry_wait() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_submit_error(SafeError::retryable("RATE_LIMITED", "临时限流"));
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.retry_wait, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::RetryWait);
    }

    /// **退避重试有终点**：到期（连接上的 `trackingDeadlineSeconds`，默认 1 小时）仍未成功 ⇒ 判失败。
    ///
    /// 为什么必须有终点（2026-10-18）：此前 `RETRY_WAIT` **没有死线也没有次数上限** ——
    /// 平台一直拒就每 30 秒重试一次、**永远不结束**，用户看到的是一个永远「进行中」的任务；
    /// 而 `attention` 那时只能说"正在退避重试"（说"到点判失败"是假话，所以上一批把那半句删了）。
    /// 现在它与「等确认」同一条语义：**等这么久还没成，就判失败，让人来决定下一步。**
    ///
    /// 判别点四条：
    /// 1. 第一次进这一档就写下死线，且**来自连接的配置**（用例配 60 秒 ⇒ 大约还剩 60 秒）；
    /// 2. 到期后**连 prepare 都不该再走**（`prepare_slots` 为空 = 根本没到提交那一步），
    ///    run 判 `FAILED` + `RETRY_EXHAUSTED`，且 `retryable: false`（否则下一拍又被排回来，死循环）；
    /// 3. **反向控制**：死线**还没到**时照旧重试（同一条路径必须还能走通）；
    /// 4. 每次重试**沿用同一个死线**，不是每失败一次就往后推一小时（那样等于没有死线）。
    ///
    /// 负对照：删掉 `execute_claimed` 开头那段到期判据 ⇒ 第 2 条红（会真的又提交一次）。
    #[tokio::test]
    async fn retry_wait_gives_up_at_the_connection_deadline() {
        let repositories = InMemoryRepositories::new();
        // 死线配成 60 秒（允许的最小值）：下面用 `set_tracking` 直接把死线挪到过去来构造"到期"。
        seed_environment_with_config(
            &repositories,
            BTreeMap::from([("trackingDeadlineSeconds".to_string(), json!(60))]),
        );
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        // 第一次提交被平台拒（可重试）⇒ 进 RETRY_WAIT 并写下死线。
        driver.push_submit_error(SafeError::retryable("RATE_LIMITED", "临时限流"));
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver.clone());
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let first = scheduler.tick().await.expect("第一拍");
        assert_eq!(first.retry_wait, 1);
        let run = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap()
            .runs[0]
            .clone();
        let run_id = run.id;
        // 1) 死线来自连接配置（60 秒），不是写死的默认 1 小时。
        let deadline = run.tracking_deadline.clone().expect("必须写下死线");
        let remaining = deadline.millis().saturating_sub(Timestamp::now_millis());
        assert!(
            remaining > 50_000 && remaining <= 60_000,
            "死线应当来自连接的配置（60 秒），实际还剩 {remaining} 毫秒"
        );
        assert!(
            run.next_poll_at.is_some(),
            "退避时间照旧要有（否则下一拍就重投）"
        );

        // 3) 死线还没到 ⇒ 照旧重试（把退避时间挪到过去，模拟"到点了该再试一次"）。
        driver.push_submit_error(SafeError::retryable("RATE_LIMITED", "又限流了"));
        let due_now = Some(Timestamp::from_millis(
            Timestamp::now_millis().saturating_sub(1),
        ));
        // ⚠️ 死线换成一个**独特的已知值**（55 秒后），不沿用第一拍写下的那个：否则第 4 条断言
        // 会退化成"两次 `now + 60s` 恰好差不到 1 毫秒"——负对照实测过一次：把实现改成
        // "每次重试都重算死线"，这条**照样绿**（两次 tick 落在同一毫秒里）。
        let distinctive = Timestamp::from_millis(Timestamp::now_millis() + 55_000);
        repositories
            .set_tracking(
                run_id,
                crate::v2::domain::operation::TrackingState::Inactive,
                due_now.clone(),
                Some(distinctive.clone()),
            )
            .await
            .expect("挪退避时间");
        let second = scheduler.tick().await.expect("第二拍");
        assert_eq!(second.retry_wait, 1, "死线没到就该继续重试");
        // 4) 死线**没被推后**：重试用的是同一个（逐字相等，不是"差不多"）。
        let again = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap()
            .runs[0]
            .clone();
        assert_eq!(
            again.tracking_deadline.as_ref().map(|d| d.0.clone()),
            Some(distinctive.0.clone()),
            "每次重试都必须沿用同一个死线（每失败一次就往后推 = 等于没有死线）"
        );

        // 2) 把死线挪到过去 ⇒ 下一拍不再提交，直接判失败。
        repositories
            .set_tracking(
                run_id,
                crate::v2::domain::operation::TrackingState::Inactive,
                due_now,
                Some(Timestamp::from_millis(
                    Timestamp::now_millis().saturating_sub(1),
                )),
            )
            .await
            .expect("把死线挪到过去");
        let prepares_before = driver.prepare_slots().len();
        let third = scheduler.tick().await.expect("第三拍");
        assert_eq!(third.failed, 1, "到期应当判失败");
        assert_eq!(
            driver.prepare_slots().len(),
            prepares_before,
            "到期之后**连 prepare 都不该再走**（那一步之后就是提交，也就是重复计费的入口）"
        );
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Failed);
        let error = persisted.runs[0].error.clone().expect("要留下原因");
        assert_eq!(error.code, "RETRY_EXHAUSTED");
        assert!(
            !error.retryable,
            "这条错误必须是**不可重试**的，否则下一拍又被排回 RETRY_WAIT（死循环）"
        );
    }

    /// **HTTP 5xx 是"提交结果不明"**：落「等确认」，**不许盲投**（2026-10-18 定的取舍）。
    ///
    /// 为什么（这是钱）：5xx 是"平台回了话"，但 500 可能是**网关 / 内部错误发生在受理之后**
    /// ⇒ 平台**可能已经收下**并照常计费。盲投 = 平台上可能有两个任务、扣两次钱。
    /// 按铁律二，这类提交与"发出去之后超时"同类，只能等对账或死线。
    ///
    /// 代价（已写进 `status.md` 与 README）：RunningHub **没有任何接口**能回查"我那条任务在不在"，
    /// 所以那边遇到 5xx 会等到死线再人工决定。换来的是**永远不会悄悄扣两次**。
    ///
    /// 判别点三条：
    /// 1. 5xx ⇒ `SUBMISSION_UNCERTAIN`（不是 `RETRY_WAIT`），且 tick 的计数也报 `uncertain`
    ///    （落点与计数必须一致——这个仓库因为两处各写一份判据踩过一次）；
    /// 2. **反向控制**：同样走"平台拒绝"的 **429** 仍然退避重试（明确拒绝 ⇒ 一个字节的活都没接
    ///    ⇒ 重试不可能重复计费）——少了这条，"把所有错都判成不确定"也能过第 1 条；
    /// 3. 错误里带着"结果不明"的标记（排查时能看出是哪条判据起的作用）。
    ///
    /// ⚠️ 测试**直接用生产那个映射函数**（`safety::map_submit_http_error`）造错误，而不是手搓一个
    /// 带标记的 `SafeError`：手搓的话，映射函数改了（比如哪天不标 5xx 了）测试照样绿 ——
    /// 那样测的就不是生产行为。
    ///
    /// 负对照：把 `map_submit_http_error` 里打标记那几行删掉 ⇒ 第 1 条红（run 会去退避重试）。
    #[tokio::test]
    async fn a_five_hundred_on_submit_waits_for_confirmation_instead_of_blind_retry() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_submit_error(
            crate::v2::infrastructure::providers::safety::map_submit_http_error(
                503,
                "Provider 返回 HTTP 503",
            ),
        );
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver.clone());
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(
            result.uncertain, 1,
            "5xx 必须计入「等确认」：落点与计数各说各话是这个仓库踩过的坑"
        );
        assert_eq!(result.retry_wait, 0, "5xx **不许**进退避重试（那就是盲投）");
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::SubmissionUncertain);
        let error = persisted.runs[0].error.clone().expect("要留下原因");
        assert!(
            error
                .details
                .get(crate::v2::domain::errors::SUBMISSION_UNKNOWN_DETAIL)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            "要带着「结果不明」的标记，排查时才知道是哪条判据起的作用"
        );

        // ② 反向控制：429 是**明确拒绝**，仍然退避重试。
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation_with_key(&repositories, "second-key").await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_submit_error(
            crate::v2::infrastructure::providers::safety::map_submit_http_error(
                429,
                "Provider 返回 HTTP 429",
            ),
        );
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver.clone());
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let second = scheduler.tick().await.expect("调度应成功");
        assert_eq!(second.retry_wait, 1, "429 是明确拒绝，该退避重试");
        assert_eq!(second.uncertain, 0);
    }

    /// **能证明「字节没出去」**的提交失败 ⇒ 直接退避重试，不落「等确认」。
    ///
    /// 为什么这条要紧（钱与时间）：`SUBMISSION_UNCERTAIN` 会一直等到死线（默认 1 小时）才判失败。
    /// 而"连接被拒"意味着对端**主动拒绝**了握手、平台什么都没收到 —— 让用户为这种提交等一小时
    /// 毫无意义，而且它**不可能**重复扣费（重投的前提是"可能已经收下"，这里证明不成立）。
    ///
    /// 判别点三条：
    /// 1. 带证据（`details.nothingSent = true`）的网络失败 ⇒ run 落 `RETRY_WAIT`，
    ///    且那次 attempt 记下原因（排查与后续判断都靠它）；
    /// 2. **退避真的设了**（`next_poll_at` 在未来）——少了这条，一个"立刻重投"的实现也能过第 1 条，
    ///    而它会在 30 秒内把平台刷爆；
    /// 3. 反向控制见 `network_error_marks_run_submission_uncertain`：**同样**的网络失败
    ///    **不带证据**时仍然落 `SUBMISSION_UNCERTAIN`（那才是"可能已经收下"）。
    ///
    /// 这条测试同时钉住一处**真实的分歧**（写它的时候抓到的）：`resolve_submit_error`（落状态）
    /// 与 `error_outcome`（报计数）原本各写了一份判据，只改一处时 run 进了 `RETRY_WAIT`
    /// 而 tick 记的是 `uncertain` —— 库里状态与计数各说各话。现在两处都调
    /// `submission_is_unknown`，所以"计数"与"落点"必须一致（本测试两个都断言）。
    ///
    /// 负对照：把 `submission_is_unknown` 里的 `&& !error.proves_nothing_was_sent()` 删掉 ⇒ 这条红。
    #[tokio::test]
    async fn a_provably_unsent_submission_retries_instead_of_waiting_for_the_deadline() {
        use crate::v2::domain::model::Timestamp;

        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        // 与 `map_network_error` 落下来的形状一致：网络类 + 可重试 + "字节没出去"的证据。
        let mut evidence = BTreeMap::new();
        evidence.insert(
            crate::v2::domain::errors::NOTHING_SENT_DETAIL.to_string(),
            json!(true),
        );
        driver.push_submit_error(
            SafeError::network("PROVIDER_UNREACHABLE_SUBMIT", "连不上 submit")
                .with_details(evidence),
        );
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(
            (result.retry_wait, result.uncertain),
            (1, 0),
            "证明得了「没出去」就该退避重试，而不是等死线"
        );
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        let run = &persisted.runs[0];
        assert_eq!(run.status, RunStatus::RetryWait);
        // 2) 退避时间必须在未来。
        assert!(
            run.next_poll_at
                .as_ref()
                .is_some_and(|next| next.millis() > Timestamp::now_millis()),
            "必须设退避时间，否则下一拍就重投：{:?}",
            run.next_poll_at
        );
        // 1) 原因留在 attempt 上（这一档是 FAILED：我们知道它没成功，也知道没发出去）。
        let attempt = repositories
            .find_latest_attempt(run.id)
            .await
            .expect("查 attempt")
            .expect("应当有 attempt");
        let stored = attempt.error.expect("原因必须留下");
        assert!(
            stored.proves_nothing_was_sent(),
            "「字节没出去」的证据要跟着原因一起留下来：{:?}",
            stored.details
        );
    }

    #[tokio::test]
    async fn network_error_marks_run_submission_uncertain() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_submit_error(SafeError::network("SUBMIT_TIMEOUT", "提交超时"));
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.uncertain, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::SubmissionUncertain);

        // 「为什么不明」必须留在 attempt 上：这是**唯一**会留下来的证据（事件载荷只有一句
        // message，日志那一行要等到死线才写）。少了它，「连接被拒」与「发出去后超时」在库里
        // 长得一模一样 —— 而这两类能不能重投是相反的回答。
        let attempt = repositories
            .find_latest_attempt(persisted.runs[0].id)
            .await
            .expect("查 attempt")
            .expect("应当有 attempt");
        assert_eq!(attempt.status, AttemptStatus::Uncertain);
        let stored = attempt
            .error
            .expect("不确定的 attempt 必须留下原因（否则排查与重投判断都没有依据）");
        assert_eq!(stored.code, "SUBMIT_TIMEOUT");
        assert_eq!(stored.message, "提交超时");
    }

    /// **一个不确定的 run 不该占住这条连接唯一的执行位**（2026-10-17 改）。
    ///
    /// 起因：一个 `SUBMISSION_UNCERTAIN` 的 run 会把执行位占满到死线（默认 1 小时），
    /// 后面的 run 全排队 —— 而平台上可能一个都没在跑。空转的代价是**整整一个小时**。
    ///
    /// 判别点三条，缺一条都不算做对：
    /// 1. 有不确定的 run 时，**下一个 run 照常被提交**；
    /// 2. **反向控制**：`ACCEPTED`（真在平台上跑）的 run 仍然占位，一个都不多领
    ///    ——少了这条，一个"永远不占位"的实现也能过第 1 条，而那正是当初被平台拒的那个 bug；
    /// 3. 不确定的那个 run **照旧留在对账队列里**（`list_active_runs`），否则它永远不会被确认。
    ///
    /// 负对照：把 `count_active_runs_for_connection` 的状态集合加回 `SUBMISSION_UNCERTAIN`
    /// （测试走替身，所以改 `infrastructure/fakes.rs` 那一处即可）⇒ 第 1 条红。
    #[tokio::test]
    async fn an_uncertain_run_does_not_hold_the_only_execution_slot() {
        use crate::v2::domain::operation::{TrackingState, Transition};

        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        // 第一个任务：提交时网络错误 ⇒ 落不确定态。
        create_operation(&repositories).await;
        let driver = driver_with(SubmitOutcome::Accepted(RemoteHandle {
            provider: "openai-images".to_string(),
            remote_id: "task-later".to_string(),
        }));
        driver.push_submit_error(SafeError::network("SUBMIT_TIMEOUT", "提交超时"));
        let service = scheduler(repositories.clone(), driver);
        let first = service.tick().await.expect("tick 应成功");
        assert_eq!(first.uncertain, 1, "第一个应当落不确定态");
        let uncertain = repositories
            .find_operation(OperationId(1))
            .await
            .expect("find")
            .expect("operation");
        let uncertain_run = uncertain.runs[0].id;
        assert_eq!(uncertain.runs[0].status, RunStatus::SubmissionUncertain);

        // 1) 再排一个待跑的：**它必须被提交**（执行位不该被"不知道"占着）。
        create_operation_with_key(&repositories, "second-key").await;
        let second = service.tick().await.expect("tick 应成功");
        assert_eq!(
            second.accepted, 1,
            "不确定的 run 不该占住执行位——它占的是「不知道」，不是平台的执行位"
        );

        // 3) 对账队列照旧包含它（`list_active_runs` 是它被确认的唯一途径）。
        let active = repositories
            .list_active_runs(100)
            .await
            .expect("查活跃 run");
        assert!(
            active.iter().any(|entry| entry.run.id == uncertain_run),
            "不确定的 run 必须继续留在对账队列里，否则永远不会被确认"
        );

        // 2) 反向控制：真在平台上跑的（ACCEPTED）仍然占位。
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        let (_, accepted_run) = create_operation(&repositories).await;
        for transition in [
            Transition::ToPreparing,
            Transition::ToSubmitting,
            Transition::ToAccepted,
        ] {
            repositories
                .transition_run(accepted_run, transition, Vec::new())
                .await
                .expect("推到已受理");
        }
        repositories
            .set_tracking(accepted_run, TrackingState::Active, None, None)
            .await
            .expect("set tracking");
        create_operation_with_key(&repositories, "third-key").await;
        let service = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Accepted(RemoteHandle {
                provider: "openai-images".to_string(),
                remote_id: "task-never".to_string(),
            })),
        );
        let tick = service.tick().await.expect("tick 应成功");
        assert_eq!(
            tick.claimed, 0,
            "已受理（真在平台上跑）的 run 必须继续占位——否则就是我们自己造成的 TASK_QUEUE_MAXED"
        );
    }

    #[tokio::test]
    async fn non_retryable_error_marks_run_failed() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        let driver = Arc::new(FakeProviderDriver::new(ProviderDescriptor {
            vendor_id: "openai".to_string(),
            protocol_id: "openai-images".to_string(),
            display_name: "OpenAI Images".to_string(),
            business_capabilities: vec!["text-to-image".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            security_requirements: Default::default(),
            connection_schema: json!({}),
            binding_schema: json!({}),
        }));
        driver.push_submit_error(SafeError::validation("MODEL_NOT_FOUND", "模型不存在"));
        let mut registry = DriverRegistry::new();
        registry.register(driver_key("openai", "openai-images"), driver);
        let scheduler = SchedulerService {
            capacity: Default::default(),
            paused: Default::default(),
            owner: "test-scheduler".to_string(),
            repositories: Arc::new(repositories.clone()),
            leases: Arc::new(repositories.clone()),
            connections: Arc::new(repositories.clone()),
            assets: Arc::new(repositories.clone()),
            artifacts: Arc::new(repositories.clone()),
            drivers: Arc::new(registry),
        };
        let result = scheduler.tick().await.expect("调度应成功");
        assert_eq!(result.failed, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::Failed);
        assert_eq!(
            persisted.operation.status,
            crate::v2::domain::operation::OperationStatus::Failed
        );
    }

    #[tokio::test]
    async fn busy_lease_returns_busy_without_claiming() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        repositories
            .acquire("other-owner", "other-token", 30)
            .await
            .expect("其他所有者应先获取租约");
        let scheduler = scheduler(
            repositories.clone(),
            driver_with(SubmitOutcome::Completed(vec![completed_artifact_draft(
                "out.png",
            )])),
        );
        let result = scheduler.tick().await.expect("tick 应返回 busy 而非报错");
        assert!(result.busy);
        assert_eq!(result.claimed, 0);
    }

    #[tokio::test]
    async fn recover_expired_submissions_transitions_stuck_submitting_runs() {
        let repositories = InMemoryRepositories::new();
        seed_environment(&repositories);
        create_operation(&repositories).await;
        // 模拟进程在提交前崩溃：run 停留在 SUBMITTING 且 attempt 已过期
        repositories.set_recovery_window(0);
        let claimed = repositories
            .claim_next_run(
                "test-scheduler",
                "fencing-token-1",
                ConnectionRevisionId(1),
                Timestamp::from_millis(0),
            )
            .await
            .expect("claim 应成功")
            .expect("应有可领取的 Run");
        assert_eq!(claimed.run.status, RunStatus::Submitting);
        let recovered = repositories
            .recover_expired_submissions()
            .await
            .expect("恢复应成功");
        assert_eq!(recovered, 1);
        let persisted = repositories
            .find_operation(OperationId(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runs[0].status, RunStatus::SubmissionUncertain);
        // 恢复后的 attempt 状态为 UNCERTAIN
        let attempt = repositories
            .find_latest_attempt(persisted.runs[0].id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.status,
            crate::v2::domain::operation::AttemptStatus::Uncertain
        );
    }
}
