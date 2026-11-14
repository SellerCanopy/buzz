//! `Store` over Postgres: one `sqlx` transaction per community.

use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::ports::store::{
    AccessRepo, AccessRow, CardsRepo, CommunityId, SettingsRepo, Store, StoreError, Tx,
};
use sqlx::{PgPool, Postgres, Transaction};

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// `pool` must come from [`crate::migrate::connect`] so the search path is `hive`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn unavailable(error: sqlx::Error) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

#[async_trait::async_trait]
impl Store for PgStore {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        let tx = self.pool.begin().await.map_err(unavailable)?;
        Ok(Box::new(PgTx { community, tx }))
    }
}

struct PgTx {
    community: CommunityId,
    tx: Transaction<'static, Postgres>,
}

#[async_trait::async_trait]
impl Tx for PgTx {
    fn access(&mut self) -> &mut dyn AccessRepo {
        self
    }
    fn settings(&mut self) -> &mut dyn SettingsRepo {
        self
    }
    fn cards(&mut self) -> &mut dyn CardsRepo {
        self
    }
    async fn commit(self: Box<Self>) -> Result<(), StoreError> {
        self.tx.commit().await.map_err(unavailable)
    }
}

fn decode_row(pubkey: String, level: i16, kind: String) -> Result<AccessRow, StoreError> {
    let level = Level::from_i16(level)
        .ok_or_else(|| StoreError::Conflict(format!("stored level {level} for {pubkey}")))?;
    let kind = ActorKind::parse(&kind)
        .ok_or_else(|| StoreError::Conflict(format!("stored kind {kind} for {pubkey}")))?;
    Ok(AccessRow {
        pubkey,
        level,
        kind,
    })
}

#[async_trait::async_trait]
impl AccessRepo for PgTx {
    async fn level(&mut self, pubkey: &str) -> Result<Option<(Level, ActorKind)>, StoreError> {
        let row: Option<(i16, String)> = sqlx::query_as(
            "SELECT level, kind FROM hive.access_levels WHERE community_id = $1 AND pubkey = $2",
        )
        .bind(self.community)
        .bind(pubkey)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(unavailable)?;
        row.map(|(level, kind)| {
            decode_row(pubkey.to_owned(), level, kind).map(|r| (r.level, r.kind))
        })
        .transpose()
    }

    async fn set_level(
        &mut self,
        pubkey: &str,
        level: Level,
        kind: ActorKind,
        by: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.access_levels (community_id, pubkey, level, kind, updated_by) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (community_id, pubkey) DO UPDATE \
             SET level = $3, kind = $4, updated_by = $5, updated_at = now()",
        )
        .bind(self.community)
        .bind(pubkey)
        .bind(level.as_i16())
        .bind(kind.as_str())
        .bind(by)
        .execute(&mut *self.tx)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    async fn list(&mut self) -> Result<Vec<AccessRow>, StoreError> {
        let rows: Vec<(String, i16, String)> = sqlx::query_as(
            "SELECT pubkey, level, kind FROM hive.access_levels \
             WHERE community_id = $1 ORDER BY pubkey",
        )
        .bind(self.community)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(unavailable)?;
        rows.into_iter()
            .map(|(pubkey, level, kind)| decode_row(pubkey, level, kind))
            .collect()
    }
}

#[async_trait::async_trait]
impl SettingsRepo for PgTx {
    async fn get(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        sqlx::query_scalar(
            "SELECT value FROM hive.settings \
             WHERE community_id = $1 AND scope = $2 AND scope_id = $3 AND key = $4",
        )
        .bind(self.community)
        .bind(scope)
        .bind(scope_id)
        .bind(key)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(unavailable)
    }

    async fn set(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.settings (community_id, scope, scope_id, key, value) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (community_id, scope, scope_id, key) DO UPDATE SET value = $5",
        )
        .bind(self.community)
        .bind(scope)
        .bind(scope_id)
        .bind(key)
        .bind(value)
        .execute(&mut *self.tx)
        .await
        .map_err(unavailable)?;
        Ok(())
    }
}

type CardRow = (uuid::Uuid, String, String, String, String, bool, String);

/// `SELECT` for [`CardRow`] followed by `$tail`, as one literal so sqlx sees a static string.
macro_rules! select_card {
    ($tail:literal) => {
        concat!(
            "SELECT id, title, description, state, workflow, archived_at IS NOT NULL, created_by \
             FROM hive.cards ",
            $tail
        )
    };
}

fn decode_card(row: CardRow) -> Result<Card, StoreError> {
    let (id, title, description, state, workflow, archived, created_by) = row;
    let state = CardState::parse(&state)
        .ok_or_else(|| StoreError::Conflict(format!("stored state {state} for card {id}")))?;
    Ok(Card {
        id,
        title,
        description,
        state,
        workflow,
        archived,
        created_by,
    })
}

#[async_trait::async_trait]
impl CardsRepo for PgTx {
    async fn list(&mut self, archived: bool) -> Result<Vec<Card>, StoreError> {
        let rows: Vec<CardRow> = sqlx::query_as(select_card!(
            "WHERE community_id = $1 AND (archived_at IS NOT NULL) = $2 \
             ORDER BY created_at DESC, id DESC"
        ))
        .bind(self.community)
        .bind(archived)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(unavailable)?;
        rows.into_iter().map(decode_card).collect()
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        let row: Option<CardRow> =
            sqlx::query_as(select_card!("WHERE community_id = $1 AND id = $2"))
                .bind(self.community)
                .bind(id)
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(unavailable)?;
        row.map(decode_card).transpose()
    }
}
