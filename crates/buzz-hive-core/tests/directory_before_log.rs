//! The relay's directory is asked before the first event is appended, never after.
//!
//! The first append of a transaction takes the community's log lock, which is held to the
//! commit, so a lookup after it keeps every other writer waiting for the relay. The store and
//! the directory here count: a lookup that finds an event already appended is a violation.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use buzz_hive_core::access::{ActorKind, RelayRole};
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::run::{stage_done, start_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::ports::store::{
    AccessRepo, AgentInfo, AgentLink, ArtifactsRepo, CardsRepo, CommunityId, Directory, EventsRepo,
    OutboxRepo, RunsRepo, SettingsRepo, Store, StoreError, TimersRepo, Tx, WorkflowsRepo,
};
use buzz_hive_core::workflow::options::WorkflowOptions;
use common::{ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY, HIVE};
use serde_json::json;
use uuid::Uuid;

/// What the spies saw since the last [`Spy::reset`].
#[derive(Default)]
struct Spy {
    /// Events appended in the transaction now open.
    appended: AtomicUsize,
    /// Directory lookups made at all.
    lookups: AtomicUsize,
    /// Lookups made after an event was appended.
    late: Mutex<Vec<&'static str>>,
}

impl Spy {
    fn reset(&self) {
        self.appended.store(0, Ordering::SeqCst);
        self.lookups.store(0, Ordering::SeqCst);
        self.late.lock().unwrap().clear();
    }

    fn lookup(&self, what: &'static str) {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        if self.appended.load(Ordering::SeqCst) > 0 {
            self.late.lock().unwrap().push(what);
        }
    }
}

struct SpyStore {
    inner: buzz_hive_core::testing::MemStore,
    spy: Arc<Spy>,
}

#[async_trait::async_trait]
impl Store for SpyStore {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        Ok(Box::new(SpyTx {
            inner: self.inner.begin(community).await?,
            spy: self.spy.clone(),
        }))
    }
}

struct SpyTx {
    inner: Box<dyn Tx>,
    spy: Arc<Spy>,
}

#[async_trait::async_trait]
impl Tx for SpyTx {
    fn access(&mut self) -> &mut dyn AccessRepo {
        self.inner.access()
    }
    fn settings(&mut self) -> &mut dyn SettingsRepo {
        self.inner.settings()
    }
    fn cards(&mut self) -> &mut dyn CardsRepo {
        self.inner.cards()
    }
    fn events(&mut self) -> &mut dyn EventsRepo {
        self
    }
    fn workflows(&mut self) -> &mut dyn WorkflowsRepo {
        self.inner.workflows()
    }
    fn runs(&mut self) -> &mut dyn RunsRepo {
        self.inner.runs()
    }
    fn outbox(&mut self) -> &mut dyn OutboxRepo {
        self.inner.outbox()
    }
    fn artifacts(&mut self) -> &mut dyn ArtifactsRepo {
        self.inner.artifacts()
    }
    fn timers(&mut self) -> &mut dyn TimersRepo {
        self.inner.timers()
    }
    async fn commit(self: Box<Self>) -> Result<Vec<Event>, StoreError> {
        self.inner.commit().await
    }
}

#[async_trait::async_trait]
impl EventsRepo for SpyTx {
    async fn append(&mut self, event: Event) -> Result<(), StoreError> {
        self.spy.appended.fetch_add(1, Ordering::SeqCst);
        self.inner.events().append(event).await
    }
    async fn after(&mut self, seq: i64, limit: i64) -> Result<Vec<Event>, StoreError> {
        self.inner.events().after(seq, limit).await
    }
    async fn latest_seq(&mut self) -> Result<i64, StoreError> {
        self.inner.events().latest_seq().await
    }
    async fn of_card(
        &mut self,
        card: Uuid,
        kind: &str,
        seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.events().of_card(card, kind, seq, limit).await
    }
}

struct SpyDirectory {
    inner: buzz_hive_core::testing::FakeDirectory,
    spy: Arc<Spy>,
}

#[async_trait::async_trait]
impl Directory for SpyDirectory {
    async fn relay_role(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError> {
        self.spy.lookup("relay_role");
        self.inner.relay_role(community, pubkey).await
    }
    async fn agent_link(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<AgentLink>, StoreError> {
        self.spy.lookup("agent_link");
        self.inner.agent_link(community, pubkey).await
    }
    async fn agents_named(
        &self,
        community: CommunityId,
        name: &str,
    ) -> Result<Vec<AgentInfo>, StoreError> {
        self.spy.lookup("agents_named");
        self.inner.agents_named(community, name).await
    }
    async fn list_agents(&self, community: CommunityId) -> Result<Vec<AgentInfo>, StoreError> {
        self.spy.lookup("list_agents");
        self.inner.list_agents(community).await
    }
    async fn channel_exists(
        &self,
        community: CommunityId,
        channel: Uuid,
    ) -> Result<bool, StoreError> {
        self.spy.lookup("channel_exists");
        self.inner.channel_exists(community, channel).await
    }
    async fn member_of_channel(
        &self,
        community: CommunityId,
        channel: Uuid,
        pubkey: &str,
    ) -> Result<bool, StoreError> {
        self.spy.lookup("member_of_channel");
        self.inner
            .member_of_channel(community, channel, pubkey)
            .await
    }
}

struct World {
    store: SpyStore,
    directory: SpyDirectory,
    catalog: buzz_hive_core::workflow::load::Catalog,
    spy: Arc<Spy>,
}

impl World {
    fn new() -> Self {
        let f = Fixture::new();
        let spy = Arc::new(Spy::default());
        Self {
            store: SpyStore {
                inner: f.store,
                spy: spy.clone(),
            },
            directory: SpyDirectory {
                inner: f.directory,
                spy: spy.clone(),
            },
            catalog: f.catalog,
            spy,
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            store: &self.store,
            directory: &self.directory,
            catalog: &self.catalog,
            hive: HIVE,
        }
    }

    /// Fails when the step just run looked something up after it appended, or never looked
    /// anything up, which would make the check mean nothing.
    fn assert_asked_first(&self, step: &str) {
        let late = self.spy.late.lock().unwrap().clone();
        assert!(
            late.is_empty(),
            "{step}: asked the directory after an append: {late:?}"
        );
        assert!(
            self.spy.lookups.load(Ordering::SeqCst) > 0,
            "{step}: the directory was never asked"
        );
    }
}

#[tokio::test]
async fn starting_reporting_and_handing_off_ask_the_directory_before_the_first_append() {
    let w = World::new();
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    set_setting(
        &w.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    // Finished cards of `three` go on to `second`.
    {
        let mut tx = w.store.begin(COMMUNITY).await.unwrap();
        tx.workflows()
            .set_options(
                "three",
                &WorkflowOptions {
                    next_workflow: "second".into(),
                    ..WorkflowOptions::default()
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let (card, _) = create_card(
        &w.store,
        &person(),
        NewCard {
            title: "Fix login".into(),
            description: String::new(),
            workflow: "three".into(),
        },
    )
    .await
    .unwrap();

    w.spy.reset();
    start_run(&w.env(), &person(), card.id).await.unwrap();
    w.assert_asked_first("start_run");

    // a to b: the result is recorded, then the next stage's task is queued.
    w.spy.reset();
    stage_done(&w.env(), &agent, card.id, "a", 1, "passed", "")
        .await
        .unwrap();
    w.assert_asked_first("stage_done a");

    w.spy.reset();
    stage_done(&w.env(), &agent, card.id, "b", 1, "passed", "")
        .await
        .unwrap();
    w.assert_asked_first("stage_done b");

    // The last stage ends the workflow, and the card starts the next one.
    w.spy.reset();
    stage_done(&w.env(), &agent, card.id, "c", 1, "passed", "")
        .await
        .unwrap();
    w.assert_asked_first("stage_done c, handing off");
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    let card = tx.cards().get_for_update(card.id).await.unwrap().unwrap();
    assert_eq!(card.workflow, "second");
    let run = tx.runs().active_for_card(card.id).await.unwrap().unwrap();
    assert_eq!(run.workflow, "second");
    assert_eq!(run.current_stage.as_deref(), Some("s1"));
}
