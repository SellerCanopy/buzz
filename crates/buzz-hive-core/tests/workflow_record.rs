//! The built-in packages are stored by content hash when the service starts.

use buzz_hive_core::orchestrator::workflows::record_builtins;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::MemStore;
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::load::load;
use buzz_hive_core::workflow::source::{MemSource, Source};

#[tokio::test]
async fn built_in_packages_are_recorded_once_per_hash() {
    let store = MemStore::new();
    let community = CommunityId::from_u128(1);
    let catalog = load(&[builtin_source()]);

    let first = record_builtins(&store, community, &catalog).await.unwrap();
    let second = record_builtins(&store, community, &catalog).await.unwrap();
    assert_eq!(first, 6);
    assert_eq!(second, 6);

    let mut tx = store.begin(community).await.unwrap();
    let stored = tx.workflows().packages().await.unwrap();
    let names: Vec<&str> = stored.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["demo", "engineering", "openspec", "qrspi", "rpi", "task"]
    );
    for package in &stored {
        assert!(package.builtin);
        assert_eq!(
            package.current_hash,
            catalog.get(&package.name).unwrap().hash()
        );
        let body = tx
            .workflows()
            .version(&package.name, &package.current_hash)
            .await
            .unwrap()
            .expect("the version body is stored");
        assert!(body.get("workflow.yaml").is_some(), "{}", package.name);
    }
}

#[tokio::test]
async fn packages_that_are_not_built_in_are_not_recorded() {
    let store = MemStore::new();
    let community = CommunityId::from_u128(1);
    let source = MemSource::new()
        .with("mine/kiln-package.yaml", "name: mine\nversion: 1.0.0\n")
        .with("mine/workflow.yaml", "version: 2\nstages: []\n");
    let catalog = load(&[Source::new("/mine", false, source)]);
    assert_eq!(
        record_builtins(&store, community, &catalog).await.unwrap(),
        0
    );
    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.workflows().packages().await.unwrap().is_empty());
}
