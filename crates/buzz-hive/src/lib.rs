//! The Beehive workflow engine service: its settings, the relay account that posts, and
//! the loop that sends queued posts. `main.rs` wires them to the HTTP routes.

pub mod config;
pub mod dir_source;
pub mod drain;
pub mod relay_adapter;
