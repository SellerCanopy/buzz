//! Storage port: one transaction per community, plus read-only relay facts.

use crate::access::{ActorKind, Level, RelayRole};
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::workflow::options::WorkflowOptions;

pub type CommunityId = uuid::Uuid;
/// 64 hex characters, lowercase.
pub type Pubkey = String;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store unavailable: {0}")]
    Unavailable(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
}

#[async_trait::async_trait]
pub trait Store: Send + Sync {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError>;

    /// Committed events of a community with seq greater than `after_seq`, oldest first.
    async fn events_after(
        &self,
        community: CommunityId,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError> {
        let mut tx = self.begin(community).await?;
        tx.events().after(after_seq, limit).await
    }
}

/// One transaction. Dropping it without `commit` rolls it back.
///
/// `commit` returns the events appended in the transaction with their seq assigned.
/// The caller publishes them on the [`crate::bus::Bus`] once the commit has succeeded.
#[async_trait::async_trait]
pub trait Tx: Send {
    fn access(&mut self) -> &mut dyn AccessRepo;
    fn settings(&mut self) -> &mut dyn SettingsRepo;
    fn cards(&mut self) -> &mut dyn CardsRepo;
    fn events(&mut self) -> &mut dyn EventsRepo;
    fn workflows(&mut self) -> &mut dyn WorkflowsRepo;
    async fn commit(self: Box<Self>) -> Result<Vec<Event>, StoreError>;
}

#[async_trait::async_trait]
pub trait AccessRepo: Send {
    async fn level(&mut self, pubkey: &str) -> Result<Option<(Level, ActorKind)>, StoreError>;
    async fn set_level(
        &mut self,
        pubkey: &str,
        level: Level,
        kind: ActorKind,
        by: &str,
    ) -> Result<(), StoreError>;
    async fn list(&mut self) -> Result<Vec<AccessRow>, StoreError>;
}

#[async_trait::async_trait]
pub trait SettingsRepo: Send {
    async fn get(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreError>;
    async fn set(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), StoreError>;
}

#[async_trait::async_trait]
pub trait CardsRepo: Send {
    /// The board's cards, newest first, or the archive when `archived` is set.
    async fn list(&mut self, archived: bool) -> Result<Vec<Card>, StoreError>;
    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError>;
    /// Like `get`, and holds the card until the transaction ends, so two changes to one card queue.
    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError>;
    /// `Conflict` when the id is taken.
    async fn insert(&mut self, card: &Card) -> Result<(), StoreError>;
    /// Writes the card's title, description, state and workflow. `NotFound` when it does not exist.
    async fn update(&mut self, card: &Card) -> Result<(), StoreError>;
    /// Sets the card aside, or brings it back. `NotFound` when it does not exist.
    async fn set_archived(&mut self, id: uuid::Uuid, archived: bool) -> Result<(), StoreError>;
    /// `NotFound` when it does not exist.
    async fn delete(&mut self, id: uuid::Uuid) -> Result<(), StoreError>;
}

#[async_trait::async_trait]
pub trait EventsRepo: Send {
    /// Adds an event to the log. The kind must pass [`crate::domain::events::check_new`].
    /// The seq is assigned by the store and returned by `Tx::commit`.
    async fn append(&mut self, event: Event) -> Result<(), StoreError>;
    /// Committed events with seq greater than `seq`, oldest first, at most `limit`.
    async fn after(&mut self, seq: i64, limit: i64) -> Result<Vec<Event>, StoreError>;
    /// The newest committed seq of this community, 0 when there is none.
    async fn latest_seq(&mut self) -> Result<i64, StoreError>;
}

/// What the store keeps of workflow packages: each version by content hash, which one is
/// current, and a person's changes to a workflow's board behavior.
#[async_trait::async_trait]
pub trait WorkflowsRepo: Send {
    /// Stores a package version by its hash, once, and makes it the package's current one.
    /// `body` is the package's files by path. Recording a hash already stored only moves
    /// the current pointer.
    async fn record_version(
        &mut self,
        name: &str,
        hash: &str,
        body: serde_json::Value,
        created_by: &str,
        builtin: bool,
    ) -> Result<(), StoreError>;
    /// Every package with a stored version, by name.
    async fn packages(&mut self) -> Result<Vec<StoredPackage>, StoreError>;
    /// The files of one stored version.
    async fn version(
        &mut self,
        name: &str,
        hash: &str,
    ) -> Result<Option<serde_json::Value>, StoreError>;
    /// Every workflow's stored options, by name. A workflow with no row has none.
    async fn options(
        &mut self,
    ) -> Result<std::collections::BTreeMap<String, WorkflowOptions>, StoreError>;
    async fn set_options(
        &mut self,
        name: &str,
        options: &WorkflowOptions,
    ) -> Result<(), StoreError>;
}

/// A package the store holds versions of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPackage {
    pub name: String,
    pub builtin: bool,
    pub current_hash: String,
}

/// Relay facts, read-only. Implemented by `buzz-hive-store::relay_read`.
#[async_trait::async_trait]
pub trait Directory: Send + Sync {
    async fn relay_role(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError>;
    /// `Some(owner pubkey)` when the pubkey is an agent, `None` when it is a person or unknown.
    async fn agent_owner(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<Pubkey>, StoreError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccessRow {
    pub pubkey: Pubkey,
    pub level: Level,
    pub kind: ActorKind,
}
