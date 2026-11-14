//! `RelayRead` finds agents by name and answers channel questions from the relay tables.

mod support;

use buzz_hive_core::ports::store::Directory;
use buzz_hive_store::relay_read::RelayRead;
use uuid::Uuid;

const OWNER: [u8; 32] = [1; 32];
const BUILDER: [u8; 32] = [2; 32];
const REVIEWER: [u8; 32] = [3; 32];
const OFFLINE: [u8; 32] = [4; 32];

async fn seed(pool: &sqlx::PgPool) -> (Uuid, Uuid, Uuid) {
    let community: Uuid = sqlx::query_scalar(
        "INSERT INTO public.communities (host) VALUES ('hive.example.org') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO public.users (community_id, pubkey, display_name) VALUES ($1, $2, 'Owner')",
    )
    .bind(community)
    .bind(OWNER.to_vec())
    .execute(pool)
    .await
    .unwrap();
    for (key, name, deactivated) in [
        (BUILDER, "Builder", false),
        (REVIEWER, "Reviewer", false),
        (OFFLINE, "Builder", true),
    ] {
        sqlx::query(
            "INSERT INTO public.users \
             (community_id, pubkey, display_name, agent_owner_pubkey, deactivated_at) \
             VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN now() END)",
        )
        .bind(community)
        .bind(key.to_vec())
        .bind(name)
        .bind(OWNER.to_vec())
        .bind(deactivated)
        .execute(pool)
        .await
        .unwrap();
    }
    let open = Uuid::from_u128(10);
    let archived = Uuid::from_u128(11);
    for (id, name, archived_at) in [(open, "updates", false), (archived, "old", true)] {
        sqlx::query(
            "INSERT INTO public.channels (id, community_id, name, created_by, archived_at) \
             VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN now() END)",
        )
        .bind(id)
        .bind(community)
        .bind(name)
        .bind(OWNER.to_vec())
        .bind(archived_at)
        .execute(pool)
        .await
        .unwrap();
    }
    for (channel, key, removed) in [
        (open, BUILDER, false),
        (open, REVIEWER, true),
        (archived, BUILDER, false),
    ] {
        sqlx::query(
            "INSERT INTO public.channel_members (community_id, channel_id, pubkey, removed_at) \
             VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END)",
        )
        .bind(community)
        .bind(channel)
        .bind(key.to_vec())
        .bind(removed)
        .execute(pool)
        .await
        .unwrap();
    }
    (community, open, archived)
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn agents_are_found_by_name_without_regard_to_case() {
    let (pool, _guard) = support::scratch_relay_db("hive_dir_agents").await;
    let (community, _, _) = seed(&pool).await;
    let directory = RelayRead::new(pool);

    let found = directory.agents_named(community, "bUiLdEr").await.unwrap();
    assert_eq!(found.len(), 1, "a deactivated agent is not a candidate");
    assert_eq!(found[0].name, "Builder");
    assert_eq!(found[0].pubkey, hex::encode(BUILDER));
    assert_eq!(found[0].owner, Some(hex::encode(OWNER)));
    // A person is not an agent, whatever the display name.
    assert!(directory
        .agents_named(community, "Owner")
        .await
        .unwrap()
        .is_empty());
    assert!(directory
        .agents_named(Uuid::from_u128(5), "Builder")
        .await
        .unwrap()
        .is_empty());

    let all = directory.list_agents(community).await.unwrap();
    let names: Vec<_> = all.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["Builder", "Reviewer"]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_channel_counts_when_it_is_live_and_a_member_when_not_removed() {
    let (pool, _guard) = support::scratch_relay_db("hive_dir_channels").await;
    let (community, open, archived) = seed(&pool).await;
    let directory = RelayRead::new(pool);

    assert!(directory.channel_exists(community, open).await.unwrap());
    assert!(!directory.channel_exists(community, archived).await.unwrap());
    assert!(!directory
        .channel_exists(community, Uuid::from_u128(99))
        .await
        .unwrap());
    assert!(!directory
        .channel_exists(Uuid::from_u128(5), open)
        .await
        .unwrap());

    let builder = hex::encode(BUILDER);
    assert!(directory
        .member_of_channel(community, open, &builder)
        .await
        .unwrap());
    assert!(!directory
        .member_of_channel(community, open, &hex::encode(REVIEWER))
        .await
        .unwrap());
    assert!(!directory
        .member_of_channel(community, open, "not hex")
        .await
        .unwrap());
}
