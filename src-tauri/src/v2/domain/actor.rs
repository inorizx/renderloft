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

/// 全部 scope 与它们的**对外名字**——这张表是唯一权威。
///
/// 为什么要一张表而不是两处 `match`：请求里写 `"operations:execute"`、响应里曾经回
/// `"OperationsExecute"`（`{:?}` 的结果）。于是"读一把 token 的 scopes，再拿去签新 token"
/// 这条最自然的用法**直接失败**，而错因看起来像"scope 不认识"。这张表同时供
/// `Scope::parse`（入）与 `Scope::wire_name`（出）使用，两边不可能再漂。
pub const SCOPE_TABLE: &[(Scope, &str)] = &[
    (Scope::RecipesRead, "recipes:read"),
    (Scope::RecipesWrite, "recipes:write"),
    (Scope::OperationsRead, "operations:read"),
    (Scope::OperationsExecute, "operations:execute"),
    (Scope::OperationsCancel, "operations:cancel"),
    (Scope::OperationsDelete, "operations:delete"),
    (
        Scope::OperationsSelectConnection,
        "operations:select-connection",
    ),
    (Scope::AssetsLocalRead, "assets:local-read"),
    (Scope::AssetsWrite, "assets:write"),
    (Scope::ArtifactsRead, "artifacts:read"),
    (Scope::ConnectionsRead, "connections:read"),
    (Scope::ConnectionsManage, "connections:manage"),
    (Scope::SettingsRead, "settings:read"),
    (Scope::SettingsManage, "settings:manage"),
    (Scope::DiagnosticsRead, "diagnostics:read"),
    (Scope::ActorTokensManage, "actor-tokens:manage"),
    (Scope::AgentsRead, "agents:read"),
    (Scope::AgentsWrite, "agents:write"),
    (Scope::AgentsRun, "agents:run"),
    (Scope::DeliveriesRead, "deliveries:read"),
    (Scope::DeliveriesRun, "deliveries:run"),
];

impl Scope {
    /// 对外名字（写进 token、出现在 `GET /actor-tokens` 的 `scopes` 里）。
    pub fn wire_name(self) -> &'static str {
        SCOPE_TABLE
            .iter()
            .find(|(scope, _)| *scope == self)
            .map(|(_, name)| *name)
            .expect("每个 scope 都必须在 SCOPE_TABLE 里（编译器保证枚举与表同源）")
    }

    /// 把对外名字解析回 scope；不认识就是 `None`（由调用方给 `SCOPE_INVALID`）。
    pub fn parse(raw: &str) -> Option<Self> {
        SCOPE_TABLE
            .iter()
            .find(|(_, name)| *name == raw)
            .map(|(scope, _)| *scope)
    }
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

    /// 入（`parse`）与出（`wire_name`）必须**逐条**同源，且表本身不能有重复名字。
    ///
    /// 判别点：曾经响应回的是 `"OperationsExecute"`、请求收的是 `"operations:execute"`——
    /// "读一把 token 的 scopes 再拿去签新 token"当场失败。这条钉住它不可能再发生。
    #[test]
    fn every_scope_has_one_wire_name_and_round_trips() {
        for (scope, name) in SCOPE_TABLE {
            assert_eq!(
                Scope::parse(name),
                Some(*scope),
                "{name} 必须能被解析回它自己"
            );
            assert_eq!(
                scope.wire_name(),
                *name,
                "wire_name 必须与表里那一条一致（否则出与入又漂了）"
            );
        }
        let mut names = SCOPE_TABLE.iter().map(|(_, name)| *name).collect::<Vec<_>>();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(unique, names.len(), "scope 名字不许重复");

        assert_eq!(Scope::parse("no-such-scope"), None);
    }

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
