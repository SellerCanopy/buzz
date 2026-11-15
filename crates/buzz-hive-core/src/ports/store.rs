//! Storage port: one transaction per community, plus read-only relay facts.

use crate::access::{ActorKind, Level, RelayRole};
use crate::domain::artifact::{ArtifactContent, ArtifactInfo, ArtifactVersion};
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::domain::run::Run;
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
    /// A fault that retrying will not fix, such as a bad column or a failed decode.
    #[error("store error: {0}")]
    Internal(String),
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
    fn runs(&mut self) -> &mut dyn RunsRepo;
    fn outbox(&mut self) -> &mut dyn OutboxRepo;
    fn artifacts(&mut self) -> &mut dyn ArtifactsRepo;
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
    /// Removes a setting. Nothing happens when it is not set.
    async fn remove(&mut self, scope: &str, scope_id: &str, key: &str) -> Result<(), StoreError>;
    /// Every setting of one scope, by key.
    async fn list(
        &mut self,
        scope: &str,
        scope_id: &str,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, StoreError>;
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
    /// A card's committed events of `kind` with seq greater than `seq`, oldest first, at most
    /// `limit`.
    async fn of_card(
        &mut self,
        card: uuid::Uuid,
        kind: &str,
        seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError>;
}

/// A card's artifacts: each save appends a version, and nothing is ever rewritten.
#[async_trait::async_trait]
pub trait ArtifactsRepo: Send {
    /// What a save writes. The artifact is created by its first save, with `handoff` from the
    /// stage's declaration (true when it declares none). `Conflict` when the artifact exists
    /// with the other kind. Versions count from 1. The caller holds the card
    /// (`CardsRepo::get_for_update`), so two saves of one name queue.
    async fn save(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        handoff: bool,
        save: &ArtifactSave,
    ) -> Result<ArtifactVersion, StoreError>;
    /// The card's artifacts, by name.
    async fn list(&mut self, card: uuid::Uuid) -> Result<Vec<ArtifactInfo>, StoreError>;
    /// Every version of one artifact, oldest first, with content. Empty when it has none.
    async fn versions(
        &mut self,
        card: uuid::Uuid,
        name: &str,
    ) -> Result<Vec<ArtifactVersion>, StoreError>;
    /// One version, or the latest when `version` is `None`.
    async fn version(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        version: Option<i32>,
    ) -> Result<Option<ArtifactVersion>, StoreError>;
    /// Sets whether later stages get the artifact. `NotFound` when it does not exist.
    async fn set_handoff(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        handoff: bool,
    ) -> Result<(), StoreError>;
    /// The names `stage` saved during its `visit`, by name.
    async fn saved_in(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Vec<String>, StoreError>;
}

/// One save of an artifact version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSave {
    pub content: ArtifactContent,
    pub stage: String,
    pub visit: u32,
    pub created_by: Pubkey,
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

/// Runs and the stage threads they open.
#[async_trait::async_trait]
pub trait RunsRepo: Send {
    /// `Conflict` when the id is taken.
    async fn insert(&mut self, run: &Run) -> Result<(), StoreError>;
    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError>;
    /// Like `get`, and holds the run until the transaction ends, so two inputs to one run queue.
    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError>;
    /// Writes the run's state, stage, visit, requester, steps, parameters and end time.
    /// `NotFound` when it does not exist.
    async fn update(&mut self, run: &Run) -> Result<(), StoreError>;
    /// A card's runs, or every run, newest first.
    async fn list(&mut self, card: Option<uuid::Uuid>) -> Result<Vec<Run>, StoreError>;
    /// The card's run that has not ended, if any.
    async fn active_for_card(&mut self, card: uuid::Uuid) -> Result<Option<Run>, StoreError>;
    /// The highest visit the card's runs have made to each stage that any has entered. A stage
    /// no run has entered is absent.
    async fn visit_counts(
        &mut self,
        card: uuid::Uuid,
    ) -> Result<std::collections::BTreeMap<String, u32>, StoreError>;
    /// Records the thread a stage task opened. Recording one already stored changes nothing.
    async fn record_thread(&mut self, thread: &StageThread) -> Result<(), StoreError>;
    async fn thread(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Option<StageThread>, StoreError>;
    /// Every thread the card's stages opened, in the order they were recorded.
    async fn threads(&mut self, card: uuid::Uuid) -> Result<Vec<StageThread>, StoreError>;
}

/// The relay thread a stage task opened. Its root is the message that mentions the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageThread {
    pub card_id: uuid::Uuid,
    pub stage: String,
    pub visit: u32,
    pub root_event_id: String,
    pub agent_pubkey: Pubkey,
    /// The channel the task was posted in, which is where the thread is read.
    pub channel_id: uuid::Uuid,
}

/// What a queued post says. The text is rendered when the post is queued, so a change to
/// the settings or the workflow afterwards does not change a post already waiting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Posting {
    Status {
        channel: uuid::Uuid,
        text: String,
    },
    StageTask {
        channel: uuid::Uuid,
        stage: String,
        visit: u32,
        /// The name the workflow gives the agent.
        agent_name: String,
        /// The agent's pubkey when the post was queued.
        agent: Pubkey,
        text: String,
    },
    /// A reply in the thread `visit` of `stage` opened, which mentions the stage's agent. It
    /// carries a repeat stage's next round, or tells a resumed stage's agent to carry on.
    StageReply {
        channel: uuid::Uuid,
        stage: String,
        visit: u32,
        agent_name: String,
        agent: Pubkey,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub id: i64,
    /// Unique per community. Queueing a key already queued adds nothing.
    pub key: String,
    pub card_id: uuid::Uuid,
    pub posting: Posting,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub done_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When a drainer last claimed the post. A claim older than its lease does not count.
    pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub attempts: i32,
    pub last_error: Option<String>,
}

/// The posts the relay has not taken yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxBacklog {
    /// Posts not done, including the ones that have failed.
    pub pending: i64,
    /// The pending posts that failed their last attempt.
    pub failed: i64,
}

/// Posts waiting for the relay. Written in the transaction that changes a run, so a crash
/// loses none and a retry repeats none.
#[async_trait::async_trait]
pub trait OutboxRepo: Send {
    /// Queues a post. `false` when the key is already queued, done or not.
    async fn enqueue(
        &mut self,
        key: &str,
        card: uuid::Uuid,
        posting: &Posting,
    ) -> Result<bool, StoreError>;
    /// Claims the next post to send: the oldest one not done of a card, other than the
    /// cards in `skip_cards`. A card's later post is never offered while an earlier one is
    /// not done, so posts of one card go in order even with two drainers. The claim is a
    /// lease of `lease`, recorded in the row, so the caller can end the transaction and send
    /// the post outside it. A post whose lease has run out is offered again.
    async fn claim_next(
        &mut self,
        skip_cards: &[uuid::Uuid],
        lease: std::time::Duration,
    ) -> Result<Option<OutboxRow>, StoreError>;
    /// Marks a post done and ends its claim.
    async fn mark_done(&mut self, id: i64) -> Result<(), StoreError>;
    /// Counts a failed attempt, keeps the cause and ends the claim.
    async fn mark_failed(&mut self, id: i64, error: &str) -> Result<(), StoreError>;
    /// Every post, done or not, oldest first.
    async fn all(&mut self) -> Result<Vec<OutboxRow>, StoreError>;
    /// How many posts are waiting, and how many of those have failed at least once.
    async fn backlog(&mut self) -> Result<OutboxBacklog, StoreError>;
    /// The post queued under `key`, done or not.
    async fn by_key(&mut self, key: &str) -> Result<Option<OutboxRow>, StoreError>;
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
    /// `Some` when the pubkey is an agent, `None` when it is a person or unknown.
    async fn agent_link(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<AgentLink>, StoreError>;
    /// Every agent whose display name is `name`, compared case-insensitively.
    async fn agents_named(
        &self,
        community: CommunityId,
        name: &str,
    ) -> Result<Vec<AgentInfo>, StoreError>;
    /// Every agent in the community, by name.
    async fn list_agents(&self, community: CommunityId) -> Result<Vec<AgentInfo>, StoreError>;
    /// Whether the channel exists and is not deleted.
    async fn channel_exists(
        &self,
        community: CommunityId,
        channel: uuid::Uuid,
    ) -> Result<bool, StoreError>;
    /// Whether `pubkey` is a current member of the channel.
    async fn member_of_channel(
        &self,
        community: CommunityId,
        channel: uuid::Uuid,
        pubkey: &str,
    ) -> Result<bool, StoreError>;
}

/// How the relay ties an agent to the person who owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLink {
    pub owner: Pubkey,
    /// The agent was switched off in Buzz. It keeps its owner but must not act.
    pub deactivated: bool,
}

/// A Buzz agent as the relay lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub name: String,
    pub pubkey: Pubkey,
    /// The agent's owner, who is a person.
    pub owner: Option<Pubkey>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccessRow {
    pub pubkey: Pubkey,
    pub level: Level,
    pub kind: ActorKind,
}
