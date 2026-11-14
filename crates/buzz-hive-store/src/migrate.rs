//! Connection and migrator for the `hive` schema.

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
const SCHEMA_LOCK: i64 = 0x0062_6565_6869_7665; // "beehive"

/// Connect with a search path of `hive`. That puts sqlx's default
/// `_sqlx_migrations` table in `hive`, so no table-name override is needed and
/// the relay's `public._sqlx_migrations` is untouched.
pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    let options = PgConnectOptions::from_str(url)?.options([("search_path", "hive")]);
    PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
}

/// Create the schema only when it is missing, then apply pending migrations.
///
/// `CREATE SCHEMA IF NOT EXISTS` needs CREATE on the database even when the
/// schema exists, and the `buzz_hive` role has no such grant.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(SCHEMA_LOCK)
        .execute(&mut *conn)
        .await?;
    let created = ensure_schema(&mut conn).await;
    // Release on every path: a lock left on a pooled connection would outlive this call.
    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(SCHEMA_LOCK)
        .execute(&mut *conn)
        .await;
    if created.is_err() || unlocked.is_err() {
        // The session may still hold the lock, so close it rather than return it to the pool.
        drop(conn.detach());
    } else {
        drop(conn);
    }
    created?;
    unlocked?;
    MIGRATOR.run(pool).await?;
    Ok(())
}

async fn ensure_schema(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'hive')")
            .fetch_one(&mut *conn)
            .await?;
    if !exists {
        sqlx::query("CREATE SCHEMA hive").execute(conn).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Migration numbers run 0001, 0002, ... with no duplicate and no gap.
    #[test]
    fn migration_numbers_are_dense() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
        let mut numbers: Vec<u32> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                assert!(name.ends_with(".sql"), "{name} is not a .sql file");
                name[..4]
                    .parse()
                    .unwrap_or_else(|_| panic!("{name} must start with a four digit number"))
            })
            .collect();
        numbers.sort_unstable();
        let want: Vec<u32> = (1..=numbers.len() as u32).collect();
        assert_eq!(
            numbers, want,
            "migration numbers must be 0001.. with no gap or duplicate"
        );
    }
}
