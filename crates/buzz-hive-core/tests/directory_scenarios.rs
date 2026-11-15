//! The in-memory directory answers the shared `newer_mention` scenarios as Postgres does.
//! `crates/buzz-hive-store/tests/postgres_directory.rs` runs the same table against the relay.

use buzz_hive_core::ports::store::Directory;
use buzz_hive_core::testing::directory_scenarios::{agent, newer_mention_scenarios};
use buzz_hive_core::testing::FakeDirectory;

#[tokio::test]
async fn the_fake_directory_follows_the_shared_newer_mention_scenarios() {
    for case in newer_mention_scenarios() {
        let directory = case
            .events
            .iter()
            .cloned()
            .fold(FakeDirectory::new(), FakeDirectory::with_event);
        let got = directory
            .newer_mention(uuid::Uuid::nil(), case.subject(), &case.root, &agent())
            .await
            .unwrap();
        assert_eq!(got, case.expect, "{}", case.name);
    }
}
