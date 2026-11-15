//! Every event kind introduced so far is appended by at least one scenario.
//!
//! Ported from `zz_event_coverage_test.go`. A phase that adds a kind to `EVENT_KINDS`
//! adds a scenario here and bumps `CURRENT_PHASE`.

mod common;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::artifact::ArtifactContent;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::EVENT_KINDS;
use buzz_hive_core::orchestrator::activity::log_work;
use buzz_hive_core::orchestrator::artifacts::{save_artifact, set_handoff};
use buzz_hive_core::orchestrator::cards::{
    add_note, archive_cards, create_card, delete_card, restore_card, update_card, CardPatch,
    NewCard,
};
use buzz_hive_core::orchestrator::run::{resume_card, skip_stage, start_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::testing::MemStore;
use common::{ctx_for, person, Fixture, BUILDER, CHANNEL};

const CURRENT_PHASE: u8 = 9;

async fn card_scenarios(store: &MemStore) {
    let ctx = person();
    let (card, _) = create_card(
        store,
        &ctx,
        NewCard {
            title: "scenario".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    update_card(
        store,
        &ctx,
        card.id,
        CardPatch {
            workflow: Some("rpi".into()),
            state: Some(CardState::Running),
        },
    )
    .await
    .unwrap();
    add_note(store, &ctx, card.id, "a note", "").await.unwrap();
    update_card(
        store,
        &ctx,
        card.id,
        CardPatch {
            state: Some(CardState::Stopped),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    archive_cards(store, &ctx, &[card.id]).await.unwrap();
    restore_card(store, &ctx, card.id).await.unwrap();
    delete_card(store, &ctx, card.id).await.unwrap();
}

/// Phase 6: the update channel, a run that starts, and the stage task that reaches the relay.
async fn run_scenarios(f: &Fixture) {
    let ctx = person();
    set_setting(
        &f.env(),
        &ctx,
        &Scope::User,
        "update_channel",
        Some(serde_json::json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &ctx,
        NewCard {
            title: "run".into(),
            workflow: "demo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &ctx, card.id).await.unwrap();
    f.drain().await.unwrap();
}

/// Phase 7: the agent reports a result, and a passed last stage ends the run.
async fn report_scenarios(f: &Fixture) {
    let ctx = person();
    set_setting(
        &f.env(),
        &ctx,
        &Scope::User,
        "update_channel",
        Some(serde_json::json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &ctx,
        NewCard {
            title: "report".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &ctx, card.id).await.unwrap();
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    f.report(&agent, card.id, "passed", "done").await.unwrap();
}

/// A card on `workflow` whose run has started and whose posts have reached the relay.
async fn begun(f: &Fixture, workflow: &str) -> uuid::Uuid {
    let ctx = person();
    let (card, _) = create_card(
        &f.store,
        &ctx,
        NewCard {
            title: workflow.into(),
            workflow: workflow.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &ctx, card.id).await.unwrap();
    f.drain().await.unwrap();
    card.id
}

/// Phase 8: results route a card, a result with no route waits, a person skips and resumes,
/// a repeat stage runs rounds, and a stage whose conditions fail is skipped.
async fn routing_scenarios(f: &Fixture) {
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    let report = |card: uuid::Uuid, result: &'static str| {
        let agent = agent.clone();
        async move {
            f.report(&agent, card, result, "").await.unwrap();
            f.drain().await.unwrap();
        }
    };

    // A result with no route waits, and a person skips the stage.
    let card = begun(f, "three").await;
    report(card, "passed").await;
    report(card, "failed").await;
    skip_stage(&f.env(), &person(), card).await.unwrap();

    // A held stage waits for a person.
    let card = begun(f, "held").await;
    report(card, "passed").await;
    resume_card(&f.env(), &person(), card).await.unwrap();

    // Rounds of a repeat stage.
    let card = begun(f, "plain").await;
    report(card, "needs_work").await;

    // Conditions the card does not meet.
    let card = begun(f, "cond").await;
    report(card, "passed").await;
}

/// Phase 9: an agent saves an artifact and logs work, and a person sets the handoff.
async fn artifact_scenarios(f: &Fixture) {
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    let card = begun(f, "docs").await;
    save_artifact(
        &f.env(),
        &agent,
        card,
        "plan",
        ArtifactContent::Text("- [ ] one".into()),
    )
    .await
    .unwrap();
    log_work(&f.env(), &agent, card, "Wrote the plan.", "", "")
        .await
        .unwrap();
    set_handoff(&f.env(), &person(), card, "plan", false)
        .await
        .unwrap();
}

#[tokio::test]
async fn every_kind_introduced_so_far_is_appended_by_some_scenario() {
    let f = Fixture::new();
    let store = &f.store;
    card_scenarios(store).await;
    run_scenarios(&f).await;
    report_scenarios(&f).await;
    routing_scenarios(&f).await;
    artifact_scenarios(&f).await;

    let committed = store.committed_kinds();
    let missing: Vec<_> = EVENT_KINDS
        .iter()
        .filter(|(kind, phase)| *phase <= CURRENT_PHASE && !committed.contains(*kind))
        .map(|(kind, _)| *kind)
        .collect();
    assert!(missing.is_empty(), "no scenario appends {missing:?}");
    for kind in &committed {
        assert!(
            EVENT_KINDS.iter().any(|(k, _)| k == kind),
            "{kind} is not in the registry"
        );
    }
}
