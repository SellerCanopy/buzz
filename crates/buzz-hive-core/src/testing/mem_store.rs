use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::access::{ActorKind, Level, RelayRole};
use crate::domain::card::Card;
use crate::domain::events::{check_new, Event};
use crate::domain::run::{Run, RunState};
use crate::ports::store::{
    AccessRepo, AccessRow, AgentInfo, CardsRepo, CommunityId, Directory, EventsRepo, OutboxRepo,
    OutboxRow, Posting, Pubkey, RunsRepo, SettingsRepo, StageThread, Store, StoreError,
    StoredPackage, Tx, WorkflowsRepo,
};
use crate::workflow::options::WorkflowOptions;

#[derive(Default, Clone)]
struct State {
    access: HashMap<(CommunityId, Pubkey), (Level, ActorKind, String)>,
    settings: HashMap<(CommunityId, String, String, String), serde_json::Value>,
    /// In creation order, so the newest card is last.
    cards: Vec<(CommunityId, Card)>,
    /// `(community, name)` to `(builtin, current hash)`.
    packages: std::collections::BTreeMap<(CommunityId, String), (bool, String)>,
    versions: std::collections::BTreeMap<(CommunityId, String, String), serde_json::Value>,
    options: std::collections::BTreeMap<(CommunityId, String), WorkflowOptions>,
    /// In creation order, so the newest run is last.
    runs: Vec<(CommunityId, Run)>,
    threads: Vec<(CommunityId, StageThread)>,
    /// In queue order.
    outbox: Vec<(CommunityId, OutboxRow)>,
    next_outbox_id: i64,
}

/// The committed event log, shared by every transaction. It sits outside `State` so
/// that a commit adds to it instead of overwriting events another commit added.
#[derive(Default)]
struct Log {
    /// In seq order.
    events: Vec<(CommunityId, Event)>,
    /// Every kind ever committed, for the coverage test.
    kinds: std::collections::BTreeSet<String>,
}

/// A `Store` held in memory. `begin` copies the state and `commit` swaps the
/// copy back in, so a dropped `Tx` rolls back.
#[derive(Default, Clone)]
pub struct MemStore {
    state: Arc<Mutex<State>>,
    log: Arc<Mutex<Log>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts a card on a community's board, as the newest. Later phases create cards through routes.
    pub fn seed_card(&self, community: CommunityId, card: Card) {
        self.state.lock().unwrap().cards.push((community, card));
    }

    /// Every event kind committed so far, across all communities.
    pub fn committed_kinds(&self) -> std::collections::BTreeSet<String> {
        self.log.lock().unwrap().kinds.clone()
    }
}

#[async_trait::async_trait]
impl Store for MemStore {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        let work = self.state.lock().unwrap().clone();
        Ok(Box::new(MemTx {
            community,
            work,
            pending: Vec::new(),
            shared: self.state.clone(),
            log: self.log.clone(),
        }))
    }
}

struct MemTx {
    community: CommunityId,
    work: State,
    /// Appended in this transaction, with no seq yet.
    pending: Vec<Event>,
    shared: Arc<Mutex<State>>,
    log: Arc<Mutex<Log>>,
}

#[async_trait::async_trait]
impl Tx for MemTx {
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
    async fn commit(self: Box<Self>) -> Result<Vec<Event>, StoreError> {
        let MemTx {
            community,
            work,
            pending,
            shared,
            log,
        } = *self;
        *shared.lock().unwrap() = work;
        let mut log = log.lock().unwrap();
        let mut committed = Vec::with_capacity(pending.len());
        for mut event in pending {
            event.seq = log.events.last().map_or(0, |(_, e)| e.seq) + 1;
            log.kinds.insert(event.kind.clone());
            log.events.push((community, event.clone()));
            committed.push(event);
        }
        Ok(committed)
    }
}

#[async_trait::async_trait]
impl EventsRepo for MemTx {
    async fn append(&mut self, event: Event) -> Result<(), StoreError> {
        check_new(&event.kind, &event.payload).map_err(|e| StoreError::Conflict(e.to_string()))?;
        self.pending.push(event);
        Ok(())
    }

    async fn after(&mut self, seq: i64, limit: i64) -> Result<Vec<Event>, StoreError> {
        let log = self.log.lock().unwrap();
        Ok(log
            .events
            .iter()
            .filter(|(community, event)| *community == self.community && event.seq > seq)
            .take(limit.max(0) as usize)
            .map(|(_, event)| event.clone())
            .collect())
    }

    async fn latest_seq(&mut self) -> Result<i64, StoreError> {
        let log = self.log.lock().unwrap();
        Ok(log
            .events
            .iter()
            .rev()
            .find(|(community, _)| *community == self.community)
            .map_or(0, |(_, event)| event.seq))
    }
}

#[async_trait::async_trait]
impl AccessRepo for MemTx {
    async fn level(&mut self, pubkey: &str) -> Result<Option<(Level, ActorKind)>, StoreError> {
        Ok(self
            .work
            .access
            .get(&(self.community, pubkey.to_owned()))
            .map(|(level, kind, _)| (*level, *kind)))
    }

    async fn set_level(
        &mut self,
        pubkey: &str,
        level: Level,
        kind: ActorKind,
        by: &str,
    ) -> Result<(), StoreError> {
        self.work.access.insert(
            (self.community, pubkey.to_owned()),
            (level, kind, by.to_owned()),
        );
        Ok(())
    }

    async fn list(&mut self) -> Result<Vec<AccessRow>, StoreError> {
        let mut rows: Vec<AccessRow> = self
            .work
            .access
            .iter()
            .filter(|((community, _), _)| *community == self.community)
            .map(|((_, pubkey), (level, kind, _))| AccessRow {
                pubkey: pubkey.clone(),
                level: *level,
                kind: *kind,
            })
            .collect();
        rows.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
        Ok(rows)
    }
}

#[async_trait::async_trait]
impl SettingsRepo for MemTx {
    async fn get(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        Ok(self
            .work
            .settings
            .get(&(
                self.community,
                scope.to_owned(),
                scope_id.to_owned(),
                key.to_owned(),
            ))
            .cloned())
    }

    async fn set(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), StoreError> {
        self.work.settings.insert(
            (
                self.community,
                scope.to_owned(),
                scope_id.to_owned(),
                key.to_owned(),
            ),
            value,
        );
        Ok(())
    }

    async fn remove(&mut self, scope: &str, scope_id: &str, key: &str) -> Result<(), StoreError> {
        self.work.settings.remove(&(
            self.community,
            scope.to_owned(),
            scope_id.to_owned(),
            key.to_owned(),
        ));
        Ok(())
    }

    async fn list(
        &mut self,
        scope: &str,
        scope_id: &str,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, StoreError> {
        Ok(self
            .work
            .settings
            .iter()
            .filter(|((community, s, id, _), _)| {
                *community == self.community && s == scope && id == scope_id
            })
            .map(|((_, _, _, key), value)| (key.clone(), value.clone()))
            .collect())
    }
}

#[async_trait::async_trait]
impl RunsRepo for MemTx {
    async fn insert(&mut self, run: &Run) -> Result<(), StoreError> {
        if self.find_run(run.id).is_some() {
            return Err(StoreError::Conflict(format!("run {} exists", run.id)));
        }
        self.work.runs.push((self.community, run.clone()));
        Ok(())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        Ok(self.find_run(id).cloned())
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        Ok(self.find_run(id).cloned())
    }

    async fn update(&mut self, run: &Run) -> Result<(), StoreError> {
        let community = self.community;
        let found = self
            .work
            .runs
            .iter_mut()
            .find(|(c, r)| *c == community && r.id == run.id)
            .map(|(_, r)| r)
            .ok_or_else(|| StoreError::NotFound(format!("run {}", run.id)))?;
        *found = run.clone();
        Ok(())
    }

    async fn list(&mut self, card: Option<uuid::Uuid>) -> Result<Vec<Run>, StoreError> {
        Ok(self
            .work
            .runs
            .iter()
            .rev()
            .filter(|(c, r)| *c == self.community && card.is_none_or(|id| r.card_id == id))
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn active_for_card(&mut self, card: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        Ok(self
            .work
            .runs
            .iter()
            .rev()
            .find(|(c, r)| *c == self.community && r.card_id == card && r.state != RunState::Ended)
            .map(|(_, r)| r.clone()))
    }

    async fn visits(&mut self, card: uuid::Uuid, stage: &str) -> Result<u32, StoreError> {
        Ok(self
            .work
            .runs
            .iter()
            .filter(|(c, r)| {
                *c == self.community
                    && r.card_id == card
                    && r.current_stage.as_deref() == Some(stage)
            })
            .map(|(_, r)| r.visit)
            .max()
            .unwrap_or(0))
    }

    async fn record_thread(&mut self, thread: &StageThread) -> Result<(), StoreError> {
        let community = self.community;
        let known = self.work.threads.iter().any(|(c, t)| {
            *c == community
                && t.card_id == thread.card_id
                && t.stage == thread.stage
                && t.visit == thread.visit
        });
        if !known {
            self.work.threads.push((community, thread.clone()));
        }
        Ok(())
    }

    async fn thread(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Option<StageThread>, StoreError> {
        Ok(self
            .work
            .threads
            .iter()
            .find(|(c, t)| {
                *c == self.community && t.card_id == card && t.stage == stage && t.visit == visit
            })
            .map(|(_, t)| t.clone()))
    }
}

#[async_trait::async_trait]
impl OutboxRepo for MemTx {
    async fn enqueue(
        &mut self,
        key: &str,
        card: uuid::Uuid,
        posting: &Posting,
    ) -> Result<bool, StoreError> {
        let community = self.community;
        if self
            .work
            .outbox
            .iter()
            .any(|(c, row)| *c == community && row.key == key)
        {
            return Ok(false);
        }
        self.work.next_outbox_id += 1;
        let row = OutboxRow {
            id: self.work.next_outbox_id,
            key: key.to_owned(),
            card_id: card,
            posting: posting.clone(),
            created_at: chrono::Utc::now(),
            done_at: None,
            attempts: 0,
            last_error: None,
        };
        self.work.outbox.push((community, row));
        Ok(true)
    }

    async fn next(&mut self, skip_cards: &[uuid::Uuid]) -> Result<Option<OutboxRow>, StoreError> {
        Ok(self
            .work
            .outbox
            .iter()
            .find(|(c, row)| {
                *c == self.community && row.done_at.is_none() && !skip_cards.contains(&row.card_id)
            })
            .map(|(_, row)| row.clone()))
    }

    async fn mark_done(&mut self, id: i64) -> Result<(), StoreError> {
        let row = self.outbox_row(id)?;
        row.done_at = Some(chrono::Utc::now());
        Ok(())
    }

    async fn mark_failed(&mut self, id: i64, error: &str) -> Result<(), StoreError> {
        let row = self.outbox_row(id)?;
        row.attempts += 1;
        row.last_error = Some(error.to_owned());
        Ok(())
    }

    async fn all(&mut self) -> Result<Vec<OutboxRow>, StoreError> {
        Ok(self
            .work
            .outbox
            .iter()
            .filter(|(c, _)| *c == self.community)
            .map(|(_, row)| row.clone())
            .collect())
    }
}

#[async_trait::async_trait]
impl CardsRepo for MemTx {
    async fn list(&mut self, archived: bool) -> Result<Vec<Card>, StoreError> {
        Ok(self
            .work
            .cards
            .iter()
            .rev()
            .filter(|(community, card)| *community == self.community && card.archived == archived)
            .map(|(_, card)| card.clone())
            .collect())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        Ok(self.find(id).map(|(_, card)| card.clone()))
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        Ok(self.find(id).map(|(_, card)| card.clone()))
    }

    async fn insert(&mut self, card: &Card) -> Result<(), StoreError> {
        if self.find(card.id).is_some() {
            return Err(StoreError::Conflict(format!("card {} exists", card.id)));
        }
        self.work.cards.push((self.community, card.clone()));
        Ok(())
    }

    async fn update(&mut self, card: &Card) -> Result<(), StoreError> {
        let found = self.find_mut(card.id)?;
        found.title = card.title.clone();
        found.description = card.description.clone();
        found.state = card.state;
        found.workflow = card.workflow.clone();
        Ok(())
    }

    async fn set_archived(&mut self, id: uuid::Uuid, archived: bool) -> Result<(), StoreError> {
        self.find_mut(id)?.archived = archived;
        Ok(())
    }

    async fn delete(&mut self, id: uuid::Uuid) -> Result<(), StoreError> {
        let before = self.work.cards.len();
        let community = self.community;
        self.work
            .cards
            .retain(|(c, card)| !(*c == community && card.id == id));
        if self.work.cards.len() == before {
            return Err(StoreError::NotFound(format!("card {id}")));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkflowsRepo for MemTx {
    async fn record_version(
        &mut self,
        name: &str,
        hash: &str,
        body: serde_json::Value,
        _created_by: &str,
        builtin: bool,
    ) -> Result<(), StoreError> {
        let community = self.community;
        self.work
            .versions
            .entry((community, name.to_owned(), hash.to_owned()))
            .or_insert(body);
        self.work
            .packages
            .insert((community, name.to_owned()), (builtin, hash.to_owned()));
        Ok(())
    }

    async fn packages(&mut self) -> Result<Vec<StoredPackage>, StoreError> {
        Ok(self
            .work
            .packages
            .iter()
            .filter(|((community, _), _)| *community == self.community)
            .map(|((_, name), (builtin, hash))| StoredPackage {
                name: name.clone(),
                builtin: *builtin,
                current_hash: hash.clone(),
            })
            .collect())
    }

    async fn version(
        &mut self,
        name: &str,
        hash: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        Ok(self
            .work
            .versions
            .get(&(self.community, name.to_owned(), hash.to_owned()))
            .cloned())
    }

    async fn options(
        &mut self,
    ) -> Result<std::collections::BTreeMap<String, WorkflowOptions>, StoreError> {
        Ok(self
            .work
            .options
            .iter()
            .filter(|((community, _), _)| *community == self.community)
            .map(|((_, name), options)| (name.clone(), options.clone()))
            .collect())
    }

    async fn set_options(
        &mut self,
        name: &str,
        options: &WorkflowOptions,
    ) -> Result<(), StoreError> {
        self.work
            .options
            .insert((self.community, name.to_owned()), options.clone());
        Ok(())
    }
}

impl MemTx {
    fn find_run(&self, id: uuid::Uuid) -> Option<&Run> {
        self.work
            .runs
            .iter()
            .find(|(community, run)| *community == self.community && run.id == id)
            .map(|(_, run)| run)
    }

    fn outbox_row(&mut self, id: i64) -> Result<&mut OutboxRow, StoreError> {
        let community = self.community;
        self.work
            .outbox
            .iter_mut()
            .find(|(c, row)| *c == community && row.id == id)
            .map(|(_, row)| row)
            .ok_or_else(|| StoreError::NotFound(format!("outbox row {id}")))
    }

    fn find(&self, id: uuid::Uuid) -> Option<&(CommunityId, Card)> {
        self.work
            .cards
            .iter()
            .find(|(community, card)| *community == self.community && card.id == id)
    }

    fn find_mut(&mut self, id: uuid::Uuid) -> Result<&mut Card, StoreError> {
        let community = self.community;
        self.work
            .cards
            .iter_mut()
            .find(|(c, card)| *c == community && card.id == id)
            .map(|(_, card)| card)
            .ok_or_else(|| StoreError::NotFound(format!("card {id}")))
    }
}

/// A `Directory` answering from two maps, keyed by pubkey. The community is ignored.
#[derive(Default, Clone)]
pub struct FakeDirectory {
    roles: HashMap<Pubkey, RelayRole>,
    owners: HashMap<Pubkey, Pubkey>,
    agents: Vec<AgentInfo>,
    channels: HashMap<uuid::Uuid, Vec<Pubkey>>,
}

impl FakeDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_role(mut self, pubkey: &str, role: RelayRole) -> Self {
        self.roles.insert(pubkey.to_owned(), role);
        self
    }

    pub fn with_agent(mut self, agent: &str, owner: &str) -> Self {
        self.owners.insert(agent.to_owned(), owner.to_owned());
        self
    }

    /// An agent the relay lists under `name`. Its owner is `owner`.
    pub fn with_named_agent(mut self, name: &str, pubkey: &str, owner: &str) -> Self {
        self.owners.insert(pubkey.to_owned(), owner.to_owned());
        self.agents.push(AgentInfo {
            name: name.to_owned(),
            pubkey: pubkey.to_owned(),
            owner: Some(owner.to_owned()),
        });
        self
    }

    /// A channel that exists, with these current members.
    pub fn with_channel(mut self, channel: uuid::Uuid, members: &[&str]) -> Self {
        self.channels
            .insert(channel, members.iter().map(|m| (*m).to_owned()).collect());
        self
    }
}

#[async_trait::async_trait]
impl Directory for FakeDirectory {
    async fn relay_role(
        &self,
        _community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError> {
        Ok(self.roles.get(pubkey).copied())
    }

    async fn agent_owner(
        &self,
        _community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<Pubkey>, StoreError> {
        Ok(self.owners.get(pubkey).cloned())
    }

    async fn agents_named(
        &self,
        _community: CommunityId,
        name: &str,
    ) -> Result<Vec<AgentInfo>, StoreError> {
        Ok(self
            .agents
            .iter()
            .filter(|a| a.name.eq_ignore_ascii_case(name))
            .cloned()
            .collect())
    }

    async fn list_agents(&self, _community: CommunityId) -> Result<Vec<AgentInfo>, StoreError> {
        let mut agents = self.agents.clone();
        agents.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.pubkey.cmp(&b.pubkey)));
        Ok(agents)
    }

    async fn channel_exists(
        &self,
        _community: CommunityId,
        channel: uuid::Uuid,
    ) -> Result<bool, StoreError> {
        Ok(self.channels.contains_key(&channel))
    }

    async fn member_of_channel(
        &self,
        _community: CommunityId,
        channel: uuid::Uuid,
        pubkey: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .channels
            .get(&channel)
            .is_some_and(|members| members.iter().any(|m| m == pubkey)))
    }
}
