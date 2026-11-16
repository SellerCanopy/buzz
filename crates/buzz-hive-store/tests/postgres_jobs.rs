//! The assistant's jobs in Postgres: one running job per thread, a deadline on the database's
//! own clock, a job that ends once, and a proposal that is decided once.

mod support;

use std::time::Duration;

use buzz_hive_core::ports::store::{CommunityId, Job, JobState, Store, StoreError};
use buzz_hive_store::pg::PgStore;
use serde_json::json;
use uuid::Uuid;

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn job(n: u128, root: Option<&str>) -> Job {
    Job {
        id: Uuid::from_u128(n),
        kind: "chat".into(),
        requester: "aa".into(),
        state: JobState::Running,
        channel: Uuid::from_u128(77),
        root_event_id: root.map(str::to_owned),
        agent: "dd".into(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_job_is_stored_read_back_and_ends_once() {
    let (store, _guard, _relay) = store("hive_jobs_end").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.jobs().insert(&job(1, None), None).await.unwrap();
    tx.jobs()
        .set_root(Uuid::from_u128(1), "root1")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let held = tx.jobs().get_for_update(Uuid::from_u128(1)).await.unwrap();
    assert_eq!(held, Some(job(1, Some("root1"))));
    tx.jobs()
        .finish(
            Uuid::from_u128(1),
            JobState::Done,
            Some(json!({"message": "hi"})),
        )
        .await
        .unwrap();
    // A second end changes nothing.
    tx.jobs()
        .finish(Uuid::from_u128(1), JobState::Failed, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let ended = tx.jobs().get_for_update(Uuid::from_u128(1)).await.unwrap();
    assert_eq!(ended.unwrap().state, JobState::Done);
    let newest = tx.jobs().latest_in_thread("root1").await.unwrap().unwrap();
    assert_eq!(newest.id, Uuid::from_u128(1));
    assert!(tx.jobs().latest_in_thread("nope").await.unwrap().is_none());
    assert!(tx
        .jobs()
        .get_for_update(Uuid::from_u128(9))
        .await
        .unwrap()
        .is_none());
    drop(tx);

    // Another community sees nothing.
    let mut tx = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(tx
        .jobs()
        .get_for_update(Uuid::from_u128(1))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn one_job_runs_in_a_thread_until_it_ends() {
    let (store, _guard, _relay) = store("hive_jobs_thread").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.jobs().insert(&job(1, Some("root")), None).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let busy = tx.jobs().insert(&job(2, Some("root")), None).await;
    assert!(matches!(busy, Err(StoreError::Conflict(_))), "{busy:?}");
    drop(tx);

    // Jobs with no thread yet do not collide, and an ended job frees its thread.
    let mut tx = store.begin(community).await.unwrap();
    tx.jobs().insert(&job(3, None), None).await.unwrap();
    tx.jobs().insert(&job(4, None), None).await.unwrap();
    tx.jobs()
        .finish(Uuid::from_u128(1), JobState::Cancelled, None)
        .await
        .unwrap();
    tx.jobs().insert(&job(2, Some("root")), None).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let newest = tx.jobs().latest_in_thread("root").await.unwrap().unwrap();
    assert_eq!(newest.id, Uuid::from_u128(2));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_job_past_its_deadline_is_overdue_and_the_next_deadline_is_counted_down() {
    let (store, _guard, _relay) = store("hive_jobs_deadline").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.jobs().next_in().await.unwrap(), None);
    tx.jobs()
        .insert(&job(1, Some("a")), Some(Duration::from_millis(100)))
        .await
        .unwrap();
    tx.jobs()
        .insert(&job(2, Some("b")), Some(Duration::from_secs(3600)))
        .await
        .unwrap();
    tx.jobs().insert(&job(3, Some("c")), None).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let next = tx.jobs().next_in().await.unwrap().unwrap();
    assert!(next <= Duration::from_millis(100), "{next:?}");
    drop(tx);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut tx = store.begin(community).await.unwrap();
    let overdue = tx.jobs().overdue(10).await.unwrap();
    assert_eq!(overdue.len(), 1);
    assert_eq!(overdue[0].id, Uuid::from_u128(1));
    assert_eq!(tx.jobs().next_in().await.unwrap(), Some(Duration::ZERO));
    tx.jobs()
        .finish(Uuid::from_u128(1), JobState::Failed, None)
        .await
        .unwrap();
    assert!(tx.jobs().overdue(10).await.unwrap().is_empty());
    let next = tx.jobs().next_in().await.unwrap().unwrap();
    assert!(next > Duration::from_secs(3000), "{next:?}");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_proposal_is_decided_once_and_stays_readable() {
    let (store, _guard, _relay) = store("hive_jobs_proposals").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    let first = tx
        .proposals()
        .insert("workflow", json!({"name": "a"}), "aa")
        .await
        .unwrap();
    let second = tx
        .proposals()
        .insert("ticket", json!({"title": "t"}), "bb")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let all = tx.proposals().open_all().await.unwrap();
    assert_eq!(
        all.iter().map(|p| p.id).collect::<Vec<_>>(),
        [first, second]
    );
    assert!(tx.proposals().resolve(first).await.unwrap());
    assert!(!tx.proposals().resolve(first).await.unwrap());
    assert!(!tx.proposals().resolve(9999).await.unwrap());
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let open = tx.proposals().open_all().await.unwrap();
    assert_eq!(open.iter().map(|p| p.id).collect::<Vec<_>>(), [second]);
    assert!(tx.proposals().open("workflow").await.unwrap().is_empty());
    let kept = tx.proposals().get(first).await.unwrap().unwrap();
    assert_eq!(kept.body["name"], "a");
    assert!(tx.proposals().get(9999).await.unwrap().is_none());
    drop(tx);

    let mut tx = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(tx.proposals().get(first).await.unwrap().is_none());
    assert!(!tx.proposals().resolve(first).await.unwrap());
}
