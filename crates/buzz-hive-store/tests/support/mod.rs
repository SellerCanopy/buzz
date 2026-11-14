//! A scratch database holding the relay schema, dropped when the guard goes out of scope.
//!
//! Mirrors `buzz-db`'s crate-private `ScratchDb`. Bind the guard to a named
//! variable for the whole test: `let (pool, _) = ..` drops the database at once.

#![allow(dead_code)]

use sqlx::Connection as _;

const DEFAULT_DATABASE_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz";

fn admin_url() -> String {
    std::env::var("BUZZ_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("TEST_DATABASE_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned())
}

pub struct Scratch {
    name: String,
    url: String,
    admin_url: String,
}

impl Scratch {
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// A new database with every relay migration applied, and a pool on it.
pub async fn scratch_relay_db(prefix: &str) -> (sqlx::PgPool, Scratch) {
    let admin_url = admin_url();
    let name = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&admin_url)
        .await
        .expect("connect admin database");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut admin)
        .await
        .expect("create scratch database");
    let _ = admin.close().await;
    let idx = admin_url.rfind('/').expect("db url has a path segment");
    let url = format!("{}/{name}", &admin_url[..idx]);
    let guard = Scratch {
        name,
        url,
        admin_url,
    };
    let pool = sqlx::PgPool::connect(&guard.url)
        .await
        .expect("connect scratch database");
    buzz_db::migration::run_migrations(&pool)
        .await
        .expect("migrate the relay schema");
    (pool, guard)
}

pub fn url_for(guard: &Scratch) -> String {
    guard.url.clone()
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let url = self.admin_url.clone();
        let statement = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
        // Drop runs on the test's runtime thread, so use a thread with its own runtime.
        let cleanup = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?
                .block_on(async {
                    let mut conn = sqlx::PgConnection::connect(&url)
                        .await
                        .map_err(|e| e.to_string())?;
                    let result = sqlx::query(sqlx::AssertSqlSafe(statement))
                        .execute(&mut conn)
                        .await;
                    let _ = conn.close().await;
                    result.map(drop).map_err(|e| e.to_string())
                })
        })
        .join();
        match cleanup {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("warning: failed to drop {}: {error}", self.name),
            Err(_) => eprintln!("warning: cleanup of {} panicked", self.name),
        }
    }
}
