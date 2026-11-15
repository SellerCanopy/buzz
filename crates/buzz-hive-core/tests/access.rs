//! The access page's list and an admin changing a level.

mod common;

use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::orchestrator::access::{list, set_level};
use buzz_hive_core::orchestrator::{Ctx, OpError};
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::resolve::resolve_actor;
use buzz_hive_core::testing::FakeDirectory;
use common::{ctx_for, Fixture, COMMUNITY};

const ADMIN: &str = "a1";
const MEMBER: &str = "b2";
const OTHER: &str = "c3";
const AGENT: &str = "d4";
const SWITCHED_OFF: &str = "e5";
const STRANGER: &str = "f6";

fn fixture() -> Fixture {
    Fixture::new().with_directory(
        FakeDirectory::new()
            .with_role(ADMIN, RelayRole::Owner)
            .with_role(MEMBER, RelayRole::Member)
            .with_role(OTHER, RelayRole::Member)
            .with_name(ADMIN, "Ada")
            .with_name(MEMBER, "Ben")
            .with_name(OTHER, "Cy")
            .with_named_agent("builder", AGENT, MEMBER)
            .with_deactivated(SWITCHED_OFF)
            .with_agent(SWITCHED_OFF, MEMBER),
    )
}

fn admin() -> Ctx {
    ctx_for(ADMIN, ActorKind::Person)
}

async fn level_of(f: &Fixture, pubkey: &str) -> Level {
    resolve_actor(&f.store, &f.directory, COMMUNITY, pubkey)
        .await
        .unwrap()
        .level
}

#[tokio::test]
async fn the_list_shows_what_each_pubkey_acts_at() {
    let f = fixture();
    set_level(&f.env(), &admin(), MEMBER, Level::Run)
        .await
        .unwrap();
    // The agent is stored above its owner, as it could be after the owner was lowered.
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.access()
        .set_level(AGENT, Level::Admin, ActorKind::Agent, ADMIN)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let entries = list(&f.env(), &admin()).await.unwrap();
    let keys: Vec<_> = entries.iter().map(|e| e.pubkey.as_str()).collect();
    assert_eq!(
        keys,
        [ADMIN, MEMBER, OTHER, AGENT],
        "people by name, then agents; a switched-off agent is absent"
    );
    for entry in &entries {
        assert_eq!(
            entry.level,
            level_of(&f, &entry.pubkey).await,
            "{}",
            entry.pubkey
        );
    }
    let agent = entries.iter().find(|e| e.pubkey == AGENT).unwrap();
    assert_eq!(agent.level, Level::Run, "capped at its owner's level");
    assert_eq!(agent.owner.as_deref(), Some(MEMBER));
    assert_eq!(agent.kind, ActorKind::Agent);
    assert_eq!(agent.name, "builder");
}

#[tokio::test]
async fn an_agent_never_shows_above_its_owner_after_the_owner_is_lowered() {
    let f = fixture();
    set_level(&f.env(), &admin(), AGENT, Level::Read)
        .await
        .unwrap();
    set_level(&f.env(), &admin(), MEMBER, Level::None)
        .await
        .unwrap();
    let entries = list(&f.env(), &admin()).await.unwrap();
    let agent = entries.iter().find(|e| e.pubkey == AGENT).unwrap();
    assert_eq!(agent.level, Level::None);
    assert_eq!(level_of(&f, AGENT).await, Level::None);
}

#[tokio::test]
async fn setting_a_level_stores_it_and_appends_an_event() {
    let f = fixture();
    let (entry, events) = set_level(&f.env(), &admin(), MEMBER, Level::Run)
        .await
        .unwrap();
    assert_eq!(entry.level, Level::Run);
    assert_eq!(entry.name, "Ben");
    assert_eq!(level_of(&f, MEMBER).await, Level::Run);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "access.changed");
    assert_eq!(events[0].actor.pubkey, ADMIN);
    assert_eq!(
        events[0].payload,
        serde_json::json!({
            "pubkey": MEMBER,
            "kind": "person",
            "level": "run",
            "previous": "read",
        })
    );
}

#[tokio::test]
async fn an_agent_may_not_be_set_above_its_owner() {
    let f = fixture();
    let error = set_level(&f.env(), &admin(), AGENT, Level::Admin)
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::FailedPrecondition(_)), "{error:?}");
    assert_eq!(
        error.to_string(),
        "agent builder is capped at read by its owner Ben"
    );
    assert_eq!(level_of(&f, AGENT).await, Level::None, "nothing was stored");
    assert!(
        f.store.committed_kinds().is_empty(),
        "a refused change leaves no event"
    );

    set_level(&f.env(), &admin(), MEMBER, Level::Admin)
        .await
        .unwrap();
    set_level(&f.env(), &admin(), AGENT, Level::Admin)
        .await
        .unwrap();
    assert_eq!(level_of(&f, AGENT).await, Level::Admin);
}

#[tokio::test]
async fn an_admin_may_not_lower_their_own_level() {
    let f = fixture();
    let error = set_level(&f.env(), &admin(), ADMIN, Level::Run)
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::FailedPrecondition(_)), "{error:?}");
    assert_eq!(level_of(&f, ADMIN).await, Level::Admin);
    // Keeping Admin is not a lowering.
    set_level(&f.env(), &admin(), ADMIN, Level::Admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_stranger_and_a_switched_off_agent_have_no_level_to_set() {
    let f = fixture();
    let error = set_level(&f.env(), &admin(), STRANGER, Level::Read)
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::NotFound(_)), "{error:?}");
    let error = set_level(&f.env(), &admin(), SWITCHED_OFF, Level::Read)
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::FailedPrecondition(_)), "{error:?}");
}
