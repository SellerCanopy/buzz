//! Two admins changing levels at the same moment cannot leave a community with no admin.

mod support;

use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::domain::events::Actor;
use buzz_hive_core::orchestrator::access::{list, set_level};
use buzz_hive_core::orchestrator::{Ctx, Env, OpError};
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::FakeDirectory;
use buzz_hive_core::workflow::load::Catalog;
use buzz_hive_store::pg::PgStore;

const FIRST: &str = "a1";
const SECOND: &str = "b2";

fn ctx(pubkey: &str, community: CommunityId) -> Ctx {
    Ctx {
        community,
        actor: Actor {
            pubkey: pubkey.into(),
            kind: ActorKind::Person,
        },
        requester: None,
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn two_admins_lowering_each_other_leave_one_admin() {
    let (_relay, guard) = support::scratch_relay_db("hive_access").await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    let store = PgStore::new(pool);
    let directory = FakeDirectory::new()
        .with_role(FIRST, RelayRole::Member)
        .with_role(SECOND, RelayRole::Member);
    let catalog = Catalog::default();
    let env = Env {
        store: &store,
        directory: &directory,
        catalog: &catalog,
        hive: "ee",
    };
    let community = CommunityId::from_u128(1);

    for round in 0..8 {
        let mut tx = store.begin(community).await.unwrap();
        for pubkey in [FIRST, SECOND] {
            tx.access()
                .set_level(pubkey, Level::Admin, ActorKind::Person, FIRST)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();

        let first = ctx(FIRST, community);
        let second = ctx(SECOND, community);
        let (a, b) = tokio::join!(
            set_level(&env, &first, SECOND, Level::Read),
            set_level(&env, &second, FIRST, Level::Read),
        );
        let outcomes = [a.map(|_| ()), b.map(|_| ())];
        let won = outcomes.iter().filter(|o| o.is_ok()).count();
        assert_eq!(won, 1, "round {round}: exactly one change goes through");
        let refused = outcomes.iter().find_map(|o| o.as_ref().err()).unwrap();
        assert!(
            matches!(refused, OpError::PermissionDenied(_)),
            "round {round}: the loser was lowered first, got {refused:?}"
        );

        let admins = list(&env, &first)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == ActorKind::Person && e.level == Level::Admin)
            .count();
        assert_eq!(admins, 1, "round {round}: one admin is left");
    }
}
