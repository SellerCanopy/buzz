//! Secret names in Postgres: set and replaced, granted and revoked, and no column for a value.

mod support;

use buzz_hive_core::domain::secret::SecretNames;
use buzz_hive_core::ports::store::{CommunityId, Store, StoreError};
use buzz_hive_store::pg::PgStore;

async fn store() -> (PgStore, support::Scratch, sqlx::PgPool, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db("hive_secrets").await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool.clone()), guard, relay_pool, pool)
}

fn names(own: &[&str], team: &[&str]) -> SecretNames {
    SecretNames {
        own: own.iter().map(|s| (*s).to_owned()).collect(),
        team: team.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn names_are_set_replaced_granted_and_revoked() {
    let (store, _guard, _relay, _pool) = store().await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.agent_tools().seed("coder", &[]).await.unwrap();
    tx.agent_tools().seed("researcher", &[]).await.unwrap();

    tx.secrets().set_team("GH_TOKEN", "aa").await.unwrap();
    tx.secrets().set_team("GH_TOKEN", "bb").await.unwrap();
    tx.secrets()
        .set_agent("coder", "DOCS_TOKEN", "aa")
        .await
        .unwrap();
    let team = tx.secrets().team().await.unwrap();
    assert_eq!(team.len(), 1);
    assert_eq!(
        (team[0].var.as_str(), team[0].updated_by.as_str()),
        ("GH_TOKEN", "bb")
    );
    let own = tx.secrets().agent_secrets().await.unwrap();
    assert_eq!(
        (own[0].agent.as_str(), own[0].var.as_str()),
        ("coder", "DOCS_TOKEN")
    );

    assert!(tx.secrets().grant("coder", "GH_TOKEN").await.unwrap());
    assert!(!tx.secrets().grant("coder", "GH_TOKEN").await.unwrap());
    assert_eq!(
        tx.secrets().grants().await.unwrap(),
        [("coder".to_owned(), "GH_TOKEN".to_owned())]
    );
    assert_eq!(
        tx.secrets().names_of("coder").await.unwrap(),
        names(&["DOCS_TOKEN"], &["GH_TOKEN"])
    );
    // An agent with no grant has no team secret.
    assert_eq!(
        tx.secrets().names_of("researcher").await.unwrap(),
        names(&[], &[])
    );

    assert!(matches!(
        tx.secrets().grant("coder", "NOT_SET").await,
        Err(StoreError::NotFound(_))
    ));
    assert!(tx.secrets().revoke("coder", "GH_TOKEN").await.unwrap());
    assert!(!tx.secrets().revoke("coder", "GH_TOKEN").await.unwrap());
    assert!(tx.secrets().grants().await.unwrap().is_empty());
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn another_community_sees_none_of_it() {
    let (store, _guard, _relay, _pool) = store().await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    tx.agent_tools().seed("coder", &[]).await.unwrap();
    tx.secrets().set_team("GH_TOKEN", "aa").await.unwrap();
    tx.secrets().grant("coder", "GH_TOKEN").await.unwrap();
    tx.commit().await.unwrap();

    let mut other = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(other.secrets().team().await.unwrap().is_empty());
    assert!(other.secrets().grants().await.unwrap().is_empty());
    assert_eq!(
        other.secrets().names_of("coder").await.unwrap(),
        names(&[], &[])
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_secret_for_an_agent_that_is_not_known_is_refused_by_the_database() {
    let (store, _guard, _relay, _pool) = store().await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    assert!(tx.secrets().set_agent("ghost", "X", "aa").await.is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn no_table_has_a_column_that_could_hold_a_value() {
    let (_store, _guard, _relay, pool) = store().await;
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns \
         WHERE table_schema = 'hive' \
           AND table_name IN ('team_secrets', 'agent_secrets', 'agent_secret_grants') \
         ORDER BY table_name, ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let got: Vec<String> = columns.iter().map(|(t, c)| format!("{t}.{c}")).collect();
    assert_eq!(
        got,
        [
            "agent_secret_grants.community_id",
            "agent_secret_grants.agent",
            "agent_secret_grants.var",
            "agent_secrets.community_id",
            "agent_secrets.agent",
            "agent_secrets.var",
            "agent_secrets.updated_at",
            "agent_secrets.updated_by",
            "team_secrets.community_id",
            "team_secrets.var",
            "team_secrets.updated_at",
            "team_secrets.updated_by",
        ]
    );
}
