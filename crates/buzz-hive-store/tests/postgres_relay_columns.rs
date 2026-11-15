mod support;

use buzz_hive_store::relay_read::verify_relay_columns;

#[tokio::test]
#[ignore = "requires Postgres"]
async fn relay_columns_match_the_current_relay_schema() {
    let (pool, _guard) = support::scratch_relay_db("hive_cols").await;
    verify_relay_columns(&pool).await.expect("relay columns");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_renamed_relay_column_is_named_in_the_error() {
    let (pool, _guard) = support::scratch_relay_db("hive_cols_bad").await;
    sqlx::query("ALTER TABLE public.relay_members RENAME COLUMN role TO member_role")
        .execute(&pool)
        .await
        .unwrap();
    let error = verify_relay_columns(&pool).await.unwrap_err().to_string();
    assert!(error.contains("relay_members.role"), "{error}");
}
