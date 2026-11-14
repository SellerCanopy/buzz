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
use buzz_hive_core::testing::MemStore;
use common::person;

const CURRENT_PHASE: u8 = 4;

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

#[tokio::test]
async fn every_kind_introduced_so_far_is_appended_by_some_scenario() {
    let store = MemStore::new();
    card_scenarios(&store).await;

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
