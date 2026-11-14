//! Every event kind introduced so far is appended by at least one scenario.
//!
//! Ported from `zz_event_coverage_test.go`. A phase that adds a kind to `EVENT_KINDS`
//! adds a scenario here and bumps `CURRENT_PHASE`.

mod common;

use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::EVENT_KINDS;
use buzz_hive_core::orchestrator::cards::{
    add_note, archive_cards, create_card, delete_card, restore_card, update_card, CardPatch,
    NewCard,
};
use buzz_hive_core::orchestrator::outbox::drain_outbox;
use buzz_hive_core::orchestrator::run::start_run;
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::testing::MemStore;
use common::{person, Fixture, CHANNEL, COMMUNITY};

const CURRENT_PHASE: u8 = 6;

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
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
}

#[tokio::test]
async fn every_kind_introduced_so_far_is_appended_by_some_scenario() {
    let f = Fixture::new();
    let store = &f.store;
    card_scenarios(store).await;
    run_scenarios(&f).await;

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
