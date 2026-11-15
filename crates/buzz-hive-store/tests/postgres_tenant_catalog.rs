//! TDD spike 2: the relay's tenant-deletion catalog checks ignore the `hive` schema.

mod support;

#[tokio::test]
#[ignore = "requires Postgres"]
async fn relay_catalog_checks_ignore_the_hive_schema() {
    let (pool, guard) = support::scratch_relay_db("hive_cat").await;
    let relay = buzz_db::Db::from_pool(pool.clone());
    relay
        .validate_deletion_catalog()
        .await
        .expect("catalog validation before hive");

    let hive = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&hive).await.unwrap();

    // Both hive tables carry a `community_id` column, so a check that looked
    // outside `public` would report drift here.
    relay
        .validate_deletion_catalog()
        .await
        .expect("catalog validation with hive present");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_catalog_check_does_see_a_table_in_public() {
    // Control: the check is live, so the test above proves something.
    let (pool, _guard) = support::scratch_relay_db("hive_cat_ctl").await;
    sqlx::query("CREATE TABLE public.hive_probe (community_id uuid NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    let relay = buzz_db::Db::from_pool(pool);
    assert!(relay.validate_deletion_catalog().await.is_err());
}
