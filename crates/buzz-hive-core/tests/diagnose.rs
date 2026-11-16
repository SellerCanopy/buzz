//! What an agent diagnoses with: a run's events, the health of the engine, and a report that
//! must quote its evidence.

mod common;

use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::diagnose::{
    health, report_diagnosis, run_events, Diagnosis, STUCK_AFTER_SECS,
};
use buzz_hive_core::orchestrator::run::start_run;
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::Store;
use common::{person, Fixture, CHANNEL, COMMUNITY};
use serde_json::json;

/// A card whose run has started, and the run's id.
async fn begun() -> (Fixture, uuid::Uuid, uuid::Uuid) {
    let f = Fixture::new();
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "Fix".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (run, _) = start_run(&f.env(), &person(), card.id).await.unwrap();
    (f, card.id, run.id)
}

#[tokio::test]
async fn a_run_is_stuck_once_its_stage_is_older_than_the_limit() {
    let (f, card, run) = begun().await;
    let now = chrono::Utc::now();
    let fresh = health(&f.store, &person(), now).await.unwrap();
    assert!(fresh.stuck_runs.is_empty(), "{fresh:?}");
    assert!(fresh.head_seq > 0);
    assert!(
        fresh.outbox_pending > 0,
        "the stage task is waiting for the relay"
    );

    let later = now + chrono::Duration::seconds(STUCK_AFTER_SECS + 60);
    let stuck = health(&f.store, &person(), later).await.unwrap();
    assert_eq!(stuck.stuck_runs.len(), 1, "{stuck:?}");
    assert_eq!(stuck.stuck_runs[0].run_id, run);
    assert_eq!(stuck.stuck_runs[0].card_id, card);
    assert!(stuck.stuck_runs[0].seconds > STUCK_AFTER_SECS);
    assert!(stuck.oldest_pending_seconds >= STUCK_AFTER_SECS);
}

#[tokio::test]
async fn a_diagnosis_without_a_quote_is_refused_and_writes_nothing() {
    let (f, card, run) = begun().await;
    let before = run_events(&f.store, &person(), run, 0, 0)
        .await
        .unwrap()
        .len();
    for evidence in [vec![], vec![("log".to_owned(), "   ".to_owned())]] {
        let refused = report_diagnosis(
            &f.store,
            &person(),
            run,
            Diagnosis {
                cause: "the push failed".into(),
                stage: String::new(),
                evidence,
                gaps: Vec::new(),
            },
        )
        .await;
        assert!(
            matches!(refused, Err(OpError::InvalidArgument(_))),
            "{refused:?}"
        );
    }
    let after = run_events(&f.store, &person(), run, 0, 0)
        .await
        .unwrap()
        .len();
    assert_eq!(before, after);
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert!(tx.cards().get(card).await.unwrap().is_some());
}
