use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::actor::ActorRef;
use super::ids::{OperationId, RunId};
use super::model::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: u64,
    pub event_type: String,
    pub operation_id: OperationId,
    pub run_id: Option<RunId>,
    pub actor: ActorRef,
    pub sequence: u64,
    pub created_at: Timestamp,
    pub safe_payload: Value,
}

impl EventRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        sequence: u64,
        event_type: impl Into<String>,
        operation_id: OperationId,
        run_id: Option<RunId>,
        actor: ActorRef,
        created_at: Timestamp,
        safe_payload: Value,
    ) -> Self {
        Self {
            id,
            event_type: event_type.into(),
            operation_id,
            run_id,
            actor,
            sequence,
            created_at,
            safe_payload,
        }
    }
}
