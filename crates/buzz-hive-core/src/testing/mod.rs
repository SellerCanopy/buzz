//! In-memory fakes so core and API tests run with no database.

mod fake_relay;
mod mem_store;

pub use fake_relay::{FakeRelay, Posted};
pub use mem_store::{FakeDirectory, MemStore};
