//! In-memory fakes so core and API tests run with no database.

pub mod directory_scenarios;
mod fake_code_host;
mod fake_relay;
mod fake_secret_sink;
mod fake_supervisor;
mod manual_clock;
mod mem_store;
pub mod outbox_scenarios;

pub use fake_code_host::FakeCodeHost;
pub use fake_relay::{FakeRelay, Posted};
pub use fake_secret_sink::FakeSecretSink;
pub use fake_supervisor::FakeSupervisor;
pub use manual_clock::ManualClock;
pub use mem_store::{FakeDirectory, MemStore};
