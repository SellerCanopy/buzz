//! `RelayRead` finds agents by name and answers channel questions from the relay tables.

mod support;

use buzz_hive_core::access::RelayRole;
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
async fn people_are_the_relay_members_that_are_not_agents() {
    let (pool, _guard) = support::scratch_relay_db("hive_dir_people").await;
    let (community, _, _) = seed(&pool).await;
    for (key, role) in [
        (hex::encode(OWNER), "owner"),
        (hex::encode(BUILDER), "member"),
        (hex::encode([9u8; 32]), "admin"),
    ] {
        sqlx::query(
            "INSERT INTO public.relay_members (community_id, pubkey, role) VALUES ($1, $2, $3)",
        )
        .bind(community)
        .bind(key)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
    }
    let directory = RelayRead::new(pool);

    let people = directory.people(community).await.unwrap();
    let found: Vec<_> = people
        .iter()
        .map(|p| (p.pubkey.clone(), p.name.clone(), p.role))
        .collect();
    assert_eq!(
        found,
        [
            // Named people come first, by name; a member with no profile row follows.
            (
                hex::encode(OWNER),
                Some("Owner".to_owned()),
                RelayRole::Owner
            ),
            (hex::encode([9u8; 32]), None, RelayRole::Admin),
        ],
        "an agent that is a relay member is not a person"
    );
    assert!(directory
        .people(Uuid::from_u128(5))
        .await
        .unwrap()
        .is_empty());
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

/// Writes `event` to the relay's events table, in `community`.
async fn insert_event(
    pool: &sqlx::PgPool,
    community: Uuid,
    event: &buzz_hive_core::ports::store::RelayEvent,
) {
    sqlx::query(
        "INSERT INTO public.events \
         (community_id, id, pubkey, created_at, kind, tags, content, sig, channel_id, deleted_at) \
         VALUES ($1, $2, $3, to_timestamp($4), $5, $6, 'hello', $7, $8, CASE WHEN $9 THEN now() END)",
    )
    .bind(community)
    .bind(hex::decode(&event.id).unwrap())
    .bind(hex::decode(&event.author).unwrap())
    .bind(event.created_at as f64)
    .bind(event.kind)
    .bind(serde_json::to_value(&event.tags).unwrap())
    .bind(vec![0u8; 64])
    .bind(event.channel_id)
    .bind(event.deleted)
    .execute(pool)
    .await
    .unwrap();
}

async fn community_named(pool: &sqlx::PgPool, host: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO public.communities (host) VALUES ($1) RETURNING id")
        .bind(host)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_relay_follows_the_shared_newer_mention_scenarios() {
    use buzz_hive_core::testing::directory_scenarios::{agent, newer_mention_scenarios};
    let (pool, _guard) = support::scratch_relay_db("hive_dir_newer").await;
    let directory = RelayRead::new(pool.clone());
    for (n, case) in newer_mention_scenarios().into_iter().enumerate() {
        // Each scenario has its own community, so the messages of one cannot reach another.
        let community = community_named(&pool, &format!("scenario{n}.example.org")).await;
        for event in &case.events {
            insert_event(&pool, community, event).await;
        }
        let got = directory
            .newer_mention(community, case.subject(), &case.root, &agent())
            .await
            .unwrap();
        assert_eq!(got, case.expect, "{}", case.name);
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_message_is_read_by_id_inside_its_own_community_with_its_deleted_flag() {
    use buzz_hive_core::testing::directory_scenarios::{agent, id, root, CHANNEL, START};
    let (pool, _guard) = support::scratch_relay_db("hive_dir_event").await;
    let directory = RelayRead::new(pool.clone());
    let (one, two) = (
        community_named(&pool, "one.example.org").await,
        community_named(&pool, "two.example.org").await,
    );
    let mut message = buzz_hive_core::ports::store::RelayEvent {
        id: id(1),
        author: id(0xb2),
        kind: 9,
        tags: vec![
            vec!["p".into(), agent()],
            vec!["e".into(), root(), String::new(), "root".into()],
        ],
        created_at: START,
        channel_id: Some(CHANNEL),
        deleted: false,
    };
    insert_event(&pool, one, &message).await;
    let found = directory.event_by_id(one, &id(1)).await.unwrap().unwrap();
    assert_eq!(found, message);
    // Another community has no such message, even though the id exists in the table.
    assert_eq!(directory.event_by_id(two, &id(1)).await.unwrap(), None);
    // An id that is not hex is no message, rather than an error.
    assert_eq!(directory.event_by_id(one, "not hex").await.unwrap(), None);

    message.id = id(2);
    message.deleted = true;
    insert_event(&pool, one, &message).await;
    let found = directory.event_by_id(one, &id(2)).await.unwrap().unwrap();
    assert!(found.deleted);
}
