//! In-memory fakes so core and API tests run with no database.

mod mem_store;

pub use mem_store::{FakeDirectory, MemStore};
