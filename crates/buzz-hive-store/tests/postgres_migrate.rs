//! TDD spike 1: `hive` keeps its own migration history beside the relay's.

mod support;

#[tokio::test]
#[ignore = "requires Postgres"]
async fn hive_keeps_its_own_migration_history() {
    let (relay_pool, guard) = support::scratch_relay_db("hive_mig").await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
        .fetch_one(&relay_pool)
        .await
        .unwrap();

    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap(); // second run is a no-op

    let hive: i64 = sqlx::query_scalar("SELECT count(*) FROM hive._sqlx_migrations")
        .fetch_one(&relay_pool)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
        .fetch_one(&relay_pool)
        .await
        .unwrap();
    assert_eq!(hive, 1);
    assert_eq!(
        before, after,
        "the relay's migration history must not change"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn migrating_without_create_on_the_database_works_once_the_schema_exists() {
    // The `buzz_hive` role has no CREATE on the database, so `migrate` must not
    // try to create a schema that is already there.
    use sqlx::postgres::PgConnectOptions;
    use std::str::FromStr;

    let (relay_pool, guard) = support::scratch_relay_db("hive_role").await;
    let role = format!("hive_probe_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} LOGIN PASSWORD 'probe'; \
         CREATE SCHEMA hive AUTHORIZATION {role}; \
         ALTER ROLE {role} SET search_path = hive;"
    )))
    .execute(&relay_pool)
    .await
    .unwrap();
    let base = PgConnectOptions::from_str(&support::url_for(&guard)).unwrap();
    let url = format!(
        "postgres://{role}:probe@{}:{}/{}",
        base.get_host(),
        base.get_port(),
        base.get_database().unwrap()
    );
    let pool = buzz_hive_store::migrate::connect(&url).await.unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = 'hive' AND table_name IN ('access_levels', 'settings')",
    )
    .fetch_one(&relay_pool)
    .await
    .unwrap();
    assert_eq!(tables, 2);
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA hive CASCADE; DROP ROLE {role};"
    )))
    .execute(&relay_pool)
    .await
    .unwrap();
}
