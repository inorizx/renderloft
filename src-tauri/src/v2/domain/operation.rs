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

/// 「为什么没在动」的**唯一一张阶梯表**：`(状态, 稳定原因码, 严重度)`。
///
/// 为什么是一张表而不是几处 `match`：这张表有**三个**消费者，它们必须给出同一个答案——
/// * `run_attention`：取原因码，再补那句人话与下一动作；
/// * `operation_attention`：按严重度挑"最该被看见"的那条；
/// * `sqlite::exec::list_operations`：**摘要列表**里那条相关子查询按同一严重度取最阻塞的 run
///   （SQL 的 `IN` 列表与 `ORDER BY CASE` **由这张表生成**，见 `list_operations`）。
///
/// 抄一份的失效模式是确定的：两边各自都绿，而"列表里挑中的那条"与"详情里挑中的那条"
/// 不是同一条 —— 调用方在列表里看到"排队中"，点进去却是"等确认"。
///
/// 严重度按"它挡住了什么"排：`SUBMISSION_UNCERTAIN` 会占住这条连接唯一的执行位
/// （后续 run 全排队），而且意味着"可能已经花了钱但不知道结果"，所以排第一。
/// **同一个原因码的状态必须同严重度**（`Queued` / `Preparing` 都是"排队中"）。
pub const ATTENTION_LADDER: &[(RunStatus, &str, u8)] = &[
    (RunStatus::SubmissionUncertain, "SUBMISSION_UNCERTAIN", 0),
    (RunStatus::RetryWait, "RETRY_BACKOFF", 1),
    (RunStatus::Submitting, "SUBMITTING", 2),
    (RunStatus::Queued, "QUEUED", 3),
    (RunStatus::Preparing, "QUEUED", 3),
];

/// 这个状态"为什么没在动"的**稳定原因码**；正常在跑的（`Accepted` / `Running`）与三种终态
/// 返回 `None` —— 它们没有"为什么没在动"可说，给了就是噪音。
///
/// 它同时是**摘要**（`OperationSummary::attention_reason`）的取值来源：仓库层只从 SQL 拿回
/// 一个原始状态串，原因码一律经这里翻译，所以 SQL 里不会出现第二份原因码。
pub fn attention_reason(status: RunStatus) -> Option<&'static str> {
    ATTENTION_LADDER
        .iter()
        .find(|(candidate, _, _)| *candidate == status)
        .map(|(_, reason, _)| *reason)
}

/// 原因码 → 严重度（越小越该被先看见）。按**原因码**而不是状态查：同一个原因码的多个状态
/// （`Queued` / `Preparing`）必须同权重，否则"两个都算排队中"这件事会随状态表的顺序漂。
fn severity_of_reason(reason: &str) -> u8 {
    ATTENTION_LADDER
        .iter()
        .find(|(_, candidate, _)| *candidate == reason)
        .map(|(_, _, severity)| *severity)
        .unwrap_or(u8::MAX)
}

/// 这个 run **为什么没在动** —— 每个"在等什么"的非终态都给出机器可读的原因与下一动作。
///
/// 为什么需要它（实测，2026-10-17）：一次提交超时后 run 落 `SubmissionUncertain`，而 operation 级
/// 投影（`project_operation_status`）只看"有没有非终态" ⇒ op 显示 `Running`。调用方据此对外报了
/// "预计 10 分钟出片"，实际它要**等到死线（默认 1 小时）才判失败**。事件流里躺着
/// `run.submission_uncertain`，但没有任何主动呈现——要发现它得逐层看 run → 读事件 → 翻源码。
///
/// 判据沿用仓库的老规矩：**把调用方需要的事实交给调用方**。所以这里给的是
/// 稳定原因码 + 人话 + 死线 + 到点会怎样 + 现在能做什么，而不是让调用方去猜状态机。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttention {
    /// 稳定的机器可读原因码（取值只有 `ATTENTION_LADDER` 里那几种）。
    pub reason: String,
    /// 一句人话：发生了什么、在等什么。
    pub message: String,
    /// 进入当前状态的时间（近似：run 的最后一次状态更新）。
    pub since: Timestamp,
    /// 死线；`None` = 没有死线，只能等外部变化（例如排队等前一个 run 结束）。
    pub deadline: Option<Timestamp>,
    /// 到死线时会发生什么。
    pub deadline_effect: Option<String>,
    /// 现在能做什么（动词名；"新建一个 operation"这类语义差别写在名字里，不靠猜）。
    pub next_actions: Vec<String>,
}

/// 这个 run 现在"为什么没在动"？**正常在跑的（Accepted / Running）与终态返回 `None`**。
///
/// 只讲事实：系统会自己做什么、到点会怎样、人能做什么。不推算、不猜。
pub fn run_attention(run: &Run) -> Option<RunAttention> {
    // 原因码只从阶梯表来（`attention_reason`），这里只补"人话 / 到点会怎样 / 下一动作"。
    let reason = attention_reason(run.status)?;
    let (message, deadline_effect, next_actions): (&str, Option<&str>, &[&str]) = match run.status {
        RunStatus::SubmissionUncertain => (
            "提交时连接断了或超时——**平台到底收没收到不知道**，所以不重投（重投可能重复扣费）。\
             系统正在对账；到死线仍确认不了就判失败，不会自动重投。",
            // 「到点会怎样」**只在真有死线时才说**（这条判据对每一档都成立，见测试）：
            // 死线是 `apply_tracking` 保证的，缺了 reconcile 还会补设；但真没有时就不许承诺 ——
            // 让调用方去等一个不会发生的事件，正是这一整套 attention 要消灭的误报。
            run.tracking_deadline
                .is_some()
                .then_some("判为失败（不会自动重投）"),
            &[
                "operation.get",
                "operation.wait",
                "operation.cancel",
                "operation.retry",
            ],
        ),
        RunStatus::RetryWait => (
            "上一次提交没成功（平台拒了、限流/5xx，或连接被拒这类「字节没出去」的失败），\
             正在退避等待后自动重试——不需要人做任何事。到死线仍未成功就判失败。",
            // ⚠️ 这半句在 2026-10-18 又变回真话了：`RETRY_WAIT` 此前**没有死线**（`apply_tracking`
            // 把它清成 `None`，调度器也没写），于是它会每 30 秒一直重试、永不结束——那时说
            // "到点判失败"是**让调用方去等一个不会发生的事件**，所以当时把这半句删掉了。
            // 现在调度器在第一次进入这一档时写下死线（连接上的 `trackingDeadlineSeconds`，
            // 默认 1 小时）并在到期后判失败，于是这里可以如实说 —— 但仍然**只在真有死线时说**
            //（那一档理论上可能没有：`Uncertain + Idempotent` 那条路今天不可达，但它也没写死线）。
            run.tracking_deadline
                .is_some()
                .then_some("判为失败（不会自动重投；重试要显式 operation retry）"),
            &["operation.get", "operation.wait", "operation.cancel"],
        ),
        RunStatus::Submitting => (
            "正在提交：已落库租约，进程若在这里中断，超过恢复窗口会自动转成「等确认」。",
            Some("超过恢复窗口后转为「等确认」（SUBMISSION_UNCERTAIN）"),
            &["operation.get", "operation.wait"],
        ),
        RunStatus::Queued | RunStatus::Preparing => (
            "排队中：这条连接同时只跑一个（与平台账号的实际并发一致），前面还有没结束的 run。",
            None,
            &["operation.get", "operation.wait", "operation.cancel"],
        ),
        // 上面 `attention_reason` 已经把这几档筛掉了。仍然逐个写出来（而不是 `_ =>`）：
        // 将来给 `RunStatus` 加一个新状态时，编译器会在这里逼我们回答"它算不算在等"。
        RunStatus::Accepted
        | RunStatus::Running
        | RunStatus::Succeeded
        | RunStatus::Failed
        | RunStatus::Cancelled => return None,
    };
    Some(RunAttention {
        reason: reason.to_string(),
        message: message.to_string(),
        since: run.updated_at.clone(),
        deadline: run.tracking_deadline.clone(),
        deadline_effect: deadline_effect.map(str::to_string),
        next_actions: next_actions.iter().map(|name| name.to_string()).collect(),
    })
}

/// 一个 operation 里"最该被看见"的那条 attention：不确定 > 退避 > 提交中 > 排队。
///
/// 排序是**按"它挡住了什么"**（表见 `ATTENTION_LADDER`）。
pub fn operation_attention(runs: &[Run]) -> Option<RunAttention> {
    runs.iter()
        .filter_map(run_attention)
        .min_by_key(|attention| severity_of_reason(&attention.reason))
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

    /// 「为什么没在动」：每个"在等什么"的非终态都要给出原因 + 死线 + 下一动作；
    /// 正常在跑的（Accepted / Running）与终态**不给**——给了就是噪音。
    ///
    /// 负对照：把 `run_attention` 里 `SubmissionUncertain` 那条分支改成 `None` ⇒ 第一条断言红。
    #[test]
    fn attention_explains_waiting_states_and_stays_quiet_while_running() {
        for (status, reason) in [
            (RunStatus::SubmissionUncertain, "SUBMISSION_UNCERTAIN"),
            (RunStatus::RetryWait, "RETRY_BACKOFF"),
            (RunStatus::Submitting, "SUBMITTING"),
            (RunStatus::Queued, "QUEUED"),
        ] {
            let attention = run_attention(&run(1, status))
                .unwrap_or_else(|| panic!("{status:?} 应当给出「为什么没在动」"));
            assert_eq!(attention.reason, reason, "{status:?}");
            assert!(!attention.message.is_empty(), "{status:?} 要给一句人话");
            assert!(
                !attention.next_actions.is_empty(),
                "{status:?} 要说下一步能做什么"
            );
        }
        // 不确定态的**下一动作必须点名恢复路径**（这是那次 40 分钟误报的教训：
        // 调用方知道"它在等"还不够，还得知道"现在能做什么"）。
        // ⚠️ "要说清到点会怎样"那半句**不在这里**验：它取决于 run 上有没有死线，
        // 所以统一放到下面那段"两种取值都验"的循环里（否则这里会造出一个生产里不存在的组合）。
        let uncertain = run_attention(&run(1, RunStatus::SubmissionUncertain)).expect("uncertain");
        assert!(
            uncertain
                .next_actions
                .iter()
                .any(|name| name == "operation.retry"),
            "恢复路径要在下一动作里点名"
        );
        // ⚠️ **"有死线才许说「到点会怎样」"** —— 这条判据现在是**一般规则**，不再是针对某一档的
        // 一次性断言：
        //
        // 历史上 `RETRY_WAIT` 写着 `deadlineEffect: "判为失败"` 而它**根本没有死线**
        //（`apply_tracking` 把它清成 `None`，调度器也没写）⇒ 那句话让调用方**去等一个不会发生的
        // 事件**。上一批把这半句删了；这一批给 `RETRY_WAIT` 真的写了死线（调度器在第一次进入
        // 这一档时写：连接上的 `trackingDeadlineSeconds`，默认 1 小时，到期判失败），于是它
        // 又可以如实说。**两种取值都要验**，因为这两句话是静态的、而死线是运行时写进去的
        // —— 它们正是会各自漂开的一对。
        //
        // 负对照：把 `RetryWait` 那档改回 `None` ⇒ "有死线却不说"红；
        // 把 `Queued` 那档塞一句 `Some("判为失败")` ⇒ "没死线却说了"红。
        for status in [RunStatus::SubmissionUncertain, RunStatus::RetryWait] {
            let mut with_deadline = run(1, status);
            with_deadline.tracking_deadline =
                Some(Timestamp::from_millis(Timestamp::now_millis() + 60_000));
            let stated = run_attention(&with_deadline).expect("等待中的档位");
            assert!(
                stated.deadline.is_some() && stated.deadline_effect.is_some(),
                "{status:?} 有死线就必须说清到点会怎样，否则调用方只能猜"
            );
            let silent = run_attention(&run(1, status)).expect("等待中的档位");
            assert!(
                silent.deadline_effect.is_none(),
                "{status:?} 没有死线就不许说「到点会怎样」——调用方会去等一个不会发生的事件"
            );
        }
        // `SUBMITTING` 的那句话说的是**另一个时钟**（租约恢复窗口，不是 `tracking_deadline`）：
        // 它没有 run 级死线可报，所以那半句必须**不提**判失败，只说到点会转成什么。
        let submitting = run_attention(&run(1, RunStatus::Submitting)).expect("submitting");
        assert!(submitting.deadline.is_none());
        let effect = submitting.deadline_effect.clone().unwrap_or_default();
        assert!(
            effect.contains("等确认") && !effect.contains("失败"),
            "提交中的那句话是关于恢复窗口的，不许混进「判失败」：{effect}"
        );
        for status in [
            RunStatus::Accepted,
            RunStatus::Running,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert!(
                run_attention(&run(1, status)).is_none(),
                "{status:?} 不该有 attention"
            );
        }
    }

    /// operation 级取"最该被看见"的那条：不确定 > 退避 > 提交中 > 排队。
    #[test]
    fn operation_attention_surfaces_the_most_blocking_run() {
        let runs = vec![
            run(1, RunStatus::Queued),
            run(2, RunStatus::SubmissionUncertain),
            run(3, RunStatus::RetryWait),
        ];
        assert_eq!(
            operation_attention(&runs).expect("应当有").reason,
            "SUBMISSION_UNCERTAIN"
        );
        assert!(operation_attention(&[run(1, RunStatus::Running)]).is_none());
    }

    /// 阶梯表是**原因码与严重度的唯一来源**：`run_attention`、`operation_attention` 与
    /// `sqlite::exec::list_operations` 的摘要子查询都从它派生。
    ///
    /// 判别点四条（少一条都留着一种"两份真相"的写法）：
    /// 1. 表里每一行的原因码都能被 `attention_reason` 取到（表与查询函数不能各说各话）；
    /// 2. 同一个原因码只能有一个严重度（`Queued` / `Preparing` 必须同权重）；
    /// 3. 表**按严重度从高到低**写（顺序本身就是可读的排序契约）；
    /// 4. 有原因码 ⇔ `run_attention` 有结论；且**只有"在等什么"的状态**才有原因码
    ///    ——正常在跑的与终态混进来，就是给调用方发噪音。
    ///
    /// 负对照：把 `attention_reason` 改成永远 `None` ⇒ 第 1、4 条红；
    /// 把 `ATTENTION_LADDER` 里 `Preparing` 的严重度改成 9 ⇒ 第 2 条红。
    #[test]
    fn the_attention_ladder_is_the_only_source_of_reasons_and_severity() {
        // 1 + 2
        for (status, reason, severity) in ATTENTION_LADDER {
            assert_eq!(
                attention_reason(*status),
                Some(*reason),
                "{status:?} 的原因码必须能从表里查到"
            );
            for (other_status, other_reason, other_severity) in ATTENTION_LADDER {
                if other_reason == reason {
                    assert_eq!(
                        other_severity, severity,
                        "{other_status:?} 与 {status:?} 共用原因码 {reason}，严重度必须一致\
                         （否则「同一件事」会有两个优先级）"
                    );
                }
            }
        }
        // 3
        let severities = ATTENTION_LADDER
            .iter()
            .map(|(_, _, severity)| *severity)
            .collect::<Vec<_>>();
        let mut sorted = severities.clone();
        sorted.sort_unstable();
        assert_eq!(severities, sorted, "表必须按严重度从高到低写（顺序可读）");

        // 4
        let all = [
            RunStatus::Queued,
            RunStatus::Preparing,
            RunStatus::Submitting,
            RunStatus::SubmissionUncertain,
            RunStatus::Accepted,
            RunStatus::Running,
            RunStatus::RetryWait,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ];
        assert_eq!(
            all.len(),
            ATTENTION_LADDER.len() + 5,
            "新增状态要在这里补一行"
        );
        for status in all {
            assert_eq!(
                attention_reason(status).is_some(),
                run_attention(&run(1, status)).is_some(),
                "{status:?}：原因码与结论必须同时有或同时无"
            );
            if matches!(
                status,
                RunStatus::Accepted | RunStatus::Running | RunStatus::Succeeded
            ) {
                assert!(
                    attention_reason(status).is_none(),
                    "{status:?} 正常在跑/已完成，不该有「为什么没在动」"
                );
            }
        }
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
