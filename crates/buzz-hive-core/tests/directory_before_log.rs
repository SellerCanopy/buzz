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
    InboxRepo, IntakeRepo, IssuesRepo, OutboxRepo, PersonInfo, RelayEvent, RunsRepo, SettingsRepo,
    Store, StoreError, TimersRepo, Tx, UsageRepo, WorkflowsRepo,
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
    fn inbox(&mut self) -> &mut dyn InboxRepo {
        self.inner.inbox()
    }
    fn usage(&mut self) -> &mut dyn UsageRepo {
        self.inner.usage()
    }
    fn intake(&mut self) -> &mut dyn IntakeRepo {
        self.inner.intake()
    }
    fn issues(&mut self) -> &mut dyn IssuesRepo {
        self.inner.issues()
    }
    fn pull_requests(&mut self) -> &mut dyn buzz_hive_core::ports::store::PullRequestsRepo {
        self.inner.pull_requests()
    }
    fn proposals(&mut self) -> &mut dyn buzz_hive_core::ports::store::ProposalsRepo {
        self.inner.proposals()
    }
    fn card_repos(&mut self) -> &mut dyn buzz_hive_core::ports::store::CardReposRepo {
        self.inner.card_repos()
    }
    fn agent_tools(&mut self) -> &mut dyn buzz_hive_core::ports::store::AgentToolsRepo {
        self.inner.agent_tools()
    }
    fn jobs(&mut self) -> &mut dyn buzz_hive_core::ports::store::JobsRepo {
        self.inner.jobs()
    }
    async fn savepoint(&mut self) -> Result<(), StoreError> {
        self.inner.savepoint().await
    }
    async fn rollback_savepoint(&mut self) -> Result<(), StoreError> {
        self.inner.rollback_savepoint().await
    }
    async fn release_savepoint(&mut self) -> Result<(), StoreError> {
        self.inner.release_savepoint().await
    }
    async fn commit(self: Box<Self>) -> Result<Vec<Event>, StoreError> {
        self.inner.commit().await
    }
}

#[async_trait::async_trait]
impl EventsRepo for SpyTx {
    async fn lock_log(&mut self) -> Result<(), StoreError> {
        self.inner.events().lock_log().await
    }
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
    async fn of_run(
        &mut self,
        card: Uuid,
        run: Uuid,
        seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.events().of_run(card, run, seq, limit).await
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
    async fn people(&self, community: CommunityId) -> Result<Vec<PersonInfo>, StoreError> {
        self.spy.lookup("people");
        self.inner.people(community).await
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
    async fn event_by_id(
        &self,
        community: CommunityId,
        id: &str,
    ) -> Result<Option<RelayEvent>, StoreError> {
        self.spy.lookup("event_by_id");
        self.inner.event_by_id(community, id).await
    }
    async fn newer_mention(
        &self,
        community: CommunityId,
        event: &RelayEvent,
        root: &str,
        agent: &str,
    ) -> Result<bool, StoreError> {
        self.spy.lookup("newer_mention");
        self.inner
            .newer_mention(community, event, root, agent)
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
        Self::of(Fixture::new())
    }

    /// A world where `reviewer` and `tester` are agents in the update channel too.
    fn with_relay_agents() -> Self {
        Self::of(
            Fixture::new().with_directory(
                buzz_hive_core::testing::FakeDirectory::new()
                    .with_named_agent("builder", BUILDER, "aa")
                    .with_named_agent("reviewer", "b2", "aa")
                    .with_named_agent("tester", "b3", "aa")
                    .with_channel(CHANNEL, &[HIVE, BUILDER, "b2", "b3"]),
            ),
        )
    }

    fn of(f: Fixture) -> Self {
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

/// A card on `gated` at its approval stage, with the plan saved. `auto` puts it in auto mode
/// first, so the step that opens the gate answers it.
async fn at_a_gate(w: &World, auto: bool) -> Uuid {
    at_a_gate_of(w, "gated", auto).await
}

/// Like [`at_a_gate`], on the workflow `workflow`.
async fn at_a_gate_of(w: &World, workflow: &str, auto: bool) -> Uuid {
    use buzz_hive_core::domain::artifact::ArtifactContent;
    use buzz_hive_core::orchestrator::artifacts::save_artifact;

    let agent = ctx_for(BUILDER, ActorKind::Agent);
    let (card, _) = create_card(
        &w.store,
        &person(),
        NewCard {
            title: "Fix login".into(),
            description: String::new(),
            workflow: workflow.into(),
        },
    )
    .await
    .unwrap();
    if auto {
        set_setting(
            &w.env(),
            &person(),
            &Scope::Card(card.id),
            "card.auto_mode",
            Some(json!(true)),
        )
        .await
        .unwrap();
    }
    start_run(&w.env(), &person(), card.id).await.unwrap();
    save_artifact(
        &w.env(),
        &agent,
        card.id,
        "plan",
        ArtifactContent::Text("Step one.".into()),
    )
    .await
    .unwrap();
    w.spy.reset();
    stage_done(&w.env(), &agent, card.id, "draft", 1, "passed", "")
        .await
        .unwrap();
    card.id
}

async fn world_with_channel() -> World {
    channel_set(World::new()).await
}

async fn channel_set(w: World) -> World {
    set_setting(
        &w.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    w
}

#[tokio::test]
async fn a_gate_a_card_in_auto_mode_answers_asks_the_directory_before_the_first_append() {
    let w = world_with_channel().await;
    at_a_gate(&w, true).await;
    w.assert_asked_first("stage_done that opens a gate auto mode approves");
}

#[tokio::test]
async fn a_persons_answer_to_a_gate_asks_the_directory_before_the_first_append() {
    let w = world_with_channel().await;
    let card = at_a_gate(&w, false).await;
    w.spy.reset();
    buzz_hive_core::orchestrator::inbox::approve(
        &w.env(),
        &person(),
        card,
        &std::collections::BTreeMap::new(),
    )
    .await
    .unwrap();
    w.assert_asked_first("approve");
}

/// Finished cards of `endgate` go on to `relay`, which other agents work.
async fn hand_gated_on_to_relay(w: &World) {
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    tx.workflows()
        .set_options(
            "endgate",
            &WorkflowOptions {
                next_workflow: "relay".into(),
                ..WorkflowOptions::default()
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn on_the_next_workflow(w: &World, card: Uuid) {
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    let run = tx.runs().active_for_card(card).await.unwrap().unwrap();
    assert_eq!(run.workflow, "relay", "the card was handed on");
}

#[tokio::test]
async fn an_approval_that_finishes_the_workflow_asks_for_the_next_ones_agents_before_the_first_append(
) {
    let w = channel_set(World::with_relay_agents()).await;
    hand_gated_on_to_relay(&w).await;
    let card = at_a_gate_of(&w, "endgate", false).await;
    w.spy.reset();
    buzz_hive_core::orchestrator::inbox::approve(
        &w.env(),
        &person(),
        card,
        &std::collections::BTreeMap::new(),
    )
    .await
    .unwrap();
    w.assert_asked_first("approve, handing off");
    on_the_next_workflow(&w, card).await;
}

#[tokio::test]
async fn auto_mode_handing_a_card_on_asks_for_the_next_workflows_agents_before_the_first_append() {
    let w = channel_set(World::with_relay_agents()).await;
    hand_gated_on_to_relay(&w).await;
    let card = at_a_gate_of(&w, "endgate", true).await;
    w.assert_asked_first("stage_done that opens a gate auto mode approves, handing off");
    on_the_next_workflow(&w, card).await;
}
