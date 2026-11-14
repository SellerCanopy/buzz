//! Runs, stage threads and the outbox keep their rules in Postgres: one active run per card,
//! one outbox row per key, and a drain that skips rows another worker holds.

mod support;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::ports::store::{CommunityId, Posting, StageThread, Store, StoreError};
use buzz_hive_store::pg::PgStore;
use uuid::Uuid;

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn card(n: u128) -> Card {
    Card {
        id: Uuid::from_u128(n),
        title: format!("card {n}"),
        description: String::new(),
        state: CardState::Idle,
        workflow: "demo".into(),
        archived: false,
        created_by: "aa".into(),
    }
}

fn status(text: &str) -> Posting {
    Posting::Status {
        channel: Uuid::from_u128(77),
        text: text.into(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_run_round_trips_and_a_card_has_one_active_run() {
    let (store, _guard, _relay) = store("hive_runs").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let mut run = Run::new(Uuid::from_u128(1), "demo", "h1");
    tx.runs().insert(&run).await.unwrap();
    let second = Run::new(Uuid::from_u128(1), "demo", "h1");
    let refused = tx.runs().insert(&second).await;
    assert!(
        matches!(refused, Err(StoreError::Conflict(_))),
        "{refused:?}"
    );
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.runs().get(run.id).await.unwrap(), Some(run.clone()));
    assert_eq!(
        tx.runs()
            .active_for_card(run.card_id)
            .await
            .unwrap()
            .map(|r| r.id),
        Some(run.id)
    );

    run.state = RunState::Running;
    run.current_stage = Some("build".into());
    run.visit = 1;
    run.requester = Some("bb".into());
    tx.runs().update(&run).await.unwrap();
    assert_eq!(tx.runs().visits(run.card_id, "build").await.unwrap(), 1);
    assert_eq!(tx.runs().visits(run.card_id, "other").await.unwrap(), 0);

    // Once the run ends the card can start another.
    run.state = RunState::Ended;
    run.ended_at = Some(chrono::Utc::now());
    tx.runs().update(&run).await.unwrap();
    assert_eq!(tx.runs().active_for_card(run.card_id).await.unwrap(), None);
    tx.runs().insert(&second).await.unwrap();
    assert_eq!(tx.runs().list(Some(run.card_id)).await.unwrap().len(), 2);
    assert_eq!(tx.runs().list(None).await.unwrap().len(), 2);
    assert!(tx
        .runs()
        .list(Some(Uuid::from_u128(9)))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn runs_stay_in_their_community() {
    let (store, _guard, _relay) = store("hive_runs_c").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let run = Run::new(Uuid::from_u128(1), "demo", "h1");
    let mut tx = store.begin(one).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.runs().insert(&run).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(two).await.unwrap();
    assert_eq!(tx.runs().get(run.id).await.unwrap(), None);
    assert!(tx.runs().list(None).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_stage_thread_is_recorded_once() {
    let (store, _guard, _relay) = store("hive_threads").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let thread = StageThread {
        card_id: Uuid::from_u128(1),
        stage: "build".into(),
        visit: 1,
        root_event_id: "e1".into(),
        agent_pubkey: "bb".into(),
    };
    tx.runs().record_thread(&thread).await.unwrap();
    // A repeat keeps the first event.
    let again = StageThread {
        root_event_id: "e2".into(),
        ..thread.clone()
    };
    tx.runs().record_thread(&again).await.unwrap();
    assert_eq!(
        tx.runs().thread(thread.card_id, "build", 1).await.unwrap(),
        Some(thread)
    );
    assert_eq!(
        tx.runs()
            .thread(Uuid::from_u128(1), "build", 2)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_outbox_keeps_one_row_per_key_and_drains_in_order() {
    let (store, _guard, _relay) = store("hive_outbox").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    let one = Uuid::from_u128(1);
    let two = Uuid::from_u128(2);
    assert!(tx
        .outbox()
        .enqueue("k1", one, &status("first"))
        .await
        .unwrap());
    assert!(!tx
        .outbox()
        .enqueue("k1", one, &status("again"))
        .await
        .unwrap());
    assert!(tx
        .outbox()
        .enqueue("k2", two, &status("second"))
        .await
        .unwrap());
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let all = tx.outbox().all().await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].posting, status("first"));
    drop(tx);

    // A card in the skip list is passed over.
    let mut tx = store.begin(community).await.unwrap();
    let head = tx.outbox().next(&[]).await.unwrap().unwrap();
    assert_eq!(head.key, "k1");
    drop(tx);
    let mut tx = store.begin(community).await.unwrap();
    let next = tx.outbox().next(&[one]).await.unwrap().unwrap();
    assert_eq!(next.key, "k2");
    tx.outbox()
        .mark_failed(next.id, "relay down")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let rows = tx.outbox().all().await.unwrap();
    assert_eq!(rows[1].attempts, 1);
    assert_eq!(rows[1].last_error.as_deref(), Some("relay down"));
    tx.outbox().mark_done(rows[0].id).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.outbox().next(&[]).await.unwrap().unwrap().key, "k2");

    // A row is found by its key whether or not it is done, and only in its community.
    let done = tx.outbox().by_key("k1").await.unwrap().unwrap();
    assert!(done.done_at.is_some());
    assert_eq!(done.posting, status("first"));
    assert!(tx
        .outbox()
        .by_key("k2")
        .await
        .unwrap()
        .unwrap()
        .done_at
        .is_none());
    assert!(tx.outbox().by_key("missing").await.unwrap().is_none());
    drop(tx);
    let mut other = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(other.outbox().by_key("k1").await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_second_worker_skips_a_row_the_first_holds() {
    let (store, _guard, _relay) = store("hive_outbox_lock").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.outbox()
        .enqueue("k1", Uuid::from_u128(1), &status("first"))
        .await
        .unwrap();
    tx.outbox()
        .enqueue("k2", Uuid::from_u128(1), &status("second"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut first = store.begin(community).await.unwrap();
    assert_eq!(first.outbox().next(&[]).await.unwrap().unwrap().key, "k1");
    // The first worker holds k1, so the second takes k2 instead of waiting or posting k1 twice.
    let mut second = store.begin(community).await.unwrap();
    assert_eq!(second.outbox().next(&[]).await.unwrap().unwrap().key, "k2");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn settings_remove_and_list_stay_in_their_scope() {
    let (store, _guard, _relay) = store("hive_settings_rl").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.settings()
        .set("engine", "", "a", serde_json::json!(1))
        .await
        .unwrap();
    tx.settings()
        .set("engine", "", "b", serde_json::json!("x"))
        .await
        .unwrap();
    tx.settings()
        .set("workflow", "demo", "a", serde_json::json!(2))
        .await
        .unwrap();
    let listed = tx.settings().list("engine", "").await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed["b"], serde_json::json!("x"));
    tx.settings().remove("engine", "", "a").await.unwrap();
    // Removing what is not set is not an error.
    tx.settings().remove("engine", "", "a").await.unwrap();
    assert_eq!(tx.settings().list("engine", "").await.unwrap().len(), 1);
    assert_eq!(
        tx.settings().list("workflow", "demo").await.unwrap().len(),
        1
    );
}
