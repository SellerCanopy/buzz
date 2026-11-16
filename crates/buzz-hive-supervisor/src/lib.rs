//! The host supervisor: the one piece of Beehive with Docker access.
//!
//! It listens on a Unix socket and does only what three signed calls ask for: write an agent's
//! `mcp.json` and environment, restart the agent, and report its settings. It never takes a
//! path or a command from a call, only an agent name that has a folder under
//! `deploy/aws/agents/`.

pub mod calls;
pub mod config;
pub mod server;
