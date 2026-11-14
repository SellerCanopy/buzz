//! The one module that reads relay tables.
//!
//! Public relay tables hold the pubkey as `text` in `relay_members` and as
//! `bytea` in `users`, so this module converts both ways.

use buzz_hive_core::access::RelayRole;
use buzz_hive_core::ports::store::{CommunityId, Directory, Pubkey, StoreError};

pub struct RelayRead {
    pool: sqlx::PgPool,
}

impl RelayRead {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// The community whose host is `host`, compared case-insensitively.
    pub async fn community_for_host(&self, host: &str) -> Result<Option<CommunityId>, StoreError> {
        sqlx::query_scalar("SELECT id FROM public.communities WHERE lower(host) = lower($1)")
            .bind(host)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)
    }
}

fn unavailable(error: sqlx::Error) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

#[async_trait::async_trait]
impl Directory for RelayRead {
    async fn relay_role(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError> {
        let role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM public.relay_members WHERE community_id = $1 AND pubkey = $2",
        )
        .bind(community)
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(role.and_then(|r| match r.as_str() {
            "owner" => Some(RelayRole::Owner),
            "admin" => Some(RelayRole::Admin),
            "member" => Some(RelayRole::Member),
            _ => None,
        }))
    }

    async fn agent_owner(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<Pubkey>, StoreError> {
        let bytes = hex::decode(pubkey).map_err(|e| StoreError::NotFound(e.to_string()))?;
        let owner: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT agent_owner_pubkey FROM public.users \
             WHERE community_id = $1 AND pubkey = $2 AND agent_owner_pubkey IS NOT NULL",
        )
        .bind(community)
        .bind(bytes)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(owner.map(hex::encode))
    }
}

/// Every relay column this crate reads. Checked at startup and by tests/postgres_relay_columns.rs.
/// Later phases add `channels` and `channel_members` (Phase 6) and `events` (Phase 15).
pub const RELAY_COLUMNS: &[(&str, &str, &str)] = &[
    ("relay_members", "community_id", "uuid"),
    ("relay_members", "pubkey", "text"),
    ("relay_members", "role", "text"),
    ("users", "community_id", "uuid"),
    ("users", "pubkey", "bytea"),
    ("users", "agent_owner_pubkey", "bytea"),
    ("users", "display_name", "text"),
    ("communities", "id", "uuid"),
    ("communities", "host", "text"),
];

/// Fail when a relay column this crate reads is missing or has another type.
pub async fn verify_relay_columns(pool: &sqlx::PgPool) -> Result<(), StoreError> {
    for (table, column, ty) in RELAY_COLUMNS {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_optional(pool)
        .await
        .map_err(unavailable)?;
        match found {
            Some(t) if t == *ty || (*ty == "text" && t == "character varying") => {}
            other => {
                return Err(StoreError::Unavailable(format!(
                    "relay column public.{table}.{column} is {other:?}, expected {ty}; update relay_read.rs"
                )))
            }
        }
    }
    Ok(())
}
