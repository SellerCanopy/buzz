use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::ports::store::{CommunityId, Store, StoreError};
use buzz_hive_core::resolve::resolve_actor;
use buzz_hive_core::resolve::Actor;
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

/// Stores `level` for `pubkey`, committed, so `resolve_actor` reads it.
async fn store_level(store: &MemStore, pubkey: &str, level: Level, kind: ActorKind) {
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(pubkey, level, kind, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn resolve(store: &MemStore, dir: &FakeDirectory, pubkey: &str) -> Actor {
    resolve_actor(store, dir, community(), pubkey)
        .await
        .unwrap()
}

#[tokio::test]
async fn owner_gets_admin_member_read_stranger_none() {
    let store = MemStore::new();
    let dir = directory();
    for (pubkey, want, member) in [
        (OWNER, Level::Admin, true),
        (MEMBER, Level::Read, true),
        (STRANGER, Level::None, false),
    ] {
        let actor = resolve(&store, &dir, pubkey).await;
        assert_eq!(actor.level, want, "{pubkey}");
        assert_eq!(actor.member, member, "{pubkey}");
        assert_eq!(actor.kind, ActorKind::Person);
        assert_eq!(actor.owner, None);
    }
}

#[tokio::test]
async fn agent_without_a_stored_level_starts_at_none() {
    let actor = resolve(&MemStore::new(), &directory(), AGENT).await;
    assert_eq!(actor.kind, ActorKind::Agent);
    assert_eq!(actor.level, Level::None);
    assert_eq!(actor.owner.as_deref(), Some(MEMBER));
    assert!(actor.member, "an agent of a member is a member");
}

#[tokio::test]
async fn agent_stored_run_under_a_read_owner_resolves_to_read() {
    let store = MemStore::new();
    store_level(&store, AGENT, Level::Run, ActorKind::Agent).await;
    let actor = resolve(&store, &directory(), AGENT).await;
    assert_eq!(actor.level, Level::Read);
}

#[tokio::test]
async fn a_stored_person_level_overrides_the_relay_role() {
    let store = MemStore::new();
    store_level(&store, MEMBER, Level::Run, ActorKind::Person).await;
    let actor = resolve(&store, &directory(), MEMBER).await;
    assert_eq!(actor.level, Level::Run);
}

#[tokio::test]
async fn a_person_removed_from_the_community_loses_a_stored_level() {
    let store = MemStore::new();
    store_level(&store, STRANGER, Level::Admin, ActorKind::Person).await;
    let actor = resolve(&store, &directory(), STRANGER).await;
    assert_eq!(actor.level, Level::None);
    assert!(!actor.member);
}

#[tokio::test]
async fn a_deactivated_agent_loses_a_stored_level() {
    let store = MemStore::new();
    store_level(&store, AGENT, Level::Run, ActorKind::Agent).await;
    store_level(&store, OWNER, Level::Admin, ActorKind::Person).await;
    let dir = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_agent(AGENT, OWNER)
        .with_deactivated(AGENT);
    let actor = resolve(&store, &dir, AGENT).await;
    assert_eq!(actor.kind, ActorKind::Agent, "it is still an agent");
    assert_eq!(actor.level, Level::None);
    assert!(!actor.member);
    // Its owner is unaffected.
    assert_eq!(resolve(&store, &dir, OWNER).await.level, Level::Admin);
}

#[tokio::test]
async fn an_agent_whose_owner_left_the_community_is_not_a_member() {
    let store = MemStore::new();
    store_level(&store, AGENT, Level::Run, ActorKind::Agent).await;
    let dir = FakeDirectory::new().with_agent(AGENT, STRANGER);
    let actor = resolve(&store, &dir, AGENT).await;
    assert_eq!(actor.level, Level::None);
    assert!(!actor.member);
}

#[tokio::test]
async fn an_owner_chain_that_loops_is_refused() {
    let dir = FakeDirectory::new()
        .with_agent("a1", "a2")
        .with_agent("a2", "a1");
    let result = resolve_actor(&MemStore::new(), &dir, community(), "a1").await;
    assert!(matches!(result, Err(StoreError::Conflict(_))));
}

#[tokio::test]
async fn an_owner_chain_that_is_too_long_is_refused() {
    let mut dir = FakeDirectory::new();
    for n in 0..10 {
        dir = dir.with_agent(&format!("k{n}"), &format!("k{}", n + 1));
    }
    let result = resolve_actor(&MemStore::new(), &dir, community(), "k0").await;
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
