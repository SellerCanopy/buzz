use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::access::{ActorKind, Level, RelayRole};
use crate::domain::card::Card;
use crate::domain::events::{check_new, Event};
use crate::ports::store::{
    AccessRepo, AccessRow, CardsRepo, CommunityId, Directory, EventsRepo, Pubkey, SettingsRepo,
    Store, StoreError, StoredPackage, Tx, WorkflowsRepo,
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
}
