use async_trait::async_trait;
use rusqlite::params;

use super::*;
use crate::v2::domain::actor::ActorKind;
use crate::v2::ports::repositories::{ActorTokenRecord, TokenStore};

#[async_trait]
impl TokenStore for SqliteRepositories {
    async fn create(&self, record: &ActorTokenRecord) -> DomainResult<i64> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let scopes_json = serde_json::to_string(&record.scopes)
            .map_err(|_| SafeError::storage("无法序列化 scopes"))?;
        connection
            .execute(
                "INSERT INTO v2_actor_tokens(
                    token_hash, actor_id, kind, client_id, scopes_json, label, expires_at, revoked, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    record.token_hash,
                    record.actor_id,
                    serde_json::to_string(&record.kind)
                        .map_err(|_| SafeError::storage("无法序列化 kind"))?,
                    record.client_id,
                    scopes_json,
                    record.label,
                    record.expires_at.as_ref().map(|t| t.0.clone()),
                    record.revoked as i64,
                    record.created_at.0
                ],
            )
            .map_err(|_| SafeError::storage("无法写入 V2 token"))?;
        Ok(connection.last_insert_rowid())
    }

    async fn find_by_hash(&self, token_hash: &str) -> DomainResult<Option<ActorTokenRecord>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, token_hash, actor_id, kind, client_id, scopes_json, label, expires_at, revoked, created_at
                 FROM v2_actor_tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 token"))?;
        Ok(row.map(token_from_row))
    }

    async fn find_by_id(&self, id: i64) -> DomainResult<Option<ActorTokenRecord>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let row = connection
            .query_row(
                "SELECT id, token_hash, actor_id, kind, client_id, scopes_json, label, expires_at, revoked, created_at
                 FROM v2_actor_tokens WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| SafeError::storage("无法读取 V2 token"))?;
        Ok(row.map(token_from_row))
    }

    async fn list(&self) -> DomainResult<Vec<ActorTokenRecord>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection
            .prepare(
                "SELECT id, token_hash, actor_id, kind, client_id, scopes_json, label, expires_at, revoked, created_at
                 FROM v2_actor_tokens ORDER BY id ASC",
            )
            .map_err(|_| SafeError::storage("无法读取 V2 tokens"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .map_err(|_| SafeError::storage("无法读取 V2 tokens"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SafeError::storage("无法读取 V2 tokens"))?;
        Ok(rows.into_iter().map(token_from_row).collect())
    }

    async fn revoke(&self, id: i64) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .execute(
                "UPDATE v2_actor_tokens SET revoked = 1 WHERE id = ?1",
                params![id],
            )
            .map_err(|_| SafeError::storage("无法撤销 V2 token"))?;
        Ok(())
    }

    async fn set_revoked(&self, id: i64, revoked: bool) -> DomainResult<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .execute(
                "UPDATE v2_actor_tokens SET revoked = ?1 WHERE id = ?2",
                params![revoked as i64, id],
            )
            .map_err(|_| SafeError::storage("无法更新 V2 token 状态"))?;
        Ok(())
    }
}

type TokenRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    String,
);

fn token_from_row(row: TokenRow) -> ActorTokenRecord {
    ActorTokenRecord {
        id: row.0,
        token_hash: row.1,
        actor_id: row.2,
        kind: serde_json::from_str(&row.3).unwrap_or(ActorKind::Http),
        client_id: row.4,
        scopes: serde_json::from_str(&row.5).unwrap_or_default(),
        label: row.6,
        expires_at: row.7.map(Timestamp),
        revoked: row.8 != 0,
        created_at: Timestamp(row.9),
    }
}
