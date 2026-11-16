//! In-memory fakes so core and API tests run with no database.

pub mod directory_scenarios;
mod fake_code_host;
mod fake_relay;
mod manual_clock;
mod mem_store;
pub mod outbox_scenarios;

pub use fake_code_host::FakeCodeHost;
pub use fake_relay::{FakeRelay, Posted};
pub use manual_clock::ManualClock;
pub use mem_store::{FakeDirectory, MemStore};
