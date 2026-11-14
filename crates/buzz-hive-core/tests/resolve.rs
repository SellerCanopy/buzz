use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::ports::store::{CommunityId, Store, StoreError};
use buzz_hive_core::resolve::resolve_actor;
use buzz_hive_core::testing::{FakeDirectory, MemStore};

const OWNER: &str = "aa";
const MEMBER: &str = "bb";
const STRANGER: &str = "cc";
const AGENT: &str = "dd";

fn community() -> CommunityId {
    CommunityId::from_u128(1)
}

fn directory() -> FakeDirectory {
    FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, MEMBER)
}

#[tokio::test]
async fn owner_gets_admin_member_read_stranger_none() {
    let store = MemStore::new();
    let dir = directory();
    let mut tx = store.begin(community()).await.unwrap();
    for (pubkey, want) in [
        (OWNER, Level::Admin),
        (MEMBER, Level::Read),
        (STRANGER, Level::None),
    ] {
        let actor = resolve_actor(&mut *tx, &dir, community(), pubkey)
            .await
            .unwrap();
        assert_eq!(actor.level, want, "{pubkey}");
        assert_eq!(actor.kind, ActorKind::Person);
        assert_eq!(actor.owner, None);
    }
}

#[tokio::test]
async fn agent_without_a_stored_level_starts_at_none() {
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    let actor = resolve_actor(&mut *tx, &directory(), community(), AGENT)
        .await
        .unwrap();
    assert_eq!(actor.kind, ActorKind::Agent);
    assert_eq!(actor.level, Level::None);
    assert_eq!(actor.owner.as_deref(), Some(MEMBER));
}

#[tokio::test]
async fn agent_stored_run_under_a_read_owner_resolves_to_read() {
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(AGENT, Level::Run, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    let actor = resolve_actor(&mut *tx, &directory(), community(), AGENT)
        .await
        .unwrap();
    assert_eq!(actor.level, Level::Read);
}

#[tokio::test]
async fn a_stored_person_level_overrides_the_relay_role() {
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(MEMBER, Level::Run, ActorKind::Person, OWNER)
        .await
        .unwrap();
    let actor = resolve_actor(&mut *tx, &directory(), community(), MEMBER)
        .await
        .unwrap();
    assert_eq!(actor.level, Level::Run);
}

#[tokio::test]
async fn an_owner_chain_that_loops_is_refused() {
    let dir = FakeDirectory::new()
        .with_agent("a1", "a2")
        .with_agent("a2", "a1");
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    let result = resolve_actor(&mut *tx, &dir, community(), "a1").await;
    assert!(matches!(result, Err(StoreError::Conflict(_))));
}

#[tokio::test]
async fn an_owner_chain_that_is_too_long_is_refused() {
    let mut dir = FakeDirectory::new();
    for n in 0..10 {
        dir = dir.with_agent(&format!("k{n}"), &format!("k{}", n + 1));
    }
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    let result = resolve_actor(&mut *tx, &dir, community(), "k0").await;
    assert!(matches!(result, Err(StoreError::Conflict(_))));
}

#[tokio::test]
async fn a_dropped_transaction_rolls_back() {
    let store = MemStore::new();
    {
        let mut tx = store.begin(community()).await.unwrap();
        tx.access()
            .set_level(MEMBER, Level::Admin, ActorKind::Person, OWNER)
            .await
            .unwrap();
    }
    let mut tx = store.begin(community()).await.unwrap();
    assert_eq!(tx.access().level(MEMBER).await.unwrap(), None);
    tx.access()
        .set_level(MEMBER, Level::Run, ActorKind::Person, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = store.begin(community()).await.unwrap();
    assert_eq!(
        tx.access().level(MEMBER).await.unwrap(),
        Some((Level::Run, ActorKind::Person))
    );
}
