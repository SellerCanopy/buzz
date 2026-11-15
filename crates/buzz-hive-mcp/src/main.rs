//! `buzz-hive-mcp`: a stdio MCP server that gives a Buzz agent the Beehive tools.
//!
//! The agent runtime starts it with `BUZZ_PRIVATE_KEY` and `BUZZ_RELAY_URL` set, as it does
//! for every MCP server. `BUZZ_HIVE_URL` says where Beehive is when that is not the relay's
//! own host.

use buzz_hive_mcp::{serve_stdio, Config, SystemClock};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // stdout carries the protocol, so logs go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let config = Config::from_env(|name| std::env::var(name).ok())?;
    serve_stdio(config, std::sync::Arc::new(SystemClock)).await
}
