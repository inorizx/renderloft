use serde::{Deserialize, Serialize};

use super::ids::ClientId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Gui,
    Cli,
    Mcp,
    Http,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    RecipesRead,
    RecipesWrite,
    OperationsRead,
    OperationsExecute,
    OperationsCancel,
    OperationsDelete,
    OperationsSelectConnection,
    AssetsLocalRead,
    AssetsWrite,
    ArtifactsRead,
    ConnectionsRead,
    ConnectionsManage,
    SettingsRead,
    SettingsManage,
    DiagnosticsRead,
    ActorTokensManage,
    AgentsRead,
    AgentsWrite,
    AgentsRun,
    DeliveriesRead,
    DeliveriesRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    pub id: String,
    pub kind: ActorKind,
    pub client_id: ClientId,
    pub scopes: Vec<Scope>,
    pub label: Option<String>,
}

impl ActorRef {
    pub fn has_scope(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    pub fn has_any_scope(&self, scopes: &[Scope]) -> bool {
        scopes.iter().any(|scope| self.has_scope(*scope))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_check_honors_explicit_scopes() {
        let actor = ActorRef {
            id: "actor-1".to_string(),
            kind: ActorKind::Mcp,
            client_id: ClientId("client-1".to_string()),
            scopes: vec![Scope::OperationsExecute],
            label: None,
        };
        assert!(actor.has_scope(Scope::OperationsExecute));
        assert!(!actor.has_scope(Scope::OperationsCancel));
        assert!(actor.has_any_scope(&[Scope::OperationsRead, Scope::OperationsExecute]));
    }
}
