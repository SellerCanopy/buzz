//! The Beehive workflow engine service.

mod config;

use std::sync::Arc;

use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::bus::Bus;
use buzz_hive_store::pg::PgStore;
use buzz_hive_store::relay_read::{verify_relay_columns, RelayRead};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = run().await {
        tracing::error!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let config = config::Config::from_env()?;

    let pool = buzz_hive_store::migrate::connect(&config.database_url)
        .await
        .map_err(|e| format!("cannot connect to the database: {e}"))?;
    buzz_hive_store::migrate::migrate(&pool)
        .await
        .map_err(|e| format!("migration failed: {e}"))?;
    verify_relay_columns(&pool)
        .await
        .map_err(|e| e.to_string())?;

    let relay = RelayRead::new(pool.clone());
    let community = relay
        .community_for_host(&config.community_host)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no community has host {}; set BUZZ_HIVE_COMMUNITY_HOST",
                config.community_host
            )
        })?;

    let app = App {
        config: Arc::new(ApiConfig {
            public_url: config.public_url.clone(),
            allowed_origins: config.allowed_origins.clone(),
        }),
        store: Arc::new(PgStore::new(pool)),
        directory: Arc::new(relay),
        minter: Arc::new(TokenMinter::new(
            config.token_secret.as_bytes(),
            Arc::new(SystemClock),
        )?),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        bus: Bus::new(0),
        community,
    };
    let bus = app.bus.clone();
    let router = buzz_hive_api::finish(buzz_hive_api::routes(&app), app);

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(listen = %config.listen, %community, "buzz-hive started");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown().await;
            // Parked polls would otherwise hold the shutdown for up to 25 seconds.
            bus.close();
        })
        .await
        .map_err(|e| format!("server error: {e}"))
}

/// Resolves on SIGTERM or Ctrl-C.
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
