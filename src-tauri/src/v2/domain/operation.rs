use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::actor::ActorRef;
use super::callback::CallbackSubscription;
use super::errors::{DomainResult, SafeError};
use super::execution::RemoteHandle;
use super::ids::{AttemptId, IdempotencyKey, OperationId, RecipeRevisionId, RunId};
use super::media::AssetRef;
use super::model::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunStatus {
    Queued,
    Preparing,
    Submitting,
    SubmissionUncertain,
    Accepted,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled
        )
    }

    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "QUEUED",
            RunStatus::Preparing => "PREPARING",
            RunStatus::Submitting => "SUBMITTING",
            RunStatus::SubmissionUncertain => "SUBMISSION_UNCERTAIN",
            RunStatus::Accepted => "ACCEPTED",
            RunStatus::Running => "RUNNING",
            RunStatus::RetryWait => "RETRY_WAIT",
            RunStatus::Succeeded => "SUCCEEDED",
            RunStatus::Failed => "FAILED",
            RunStatus::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "QUEUED" => RunStatus::Queued,
            "PREPARING" => RunStatus::Preparing,
            "SUBMITTING" => RunStatus::Submitting,
            "SUBMISSION_UNCERTAIN" => RunStatus::SubmissionUncertain,
            "ACCEPTED" => RunStatus::Accepted,
            "RUNNING" => RunStatus::Running,
            "RETRY_WAIT" => RunStatus::RetryWait,
            "SUCCEEDED" => RunStatus::Succeeded,
            "FAILED" => RunStatus::Failed,
            "CANCELLED" => RunStatus::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationStatus {
    Queued,
    Running,
    Succeeded,
    Cancelled,
    Failed,
    Partial,
}

impl OperationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OperationStatus::Succeeded
                | OperationStatus::Cancelled
                | OperationStatus::Failed
                | OperationStatus::Partial
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OperationStatus::Queued => "QUEUED",
            OperationStatus::Running => "RUNNING",
            OperationStatus::Succeeded => "SUCCEEDED",
            OperationStatus::Cancelled => "CANCELLED",
            OperationStatus::Failed => "FAILED",
            OperationStatus::Partial => "PARTIAL",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "QUEUED" => OperationStatus::Queued,
            "RUNNING" => OperationStatus::Running,
            "SUCCEEDED" => OperationStatus::Succeeded,
            "CANCELLED" => OperationStatus::Cancelled,
            "FAILED" => OperationStatus::Failed,
            "PARTIAL" => OperationStatus::Partial,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttemptStatus {
    Preparing,
    Submitting,
    Accepted,
    Succeeded,
    Failed,
    Uncertain,
    Cancelled,
}

impl AttemptStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AttemptStatus::Succeeded
                | AttemptStatus::Failed
                | AttemptStatus::Uncertain
                | AttemptStatus::Cancelled
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AttemptStatus::Preparing => "PREPARING",
            AttemptStatus::Submitting => "SUBMITTING",
            AttemptStatus::Accepted => "ACCEPTED",
            AttemptStatus::Succeeded => "SUCCEEDED",
            AttemptStatus::Failed => "FAILED",
            AttemptStatus::Uncertain => "UNCERTAIN",
            AttemptStatus::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "PREPARING" => AttemptStatus::Preparing,
            "SUBMITTING" => AttemptStatus::Submitting,
            "ACCEPTED" => AttemptStatus::Accepted,
            "SUCCEEDED" => AttemptStatus::Succeeded,
            "FAILED" => AttemptStatus::Failed,
            "UNCERTAIN" => AttemptStatus::Uncertain,
            "CANCELLED" => AttemptStatus::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrackingState {
    #[default]
    Inactive,
    Active,
    Paused,
    Exhausted,
}

impl TrackingState {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackingState::Inactive => "inactive",
            TrackingState::Active => "active",
            TrackingState::Paused => "paused",
            TrackingState::Exhausted => "exhausted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "inactive" => TrackingState::Inactive,
            "active" => TrackingState::Active,
            "paused" => TrackingState::Paused,
            "exhausted" => TrackingState::Exhausted,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: OperationId,
    pub actor: ActorRef,
    pub origin: String,
    pub idempotency_key: IdempotencyKey,
    pub request_hash: String,
    pub status: OperationStatus,
    pub metadata: BTreeMap<String, Value>,
    pub callback: Option<CallbackSubscription>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub operation_id: OperationId,
    pub client_run_id: String,
    pub recipe_revision_id: RecipeRevisionId,
    pub connection_revision_id: super::ids::ConnectionRevisionId,
    pub sequence: u32,
    pub inputs: BTreeMap<String, Value>,
    pub assets: Vec<AssetRef>,
    pub status: RunStatus,
    pub tracking_state: TrackingState,
    pub tracking_deadline: Option<Timestamp>,
    pub next_poll_at: Option<Timestamp>,
    pub last_polled_at: Option<Timestamp>,
    /// 终态错误（失败/取消时结构化记录，用于详情直接展示）。
    pub error: Option<SafeError>,
    /// **平台报的用量**（原样转述，形如 `{unit, amount}`）。
    ///
    /// `None` = "平台没说"；`0` = "这次免费"（本地自建 ComfyUI）——两者不是一回事。
    /// 产品**不推算成本**：这里只可能出现平台自己报过的数。
    pub usage: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAttempt {
    pub id: AttemptId,
    pub run_id: RunId,
    pub number: u32,
    pub status: AttemptStatus,
    pub request_fingerprint: String,
    pub fencing_token: String,
    pub remote_handle: Option<RemoteHandle>,
    pub error: Option<SafeError>,
    pub started_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    ToPreparing,
    ToSubmitting,
    ToAccepted,
    ToRunning,
    ToSucceeded,
    ToFailed,
    ToCancelled,
    ToSubmissionUncertain,
    ToRetryWait,
}

impl Run {
    pub fn apply(&mut self, transition: Transition, at: Timestamp) -> DomainResult<()> {
        let next = next_status(self.status, transition)?;
        self.status = next;
        self.tracking_state = tracking_for(next);
        self.updated_at = at;
        Ok(())
    }

    /// 迁移之后重算**跟踪状态与死线**（仓库在每次 `transition_run` 之后调用）。
    ///
    /// ⚠️ 死线**只在还没有的时候**补一条默认值，不是每次重算。它的含义是"**这一次提交**
    /// 最多跟多久"——提交成功那一刻按连接的配置写定（`application/scheduler.rs` 的
    /// `ToAccepted` 分支，配置见 `domain::connection_options`）。每次迁移都重算的话，
    /// Accepted→Running 这一次迁移会把死线往后推，**用户在连接上配的上限就永远不生效**。
    ///
    /// 为什么放在实体上，而不是留在各仓库里：这条规则原来在 `sqlite/exec.rs` 与
    /// `infrastructure/fakes.rs` **各有一份**，而"1 小时"这个数字更是在四处各写了一遍。
    /// 只改一处的话，替身会与生产悄悄不一致——**替身不一致 = 测试替生产说谎**，
    /// 而这类漂移在这个仓库里已经咬过一次（"旧替身没有'在跑几个'的状态，所以那个场景
    /// 根本写不出来"）。
    pub fn apply_tracking(&mut self, at: Timestamp) {
        match self.status {
            RunStatus::Accepted | RunStatus::Running | RunStatus::SubmissionUncertain => {
                self.tracking_state = TrackingState::Active;
                if self.tracking_deadline.is_none() {
                    self.tracking_deadline = Some(Timestamp::from_millis(
                        at.millis() + super::connection_options::DEFAULT_TRACKING_DEADLINE_MILLIS,
                    ));
                }
            }
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled => {
                self.tracking_state = TrackingState::Exhausted;
                self.tracking_deadline = None;
                self.next_poll_at = None;
            }
            _ => {
                self.tracking_state = TrackingState::Inactive;
                self.tracking_deadline = None;
            }
        }
    }
}

fn tracking_for(status: RunStatus) -> TrackingState {
    match status {
        RunStatus::Accepted | RunStatus::Running | RunStatus::SubmissionUncertain => {
            TrackingState::Active
        }
        RunStatus::Queued | RunStatus::Preparing | RunStatus::Submitting | RunStatus::RetryWait => {
            TrackingState::Inactive
        }
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled => TrackingState::Exhausted,
    }
}

fn next_status(current: RunStatus, transition: Transition) -> DomainResult<RunStatus> {
    use RunStatus as Status;
    use Transition as T;
    let ok = matches!(
        (current, transition),
        (Status::Queued, T::ToPreparing)
            | (Status::Queued, T::ToCancelled)
            | (Status::Preparing, T::ToSubmitting)
            | (Status::Preparing, T::ToRetryWait)
            | (Status::Preparing, T::ToFailed)
            | (Status::Preparing, T::ToCancelled)
            | (Status::Submitting, T::ToAccepted)
            | (Status::Submitting, T::ToSucceeded)
            | (Status::Submitting, T::ToRetryWait)
            | (Status::Submitting, T::ToFailed)
            | (Status::Submitting, T::ToCancelled)
            | (Status::Submitting, T::ToSubmissionUncertain)
            | (Status::SubmissionUncertain, T::ToAccepted)
            | (Status::SubmissionUncertain, T::ToRunning)
            | (Status::SubmissionUncertain, T::ToSucceeded)
            | (Status::SubmissionUncertain, T::ToFailed)
            | (Status::SubmissionUncertain, T::ToCancelled)
            | (Status::SubmissionUncertain, T::ToRetryWait)
            | (Status::Accepted, T::ToRunning)
            | (Status::Accepted, T::ToSucceeded)
            | (Status::Accepted, T::ToFailed)
            | (Status::Accepted, T::ToCancelled)
            | (Status::Running, T::ToSucceeded)
            | (Status::Running, T::ToFailed)
            | (Status::Running, T::ToCancelled)
            | (Status::RetryWait, T::ToPreparing)
            | (Status::RetryWait, T::ToFailed)
            | (Status::RetryWait, T::ToCancelled)
    );
    if ok {
        Ok(transition_target(transition))
    } else {
        Err(SafeError::validation(
            "ILLEGAL_RUN_TRANSITION",
            format!("Run 状态转换非法：{current:?} -> {transition:?}"),
        ))
    }
}

fn transition_target(transition: Transition) -> RunStatus {
    use RunStatus as Status;
    use Transition as T;
    match transition {
        T::ToPreparing => Status::Preparing,
        T::ToSubmitting => Status::Submitting,
        T::ToAccepted => Status::Accepted,
        T::ToRunning => Status::Running,
        T::ToSucceeded => Status::Succeeded,
        T::ToFailed => Status::Failed,
        T::ToCancelled => Status::Cancelled,
        T::ToSubmissionUncertain => Status::SubmissionUncertain,
        T::ToRetryWait => Status::RetryWait,
    }
}

pub fn project_operation_status(runs: &[Run]) -> OperationStatus {
    use OperationStatus as Status;
    let terminal = |status: RunStatus| status.is_terminal();
    let counts = (
        runs.iter()
            .filter(|run| run.status == RunStatus::Queued)
            .count(),
        runs.iter()
            .filter(|run| run.status == RunStatus::Succeeded)
            .count(),
        runs.iter()
            .filter(|run| run.status == RunStatus::Cancelled)
            .count(),
        runs.iter()
            .filter(|run| run.status == RunStatus::Failed)
            .count(),
    );
    if runs.is_empty() {
        return Status::Queued;
    }
    if counts.0 == runs.len() {
        return Status::Queued;
    }
    if runs.iter().any(|run| !terminal(run.status)) {
        return Status::Running;
    }
    if counts.1 == runs.len() {
        return Status::Succeeded;
    }
    if counts.2 == runs.len() {
        return Status::Cancelled;
    }
    if counts.1 > 0 {
        return Status::Partial;
    }
    Status::Failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::ids::{ConnectionRevisionId, RecipeRevisionId};

    fn run(id: u64, status: RunStatus) -> Run {
        Run {
            usage: None,
            id: RunId(id),
            operation_id: OperationId(1),
            client_run_id: "run-1".to_string(),
            recipe_revision_id: RecipeRevisionId(1),
            connection_revision_id: ConnectionRevisionId(1),
            sequence: 0,
            inputs: BTreeMap::new(),
            assets: vec![],
            status,
            tracking_state: tracking_for(status),
            tracking_deadline: None,
            next_poll_at: None,
            last_polled_at: None,
            error: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn queued_run_can_start_preparing() {
        let mut run = run(1, RunStatus::Queued);
        assert!(run.apply(Transition::ToPreparing, Timestamp::now()).is_ok());
        assert_eq!(run.status, RunStatus::Preparing);
    }

    #[test]
    fn terminal_run_rejects_active_transition() {
        let mut run = run(1, RunStatus::Succeeded);
        assert!(run.apply(Transition::ToRunning, Timestamp::now()).is_err());
    }

    /// 迁移**不得**把已有的死线往后推。
    ///
    /// 这是本轮修的语义问题：原来的实现每次迁移都重算死线 ⇒ Accepted→Running 会把"1 小时"
    /// 变成"从 Running 起再 1 小时"，于是**用户在连接上配的上限永远不生效**。
    ///
    /// 反向控制：**没有**死线的 run 仍必须被补上默认值——历史脏数据（旧版本清空过死线的
    /// run）靠它自愈，少了这半条，一个"永远不写死线"的实现也能过前半条。
    #[test]
    fn apply_tracking_keeps_an_existing_deadline() {
        let mut run = run(1, RunStatus::Accepted);
        run.apply_tracking(Timestamp::now());
        let default_deadline = run.tracking_deadline.clone().expect("受理后应补上默认死线");

        // 手工换成"很近"的一条：模拟提交时按**连接配置**写下的那个值。
        let pinned = Timestamp::from_millis(Timestamp::now_millis() + 120_000);
        run.tracking_deadline = Some(pinned.clone());

        // 再迁一次（Accepted → Running），然后照旧重算跟踪状态。
        run.apply(Transition::ToRunning, Timestamp::now())
            .expect("Accepted → Running 应当合法");
        run.apply_tracking(Timestamp::now());
        assert_eq!(
            run.tracking_deadline,
            Some(pinned),
            "已有的死线不得被迁移推后——推后等于连接上配的上限不生效"
        );
        assert_ne!(run.tracking_deadline, Some(default_deadline));

        // 反向控制：死线缺失时必须补默认值。
        run.tracking_deadline = None;
        run.apply_tracking(Timestamp::now());
        assert!(
            run.tracking_deadline.is_some(),
            "缺死线的 run 必须被补上默认值，否则它会永远卡在对账循环里"
        );
    }

    /// 终态把死线清掉（死线只对"还在跟"的 run 有意义）。
    #[test]
    fn terminal_status_clears_the_deadline() {
        let mut run = run(1, RunStatus::Accepted);
        run.apply_tracking(Timestamp::now());
        assert!(run.tracking_deadline.is_some());

        run.apply(Transition::ToSucceeded, Timestamp::now())
            .expect("Accepted → Succeeded 应当合法");
        run.apply_tracking(Timestamp::now());
        assert_eq!(run.tracking_deadline, None);
        assert_eq!(run.tracking_state, TrackingState::Exhausted);
    }

    #[test]
    fn retry_wait_can_return_to_preparing() {
        let mut run = run(1, RunStatus::RetryWait);
        assert!(run.apply(Transition::ToPreparing, Timestamp::now()).is_ok());
        assert_eq!(run.status, RunStatus::Preparing);
    }

    #[test]
    fn submitting_can_become_succeeded_synchronously() {
        let mut run = run(1, RunStatus::Submitting);
        assert!(run.apply(Transition::ToSucceeded, Timestamp::now()).is_ok());
    }

    #[test]
    fn submitting_can_become_uncertain() {
        let mut run = run(1, RunStatus::Submitting);
        assert!(run
            .apply(Transition::ToSubmissionUncertain, Timestamp::now())
            .is_ok());
    }

    #[test]
    fn uncertain_can_enter_retry_wait() {
        let mut run = run(1, RunStatus::SubmissionUncertain);
        assert!(run.apply(Transition::ToRetryWait, Timestamp::now()).is_ok());
    }

    #[test]
    fn queued_run_rejects_submitting_directly() {
        let mut run = run(1, RunStatus::Queued);
        assert!(run
            .apply(Transition::ToSubmitting, Timestamp::now())
            .is_err());
    }

    #[test]
    fn cancelled_run_stays_terminal() {
        let mut run = run(1, RunStatus::Cancelled);
        assert!(run.apply(Transition::ToAccepted, Timestamp::now()).is_err());
    }

    #[test]
    fn operation_status_projection_all_queued() {
        let runs = vec![run(1, RunStatus::Queued), run(2, RunStatus::Queued)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Queued);
    }

    #[test]
    fn operation_status_projection_mixed_active() {
        let runs = vec![run(1, RunStatus::Queued), run(2, RunStatus::Running)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Running);
    }

    #[test]
    fn operation_status_projection_all_succeeded() {
        let runs = vec![run(1, RunStatus::Succeeded), run(2, RunStatus::Succeeded)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Succeeded);
    }

    #[test]
    fn operation_status_projection_partial() {
        let runs = vec![run(1, RunStatus::Succeeded), run(2, RunStatus::Failed)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Partial);
    }

    #[test]
    fn operation_status_projection_all_failed() {
        let runs = vec![run(1, RunStatus::Failed), run(2, RunStatus::Failed)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Failed);
    }

    #[test]
    fn operation_status_projection_all_cancelled() {
        let runs = vec![run(1, RunStatus::Cancelled), run(2, RunStatus::Cancelled)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Cancelled);
    }

    #[test]
    fn operation_status_projection_mixed_terminal_and_queued_is_running() {
        let runs = vec![run(1, RunStatus::Succeeded), run(2, RunStatus::Queued)];
        assert_eq!(project_operation_status(&runs), OperationStatus::Running);
    }

    #[test]
    fn operation_status_projection_empty_is_queued() {
        assert_eq!(project_operation_status(&[]), OperationStatus::Queued);
    }
}
