//! The Beehive workflow engine service.

use std::sync::Arc;

use buzz_hive::{config, dir_source, drain, github_adapter, intake, relay_adapter, timers};
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::orchestrator::workflows::record_builtins;
use buzz_hive_core::ports::code_host::CodeHost;
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::source::{CatalogSource, Source, Sources};
use buzz_hive_store::migrate::PoolConfig;
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

    // Migrations run on their own small pool with no statement timeout, because a migration
    // may legitimately take long. It is closed before the service pools open.
    let migrator = buzz_hive_store::migrate::connect_with(
        &config.database_url,
        &PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .map_err(|e| format!("cannot connect to the database: {e}"))?;
    buzz_hive_store::migrate::migrate(&migrator)
        .await
        .map_err(|e| format!("migration failed: {e}"))?;
    migrator.close().await;

    // Two pools. A request holds a connection of the first for its transaction and may then
    // look something up in the relay's tables. Sharing one pool let ten such requests hold
    // every connection while each waited for an eleventh.
    let timeouts = |max_connections| PoolConfig {
        max_connections,
        acquire_timeout: config.db_acquire_timeout,
        statement_timeout: Some(config::DB_STATEMENT_TIMEOUT),
        lock_timeout: Some(config::DB_LOCK_TIMEOUT),
    };
    let pool = buzz_hive_store::migrate::connect_with(
        &config.database_url,
        &timeouts(config.db_max_connections),
    )
    .await
    .map_err(|e| format!("cannot connect to the database: {e}"))?;
    let relay_pool = buzz_hive_store::migrate::connect_with(
        &config.database_url,
        &timeouts(config.relay_db_max_connections),
    )
    .await
    .map_err(|e| format!("cannot connect to the database: {e}"))?;
    verify_relay_columns(&relay_pool)
        .await
        .map_err(|e| e.to_string())?;

    let relay = RelayRead::new(relay_pool);
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

    let mut sources = vec![builtin_source()];
    if let Some(dir) = &config.workflows_dir {
        sources.push(Source::new(
            dir.display().to_string(),
            false,
            dir_source::DirSource::new(dir),
        ));
    }
    let catalog = Arc::new(Sources(sources));
    let store = Arc::new(PgStore::new(pool));
    let recorded = record_builtins(store.as_ref(), community, &catalog.load())
        .await
        .map_err(|e| format!("cannot record the built-in workflows: {e}"))?;
    tracing::info!(recorded, "built-in workflows recorded");

    // The Beehive account posts to the update channel. Without a key the service still
    // serves everything else, and a start names the missing variable.
    let relay_adapter = match (&config.nostr_key, &config.relay_url) {
        (Some(key), Some(url)) => {
            let keys = nostr::Keys::parse(key).map_err(|_| {
                "BUZZ_HIVE_NOSTR_KEY is not an nsec or 64 hex characters".to_owned()
            })?;
            Some(Arc::new(relay_adapter::RelayAdapter::new(
                keys,
                url.clone(),
            )))
        }
        _ => {
            tracing::warn!("BUZZ_HIVE_NOSTR_KEY is not set: runs cannot be started");
            None
        }
    };
    let hive_pubkey = relay_adapter
        .as_ref()
        .map(|adapter| adapter.pubkey())
        .unwrap_or_default();

    // One team token reads GitHub. Without it the service runs, and the intake screens and
    // the issue pickers say so.
    let code_host: Option<Arc<dyn CodeHost>> = match &config.github_token {
        Some(token) => Some(Arc::new(github_adapter::GitHub::new(
            &config.github_api_url,
            token,
        )?)),
        None => {
            tracing::warn!("GH_TOKEN is not set: issue intake is off");
            None
        }
    };

    let app = App {
        config: Arc::new(ApiConfig {
            public_url: config.public_url.clone(),
            allowed_origins: config.allowed_origins.clone(),
        }),
        store,
        directory: Arc::new(relay),
        minter: Arc::new(TokenMinter::new(
            config.token_secret.as_bytes(),
            Arc::new(SystemClock),
        )?),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        auth_limit: buzz_hive_api::rate::auth_limiter(),
        bus: Bus::new(0),
        catalog,
        community,
        hive_pubkey: hive_pubkey.clone(),
        outbox: Arc::new(tokio::sync::Notify::new()),
        timers: Arc::new(tokio::sync::Notify::new()),
        code_host: code_host.clone(),
        intake: Arc::new(tokio::sync::Notify::new()),
    };
    let bus = app.bus.clone();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let mut timer_task = relay_adapter.as_ref().map(|_| {
        let timers = timers::TimerLoop {
            store: app.store.clone(),
            directory: app.directory.clone(),
            catalog: app.catalog.clone(),
            bus: app.bus.clone(),
            community,
            hive: hive_pubkey.clone(),
            wake: app.timers.clone(),
            outbox: app.outbox.clone(),
        };
        tokio::spawn(timers.run(stopped.clone()))
    });
    let mut intake_task = code_host.map(|host| {
        let supervisor = intake::IntakeSupervisor {
            shared: Arc::new(intake::Shared {
                store: app.store.clone(),
                directory: app.directory.clone(),
                catalog: app.catalog.clone(),
                host,
                bus: app.bus.clone(),
                community,
                hive: hive_pubkey.clone(),
                outbox: app.outbox.clone(),
                gate: Default::default(),
                random: intake::system_random(),
            }),
            wake: app.intake.clone(),
        };
        tokio::spawn(supervisor.run(stopped.clone()))
    });
    let mut drain = relay_adapter.map(|relay| {
        let drain = drain::Drain {
            store: app.store.clone(),
            directory: app.directory.clone(),
            relay,
            bus: app.bus.clone(),
            community,
            hive: hive_pubkey.clone(),
            wake: app.outbox.clone(),
        };
        tokio::spawn(drain.run(stopped))
    });
    let router = buzz_hive_api::finish(buzz_hive_api::routes(), app);

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(listen = %config.listen, %community, account = %hive_pubkey, "buzz-hive started");
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown().await;
            // Parked polls would otherwise hold the shutdown for up to 25 seconds.
            bus.close();
        })
        .await
        .map_err(|e| format!("server error: {e}"));
    // Stop the drain on every exit, a server error included, then give it a moment to finish
    // the post it is sending. Its claim on a post it leaves behind runs out by itself, and the
    // post is sent again as the same relay event.
    let _ = stop.send(true);
    if let Some(task) = drain.as_mut() {
        if tokio::time::timeout(DRAIN_SHUTDOWN_GRACE, &mut *task)
            .await
            .is_err()
        {
            tracing::warn!("the outbox drain did not stop in time; it was cut off");
            task.abort();
        }
    }
    if let Some(task) = intake_task.as_mut() {
        // The supervisor waits for its own loops, so give it their grace and a little more.
        if tokio::time::timeout(DRAIN_SHUTDOWN_GRACE * 2, &mut *task)
            .await
            .is_err()
        {
            tracing::warn!("the intake loops did not stop in time; they were cut off");
            task.abort();
        }
    }
    if let Some(task) = timer_task.as_mut() {
        if tokio::time::timeout(DRAIN_SHUTDOWN_GRACE, &mut *task)
            .await
            .is_err()
        {
            tracing::warn!("the timer loop did not stop in time; it was cut off");
            task.abort();
        }
    }
    served
}

/// How long shutdown waits for the drain to finish the post in flight.
const DRAIN_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

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
