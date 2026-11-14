//! Pure logic and ports for the Beehive workflow engine.
//!
//! This crate has no database, web or Buzz dependency. Everything it needs
//! from the outside world is a trait in [`ports`], so its tests run against
//! the in-memory fakes in `testing` with no infrastructure.

pub mod access;
pub mod ports;
pub mod resolve;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
