use std::sync::Arc;

use buzz_hive_supervisor::calls::Service;
use buzz_hive_supervisor::config::Config;
use buzz_hive_supervisor::server;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = run().await {
        eprintln!("buzz-hive-supervisor: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let service = Arc::new(Service::new(Config::from_env()?)?);
    let listener = server::bind(&service)?;
    tracing::info!(socket = %service.config().socket.display(), "supervisor listening");
    tokio::select! {
        () = server::serve(service, listener) => Ok(()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
