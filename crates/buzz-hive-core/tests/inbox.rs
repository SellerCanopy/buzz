//! Needs you: the items a run opens when it waits for a person, and every way they close.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::artifact::ArtifactContent;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::inbox::{InboxItem, InboxKind};
use buzz_hive_core::domain::run::RunState;
use buzz_hive_core::orchestrator::artifacts::save_artifact;
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::inbox::{
    answer, approve, ask_gate, list, send_back, send_message,
};
use buzz_hive_core::orchestrator::run::{retry_run, skip_stage, start_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::{timers, OpError};
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::testing::ManualClock;
use common::{agent, ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY};
use serde_json::json;
use uuid::Uuid;

const MINUTE: Duration = Duration::from_secs(60);

fn builder() -> buzz_hive_core::orchestrator::Ctx {
    ctx_for(BUILDER, ActorKind::Agent)
}

async fn set(f: &Fixture, key: &str, value: serde_json::Value) {
    set_setting(&f.env(), &person(), &Scope::User, key, Some(value))
        .await
        .unwrap();
}

async fn begin(f: &Fixture, workflow: &str) -> Uuid {
    set(f, "update_channel", json!(CHANNEL.to_string())).await;
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "Write the docs".into(),
            description: "Plain words.".into(),
            workflow: workflow.into(),
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &person(), card.id).await.unwrap();
    f.drain().await.unwrap();
    card.id
}

/// A card on `workflow` whose run has started and whose first task has reached the relay.
async fn started(workflow: &str) -> (Fixture, Uuid) {
    let f = Fixture::new();
    let card = begin(&f, workflow).await;
    (f, card)
}

async fn state(f: &Fixture, card: Uuid) -> CardState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.cards().get(card).await.unwrap().unwrap().state
}

async fn run_state(f: &Fixture, card: Uuid) -> RunState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.runs().list(Some(card)).await.unwrap()[0].state
}

async fn open_items(f: &Fixture, card: Uuid) -> Vec<InboxItem> {
    list(&f.store, &person(), Some(card), false).await.unwrap()
}

/// The one open item, of `kind`.
async fn the_open(f: &Fixture, card: Uuid, kind: InboxKind) -> InboxItem {
    let open = open_items(f, card).await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].kind, kind, "{open:?}");
    open.into_iter().next().unwrap()
}

fn kinds(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

fn posts(f: &Fixture) -> Vec<String> {
    f.relay.posts().into_iter().map(|p| p.text).collect()
}

async fn save_plan(f: &Fixture, card: Uuid, body: &str) {
    save_artifact(
        &f.env(),
        &builder(),
        card,
        "plan",
        ArtifactContent::Text(body.into()),
    )
    .await
    .unwrap();
}

/// Fires every timer that is due.
async fn fire_due(f: &Fixture) {
    for timer in timers::due(&f.store, COMMUNITY, 100).await.unwrap() {
        timers::fire(&f.env(), COMMUNITY, &timer).await.unwrap();
    }
}

/// A card on `solo` whose one retry is used up, so its run has failed.
async fn failed_card() -> (Fixture, Uuid) {
    let (f, clock): (Fixture, ManualClock) = Fixture::timed();
    set(&f, "execution.timeout", json!("10m")).await;
    set(&f, "execution.retries.max", json!(1)).await;
    set(&f, "execution.retries.backoff", json!("1m")).await;
    let card = begin(&f, "solo").await;
    clock.advance(10 * MINUTE);
    fire_due(&f).await;
    clock.advance(MINUTE);
    fire_due(&f).await;
    f.drain().await.unwrap();
    clock.advance(10 * MINUTE);
    fire_due(&f).await;
    assert_eq!(state(&f, card).await, CardState::Failed);
    (f, card)
}

/// A card at the approval stage `gate`, with a saved plan.
async fn at_the_gate() -> (Fixture, Uuid) {
    let (f, card) = started("gated").await;
    save_plan(&f, card, "Step one.").await;
    f.report(&builder(), card, "passed", "plan is ready")
        .await
        .unwrap();
    f.drain().await.unwrap();
    (f, card)
}

#[tokio::test]
async fn an_approval_stage_opens_an_item_that_lists_the_artifacts() {
    let (f, card) = at_the_gate().await;
    assert_eq!(state(&f, card).await, CardState::Waiting);
    let item = the_open(&f, card, InboxKind::Approval).await;
    assert_eq!(item.stage, "gate");
    assert_eq!(item.choices(), &["approve", "reject"]);
    assert_eq!(item.detail["artifacts"][0]["name"], "plan");
    assert_eq!(item.detail["artifacts"][0]["version"], 1);
    assert!(posts(&f)
        .iter()
        .any(|text| text.contains("waiting for approval of: plan")));
}

#[tokio::test]
async fn approving_records_the_versions_and_moves_the_card_on() {
    let (f, card) = at_the_gate().await;
    let (card_after, events) = approve(&f.env(), &person(), card, &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(card_after.state, CardState::Running);
    let kinds = kinds(&events);
    for expected in ["inbox.resolved", "approval.recorded", "stage.routed"] {
        assert!(kinds.contains(&expected), "{kinds:?}");
    }
    let recorded = events
        .iter()
        .find(|e| e.kind == "approval.recorded")
        .unwrap();
    assert_eq!(recorded.payload["artifacts"][0]["name"], "plan");
    assert_eq!(recorded.payload["artifacts"][0]["version"], 1);
    assert!(open_items(&f, card).await.is_empty());
    assert_eq!(f.at(card).await.0, "ship");

    let resolved = list(&f.store, &person(), Some(card), true).await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].answer.as_deref(), Some("approve"));
    assert_eq!(resolved[0].resolved_by.as_deref(), Some("aa"));
}

#[tokio::test]
async fn approving_a_named_version_checks_it_exists() {
    let (f, card) = at_the_gate().await;
    let missing = approve(
        &f.env(),
        &person(),
        card,
        &BTreeMap::from([("plan".to_owned(), 7)]),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(missing, OpError::FailedPrecondition(_)),
        "{missing:?}"
    );
    let other = approve(
        &f.env(),
        &person(),
        card,
        &BTreeMap::from([("logo".to_owned(), 1)]),
    )
    .await
    .unwrap_err();
    assert!(matches!(other, OpError::InvalidArgument(_)), "{other:?}");
    // Nothing changed.
    the_open(&f, card, InboxKind::Approval).await;
}

#[tokio::test]
async fn an_artifact_with_no_saved_version_cannot_be_approved() {
    let (f, card) = started("gated").await;
    // The person moves the card past draft without the plan.
    f.report(&builder(), card, "passed", "done").await.unwrap();
    the_open(&f, card, InboxKind::MissingOutput).await;
    skip_stage(&f.env(), &person(), card).await.unwrap();
    the_open(&f, card, InboxKind::Approval).await;
    let refused = approve(&f.env(), &person(), card, &BTreeMap::new())
        .await
        .unwrap_err();
    match refused {
        OpError::FailedPrecondition(message) => {
            assert!(message.contains("send the card back"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn rejecting_needs_a_reason_and_sends_it_to_the_stage_the_card_goes_back_to() {
    let (f, card) = at_the_gate().await;
    for blank in ["", "   \n"] {
        let refused = send_back(&f.env(), &person(), card, blank)
            .await
            .unwrap_err();
        assert!(
            matches!(refused, OpError::InvalidArgument(_)),
            "{refused:?}"
        );
    }
    let item = the_open(&f, card, InboxKind::Approval).await;
    let refused = answer(&f.env(), &person(), item.id, "reject", "")
        .await
        .unwrap_err();
    assert!(
        matches!(refused, OpError::InvalidArgument(_)),
        "{refused:?}"
    );

    let (card_after, events) = send_back(&f.env(), &person(), card, "Add a rollback step.")
        .await
        .unwrap();
    assert_eq!(card_after.state, CardState::Running);
    let kinds = kinds(&events);
    assert!(kinds.contains(&"card.sent_back"), "{kinds:?}");
    assert!(kinds.contains(&"inbox.resolved"), "{kinds:?}");
    assert_eq!(f.at(card).await, ("draft".to_owned(), 2));

    f.drain().await.unwrap();
    let task = posts(&f)
        .into_iter()
        .find(|text| text.contains("Add a rollback step."))
        .expect("the sent back comments reach the agent");
    assert!(task.contains("A person sent the card back"), "{task}");
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn the_reason_is_kept_in_the_cards_activity() {
    let (f, card) = at_the_gate().await;
    send_back(&f.env(), &person(), card, "Needs a rollback step.")
        .await
        .unwrap();
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let events = tx
        .events()
        .of_card(card, "activity.added", 0, 100)
        .await
        .unwrap();
    let entry = events
        .iter()
        .find(|e| e.kind == "activity.added" && e.payload["type"] == "answer")
        .expect("an answer entry");
    assert_eq!(entry.payload["detail"], "Needs a rollback step.");
    assert_eq!(entry.payload["choice"], "reject");
}

#[tokio::test]
async fn an_agents_question_waits_for_a_reply_that_reaches_its_thread() {
    let (f, card) = started("three").await;
    f.report(&builder(), card, "needs_input", "Which database?")
        .await
        .unwrap();
    let item = the_open(&f, card, InboxKind::Question).await;
    assert!(item.title.contains("Which database?"), "{}", item.title);
    assert_eq!(item.choices(), &["reply", "skip", "stop"]);

    let empty = answer(&f.env(), &person(), item.id, "reply", " ")
        .await
        .unwrap_err();
    assert!(matches!(empty, OpError::InvalidArgument(_)), "{empty:?}");

    let (resolved, events) = answer(&f.env(), &person(), item.id, "reply", "Postgres.")
        .await
        .unwrap();
    assert_eq!(resolved.answer.as_deref(), Some("reply"));
    assert!(kinds(&events).contains(&"inbox.resolved"));
    assert_eq!(state(&f, card).await, CardState::Running);
    f.drain().await.unwrap();
    assert!(posts(&f).iter().any(|text| text.contains("Postgres.")));
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn an_agent_asks_through_the_gate_and_only_for_its_own_stage() {
    let (f, card) = started("three").await;
    let (stage, visit) = f.at(card).await;

    let wrong_stage = ask_gate(&f.env(), &builder(), card, "b", visit, "Which?")
        .await
        .unwrap_err();
    assert!(
        matches!(wrong_stage, OpError::FailedPrecondition(_)),
        "{wrong_stage:?}"
    );
    let blank = ask_gate(&f.env(), &builder(), card, &stage, visit, "  ")
        .await
        .unwrap_err();
    assert!(matches!(blank, OpError::InvalidArgument(_)), "{blank:?}");
    let stranger = ask_gate(&f.env(), &agent(), card, &stage, visit, "Which?")
        .await
        .unwrap_err();
    assert!(
        matches!(stranger, OpError::PermissionDenied(_)),
        "{stranger:?}"
    );

    let (run, events) = ask_gate(&f.env(), &builder(), card, &stage, visit, "Which database?")
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Waiting);
    assert!(kinds(&events).contains(&"inbox.opened"));
    let item = the_open(&f, card, InboxKind::Question).await;
    assert!(item.title.contains("Which database?"), "{}", item.title);
}

#[tokio::test]
async fn skipping_a_question_moves_the_card_on_as_passed() {
    let (f, card) = started("three").await;
    f.report(&builder(), card, "needs_input", "Which?")
        .await
        .unwrap();
    let item = the_open(&f, card, InboxKind::Question).await;
    answer(&f.env(), &person(), item.id, "skip", "")
        .await
        .unwrap();
    assert_eq!(f.at(card).await.0, "b");
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn a_route_that_cannot_go_on_waits_for_skip_or_stop() {
    let (f, card) = started("loop").await;
    f.report(&builder(), card, "passed", "").await.unwrap();
    // b fails back to a once, then its loop limit holds the card.
    f.report(&builder(), card, "failed", "broke").await.unwrap();
    f.report(&builder(), card, "passed", "").await.unwrap();
    f.report(&builder(), card, "failed", "broke again")
        .await
        .unwrap();
    let item = the_open(&f, card, InboxKind::Route).await;
    assert_eq!(item.choices(), &["skip", "stop"]);

    let wrong = answer(&f.env(), &person(), item.id, "approve", "")
        .await
        .unwrap_err();
    assert!(matches!(wrong, OpError::InvalidArgument(_)), "{wrong:?}");

    answer(&f.env(), &person(), item.id, "stop", "")
        .await
        .unwrap();
    assert_eq!(state(&f, card).await, CardState::Stopped);
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn a_stage_that_never_reports_asks_and_retry_asks_the_agent_to_carry_on() {
    let (f, clock): (Fixture, ManualClock) = Fixture::timed();
    set(&f, "execution.timeout", json!("10m")).await;
    set(&f, "execution.retries.max", json!(0)).await;
    let card = begin(&f, "solo").await;
    clock.advance(10 * MINUTE);
    fire_due(&f).await;
    let item = the_open(&f, card, InboxKind::NoResult).await;
    assert_eq!(item.choices(), &["retry", "skip", "stop"]);

    answer(&f.env(), &person(), item.id, "retry", "")
        .await
        .unwrap();
    assert_eq!(state(&f, card).await, CardState::Running);
    assert!(open_items(&f, card).await.is_empty());
    // The stage has its time again.
    assert_eq!(
        timers::next_in(&f.store, COMMUNITY).await.unwrap(),
        Some(10 * MINUTE)
    );
}

#[tokio::test]
async fn a_missing_output_is_answered_by_retry() {
    let (f, card) = started("gated").await;
    f.report(&builder(), card, "passed", "done").await.unwrap();
    let item = the_open(&f, card, InboxKind::MissingOutput).await;
    assert!(item.title.contains("plan"), "{}", item.title);
    answer(&f.env(), &person(), item.id, "retry", "")
        .await
        .unwrap();
    assert_eq!(state(&f, card).await, CardState::Running);
    assert_eq!(f.at(card).await.0, "draft");
    f.drain().await.unwrap();
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn a_failed_run_opens_an_item_that_retry_answers_with_a_new_run() {
    let (f, card) = failed_card().await;
    let item = the_open(&f, card, InboxKind::Failure).await;
    assert_eq!(item.choices(), &["retry", "dismiss"]);

    let (_, events) = answer(&f.env(), &person(), item.id, "retry", "")
        .await
        .unwrap();
    assert!(
        kinds(&events).contains(&"run.started"),
        "{:?}",
        kinds(&events)
    );
    assert_eq!(state(&f, card).await, CardState::Running);
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn dismissing_a_failure_closes_it_and_leaves_the_card_failed() {
    let (f, card) = failed_card().await;
    let item = the_open(&f, card, InboxKind::Failure).await;
    answer(&f.env(), &person(), item.id, "dismiss", "")
        .await
        .unwrap();
    assert_eq!(state(&f, card).await, CardState::Failed);
    assert!(open_items(&f, card).await.is_empty());
    // The run can still be retried by hand.
    let run_id = {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.runs().list(Some(card)).await.unwrap()[0].id
    };
    retry_run(&f.env(), &person(), run_id).await.unwrap();
}

#[tokio::test]
async fn an_item_answers_once() {
    let (f, card) = at_the_gate().await;
    let item = the_open(&f, card, InboxKind::Approval).await;
    answer(&f.env(), &person(), item.id, "approve", "")
        .await
        .unwrap();
    let again = answer(&f.env(), &person(), item.id, "approve", "")
        .await
        .unwrap_err();
    assert!(matches!(again, OpError::FailedPrecondition(_)), "{again:?}");
    let gone = answer(&f.env(), &person(), Uuid::new_v4(), "approve", "")
        .await
        .unwrap_err();
    assert!(matches!(gone, OpError::NotFound(_)), "{gone:?}");
    let no_gate = approve(&f.env(), &person(), card, &BTreeMap::new())
        .await
        .unwrap_err();
    assert!(
        matches!(no_gate, OpError::FailedPrecondition(_)),
        "{no_gate:?}"
    );
}

#[tokio::test]
async fn an_agent_cannot_answer_send_back_or_write_to_a_stage() {
    let (f, card) = at_the_gate().await;
    let item = the_open(&f, card, InboxKind::Approval).await;
    for who in [builder(), agent()] {
        let a = answer(&f.env(), &who, item.id, "approve", "")
            .await
            .unwrap_err();
        assert!(matches!(a, OpError::PermissionDenied(_)), "{a:?}");
        let b = send_back(&f.env(), &who, card, "no").await.unwrap_err();
        assert!(matches!(b, OpError::PermissionDenied(_)), "{b:?}");
        let c = send_message(&f.env(), &who, card, "hello")
            .await
            .unwrap_err();
        assert!(matches!(c, OpError::PermissionDenied(_)), "{c:?}");
    }
    the_open(&f, card, InboxKind::Approval).await;
}

#[tokio::test]
async fn a_person_moving_the_card_on_closes_its_item_as_superseded() {
    let (f, card) = started("three").await;
    f.report(&builder(), card, "needs_input", "Which?")
        .await
        .unwrap();
    let item = the_open(&f, card, InboxKind::Question).await;
    let (_, events) = skip_stage(&f.env(), &person(), card).await.unwrap();
    assert!(kinds(&events).contains(&"inbox.resolved"));
    let all = list(&f.store, &person(), Some(card), true).await.unwrap();
    let closed = all.iter().find(|i| i.id == item.id).unwrap();
    assert_eq!(closed.answer.as_deref(), Some("superseded"));
    assert!(open_items(&f, card).await.is_empty());
}

#[tokio::test]
async fn open_items_come_first_across_cards() {
    let f = Fixture::new();
    let one = begin(&f, "three").await;
    let two = begin(&f, "three").await;
    f.report(&builder(), one, "needs_input", "First?")
        .await
        .unwrap();
    let item = the_open(&f, one, InboxKind::Question).await;
    answer(&f.env(), &person(), item.id, "stop", "")
        .await
        .unwrap();
    f.report(&builder(), two, "needs_input", "Second?")
        .await
        .unwrap();

    let all = list(&f.store, &person(), None, true).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].is_open() && !all[1].is_open(), "{all:?}");
    assert_eq!(
        list(&f.store, &person(), None, false).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_person_writes_to_the_agent_without_changing_the_run() {
    let (f, card) = started("three").await;
    let (id, events) = send_message(&f.env(), &person(), card, "  Use the staging copy.  ")
        .await
        .unwrap();
    assert_eq!(id.len(), 32);
    assert_eq!(kinds(&events), ["activity.added"]);
    f.drain().await.unwrap();
    let reply = f
        .relay
        .posts()
        .into_iter()
        .find(|p| p.text.contains("Use the staging copy."))
        .expect("the message reaches the relay");
    assert!(reply.root.is_some());
    assert_eq!(reply.mention.as_deref(), Some(BUILDER));
    assert_eq!(run_state(&f, card).await, RunState::Running);

    let blank = send_message(&f.env(), &person(), card, " ")
        .await
        .unwrap_err();
    assert!(matches!(blank, OpError::InvalidArgument(_)), "{blank:?}");
    let long = send_message(&f.env(), &person(), card, &"x".repeat(33 * 1024))
        .await
        .unwrap_err();
    assert!(matches!(long, OpError::InvalidArgument(_)), "{long:?}");
}

#[tokio::test]
async fn a_card_with_no_run_cannot_be_written_to() {
    let f = Fixture::new();
    set(&f, "update_channel", json!(CHANNEL.to_string())).await;
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "x".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let refused = send_message(&f.env(), &person(), card.id, "hi")
        .await
        .unwrap_err();
    assert!(
        matches!(refused, OpError::FailedPrecondition(_)),
        "{refused:?}"
    );
}

#[tokio::test]
async fn usage_counts_only_what_agents_started() {
    use buzz_hive_core::orchestrator::usage;

    let f = Fixture::new();
    // A person creates and starts a card; the builder agent creates one and starts its run.
    set(&f, "update_channel", json!(CHANNEL.to_string())).await;
    let by_person = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "mine".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    start_run(&f.env(), &person(), by_person.id).await.unwrap();
    let by_agent = create_card(
        &f.store,
        &builder(),
        NewCard {
            title: "theirs".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    start_run(&f.env(), &builder(), by_agent.id).await.unwrap();

    let found = usage::get(&f.store, &person()).await.unwrap();
    assert_eq!(found.runs, 2);
    assert_eq!(found.ended_runs, 0);
    assert_eq!(found.weeks.len(), 8);
    let this_week = found.weeks.last().unwrap();
    assert_eq!(this_week.cards_created_by_agents, 1);
    assert_eq!(this_week.runs_started_by_agents, 1);
    assert!(found
        .weeks
        .iter()
        .rev()
        .skip(1)
        .all(|w| w.cards_created_by_agents == 0 && w.runs_started_by_agents == 0));
}

#[tokio::test]
async fn the_activity_list_filters_by_type_author_and_stage() {
    use buzz_hive_core::orchestrator::activity::{list_activity, ActivityFilter};

    let (f, card) = at_the_gate().await;
    send_back(&f.env(), &person(), card, "Needs a rollback step.")
        .await
        .unwrap();

    let (all, seq) = list_activity(&f.store, &person(), card, &ActivityFilter::default())
        .await
        .unwrap();
    assert!(seq >= all.last().unwrap().seq);
    let answers = ActivityFilter {
        types: vec!["answer".into()],
        ..Default::default()
    };
    let (found, _) = list_activity(&f.store, &person(), card, &answers)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].payload["detail"], "Needs a rollback step.");

    let by_agents = ActivityFilter {
        author: Some(ActorKind::Agent),
        types: vec!["answer".into()],
        ..Default::default()
    };
    let (none, _) = list_activity(&f.store, &person(), card, &by_agents)
        .await
        .unwrap();
    assert!(none.is_empty());

    let at_gate = ActivityFilter {
        stage: Some("gate".into()),
        visit: Some(1),
        ..Default::default()
    };
    let (gate, _) = list_activity(&f.store, &person(), card, &at_gate)
        .await
        .unwrap();
    assert!(gate.iter().all(|e| e.payload["stage"] == "gate"));
    assert!(!gate.is_empty());

    let too_many = ActivityFilter {
        limit: 1001,
        ..Default::default()
    };
    assert!(matches!(
        list_activity(&f.store, &person(), card, &too_many).await,
        Err(OpError::InvalidArgument(_))
    ));
    assert!(matches!(
        list_activity(&f.store, &person(), Uuid::new_v4(), &answers).await,
        Err(OpError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_full_payload_is_read_only_for_an_event_on_the_runs_card() {
    use buzz_hive_core::orchestrator::activity::{full_payload, list_activity, ActivityFilter};

    let (f, card) = at_the_gate().await;
    send_back(&f.env(), &person(), card, "Needs a rollback step.")
        .await
        .unwrap();
    let (entries, _) = list_activity(&f.store, &person(), card, &ActivityFilter::default())
        .await
        .unwrap();
    let entry = entries.last().unwrap();
    let run = entry.run_id.expect("the entry names its run");
    let payload = full_payload(&f.store, &person(), run, entry.seq)
        .await
        .unwrap();
    assert_eq!(payload, entry.payload);

    // An event of another card is not this run's to read.
    let other = begin(&f, "solo").await;
    let other_run = {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.runs().list(Some(other)).await.unwrap()[0].id
    };
    let crossed = full_payload(&f.store, &person(), other_run, entry.seq).await;
    assert!(matches!(crossed, Err(OpError::NotFound(_))), "{crossed:?}");
    let far = full_payload(&f.store, &person(), run, 999_999).await;
    assert!(matches!(far, Err(OpError::NotFound(_))), "{far:?}");
    let unknown = full_payload(&f.store, &person(), Uuid::new_v4(), entry.seq).await;
    assert!(matches!(unknown, Err(OpError::NotFound(_))), "{unknown:?}");
}
