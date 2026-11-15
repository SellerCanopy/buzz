//! Auto mode: a card that answers its own gates, retries and resumes by rule, and bounds its
//! loops. Ported from the Go engine's `scenario_auto_mode_test.go`, without the cost cases.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::artifact::ArtifactContent;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::inbox::{InboxItem, InboxKind};
use buzz_hive_core::orchestrator::activity::{list_activity, ActivityFilter};
use buzz_hive_core::orchestrator::artifacts::save_artifact;
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::inbox::{approve, list};
use buzz_hive_core::orchestrator::run::{skip_stage, start_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::{timers, usage, OpError};
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::testing::ManualClock;
use common::{ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY, HIVE};
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

async fn auto_on(f: &Fixture, card: Uuid) -> Vec<Event> {
    set_setting(
        &f.env(),
        &person(),
        &Scope::Card(card),
        "card.auto_mode",
        Some(json!(true)),
    )
    .await
    .unwrap()
    .1
}

/// A card on `workflow`, not yet started.
async fn card_on(f: &Fixture, workflow: &str) -> Uuid {
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
    card.id
}

/// A card on `workflow` in auto mode, whose run has started.
async fn auto_card(f: &Fixture, workflow: &str) -> Uuid {
    let card = card_on(f, workflow).await;
    auto_on(f, card).await;
    start_run(&f.env(), &person(), card).await.unwrap();
    f.drain().await.unwrap();
    card
}

async fn save_plan(f: &Fixture, card: Uuid) {
    save_artifact(
        &f.env(),
        &builder(),
        card,
        "plan",
        ArtifactContent::Text("Step one.".into()),
    )
    .await
    .unwrap();
}

async fn items(f: &Fixture, card: Uuid, resolved: bool) -> Vec<InboxItem> {
    list(&f.store, &person(), Some(card), resolved)
        .await
        .unwrap()
}

async fn open_items(f: &Fixture, card: Uuid) -> Vec<InboxItem> {
    items(f, card, false).await
}

/// The items auto mode answered with `choice`, oldest first.
async fn answered_by_the_engine(f: &Fixture, card: Uuid, choice: &str) -> Vec<InboxItem> {
    let mut found: Vec<InboxItem> = items(f, card, true)
        .await
        .into_iter()
        .filter(|i| i.resolved_by.as_deref() == Some(HIVE) && i.answer.as_deref() == Some(choice))
        .collect();
    found.reverse();
    found
}

async fn state(f: &Fixture, card: Uuid) -> CardState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.cards().get(card).await.unwrap().unwrap().state
}

async fn runs_of(f: &Fixture, card: Uuid) -> usize {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.runs().list(Some(card)).await.unwrap().len()
}

/// The card's activity entries of `kind`, oldest first.
async fn entries(f: &Fixture, card: Uuid, kind: &str) -> Vec<Event> {
    let filter = ActivityFilter {
        types: vec![kind.into()],
        ..Default::default()
    };
    list_activity(&f.store, &person(), card, &filter)
        .await
        .unwrap()
        .0
}

async fn fire_due(f: &Fixture) {
    for timer in timers::due(&f.store, COMMUNITY, 100).await.unwrap() {
        timers::fire(&f.env(), COMMUNITY, &timer).await.unwrap();
    }
}

#[tokio::test]
async fn auto_mode_approves_a_gate_whose_artifacts_are_saved() {
    let f = Fixture::new();
    let card = auto_card(&f, "gated").await;
    save_plan(&f, card).await;
    f.report(&builder(), card, "passed", "plan is ready")
        .await
        .unwrap();

    // The gate was answered in the step that opened it: no person, and the card moved on.
    assert!(open_items(&f, card).await.is_empty());
    assert_eq!(f.at(card).await.0, "ship");
    assert_eq!(state(&f, card).await, CardState::Running);
    let approved = answered_by_the_engine(&f, card, "approve").await;
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].kind, InboxKind::Approval);

    // Activity says what auto mode did, who did it, and why.
    let auto = entries(&f, card, "auto").await;
    assert_eq!(auto.len(), 1);
    assert_eq!(auto[0].actor.pubkey, HIVE);
    assert_eq!(auto[0].payload["detail"], "approved plan v1");
    assert_eq!(auto[0].payload["choice"], "approve");
    assert_eq!(auto[0].payload["stage"], "gate");
    assert!(entries(&f, card, "answer").await.is_empty());

    // The task for the next stage reached the relay.
    f.drain().await.unwrap();
    assert!(f.relay.posts().iter().any(|p| p.text.contains("Do ship")));
}

#[tokio::test]
async fn auto_mode_off_leaves_the_gate_for_a_person() {
    let f = Fixture::new();
    let card = card_on(&f, "gated").await;
    start_run(&f.env(), &person(), card).await.unwrap();
    save_plan(&f, card).await;
    f.report(&builder(), card, "passed", "plan is ready")
        .await
        .unwrap();
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::Approval);
    assert!(entries(&f, card, "auto").await.is_empty());
}

#[tokio::test]
async fn turning_auto_mode_on_answers_the_gate_the_card_already_waits_at() {
    let f = Fixture::new();
    let card = card_on(&f, "gated").await;
    start_run(&f.env(), &person(), card).await.unwrap();
    save_plan(&f, card).await;
    f.report(&builder(), card, "passed", "plan is ready")
        .await
        .unwrap();
    assert_eq!(open_items(&f, card).await.len(), 1);

    let events = auto_on(&f, card).await;
    assert!(open_items(&f, card).await.is_empty());
    assert_eq!(f.at(card).await.0, "ship");
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    for expected in ["setting.changed", "inbox.resolved", "approval.recorded"] {
        assert!(kinds.contains(&expected), "{kinds:?}");
    }
    assert_eq!(entries(&f, card, "auto").await.len(), 1);
}

#[tokio::test]
async fn a_gate_with_a_missing_artifact_stays_with_a_person_and_says_why() {
    let f = Fixture::new();
    let card = auto_card(&f, "gated").await;
    // The agent passes the draft without saving the plan. That wait is a person's too.
    f.report(&builder(), card, "passed", "done").await.unwrap();
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::MissingOutput);

    skip_stage(&f.env(), &person(), card).await.unwrap();
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::Approval);
    assert!(answered_by_the_engine(&f, card, "approve").await.is_empty());
    let notes = entries(&f, card, "note").await;
    let last = notes.last().expect("a note says why");
    assert_eq!(last.actor.pubkey, HIVE);
    assert_eq!(
        last.payload["detail"],
        "artifact `plan` has no saved version"
    );
}

#[tokio::test]
async fn an_agent_cannot_approve_a_gate_with_a_missing_artifact_and_the_refusal_names_it() {
    let f = Fixture::new();
    // Auto mode is off: the rule binds the agent anyway.
    let card = card_on(&f, "gated").await;
    start_run(&f.env(), &person(), card).await.unwrap();
    f.report(&builder(), card, "passed", "done").await.unwrap();
    skip_stage(&f.env(), &person(), card).await.unwrap();

    let refused = approve(&f.env(), &builder(), card, &BTreeMap::new())
        .await
        .unwrap_err();
    match refused {
        OpError::FailedPrecondition(message) => {
            assert!(message.contains("`plan`"), "{message}");
            assert!(message.contains("no saved version"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(open_items(&f, card).await.len(), 1);

    // With the plan saved an agent may approve, as a person may.
    let saved = card_on(&f, "gated").await;
    start_run(&f.env(), &person(), saved).await.unwrap();
    save_plan(&f, saved).await;
    f.report(&builder(), saved, "passed", "plan is ready")
        .await
        .unwrap();
    approve(&f.env(), &builder(), saved, &BTreeMap::new())
        .await
        .unwrap();
    assert!(open_items(&f, saved).await.is_empty());
    // It was the agent's answer, not the engine's, so Activity says so.
    assert_eq!(entries(&f, saved, "answer").await.len(), 1);
    assert!(entries(&f, saved, "auto").await.is_empty());
}

#[tokio::test]
async fn an_answer_that_cannot_be_carried_out_is_undone_and_left_for_a_person() {
    let f = Fixture::new();
    let card = auto_card(&f, "gatedaway").await;
    save_plan(&f, card).await;
    // The step that opens the gate succeeds even though auto mode cannot approve it: the next
    // stage names an agent that does not exist.
    let (run, _) = f
        .report(&builder(), card, "passed", "plan is ready")
        .await
        .unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("gate"));
    assert_eq!(state(&f, card).await, CardState::Waiting);
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::Approval);
    assert!(open[0].resolved_by.is_none());
    assert!(entries(&f, card, "auto").await.is_empty());
    let notes = entries(&f, card, "note").await;
    let detail = notes.last().unwrap().payload["detail"].as_str().unwrap();
    assert!(detail.starts_with("auto mode could not answer"), "{detail}");
    assert!(detail.contains("ghost"), "{detail}");
    // Nothing of the failed answer is left behind: no approval, no task.
    f.drain().await.unwrap();
    assert!(!f.relay.posts().iter().any(|p| p.text.contains("Ship it")));
}

#[tokio::test]
async fn a_failed_run_is_started_again_twice_then_left_for_a_person() {
    let (f, clock): (Fixture, ManualClock) = Fixture::timed();
    set(&f, "execution.timeout", json!("10m")).await;
    set(&f, "execution.retries.max", json!(1)).await;
    set(&f, "execution.retries.backoff", json!("1m")).await;
    let card = auto_card(&f, "solo").await;
    // Each run times out, retries once, and fails for good.
    for _ in 0..3 {
        clock.advance(10 * MINUTE);
        fire_due(&f).await;
        clock.advance(MINUTE);
        fire_due(&f).await;
        f.drain().await.unwrap();
        clock.advance(10 * MINUTE);
        fire_due(&f).await;
    }
    assert_eq!(runs_of(&f, card).await, 3);
    assert_eq!(state(&f, card).await, CardState::Failed);
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::Failure);
    assert_eq!(answered_by_the_engine(&f, card, "retry").await.len(), 2);
    let auto = entries(&f, card, "auto").await;
    assert_eq!(auto.len(), 2);
    assert!(auto[0].payload["detail"]
        .as_str()
        .unwrap()
        .contains("retry 1 of 2"));

    // A run auto mode started again is not one an agent started.
    let found = usage::get(&f.store, &person(), HIVE).await.unwrap();
    assert_eq!(found.runs, 3);
    assert!(found.weeks.iter().all(|w| w.runs_started_by_agents == 0));
}

#[tokio::test]
async fn a_stage_that_never_reports_is_asked_to_carry_on_twice_then_left_for_a_person() {
    let (f, clock): (Fixture, ManualClock) = Fixture::timed();
    set(&f, "execution.timeout", json!("10m")).await;
    set(&f, "execution.retries.max", json!(0)).await;
    let card = auto_card(&f, "solo").await;
    for _ in 0..3 {
        clock.advance(10 * MINUTE);
        fire_due(&f).await;
    }
    assert_eq!(runs_of(&f, card).await, 1);
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::NoResult);
    assert_eq!(answered_by_the_engine(&f, card, "retry").await.len(), 2);
    assert_eq!(state(&f, card).await, CardState::Waiting);
}

#[tokio::test]
async fn a_loop_with_no_limit_stops_at_the_auto_mode_limit_with_a_needs_you_item() {
    let f = Fixture::new();
    let card = auto_card(&f, "unbounded").await;
    // Ten trips back are allowed.
    for _ in 0..10 {
        f.report(&builder(), card, "passed", "").await.unwrap();
        f.report(&builder(), card, "failed", "broke").await.unwrap();
        assert_eq!(f.at(card).await.0, "a");
        assert!(open_items(&f, card).await.is_empty());
    }
    f.report(&builder(), card, "passed", "").await.unwrap();
    f.report(&builder(), card, "failed", "broke").await.unwrap();
    let open = open_items(&f, card).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, InboxKind::Route);
    assert!(
        open[0].title.contains("its loop limit is 10"),
        "{}",
        open[0].title
    );
    assert_eq!(state(&f, card).await, CardState::Waiting);
}

#[tokio::test]
async fn the_same_loop_is_not_bounded_when_the_card_is_not_in_auto_mode() {
    let f = Fixture::new();
    let card = card_on(&f, "unbounded").await;
    start_run(&f.env(), &person(), card).await.unwrap();
    for _ in 0..12 {
        f.report(&builder(), card, "passed", "").await.unwrap();
        f.report(&builder(), card, "failed", "broke").await.unwrap();
    }
    assert!(open_items(&f, card).await.is_empty());
    assert_eq!(f.at(card).await.0, "a");
}

#[tokio::test]
async fn auto_mode_belongs_to_a_card() {
    let f = Fixture::new();
    let refused = set_setting(
        &f.env(),
        &person(),
        &Scope::Workflow("gated".into()),
        "card.auto_mode",
        Some(json!(true)),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(refused, OpError::InvalidArgument(_)),
        "{refused:?}"
    );
    let user = set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "card.auto_mode",
        Some(json!(true)),
    )
    .await
    .unwrap_err();
    assert!(matches!(user, OpError::InvalidArgument(_)), "{user:?}");
    let text = set_setting(
        &f.env(),
        &person(),
        &Scope::Card(Uuid::new_v4()),
        "card.auto_mode",
        Some(json!("yes")),
    )
    .await
    .unwrap_err();
    assert!(matches!(text, OpError::InvalidArgument(_)), "{text:?}");
}
