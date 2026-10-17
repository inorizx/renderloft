//! 只读预检（dry-run）：在**花钱之前**验证一次提交的形状。
//!
//! 为什么需要一个独立的用例，而不是让客户端"发了看报错"：
//! * 真实提交会**真的花钱**（每个 run 一笔），形状错在提交前是免费的，在提交后不是；
//! * Agent 尤其是这样——它按描述组装参数，却没有任何一条通道能在提交前把组装结果
//!   对一遍（旧世界里第一次反馈就是一次计费失败）。
//!
//! 边界（承诺）：
//! * **与真实提交共用同一套解析**（`CreateOperation::preflight`）——预检说"过"的东西，
//!   提交时不会因为形状被拒（revision 被并发改动这类时间差除外）；
//! * **不落库、不调度、不碰 provider、不设花钱闸**；
//! * 上传资产的动作由**调用方**做（CLI `--asset` 会先上传再预检；上传本身不计费）。
use std::sync::Arc;

use serde::Serialize;

use crate::v2::domain::actor::ActorRef;
use crate::v2::domain::errors::DomainResult;

use super::create_operation::{CreateOperation, ResolvedRun, RunRequest};

pub struct ValidateOperation {
    /// 复用 create 用例的解析路径：**一份判据**，两个入口（提交 / 预检）。
    pub create: Arc<CreateOperation>,
}

impl ValidateOperation {
    pub fn new(create: Arc<CreateOperation>) -> Self {
        Self { create }
    }

    pub async fn execute(
        &self,
        actor: &ActorRef,
        runs: Vec<RunRequest>,
    ) -> DomainResult<ValidateReport> {
        let resolved = self.create.preflight(actor, &runs).await?;
        Ok(ValidateReport {
            dry_run: true,
            valid: true,
            runs: resolved.into_iter().map(ValidatedRun::from).collect(),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateReport {
    /// 恒为 `true`：这个回执本身就是 dry-run 的产物。
    pub dry_run: bool,
    /// 走到这里就是 `true`（有错会以统一错误信封返回，稳定 code）。
    pub valid: bool,
    pub runs: Vec<ValidatedRun>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedRun {
    pub client_run_id: String,
    pub recipe_revision_id: u64,
    pub connection_revision_id: u64,
    /// 资产解析结果：**字段键 → assetId**（旧位置数组在这里已经换算成字段键）。
    pub assets: Vec<ValidatedAsset>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedAsset {
    pub field_key: String,
    pub asset_id: u64,
}

impl From<ResolvedRun> for ValidatedRun {
    fn from(run: ResolvedRun) -> Self {
        Self {
            client_run_id: run.client_run_id,
            recipe_revision_id: run.recipe_revision_id.0,
            connection_revision_id: run.connection_revision_id.0,
            assets: run
                .assets
                .into_iter()
                .map(|asset| ValidatedAsset {
                    field_key: asset.field_key.unwrap_or_default(),
                    asset_id: asset.asset_id.0,
                })
                .collect(),
        }
    }
}
