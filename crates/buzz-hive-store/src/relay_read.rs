//! The one module that reads relay tables.
//!
//! Public relay tables hold the pubkey as `text` in `relay_members` and as
//! `bytea` in `users`, so this module converts both ways.

use buzz_hive_core::access::RelayRole;
use buzz_hive_core::ports::store::{AgentInfo, CommunityId, Directory, Pubkey, StoreError};

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

    async fn agents_named(
        &self,
        community: CommunityId,
        name: &str,
    ) -> Result<Vec<AgentInfo>, StoreError> {
        let rows: Vec<(Vec<u8>, String, Vec<u8>)> = sqlx::query_as(
            "SELECT pubkey, display_name, agent_owner_pubkey FROM public.users \
             WHERE community_id = $1 AND agent_owner_pubkey IS NOT NULL \
               AND deactivated_at IS NULL AND display_name IS NOT NULL \
               AND lower(display_name) = lower($2) \
             ORDER BY pubkey",
        )
        .bind(community)
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(rows.into_iter().map(agent).collect())
    }

    async fn list_agents(&self, community: CommunityId) -> Result<Vec<AgentInfo>, StoreError> {
        let rows: Vec<(Vec<u8>, String, Vec<u8>)> = sqlx::query_as(
            "SELECT pubkey, display_name, agent_owner_pubkey FROM public.users \
             WHERE community_id = $1 AND agent_owner_pubkey IS NOT NULL \
               AND deactivated_at IS NULL AND display_name IS NOT NULL \
             ORDER BY lower(display_name), pubkey",
        )
        .bind(community)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(rows.into_iter().map(agent).collect())
    }

    async fn channel_exists(
        &self,
        community: CommunityId,
        channel: uuid::Uuid,
    ) -> Result<bool, StoreError> {
        // A deleted or archived channel takes no posts, so it does not count.
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM public.channels \
             WHERE community_id = $1 AND id = $2 AND deleted_at IS NULL AND archived_at IS NULL)",
        )
        .bind(community)
        .bind(channel)
        .fetch_one(&self.pool)
        .await
        .map_err(unavailable)
    }

    async fn member_of_channel(
        &self,
        community: CommunityId,
        channel: uuid::Uuid,
        pubkey: &str,
    ) -> Result<bool, StoreError> {
        let Ok(bytes) = hex::decode(pubkey) else {
            return Ok(false);
        };
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM public.channel_members \
             WHERE community_id = $1 AND channel_id = $2 AND pubkey = $3 AND removed_at IS NULL)",
        )
        .bind(community)
        .bind(channel)
        .bind(bytes)
        .fetch_one(&self.pool)
        .await
        .map_err(unavailable)
    }
}

fn agent((pubkey, name, owner): (Vec<u8>, String, Vec<u8>)) -> AgentInfo {
    AgentInfo {
        name,
        pubkey: hex::encode(pubkey),
        owner: Some(hex::encode(owner)),
    }
}

/// Every relay column this crate reads. Checked at startup and by tests/postgres_relay_columns.rs.
/// Later phases add `events` (Phase 15).
pub const RELAY_COLUMNS: &[(&str, &str, &str)] = &[
    ("relay_members", "community_id", "uuid"),
    ("relay_members", "pubkey", "text"),
    ("relay_members", "role", "text"),
    ("users", "community_id", "uuid"),
    ("users", "pubkey", "bytea"),
    ("users", "agent_owner_pubkey", "bytea"),
    ("users", "display_name", "text"),
    ("users", "deactivated_at", "timestamp with time zone"),
    ("channels", "community_id", "uuid"),
    ("channels", "id", "uuid"),
    ("channels", "archived_at", "timestamp with time zone"),
    ("channels", "deleted_at", "timestamp with time zone"),
    ("channel_members", "community_id", "uuid"),
    ("channel_members", "channel_id", "uuid"),
    ("channel_members", "pubkey", "bytea"),
    ("channel_members", "removed_at", "timestamp with time zone"),
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
