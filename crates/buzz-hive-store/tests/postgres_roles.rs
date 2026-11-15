//! `deploy/aws/hive-roles.sql` gives the `buzz_hive` role its schema and read access to the
//! relay columns the service reads, and nothing more.

mod support;

use buzz_hive_core::ports::store::Directory;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

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
           AND table_name IN ('communities', 'channels', 'channel_members', 'relay_members', 'users')",
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

    // It cannot write or alter any relay table, or read one it was not granted.
    for statement in [
        "INSERT INTO public.channels (id) VALUES (gen_random_uuid())",
        "UPDATE public.relay_members SET role = 'owner'",
        "DELETE FROM public.users",
        "SELECT 1 FROM public.events LIMIT 1",
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
