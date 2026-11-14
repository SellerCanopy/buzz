use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::access::{ActorKind, Level, RelayRole};
use crate::domain::card::Card;
use crate::ports::store::{
    AccessRepo, AccessRow, CardsRepo, CommunityId, Directory, Pubkey, SettingsRepo, Store,
    StoreError, Tx,
};

#[derive(Default, Clone)]
struct State {
    access: HashMap<(CommunityId, Pubkey), (Level, ActorKind, String)>,
    settings: HashMap<(CommunityId, String, String, String), serde_json::Value>,
    /// In creation order, so the newest card is last.
    cards: Vec<(CommunityId, Card)>,
}

/// A `Store` held in memory. `begin` copies the state and `commit` swaps the
/// copy back in, so a dropped `Tx` rolls back.
#[derive(Default, Clone)]
pub struct MemStore {
    state: Arc<Mutex<State>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts a card on a community's board, as the newest. Later phases create cards through routes.
    pub fn seed_card(&self, community: CommunityId, card: Card) {
        self.state.lock().unwrap().cards.push((community, card));
    }
}

#[async_trait::async_trait]
impl Store for MemStore {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        let work = self.state.lock().unwrap().clone();
        Ok(Box::new(MemTx {
            community,
            work,
            shared: self.state.clone(),
        }))
    }
}

struct MemTx {
    community: CommunityId,
    work: State,
    shared: Arc<Mutex<State>>,
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
    async fn commit(self: Box<Self>) -> Result<(), StoreError> {
        *self.shared.lock().unwrap() = self.work;
        Ok(())
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
        Ok(self
            .work
            .cards
            .iter()
            .find(|(community, card)| *community == self.community && card.id == id)
            .map(|(_, card)| card.clone()))
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
