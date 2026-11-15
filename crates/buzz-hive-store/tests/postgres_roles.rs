//! `deploy/aws/hive-roles.sql` gives the `buzz_hive` role its schema and read access to the
//! relay columns the service reads, and nothing more.

mod support;

use buzz_hive_core::ports::store::Directory;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

/// Whether a later message in the thread of `root` mentions `agent`, as `directory` reads it.
async fn newer_mention(
    directory: &buzz_hive_store::relay_read::RelayRead,
    community: uuid::Uuid,
    event: &buzz_hive_core::ports::store::RelayEvent,
    root: &str,
    agent: &str,
) -> bool {
    directory
        .newer_mention(community, event, root, agent)
        .await
        .expect("newer_mention as buzz_hive")
}

const PASSWORD: &str = "role-test-password";

fn roles_sql() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/aws/hive-roles.sql"
    );
    let sql = std::fs::read_to_string(path).expect("read hive-roles.sql");
    // psql fills :'hive_password' in. sqlx has no psql variables, so use a literal.
    assert!(
        sql.contains(":'hive_password'"),
        "the password placeholder moved"
    );
    sql.replace(":'hive_password'", &format!("'{PASSWORD}'"))
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_hive_role_owns_its_schema_and_only_reads_the_relay() {
    let (admin_pool, guard) = support::scratch_relay_db("hive_roles").await;
    let sql = roles_sql();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))
        .execute(&admin_pool)
        .await
        .expect("apply hive-roles.sql");
    // The deploy runs it on every start, so a second run must change nothing.
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&admin_pool)
        .await
        .expect("apply hive-roles.sql again");

    let options = PgConnectOptions::from_str(&support::url_for(&guard))
        .unwrap()
        .username("buzz_hive")
        .password(PASSWORD);
    // The role's own search_path is `hive`, so connect without setting it here.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect as buzz_hive");

    // buzz-hive can migrate and write in its own schema with no CREATE on the database.
    buzz_hive_store::migrate::migrate(&pool)
        .await
        .expect("migrate as buzz_hive");
    sqlx::query(
        "INSERT INTO hive.settings (community_id, scope, scope_id, key, value) \
         VALUES (gen_random_uuid(), 'engine', '', 'k', '1'::jsonb)",
    )
    .execute(&pool)
    .await
    .expect("insert into hive.settings");
    let search_path: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(search_path, "hive");

    // It reads every relay table it needs, and verify_relay_columns sees every column.
    for table in [
        "communities",
        "channels",
        "channel_members",
        "relay_members",
        "users",
        "events",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT 1 FROM public.{table} LIMIT 1"
        )))
        .fetch_optional(&pool)
        .await
        .unwrap_or_else(|e| panic!("select from public.{table}: {e}"));
    }
    buzz_hive_store::relay_read::verify_relay_columns(&pool)
        .await
        .expect("relay columns visible to buzz_hive");

    // The grants are by column: the role may read exactly the columns the service reads
    // and no other column of those tables.
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' \
           AND table_name IN ('communities', 'channels', 'channel_members', 'relay_members', 'users', 'events')",
    )
    .fetch_all(&admin_pool)
    .await
    .unwrap();
    assert!(
        columns.iter().any(|(t, c)| t == "users" && c == "pubkey"),
        "the relay schema was not read"
    );
    for (table, column) in &columns {
        let readable: bool =
            sqlx::query_scalar("SELECT has_column_privilege('buzz_hive', $1, $2, 'SELECT')")
                .bind(format!("public.{table}"))
                .bind(column)
                .fetch_one(&admin_pool)
                .await
                .unwrap();
        let wanted = buzz_hive_store::relay_read::RELAY_COLUMNS
            .iter()
            .any(|(t, c, _)| t == table && c == column);
        assert_eq!(
            readable, wanted,
            "public.{table}.{column}: readable by buzz_hive {readable}, listed in RELAY_COLUMNS {wanted}"
        );
    }
    let directory = buzz_hive_store::relay_read::RelayRead::new(pool.clone());
    let none = directory
        .relay_role(uuid::Uuid::from_u128(1), &"ab".repeat(32))
        .await
        .expect("relay_role as buzz_hive");
    assert_eq!(none, None);

    // The two reads a requester claim makes work with only the granted columns: the message
    // by id, and whether a newer message in its thread mentions the agent.
    let community: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO public.communities (host) VALUES ('roles.example.org') RETURNING id",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    let channel = uuid::Uuid::new_v4();
    let (agent, person) = ("a1".repeat(32), "b2".repeat(32));
    let root = "c3".repeat(32);
    let post = |n: u8, kind: i32, secs: i64, tags: serde_json::Value, deleted: bool| {
        let admin_pool = admin_pool.clone();
        let (person, id) = (person.clone(), hex::encode([n; 32]));
        async move {
            sqlx::query(
                "INSERT INTO public.events \
                 (community_id, id, pubkey, created_at, kind, tags, content, sig, channel_id, deleted_at) \
                 VALUES ($1, $2, $3, to_timestamp($4), $5, $6, 'hello', $7, $8, CASE WHEN $9 THEN now() END)",
            )
            .bind(community)
            .bind(hex::decode(id).unwrap())
            .bind(hex::decode(person).unwrap())
            .bind(secs as f64)
            .bind(kind)
            .bind(tags)
            .bind(vec![0u8; 64])
            .bind(channel)
            .bind(deleted)
            .execute(&admin_pool)
            .await
            .unwrap();
        }
    };
    let mention = serde_json::json!([
        ["h", channel.to_string()],
        ["p", agent],
        ["e", root, "", "root"]
    ]);
    let now = chrono::Utc::now().timestamp() - 600;
    post(1, 9, now, mention.clone(), false).await;
    let first = directory
        .event_by_id(community, &hex::encode([1u8; 32]))
        .await
        .expect("event_by_id as buzz_hive")
        .expect("the event is there");
    assert_eq!((first.author.as_str(), first.kind), (person.as_str(), 9));
    assert_eq!(first.created_at, now);
    assert_eq!(first.channel_id, Some(channel));
    assert!(first.mentions(&agent) && !first.deleted);
    assert_eq!(first.thread_root(), root);
    assert!(directory
        .event_by_id(community, &hex::encode([9u8; 32]))
        .await
        .unwrap()
        .is_none());
    assert!(!newer_mention(&directory, community, &first, &root, &agent).await);
    // A reaction that names the agent and the root is not a message, and neither is a deleted
    // message. A message in another thread does not count.
    post(2, 7, now + 10, mention.clone(), false).await;
    post(3, 9, now + 20, mention.clone(), true).await;
    post(
        4,
        9,
        now + 30,
        serde_json::json!([
            ["h", channel.to_string()],
            ["p", agent],
            ["e", "d4".repeat(32), "", "root"]
        ]),
        false,
    )
    .await;
    assert!(!newer_mention(&directory, community, &first, &root, &agent).await);
    post(5, 9, now + 40, mention, false).await;
    assert!(newer_mention(&directory, community, &first, &root, &agent).await);
    let last = directory
        .event_by_id(community, &hex::encode([5u8; 32]))
        .await
        .unwrap()
        .unwrap();
    assert!(!newer_mention(&directory, community, &last, &root, &agent).await);

    // It cannot write or alter any relay table, or read one it was not granted.
    for statement in [
        "INSERT INTO public.channels (id) VALUES (gen_random_uuid())",
        "UPDATE public.relay_members SET role = 'owner'",
        "DELETE FROM public.users",
        "SELECT content FROM public.events LIMIT 1",
        "SELECT sig FROM public.events LIMIT 1",
        "UPDATE public.events SET kind = 1",
    ] {
        let error = sqlx::query(statement)
            .execute(&pool)
            .await
            .expect_err(statement);
        let code = error
            .as_database_error()
            .and_then(|e| e.code().map(|c| c.to_string()));
        assert_eq!(code.as_deref(), Some("42501"), "{statement}: {error}");
    }

    pool.close().await;
    // The role is cluster wide, so remove it again for the next run.
    let _ = sqlx::raw_sql("DROP OWNED BY buzz_hive; DROP ROLE buzz_hive;")
        .execute(&admin_pool)
        .await;
}
