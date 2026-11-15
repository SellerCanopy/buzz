//! `Store` over Postgres: one `sqlx` transaction per community.

use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::domain::artifact::{
    ArtifactContent, ArtifactInfo, ArtifactKind, ArtifactVersion,
};
use buzz_hive_core::domain::card::{Card, CardState, PullRequest};
use buzz_hive_core::domain::events::{check_new, Actor, Event};
use buzz_hive_core::domain::inbox::{InboxItem, InboxKind};
use buzz_hive_core::domain::intake::{
    IntakeRule, Issue, IssueState, RepoRef, RuleFilter, StoredIssue,
};
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::domain::timer::{Timer, TimerKind};
use buzz_hive_core::domain::usage::{Usage, UsageWeek};
use buzz_hive_core::ports::store::{
    AccessRepo, AccessRow, ArtifactSave, ArtifactsRepo, CardsRepo, CommunityId, EventsRepo,
    InboxRepo, IntakeRepo, IssuesRepo, OutboxBacklog, OutboxRepo, OutboxRow, Posting,
    PullRequestsRepo, RunsRepo, SettingsRepo, StageThread, Store, StoreError, StoredPackage,
    TimersRepo, Tx, UsageRepo, WorkflowsRepo,
};
use buzz_hive_core::workflow::options::WorkflowOptions;
use sqlx::{PgPool, Postgres, Transaction};

use crate::db_error::store_error;

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// `pool` must come from [`crate::migrate::connect`] so the search path is `hive`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Store for PgStore {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        let tx = self.pool.begin().await.map_err(store_error)?;
        Ok(Box::new(PgTx {
            community,
            tx,
            appended: Vec::new(),
            locked_log: false,
            savepoints: Vec::new(),
        }))
    }
}

struct PgTx {
    community: CommunityId,
    tx: Transaction<'static, Postgres>,
    /// Events appended in this transaction, with the seq Postgres gave them.
    appended: Vec<Event>,
    /// Whether this transaction holds the event-log lock.
    locked_log: bool,
    /// For each open savepoint, newest last: how many events were appended and whether the
    /// log lock was held when it opened. Postgres releases a lock taken after a savepoint
    /// when the savepoint is rolled back, so the flag goes back with it.
    savepoints: Vec<(usize, bool)>,
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
    fn events(&mut self) -> &mut dyn EventsRepo {
        self
    }
    fn workflows(&mut self) -> &mut dyn WorkflowsRepo {
        self
    }
    fn runs(&mut self) -> &mut dyn RunsRepo {
        self
    }
    fn outbox(&mut self) -> &mut dyn OutboxRepo {
        self
    }
    fn artifacts(&mut self) -> &mut dyn ArtifactsRepo {
        self
    }
    fn timers(&mut self) -> &mut dyn TimersRepo {
        self
    }
    fn inbox(&mut self) -> &mut dyn InboxRepo {
        self
    }
    fn usage(&mut self) -> &mut dyn UsageRepo {
        self
    }
    fn intake(&mut self) -> &mut dyn IntakeRepo {
        self
    }
    fn issues(&mut self) -> &mut dyn IssuesRepo {
        self
    }
    fn pull_requests(&mut self) -> &mut dyn PullRequestsRepo {
        self
    }
    async fn savepoint(&mut self) -> Result<(), StoreError> {
        // Savepoints of one name nest: each statement below names the newest.
        sqlx::query("SAVEPOINT hive_sp")
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        self.savepoints.push((self.appended.len(), self.locked_log));
        Ok(())
    }
    async fn rollback_savepoint(&mut self) -> Result<(), StoreError> {
        let (appended, locked_log) = self
            .savepoints
            .pop()
            .ok_or_else(|| StoreError::Conflict("no savepoint is open".into()))?;
        sqlx::query("ROLLBACK TO SAVEPOINT hive_sp")
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        // The savepoint stays open after a rollback to it, so close it.
        sqlx::query("RELEASE SAVEPOINT hive_sp")
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        self.appended.truncate(appended);
        self.locked_log = locked_log;
        Ok(())
    }
    async fn release_savepoint(&mut self) -> Result<(), StoreError> {
        self.savepoints
            .pop()
            .ok_or_else(|| StoreError::Conflict("no savepoint is open".into()))?;
        sqlx::query("RELEASE SAVEPOINT hive_sp")
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }
    async fn commit(self: Box<Self>) -> Result<Vec<Event>, StoreError> {
        let PgTx { tx, appended, .. } = *self;
        tx.commit().await.map_err(store_error)?;
        Ok(appended)
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
        .map_err(store_error)?;
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
        .map_err(store_error)?;
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
        .map_err(store_error)?;
        rows.into_iter()
            .map(|(pubkey, level, kind)| decode_row(pubkey, level, kind))
            .collect()
    }

    async fn lock(&mut self) -> Result<(), StoreError> {
        // Its own key, apart from the event log's, so a change to levels never queues the
        // community's other writers. A change takes this first and the log's second, and no
        // path takes them the other way round.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('hive.access'), hashtext($1::text))")
            .bind(self.community)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
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
        .map_err(store_error)
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
        .map_err(store_error)?;
        Ok(())
    }

    async fn remove(&mut self, scope: &str, scope_id: &str, key: &str) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM hive.settings \
             WHERE community_id = $1 AND scope = $2 AND scope_id = $3 AND key = $4",
        )
        .bind(self.community)
        .bind(scope)
        .bind(scope_id)
        .bind(key)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn list(
        &mut self,
        scope: &str,
        scope_id: &str,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, StoreError> {
        let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT key, value FROM hive.settings \
             WHERE community_id = $1 AND scope = $2 AND scope_id = $3",
        )
        .bind(self.community)
        .bind(scope)
        .bind(scope_id)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(rows.into_iter().collect())
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
        .map_err(store_error)?;
        rows.into_iter().map(decode_card).collect()
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        let row: Option<CardRow> =
            sqlx::query_as(select_card!("WHERE community_id = $1 AND id = $2"))
                .bind(self.community)
                .bind(id)
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(store_error)?;
        row.map(decode_card).transpose()
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        let row: Option<CardRow> = sqlx::query_as(select_card!(
            "WHERE community_id = $1 AND id = $2 FOR UPDATE"
        ))
        .bind(self.community)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_card).transpose()
    }

    async fn insert(&mut self, card: &Card) -> Result<(), StoreError> {
        let inserted = sqlx::query(
            "INSERT INTO hive.cards \
             (community_id, id, title, description, state, workflow, archived_at, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $7 THEN now() END, $8) \
             ON CONFLICT (community_id, id) DO NOTHING",
        )
        .bind(self.community)
        .bind(card.id)
        .bind(&card.title)
        .bind(&card.description)
        .bind(card.state.as_str())
        .bind(&card.workflow)
        .bind(card.archived)
        .bind(&card.created_by)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        if inserted.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!("card {} exists", card.id)));
        }
        Ok(())
    }

    async fn update(&mut self, card: &Card) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE hive.cards SET title = $3, description = $4, state = $5, workflow = $6 \
             WHERE community_id = $1 AND id = $2",
        )
        .bind(self.community)
        .bind(card.id)
        .bind(&card.title)
        .bind(&card.description)
        .bind(card.state.as_str())
        .bind(&card.workflow)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        not_found_if_none(updated.rows_affected(), card.id)
    }

    async fn set_archived(&mut self, id: uuid::Uuid, archived: bool) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE hive.cards \
             SET archived_at = CASE WHEN $3 THEN COALESCE(archived_at, now()) END \
             WHERE community_id = $1 AND id = $2",
        )
        .bind(self.community)
        .bind(id)
        .bind(archived)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        not_found_if_none(updated.rows_affected(), id)
    }

    async fn delete(&mut self, id: uuid::Uuid) -> Result<(), StoreError> {
        let deleted = sqlx::query("DELETE FROM hive.cards WHERE community_id = $1 AND id = $2")
            .bind(self.community)
            .bind(id)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        not_found_if_none(deleted.rows_affected(), id)
    }
}

fn not_found_if_none(rows: u64, id: uuid::Uuid) -> Result<(), StoreError> {
    if rows == 0 {
        Err(StoreError::NotFound(format!("card {id}")))
    } else {
        Ok(())
    }
}

type EventRow = (
    i64,
    String,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    String,
    String,
    serde_json::Value,
    chrono::DateTime<chrono::Utc>,
);

fn decode_event(row: EventRow) -> Result<Event, StoreError> {
    let (seq, kind, card_id, run_id, pubkey, actor_kind, payload, at) = row;
    let kind_of_actor = ActorKind::parse(&actor_kind)
        .ok_or_else(|| StoreError::Conflict(format!("stored actor kind {actor_kind} at {seq}")))?;
    Ok(Event {
        seq,
        kind,
        card_id,
        run_id,
        actor: Actor {
            pubkey,
            kind: kind_of_actor,
        },
        payload,
        at,
    })
}

#[async_trait::async_trait]
impl EventsRepo for PgTx {
    async fn append(&mut self, mut event: Event) -> Result<(), StoreError> {
        check_new(&event.kind, &event.payload).map_err(|e| StoreError::Conflict(e.to_string()))?;
        if !self.locked_log {
            // `bigserial` hands out seq before commit, so without this a reader could see
            // seq 6 while seq 5 is still uncommitted and move past it for good. Holding
            // the lock to commit makes seq order the same as commit order. A reader follows
            // one community's events, so the lock is per community: another community's
            // writers do not queue behind this one.
            sqlx::query(
                "SELECT pg_advisory_xact_lock(hashtext('hive.events'), hashtext($1::text))",
            )
            .bind(self.community)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
            self.locked_log = true;
        }
        event.seq = sqlx::query_scalar(
            "INSERT INTO hive.events \
             (community_id, kind, card_id, run_id, actor_pubkey, actor_kind, payload, at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING seq",
        )
        .bind(self.community)
        .bind(&event.kind)
        .bind(event.card_id)
        .bind(event.run_id)
        .bind(&event.actor.pubkey)
        .bind(event.actor.kind.as_str())
        .bind(&event.payload)
        .bind(event.at)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        self.appended.push(event);
        Ok(())
    }

    async fn after(&mut self, seq: i64, limit: i64) -> Result<Vec<Event>, StoreError> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT seq, kind, card_id, run_id, actor_pubkey, actor_kind, payload, at \
             FROM hive.events WHERE community_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
        )
        .bind(self.community)
        .bind(seq)
        .bind(limit.max(0))
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_event).collect()
    }

    async fn latest_seq(&mut self) -> Result<i64, StoreError> {
        sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM hive.events WHERE community_id = $1")
            .bind(self.community)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(store_error)
    }

    async fn of_card(
        &mut self,
        card: uuid::Uuid,
        kind: &str,
        seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT seq, kind, card_id, run_id, actor_pubkey, actor_kind, payload, at \
             FROM hive.events \
             WHERE community_id = $1 AND card_id = $2 AND kind = $3 AND seq > $4 \
             ORDER BY seq LIMIT $5",
        )
        .bind(self.community)
        .bind(card)
        .bind(kind)
        .bind(seq)
        .bind(limit.max(0))
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_event).collect()
    }
}

type VersionRow = (
    String,
    i32,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    i32,
    String,
    chrono::DateTime<chrono::Utc>,
);

/// `SELECT` for [`VersionRow`] followed by `$tail`, as one literal so sqlx sees a static string.
macro_rules! select_version {
    ($tail:literal) => {
        concat!(
            "SELECT name, version, body, sha256, size, media_type, media_ref, stage, visit, \
             created_by, created_at FROM hive.artifact_versions ",
            $tail
        )
    };
}

fn decode_version(card: uuid::Uuid, row: VersionRow) -> Result<ArtifactVersion, StoreError> {
    let (name, version, body, sha256, size, media_type, media_ref, stage, visit, created_by, at) =
        row;
    let bad =
        |what: &str| StoreError::Conflict(format!("stored {what} for artifact {name} v{version}"));
    let content = match (body, sha256) {
        (Some(body), None) => ArtifactContent::Text(body),
        (None, Some(sha256)) => ArtifactContent::File {
            sha256: sha256.clone(),
            size: size.ok_or_else(|| bad("size"))?,
            media_type: media_type.unwrap_or_default(),
            media_ref: media_ref.unwrap_or(sha256),
        },
        _ => return Err(bad("content")),
    };
    Ok(ArtifactVersion {
        card_id: card,
        name: name.clone(),
        version,
        content,
        stage,
        visit: u32::try_from(visit).map_err(|_| bad("visit"))?,
        created_by,
        created_at: at,
    })
}

#[async_trait::async_trait]
impl ArtifactsRepo for PgTx {
    async fn save(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        handoff: bool,
        save: &ArtifactSave,
    ) -> Result<ArtifactVersion, StoreError> {
        let kind = save.content.kind();
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM hive.artifacts WHERE community_id = $1 AND card_id = $2 AND name = $3",
        )
        .bind(self.community)
        .bind(card)
        .bind(name)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        match existing {
            Some(other) if other != kind.as_str() => {
                return Err(StoreError::Conflict(format!(
                    "artifact {name} is a {other} artifact"
                )));
            }
            Some(_) => {}
            None => {
                sqlx::query(
                    "INSERT INTO hive.artifacts (community_id, card_id, name, kind, handoff) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(self.community)
                .bind(card)
                .bind(name)
                .bind(kind.as_str())
                .bind(handoff)
                .execute(&mut *self.tx)
                .await
                .map_err(store_error)?;
            }
        }
        let (body, sha256, size, media_type, media_ref) = match &save.content {
            ArtifactContent::Text(body) => (Some(body.as_str()), None, None, None, None),
            ArtifactContent::File {
                sha256,
                size,
                media_type,
                media_ref,
            } => (
                None,
                Some(sha256.as_str()),
                Some(*size),
                Some(media_type.as_str()),
                Some(media_ref.as_str()),
            ),
        };
        let row: VersionRow = sqlx::query_as(
            "INSERT INTO hive.artifact_versions \
             (community_id, card_id, name, version, body, sha256, size, media_type, media_ref, \
              stage, visit, created_by) \
             SELECT $1::uuid, $2::uuid, $3::text, COALESCE(max(version), 0) + 1, $4::text, $5::text, \
             $6::bigint, $7::text, $8::text, $9::text, $10::int, $11::text \
             FROM hive.artifact_versions WHERE community_id = $1 AND card_id = $2 AND name = $3 \
             RETURNING name, version, body, sha256, size, media_type, media_ref, stage, visit, \
             created_by, created_at",
        )
        .bind(self.community)
        .bind(card)
        .bind(name)
        .bind(body)
        .bind(sha256)
        .bind(size)
        .bind(media_type)
        .bind(media_ref)
        .bind(&save.stage)
        .bind(save.visit as i32)
        .bind(&save.created_by)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        decode_version(card, row)
    }

    async fn list(&mut self, card: uuid::Uuid) -> Result<Vec<ArtifactInfo>, StoreError> {
        let rows: Vec<(String, String, bool, Option<i32>)> = sqlx::query_as(
            "SELECT a.name, a.kind, a.handoff, \
                    (SELECT max(v.version) FROM hive.artifact_versions v \
                      WHERE v.community_id = a.community_id AND v.card_id = a.card_id \
                        AND v.name = a.name) \
             FROM hive.artifacts a WHERE a.community_id = $1 AND a.card_id = $2 ORDER BY a.name",
        )
        .bind(self.community)
        .bind(card)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter()
            .filter_map(|(name, kind, handoff, latest)| Some((name, kind, handoff, latest?)))
            .map(|(name, kind, handoff, latest)| {
                let kind = ArtifactKind::parse(&kind).ok_or_else(|| {
                    StoreError::Conflict(format!("stored kind {kind} for artifact {name}"))
                })?;
                Ok(ArtifactInfo {
                    name,
                    kind,
                    latest,
                    handoff,
                })
            })
            .collect()
    }

    async fn versions(
        &mut self,
        card: uuid::Uuid,
        name: &str,
    ) -> Result<Vec<ArtifactVersion>, StoreError> {
        let rows: Vec<VersionRow> = sqlx::query_as(select_version!(
            "WHERE community_id = $1 AND card_id = $2 AND name = $3 ORDER BY version"
        ))
        .bind(self.community)
        .bind(card)
        .bind(name)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(|r| decode_version(card, r)).collect()
    }

    async fn version(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        version: Option<i32>,
    ) -> Result<Option<ArtifactVersion>, StoreError> {
        let row: Option<VersionRow> = sqlx::query_as(select_version!(
            "WHERE community_id = $1 AND card_id = $2 AND name = $3 \
             AND ($4::int IS NULL OR version = $4) ORDER BY version DESC LIMIT 1"
        ))
        .bind(self.community)
        .bind(card)
        .bind(name)
        .bind(version)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(|r| decode_version(card, r)).transpose()
    }

    async fn set_handoff(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        handoff: bool,
    ) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE hive.artifacts SET handoff = $4 \
             WHERE community_id = $1 AND card_id = $2 AND name = $3",
        )
        .bind(self.community)
        .bind(card)
        .bind(name)
        .bind(handoff)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("artifact {name}")));
        }
        Ok(())
    }

    async fn saved_in(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT DISTINCT name FROM hive.artifact_versions \
             WHERE community_id = $1 AND card_id = $2 AND stage = $3 AND visit = $4 ORDER BY name",
        )
        .bind(self.community)
        .bind(card)
        .bind(stage)
        .bind(visit as i32)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)
    }
}

#[async_trait::async_trait]
impl WorkflowsRepo for PgTx {
    async fn record_version(
        &mut self,
        name: &str,
        hash: &str,
        body: serde_json::Value,
        created_by: &str,
        builtin: bool,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.workflow_versions (community_id, name, hash, body, created_by) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (community_id, name, hash) DO NOTHING",
        )
        .bind(self.community)
        .bind(name)
        .bind(hash)
        .bind(body)
        .bind(created_by)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        sqlx::query(
            "INSERT INTO hive.workflow_packages (community_id, name, builtin, current_hash) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (community_id, name) DO UPDATE \
             SET builtin = $3, current_hash = $4, updated_at = now()",
        )
        .bind(self.community)
        .bind(name)
        .bind(builtin)
        .bind(hash)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn packages(&mut self) -> Result<Vec<StoredPackage>, StoreError> {
        let rows: Vec<(String, bool, String)> = sqlx::query_as(
            "SELECT name, builtin, current_hash FROM hive.workflow_packages \
             WHERE community_id = $1 ORDER BY name",
        )
        .bind(self.community)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(rows
            .into_iter()
            .map(|(name, builtin, current_hash)| StoredPackage {
                name,
                builtin,
                current_hash,
            })
            .collect())
    }

    async fn version(
        &mut self,
        name: &str,
        hash: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        sqlx::query_scalar(
            "SELECT body FROM hive.workflow_versions \
             WHERE community_id = $1 AND name = $2 AND hash = $3",
        )
        .bind(self.community)
        .bind(name)
        .bind(hash)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)
    }

    async fn options(
        &mut self,
    ) -> Result<std::collections::BTreeMap<String, WorkflowOptions>, StoreError> {
        let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT name, options FROM hive.workflow_options WHERE community_id = $1",
        )
        .bind(self.community)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter()
            .map(|(name, value)| {
                serde_json::from_value(value)
                    .map(|options| (name.clone(), options))
                    .map_err(|e| StoreError::Conflict(format!("stored options for {name}: {e}")))
            })
            .collect()
    }

    async fn set_options(
        &mut self,
        name: &str,
        options: &WorkflowOptions,
    ) -> Result<(), StoreError> {
        let value = serde_json::to_value(options)
            .map_err(|e| StoreError::Conflict(format!("options for {name}: {e}")))?;
        sqlx::query(
            "INSERT INTO hive.workflow_options (community_id, name, options) VALUES ($1, $2, $3) \
             ON CONFLICT (community_id, name) DO UPDATE SET options = $3",
        )
        .bind(self.community)
        .bind(name)
        .bind(value)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}

type RunRow = (
    uuid::Uuid,
    uuid::Uuid,
    String,
    String,
    String,
    Option<String>,
    i32,
    Option<String>,
    serde_json::Value,
    serde_json::Value,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// `SELECT` for [`RunRow`] followed by `$tail`, as one literal so sqlx sees a static string.
macro_rules! select_run {
    ($tail:literal) => {
        concat!(
            "SELECT id, card_id, workflow, workflow_hash, state, current_stage, visit, requester, \
             steps, parameters, started_at, ended_at FROM hive.runs ",
            $tail
        )
    };
}

fn decode_run(row: RunRow) -> Result<Run, StoreError> {
    let (
        id,
        card_id,
        workflow,
        workflow_hash,
        state,
        current_stage,
        visit,
        requester,
        steps,
        parameters,
        started_at,
        ended_at,
    ) = row;
    let state = RunState::parse(&state)
        .ok_or_else(|| StoreError::Conflict(format!("stored state {state} for run {id}")))?;
    Ok(Run {
        id,
        card_id,
        workflow,
        workflow_hash,
        state,
        current_stage,
        visit: u32::try_from(visit)
            .map_err(|_| StoreError::Conflict(format!("stored visit {visit} for run {id}")))?,
        requester,
        steps: serde_json::from_value(steps)
            .map_err(|e| StoreError::Conflict(format!("stored steps of run {id}: {e}")))?,
        parameters: serde_json::from_value(parameters)
            .map_err(|e| StoreError::Conflict(format!("stored parameters of run {id}: {e}")))?,
        started_at,
        ended_at,
    })
}

/// A run's steps and parameters as the JSON the columns hold.
fn encode_run(run: &Run) -> Result<(serde_json::Value, serde_json::Value), StoreError> {
    let to_json = |value: Result<serde_json::Value, serde_json::Error>| {
        value.map_err(|e| StoreError::Conflict(format!("run {} cannot be stored: {e}", run.id)))
    };
    Ok((
        to_json(serde_json::to_value(&run.steps))?,
        to_json(serde_json::to_value(&run.parameters))?,
    ))
}

#[async_trait::async_trait]
impl RunsRepo for PgTx {
    async fn insert(&mut self, run: &Run) -> Result<(), StoreError> {
        let (steps, parameters) = encode_run(run)?;
        let inserted = sqlx::query(
            "INSERT INTO hive.runs \
             (community_id, id, card_id, workflow, workflow_hash, state, current_stage, visit, \
              requester, steps, parameters, started_at, ended_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT DO NOTHING",
        )
        .bind(self.community)
        .bind(run.id)
        .bind(run.card_id)
        .bind(&run.workflow)
        .bind(&run.workflow_hash)
        .bind(run.state.as_str())
        .bind(&run.current_stage)
        .bind(run.visit as i32)
        .bind(&run.requester)
        .bind(steps)
        .bind(parameters)
        .bind(run.started_at)
        .bind(run.ended_at)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        // `ON CONFLICT DO NOTHING` with no target covers the id and the one-active-run index,
        // and leaves the transaction usable, which a raised unique violation would not.
        if inserted.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!(
                "run {} exists, or card {} already has a run that has not ended",
                run.id, run.card_id
            )));
        }
        Ok(())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        let row: Option<RunRow> =
            sqlx::query_as(select_run!("WHERE community_id = $1 AND id = $2"))
                .bind(self.community)
                .bind(id)
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(store_error)?;
        row.map(decode_run).transpose()
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        let row: Option<RunRow> = sqlx::query_as(select_run!(
            "WHERE community_id = $1 AND id = $2 FOR UPDATE"
        ))
        .bind(self.community)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_run).transpose()
    }

    async fn update(&mut self, run: &Run) -> Result<(), StoreError> {
        let (steps, parameters) = encode_run(run)?;
        let updated = sqlx::query(
            "UPDATE hive.runs SET state = $3, current_stage = $4, visit = $5, requester = $6, \
             steps = $7, parameters = $8, ended_at = $9 WHERE community_id = $1 AND id = $2",
        )
        .bind(self.community)
        .bind(run.id)
        .bind(run.state.as_str())
        .bind(&run.current_stage)
        .bind(run.visit as i32)
        .bind(&run.requester)
        .bind(steps)
        .bind(parameters)
        .bind(run.ended_at)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("run {}", run.id)));
        }
        Ok(())
    }

    async fn list(&mut self, card: Option<uuid::Uuid>) -> Result<Vec<Run>, StoreError> {
        let rows: Vec<RunRow> = sqlx::query_as(select_run!(
            "WHERE community_id = $1 AND ($2::uuid IS NULL OR card_id = $2) \
             ORDER BY started_at DESC, id DESC"
        ))
        .bind(self.community)
        .bind(card)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_run).collect()
    }

    async fn active_for_card(&mut self, card: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        let row: Option<RunRow> = sqlx::query_as(select_run!(
            "WHERE community_id = $1 AND card_id = $2 AND state <> 'ended'"
        ))
        .bind(self.community)
        .bind(card)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_run).transpose()
    }

    async fn visit_counts(
        &mut self,
        card: uuid::Uuid,
    ) -> Result<std::collections::BTreeMap<String, u32>, StoreError> {
        // A run's own `visit` covers a run made before steps were kept.
        let rows: Vec<(String, i32)> = sqlx::query_as(
            "SELECT stage, max(visit) FROM ( \
                 SELECT current_stage AS stage, visit FROM hive.runs \
                 WHERE community_id = $1 AND card_id = $2 AND current_stage IS NOT NULL \
                 UNION ALL \
                 SELECT step ->> 'stage', (step ->> 'visit')::int \
                 FROM hive.runs, jsonb_array_elements(steps) AS step \
                 WHERE community_id = $1 AND card_id = $2 \
             ) AS visits GROUP BY stage",
        )
        .bind(self.community)
        .bind(card)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(rows
            .into_iter()
            .map(|(stage, visit)| (stage, visit.max(0) as u32))
            .collect())
    }

    async fn record_thread(&mut self, thread: &StageThread) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.stage_threads \
             (community_id, card_id, stage, visit, root_event_id, agent_pubkey, channel_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (community_id, card_id, stage, visit) DO NOTHING",
        )
        .bind(self.community)
        .bind(thread.card_id)
        .bind(&thread.stage)
        .bind(thread.visit as i32)
        .bind(&thread.root_event_id)
        .bind(&thread.agent_pubkey)
        .bind(thread.channel_id)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn thread_by_root(&mut self, root: &str) -> Result<Option<StageThread>, StoreError> {
        let row: Option<(uuid::Uuid, String, i32, String, uuid::Uuid)> = sqlx::query_as(
            "SELECT card_id, stage, visit, agent_pubkey, channel_id FROM hive.stage_threads \
             WHERE community_id = $1 AND root_event_id = $2",
        )
        .bind(self.community)
        .bind(root)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(row.map(
            |(card_id, stage, visit, agent_pubkey, channel_id)| StageThread {
                card_id,
                stage,
                visit: visit as u32,
                root_event_id: root.to_owned(),
                agent_pubkey,
                channel_id,
            },
        ))
    }

    async fn thread(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Option<StageThread>, StoreError> {
        let row: Option<(String, String, uuid::Uuid)> = sqlx::query_as(
            "SELECT root_event_id, agent_pubkey, channel_id FROM hive.stage_threads \
             WHERE community_id = $1 AND card_id = $2 AND stage = $3 AND visit = $4",
        )
        .bind(self.community)
        .bind(card)
        .bind(stage)
        .bind(visit as i32)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(
            row.map(|(root_event_id, agent_pubkey, channel_id)| StageThread {
                card_id: card,
                stage: stage.to_owned(),
                visit,
                root_event_id,
                agent_pubkey,
                channel_id,
            }),
        )
    }

    async fn threads(&mut self, card: uuid::Uuid) -> Result<Vec<StageThread>, StoreError> {
        let rows: Vec<(String, i32, String, String, uuid::Uuid)> = sqlx::query_as(
            "SELECT stage, visit, root_event_id, agent_pubkey, channel_id \
             FROM hive.stage_threads WHERE community_id = $1 AND card_id = $2 ORDER BY seq",
        )
        .bind(self.community)
        .bind(card)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(rows
            .into_iter()
            .map(
                |(stage, visit, root_event_id, agent_pubkey, channel_id)| StageThread {
                    card_id: card,
                    stage,
                    visit: visit.max(0) as u32,
                    root_event_id,
                    agent_pubkey,
                    channel_id,
                },
            )
            .collect())
    }
}

type OutboxDbRow = (
    i64,
    String,
    uuid::Uuid,
    serde_json::Value,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    i32,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// `SELECT` for [`OutboxDbRow`] followed by `$tail`.
macro_rules! select_outbox {
    ($tail:literal) => {
        concat!(
            "SELECT id, key, card_id, effect, created_at, done_at, attempts, last_error, claimed_at \
             FROM hive.outbox ",
            $tail
        )
    };
}

fn decode_outbox(row: OutboxDbRow) -> Result<OutboxRow, StoreError> {
    let (id, key, card_id, effect, created_at, done_at, attempts, last_error, claimed_at) = row;
    let posting: Posting = serde_json::from_value(effect)
        .map_err(|e| StoreError::Conflict(format!("stored post {key}: {e}")))?;
    Ok(OutboxRow {
        id,
        key,
        card_id,
        posting,
        created_at,
        done_at,
        claimed_at,
        attempts,
        last_error,
    })
}

#[async_trait::async_trait]
impl TimersRepo for PgTx {
    async fn arm(
        &mut self,
        run: uuid::Uuid,
        stage: &str,
        kind: TimerKind,
        after: std::time::Duration,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.timers (community_id, run_id, stage, kind, fire_at) \
             VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5)) \
             ON CONFLICT (community_id, run_id, stage, kind) DO UPDATE SET fire_at = EXCLUDED.fire_at",
        )
        .bind(self.community)
        .bind(run)
        .bind(stage)
        .bind(kind.as_str())
        .bind(after.as_secs_f64())
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn clear_run(&mut self, run: uuid::Uuid) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM hive.timers WHERE community_id = $1 AND run_id = $2")
            .bind(self.community)
            .bind(run)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn due(&mut self, limit: i64) -> Result<Vec<Timer>, StoreError> {
        let rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
            "SELECT run_id, stage, kind FROM hive.timers \
             WHERE community_id = $1 AND fire_at <= now() ORDER BY fire_at, run_id LIMIT $2",
        )
        .bind(self.community)
        .bind(limit)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter()
            .map(|(run_id, stage, kind)| {
                let kind = TimerKind::parse(&kind)
                    .ok_or_else(|| StoreError::Conflict(format!("timer kind {kind:?}")))?;
                Ok(Timer {
                    run_id,
                    stage,
                    kind,
                })
            })
            .collect()
    }

    async fn take_due(
        &mut self,
        run: uuid::Uuid,
        stage: &str,
        kind: TimerKind,
    ) -> Result<bool, StoreError> {
        let taken = sqlx::query(
            "DELETE FROM hive.timers WHERE community_id = $1 AND run_id = $2 \
             AND stage = $3 AND kind = $4 AND fire_at <= now()",
        )
        .bind(self.community)
        .bind(run)
        .bind(stage)
        .bind(kind.as_str())
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(taken.rows_affected() == 1)
    }

    async fn remove(
        &mut self,
        run: uuid::Uuid,
        stage: &str,
        kind: TimerKind,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM hive.timers WHERE community_id = $1 AND run_id = $2 \
             AND stage = $3 AND kind = $4",
        )
        .bind(self.community)
        .bind(run)
        .bind(stage)
        .bind(kind.as_str())
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn next_in(&mut self) -> Result<Option<std::time::Duration>, StoreError> {
        let secs: Option<f64> = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM (min(fire_at) - now()))::float8 \
             FROM hive.timers WHERE community_id = $1",
        )
        .bind(self.community)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(secs.map(|secs| std::time::Duration::from_secs_f64(secs.max(0.0))))
    }
}

type InboxRow = (
    uuid::Uuid,
    uuid::Uuid,
    uuid::Uuid,
    String,
    i32,
    String,
    String,
    serde_json::Value,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
    Option<String>,
);

/// The columns of an [`InboxRow`], as a literal so queries can be built with `concat!`.
macro_rules! inbox_columns {
    () => {
        "id, card_id, run_id, stage, visit, kind, title, detail, opened_at, resolved_at, \
         resolved_by, answer"
    };
}

fn decode_inbox(row: InboxRow) -> Result<InboxItem, StoreError> {
    let (
        id,
        card_id,
        run_id,
        stage,
        visit,
        kind,
        title,
        detail,
        opened_at,
        resolved_at,
        resolved_by,
        answer,
    ) = row;
    Ok(InboxItem {
        id,
        card_id,
        run_id,
        stage,
        visit: u32::try_from(visit)
            .map_err(|_| StoreError::Conflict(format!("stored visit {visit} for item {id}")))?,
        kind: InboxKind::parse(&kind)
            .ok_or_else(|| StoreError::Conflict(format!("stored kind {kind} for item {id}")))?,
        title,
        detail,
        opened_at,
        resolved_at,
        resolved_by,
        answer,
    })
}

#[async_trait::async_trait]
impl InboxRepo for PgTx {
    async fn insert(&mut self, item: &InboxItem) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.inbox (community_id, id, card_id, run_id, stage, visit, kind, \
             title, detail, opened_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(self.community)
        .bind(item.id)
        .bind(item.card_id)
        .bind(item.run_id)
        .bind(&item.stage)
        .bind(i32::try_from(item.visit).unwrap_or(i32::MAX))
        .bind(item.kind.as_str())
        .bind(&item.title)
        .bind(&item.detail)
        .bind(item.opened_at)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<InboxItem>, StoreError> {
        let row: Option<InboxRow> = sqlx::query_as(concat!(
            "SELECT ",
            inbox_columns!(),
            " FROM hive.inbox WHERE community_id = $1 AND id = $2"
        ))
        .bind(self.community)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_inbox).transpose()
    }

    async fn list(
        &mut self,
        card: Option<uuid::Uuid>,
        include_resolved: bool,
    ) -> Result<Vec<InboxItem>, StoreError> {
        let rows: Vec<InboxRow> = sqlx::query_as(concat!(
            "SELECT ",
            inbox_columns!(),
            " FROM hive.inbox WHERE community_id = $1 \
             AND ($2::uuid IS NULL OR card_id = $2) AND ($3 OR resolved_at IS NULL) \
             ORDER BY (resolved_at IS NOT NULL), opened_at DESC, id"
        ))
        .bind(self.community)
        .bind(card)
        .bind(include_resolved)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_inbox).collect()
    }

    async fn resolve(
        &mut self,
        id: uuid::Uuid,
        answer: &str,
        by: &str,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "UPDATE hive.inbox SET resolved_at = now(), resolved_by = $3, answer = $4 \
             WHERE community_id = $1 AND id = $2 AND resolved_at IS NULL",
        )
        .bind(self.community)
        .bind(id)
        .bind(by)
        .bind(answer)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(done.rows_affected() == 1)
    }

    async fn resolve_card(
        &mut self,
        card: uuid::Uuid,
        answer: &str,
        by: &str,
    ) -> Result<Vec<InboxItem>, StoreError> {
        let rows: Vec<InboxRow> = sqlx::query_as(concat!(
            "UPDATE hive.inbox SET resolved_at = now(), resolved_by = $3, answer = $4 \
             WHERE community_id = $1 AND card_id = $2 AND resolved_at IS NULL RETURNING ",
            inbox_columns!()
        ))
        .bind(self.community)
        .bind(card)
        .bind(by)
        .bind(answer)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        let mut items = rows
            .into_iter()
            .map(decode_inbox)
            .collect::<Result<Vec<_>, _>>()?;
        items.sort_by_key(|item| (std::cmp::Reverse(item.opened_at), item.id));
        Ok(items)
    }
}

#[async_trait::async_trait]
impl UsageRepo for PgTx {
    async fn usage(&mut self, weeks: u32, engine: &str) -> Result<Usage, StoreError> {
        let (runs, ended, average, longest): (i64, i64, Option<f64>, Option<f64>) = sqlx::query_as(
            "SELECT count(*), count(ended_at), \
                 avg(EXTRACT(EPOCH FROM (ended_at - started_at)))::float8, \
                 max(EXTRACT(EPOCH FROM (ended_at - started_at)))::float8 \
                 FROM hive.runs WHERE community_id = $1",
        )
        .bind(self.community)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        // One row per week, so a week with nothing still shows. A week starts Monday, UTC.
        let rows: Vec<(chrono::NaiveDate, i64, i64)> = sqlx::query_as(
            "WITH w AS (SELECT generate_series( \
                 date_trunc('week', now() AT TIME ZONE 'utc') - make_interval(weeks => $2 - 1), \
                 date_trunc('week', now() AT TIME ZONE 'utc'), interval '1 week') AS starts) \
             SELECT w.starts::date, \
                 count(e.seq) FILTER (WHERE e.kind = 'card.created'), \
                 count(e.seq) FILTER (WHERE e.kind = 'run.started') \
             FROM w LEFT JOIN hive.events e ON e.community_id = $1 \
                 AND e.actor_kind = 'agent' AND e.actor_pubkey <> $3 \
                 AND e.kind IN ('card.created', 'run.started') \
                 AND e.at >= w.starts AT TIME ZONE 'utc' \
                 AND e.at < (w.starts + interval '1 week') AT TIME ZONE 'utc' \
             GROUP BY w.starts ORDER BY w.starts",
        )
        .bind(self.community)
        .bind(i32::try_from(weeks).unwrap_or(i32::MAX))
        .bind(engine)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
        let seconds = |s: Option<f64>| s.map_or(0, |s| s.max(0.0) as u64);
        Ok(Usage {
            runs: count(runs),
            ended_runs: count(ended),
            average_run_seconds: seconds(average),
            longest_run_seconds: seconds(longest),
            weeks: rows
                .into_iter()
                .map(|(starts, cards, started)| UsageWeek {
                    starts,
                    cards_created_by_agents: count(cards),
                    runs_started_by_agents: count(started),
                })
                .collect(),
        })
    }
}

#[async_trait::async_trait]
impl OutboxRepo for PgTx {
    async fn enqueue(
        &mut self,
        key: &str,
        card: uuid::Uuid,
        posting: &Posting,
    ) -> Result<bool, StoreError> {
        let effect = serde_json::to_value(posting)
            .map_err(|e| StoreError::Conflict(format!("post {key}: {e}")))?;
        let inserted = sqlx::query(
            "INSERT INTO hive.outbox (community_id, key, card_id, effect) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (community_id, key) DO NOTHING",
        )
        .bind(self.community)
        .bind(key)
        .bind(card)
        .bind(effect)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn claim_next(
        &mut self,
        skip_cards: &[uuid::Uuid],
        lease: std::time::Duration,
    ) -> Result<Option<OutboxRow>, StoreError> {
        // Only a card's oldest pending post is offered, so a later post never goes ahead of
        // one that is in flight or failed. SKIP LOCKED keeps two claimers off the same row.
        let row: Option<OutboxDbRow> = sqlx::query_as(
            "UPDATE hive.outbox SET claimed_at = now() WHERE id = ( \
               SELECT o.id FROM hive.outbox o \
               WHERE o.community_id = $1 AND o.done_at IS NULL \
                 AND NOT (o.card_id = ANY($2)) \
                 AND (o.claimed_at IS NULL OR o.claimed_at < now() - make_interval(secs => $3)) \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM hive.outbox e \
                   WHERE e.community_id = o.community_id AND e.card_id = o.card_id \
                     AND e.done_at IS NULL AND e.id < o.id) \
               ORDER BY o.id LIMIT 1 FOR UPDATE SKIP LOCKED) \
             RETURNING id, key, card_id, effect, created_at, done_at, attempts, last_error, \
                       claimed_at",
        )
        .bind(self.community)
        .bind(skip_cards)
        .bind(lease.as_secs_f64())
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_outbox).transpose()
    }

    async fn mark_done(&mut self, id: i64) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE hive.outbox SET done_at = now(), claimed_at = NULL \
             WHERE community_id = $1 AND id = $2",
        )
        .bind(self.community)
        .bind(id)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn mark_failed(&mut self, id: i64, error: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE hive.outbox SET attempts = attempts + 1, last_error = $3, claimed_at = NULL \
             WHERE community_id = $1 AND id = $2",
        )
        .bind(self.community)
        .bind(id)
        .bind(error)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn all(&mut self) -> Result<Vec<OutboxRow>, StoreError> {
        let rows: Vec<OutboxDbRow> =
            sqlx::query_as(select_outbox!("WHERE community_id = $1 ORDER BY id"))
                .bind(self.community)
                .fetch_all(&mut *self.tx)
                .await
                .map_err(store_error)?;
        rows.into_iter().map(decode_outbox).collect()
    }

    async fn backlog(&mut self) -> Result<OutboxBacklog, StoreError> {
        let (pending, failed): (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE attempts > 0) FROM hive.outbox \
             WHERE community_id = $1 AND done_at IS NULL",
        )
        .bind(self.community)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(OutboxBacklog { pending, failed })
    }

    async fn by_key(&mut self, key: &str) -> Result<Option<OutboxRow>, StoreError> {
        let row: Option<OutboxDbRow> =
            sqlx::query_as(select_outbox!("WHERE community_id = $1 AND key = $2"))
                .bind(self.community)
                .bind(key)
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(store_error)?;
        row.map(decode_outbox).transpose()
    }
}

type RuleRow = (
    uuid::Uuid,
    String,
    String,
    serde_json::Value,
    String,
    bool,
    i32,
);

fn decode_rule(row: RuleRow) -> Result<IntakeRule, StoreError> {
    let (id, name, repo, filter, workflow, enabled, interval_s) = row;
    Ok(IntakeRule {
        id,
        name,
        repo: RepoRef::parse(&repo)
            .map_err(|e| StoreError::Conflict(format!("stored repo of rule {id}: {e}")))?,
        filter: serde_json::from_value::<RuleFilter>(filter)
            .map_err(|e| StoreError::Conflict(format!("stored filter of rule {id}: {e}")))?,
        workflow,
        enabled,
        interval_s: u32::try_from(interval_s)
            .map_err(|_| StoreError::Conflict(format!("stored interval of rule {id}")))?,
    })
}

#[async_trait::async_trait]
impl IntakeRepo for PgTx {
    async fn list(&mut self) -> Result<Vec<IntakeRule>, StoreError> {
        let rows: Vec<RuleRow> = sqlx::query_as(
            "SELECT id, name, repo, filter, workflow, enabled, interval_s \
             FROM hive.intake_rules WHERE community_id = $1 ORDER BY created_at, id",
        )
        .bind(self.community)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_rule).collect()
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<IntakeRule>, StoreError> {
        let row: Option<RuleRow> = sqlx::query_as(
            "SELECT id, name, repo, filter, workflow, enabled, interval_s \
             FROM hive.intake_rules WHERE community_id = $1 AND id = $2",
        )
        .bind(self.community)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_rule).transpose()
    }

    async fn save(&mut self, rule: &IntakeRule) -> Result<(), StoreError> {
        let filter = serde_json::to_value(&rule.filter)
            .map_err(|e| StoreError::Internal(format!("rule filter: {e}")))?;
        sqlx::query(
            "INSERT INTO hive.intake_rules \
             (community_id, id, name, repo, filter, workflow, enabled, interval_s) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (community_id, id) DO UPDATE SET name = EXCLUDED.name, \
             repo = EXCLUDED.repo, filter = EXCLUDED.filter, workflow = EXCLUDED.workflow, \
             enabled = EXCLUDED.enabled, interval_s = EXCLUDED.interval_s",
        )
        .bind(self.community)
        .bind(rule.id)
        .bind(&rule.name)
        .bind(rule.repo.to_string())
        .bind(filter)
        .bind(&rule.workflow)
        .bind(rule.enabled)
        .bind(i32::try_from(rule.interval_s).unwrap_or(i32::MAX))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn delete(&mut self, id: uuid::Uuid) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM hive.intake_rules WHERE community_id = $1 AND id = $2")
            .bind(self.community)
            .bind(id)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        if done.rows_affected() == 0 {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE hive.issues SET rule_id = NULL WHERE community_id = $1 AND rule_id = $2",
        )
        .bind(self.community)
        .bind(id)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(true)
    }
}

type IssueRow = (
    String,
    i64,
    String,
    String,
    Vec<String>,
    Vec<String>,
    String,
    String,
    String,
    Option<String>,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    String,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// The columns of an [`IssueRow`], as a literal so queries can be built with `concat!`.
macro_rules! issue_columns {
    () => {
        "repo, number, title, body, labels, assignees, author, state, url, etag, card_id, \
         rule_id, left_reason, last_comment_at"
    };
}

fn decode_issue(row: IssueRow) -> Result<StoredIssue, StoreError> {
    let (
        repo,
        number,
        title,
        body,
        labels,
        assignees,
        author,
        state,
        url,
        etag,
        card_id,
        rule_id,
        left,
        last_comment_at,
    ) = row;
    let repo_ref = RepoRef::parse(&repo)
        .map_err(|e| StoreError::Conflict(format!("stored issue repo: {e}")))?;
    Ok(StoredIssue {
        issue: Issue {
            number: u64::try_from(number)
                .map_err(|_| StoreError::Conflict(format!("stored number {number} in {repo}")))?,
            repo: repo_ref,
            title,
            body,
            labels,
            assignees,
            author,
            state: IssueState::parse(&state)
                .ok_or_else(|| StoreError::Conflict(format!("stored state {state} in {repo}")))?,
            url,
            etag,
        },
        card_id,
        rule_id,
        left,
        last_comment_at,
    })
}

fn issue_number(number: u64) -> Result<i64, StoreError> {
    i64::try_from(number).map_err(|_| StoreError::Conflict(format!("issue number {number}")))
}

#[async_trait::async_trait]
impl PullRequestsRepo for PgTx {
    async fn put(&mut self, card: uuid::Uuid, pull: &PullRequest) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hive.pull_requests (community_id, card_id, repo, number, url, head, \
             base, state, draft) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (community_id, card_id) DO UPDATE SET repo = EXCLUDED.repo, \
             number = EXCLUDED.number, url = EXCLUDED.url, head = EXCLUDED.head, \
             base = EXCLUDED.base, state = EXCLUDED.state, draft = EXCLUDED.draft, \
             updated_at = now()",
        )
        .bind(self.community)
        .bind(card)
        .bind(&pull.repo)
        .bind(issue_number(pull.number)?)
        .bind(&pull.url)
        .bind(&pull.head)
        .bind(&pull.base)
        .bind(&pull.state)
        .bind(pull.draft)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn by_card(&mut self, card: uuid::Uuid) -> Result<Option<PullRequest>, StoreError> {
        let row: Option<(String, i64, String, String, String, String, bool)> = sqlx::query_as(
            "SELECT repo, number, url, head, base, state, draft FROM hive.pull_requests \
             WHERE community_id = $1 AND card_id = $2",
        )
        .bind(self.community)
        .bind(card)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(row.map(
            |(repo, number, url, head, base, state, draft)| PullRequest {
                repo,
                number: number as u64,
                url,
                head,
                base,
                state,
                draft,
            },
        ))
    }
}

#[async_trait::async_trait]
impl IssuesRepo for PgTx {
    async fn upsert(
        &mut self,
        issue: &Issue,
        seen_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<StoredIssue, StoreError> {
        let row: IssueRow = sqlx::query_as(concat!(
            "INSERT INTO hive.issues (community_id, repo, number, title, body, labels, \
             assignees, author, state, url, etag, last_seen, last_comment_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12) \
             ON CONFLICT (community_id, repo, number) DO UPDATE SET title = EXCLUDED.title, \
             body = EXCLUDED.body, labels = EXCLUDED.labels, assignees = EXCLUDED.assignees, \
             author = EXCLUDED.author, state = EXCLUDED.state, url = EXCLUDED.url, \
             etag = COALESCE(EXCLUDED.etag, hive.issues.etag), last_seen = EXCLUDED.last_seen \
             RETURNING ",
            issue_columns!()
        ))
        .bind(self.community)
        .bind(issue.repo.to_string())
        .bind(issue_number(issue.number)?)
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(&issue.labels)
        .bind(&issue.assignees)
        .bind(&issue.author)
        .bind(issue.state.as_str())
        .bind(&issue.url)
        .bind(&issue.etag)
        .bind(seen_at)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        decode_issue(row)
    }

    async fn get(
        &mut self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Option<StoredIssue>, StoreError> {
        let row: Option<IssueRow> = sqlx::query_as(concat!(
            "SELECT ",
            issue_columns!(),
            " FROM hive.issues WHERE community_id = $1 AND repo = $2 AND number = $3"
        ))
        .bind(self.community)
        .bind(repo.to_string())
        .bind(issue_number(number)?)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_issue).transpose()
    }

    async fn by_card(&mut self, card: uuid::Uuid) -> Result<Option<StoredIssue>, StoreError> {
        let row: Option<IssueRow> = sqlx::query_as(concat!(
            "SELECT ",
            issue_columns!(),
            " FROM hive.issues WHERE community_id = $1 AND card_id = $2 LIMIT 1"
        ))
        .bind(self.community)
        .bind(card)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(decode_issue).transpose()
    }

    async fn of_rule(&mut self, rule: uuid::Uuid) -> Result<Vec<StoredIssue>, StoreError> {
        let rows: Vec<IssueRow> = sqlx::query_as(concat!(
            "SELECT ",
            issue_columns!(),
            " FROM hive.issues WHERE community_id = $1 AND rule_id = $2 \
             AND card_id IS NOT NULL ORDER BY repo, number"
        ))
        .bind(self.community)
        .bind(rule)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_issue).collect()
    }

    async fn attached(&mut self) -> Result<Vec<StoredIssue>, StoreError> {
        let rows: Vec<IssueRow> = sqlx::query_as(concat!(
            "SELECT ",
            issue_columns!(),
            " FROM hive.issues WHERE community_id = $1 AND card_id IS NOT NULL \
             ORDER BY repo, number"
        ))
        .bind(self.community)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_issue).collect()
    }

    async fn watched(&mut self) -> Result<Vec<StoredIssue>, StoreError> {
        let rows: Vec<IssueRow> = sqlx::query_as(concat!(
            "SELECT ",
            issue_columns!(),
            " FROM hive.issues WHERE community_id = $1 AND card_id IS NOT NULL \
             AND left_reason = '' AND state = 'open' ORDER BY repo, number"
        ))
        .bind(self.community)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(decode_issue).collect()
    }

    async fn attach(
        &mut self,
        repo: &RepoRef,
        number: u64,
        card: uuid::Uuid,
        rule: Option<uuid::Uuid>,
    ) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE hive.issues SET card_id = $4, rule_id = $5 \
             WHERE community_id = $1 AND repo = $2 AND number = $3",
        )
        .bind(self.community)
        .bind(repo.to_string())
        .bind(issue_number(number)?)
        .bind(card)
        .bind(rule)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        missing_unless_one(done.rows_affected(), repo, number)
    }

    async fn set_left(
        &mut self,
        repo: &RepoRef,
        number: u64,
        left: &str,
    ) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE hive.issues SET left_reason = $4 \
             WHERE community_id = $1 AND repo = $2 AND number = $3",
        )
        .bind(self.community)
        .bind(repo.to_string())
        .bind(issue_number(number)?)
        .bind(left)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        missing_unless_one(done.rows_affected(), repo, number)
    }

    async fn set_last_comment(
        &mut self,
        repo: &RepoRef,
        number: u64,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE hive.issues SET last_comment_at = $4 \
             WHERE community_id = $1 AND repo = $2 AND number = $3",
        )
        .bind(self.community)
        .bind(repo.to_string())
        .bind(issue_number(number)?)
        .bind(at)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        missing_unless_one(done.rows_affected(), repo, number)
    }
}

fn missing_unless_one(rows: u64, repo: &RepoRef, number: u64) -> Result<(), StoreError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(StoreError::NotFound(format!(
            "issue {repo}#{number} has not been read"
        )))
    }
}
