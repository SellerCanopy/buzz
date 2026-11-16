//! The host supervisor: the one piece of Beehive with Docker access.
//!
//! It listens on a Unix socket and does only what a closed set of signed calls asks for: write an
//! agent's `mcp.json` and environment, restart the agent, report its settings, and run the Claude
//! CLI's OAuth sign-in for one of its tool servers. It never takes a
//! path or a command from a call, only an agent name that has a folder under
//! `deploy/aws/agents/`. It is also the only reader of secret values ([`secrets`]): the
//! engine writes them and cannot read them back.

pub mod calls;
pub mod config;
pub mod login;
pub mod secrets;
pub mod server;
