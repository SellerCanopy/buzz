//! Pure logic and ports for the Beehive workflow engine.
//!
//! This crate has no database, web or Buzz dependency. Everything it needs
//! from the outside world is a trait in [`ports`], so its tests run against
//! the in-memory fakes in `testing` with no infrastructure.

pub mod access;
pub mod background;
pub mod bus;
pub mod config;
pub mod domain;
pub mod orchestrator;
pub mod ports;
pub mod resolve;
pub mod rules;
pub mod supervisor_wire;
pub mod tool_names;
pub mod workflow;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
