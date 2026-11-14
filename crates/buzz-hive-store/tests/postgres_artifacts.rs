//! Artifacts keep their rules in Postgres: versions count from 1, a version holds a body or a
//! file pointer and never both or neither, and a card's removal takes its artifacts with it.

mod support;

use buzz_hive_core::domain::artifact::{ArtifactContent, ArtifactKind};
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::ports::store::{ArtifactSave, CommunityId, Store, StoreError};
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

fn text(body: &str, stage: &str, visit: u32) -> ArtifactSave {
    ArtifactSave {
        content: ArtifactContent::Text(body.into()),
        stage: stage.into(),
        visit,
        created_by: "aa".into(),
    }
}

fn file(stage: &str, visit: u32) -> ArtifactSave {
    ArtifactSave {
        content: ArtifactContent::File {
            sha256: "ab".repeat(32),
            size: 12,
            media_type: "image/png".into(),
            media_ref: "https://media.example.org/x.png".into(),
        },
        stage: stage.into(),
        visit,
        created_by: "aa".into(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn versions_append_and_the_latest_is_read_back() {
    let (store, _guard, _relay) = store("hive_artifacts").await;
    let community = CommunityId::from_u128(1);
    let id = Uuid::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let one = tx
        .artifacts()
        .save(id, "plan", true, &text("a", "draft", 1))
        .await
        .unwrap();
    let two = tx
        .artifacts()
        .save(id, "plan", false, &text("b", "draft", 2))
        .await
        .unwrap();
    assert_eq!((one.version, two.version), (1, 2));
    tx.artifacts()
        .save(id, "logo", true, &file("draft", 1))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let list = tx.artifacts().list(id).await.unwrap();
    assert_eq!(
        list.iter()
            .map(|a| (a.name.as_str(), a.kind, a.latest, a.handoff))
            .collect::<Vec<_>>(),
        vec![
            ("logo", ArtifactKind::File, 1, true),
            ("plan", ArtifactKind::Text, 2, true)
        ],
        "a later save keeps the handoff flag the first save set"
    );
    let latest = tx
        .artifacts()
        .version(id, "plan", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.content, ArtifactContent::Text("b".into()));
    let first = tx
        .artifacts()
        .version(id, "plan", Some(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.content, ArtifactContent::Text("a".into()));
    assert_eq!(
        tx.artifacts().version(id, "plan", Some(3)).await.unwrap(),
        None
    );
    assert_eq!(tx.artifacts().versions(id, "plan").await.unwrap().len(), 2);
    let pointer = tx
        .artifacts()
        .version(id, "logo", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pointer.content, file("draft", 1).content);
    assert_eq!(
        tx.artifacts().saved_in(id, "draft", 1).await.unwrap(),
        vec!["logo", "plan"]
    );
    assert_eq!(
        tx.artifacts().saved_in(id, "draft", 2).await.unwrap(),
        vec!["plan"]
    );
    assert!(tx
        .artifacts()
        .saved_in(id, "review", 1)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_artifact_keeps_its_kind_and_handoff_can_be_set() {
    let (store, _guard, _relay) = store("hive_artifacts_kind").await;
    let community = CommunityId::from_u128(1);
    let id = Uuid::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.artifacts()
        .save(id, "plan", true, &text("a", "draft", 1))
        .await
        .unwrap();
    let refused = tx
        .artifacts()
        .save(id, "plan", true, &file("draft", 1))
        .await;
    assert!(
        matches!(refused, Err(StoreError::Conflict(_))),
        "{refused:?}"
    );
    drop(tx);

    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.artifacts()
        .save(id, "plan", true, &text("a", "draft", 1))
        .await
        .unwrap();
    tx.artifacts().set_handoff(id, "plan", false).await.unwrap();
    assert!(!tx.artifacts().list(id).await.unwrap()[0].handoff);
    let missing = tx.artifacts().set_handoff(id, "nope", false).await;
    assert!(
        matches!(missing, Err(StoreError::NotFound(_))),
        "{missing:?}"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_version_with_both_or_neither_body_and_hash_fails_the_constraint() {
    let (store, _guard, relay) = store("hive_artifacts_check").await;
    let community = CommunityId::from_u128(1);
    let id = Uuid::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.artifacts()
        .save(id, "plan", true, &text("a", "draft", 1))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    for (body, sha) in [(Some("x"), Some("ab")), (None, None)] {
        let result = sqlx::query(
            "INSERT INTO hive.artifact_versions \
             (community_id, card_id, name, version, body, sha256, stage, created_by) \
             VALUES ($1, $2, 'plan', 2, $3, $4, 'draft', 'aa')",
        )
        .bind(community)
        .bind(id)
        .bind(body)
        .bind(sha)
        .execute(&relay)
        .await;
        assert!(
            result.is_err(),
            "body {body:?} and sha256 {sha:?} were accepted"
        );
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn removing_a_card_removes_its_artifacts() {
    let (store, _guard, _relay) = store("hive_artifacts_cascade").await;
    let community = CommunityId::from_u128(1);
    let id = Uuid::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.artifacts()
        .save(id, "plan", true, &text("a", "draft", 1))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    tx.cards().delete(id).await.unwrap();
    assert!(tx.artifacts().list(id).await.unwrap().is_empty());
    assert!(tx
        .artifacts()
        .versions(id, "plan")
        .await
        .unwrap()
        .is_empty());
}
