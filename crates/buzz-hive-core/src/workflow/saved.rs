//! The packages people save, kept loaded beside the packages read from sources.
//!
//! [`CatalogSource::load`](crate::workflow::source::CatalogSource::load) is synchronous and
//! runs on every operation, so it cannot ask the store. [`SavedPackages`] holds what the
//! store last said and is refreshed at startup and after every change to a saved workflow.
//! One engine serves a community, so nothing else changes the store's versions between
//! refreshes.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::ports::store::{CommunityId, KeptVersion, Store, StoreError};
use crate::workflow::load::{parse, Catalog};
use crate::workflow::source::{CatalogSource, Sources};

/// Where the engine says a saved package came from, in paths and messages.
pub const SAVED_LABEL: &str = "saved";

/// The saved packages and the older versions runs still use. Clones share one list.
#[derive(Clone, Default)]
pub struct SavedPackages {
    kept: Arc<RwLock<Vec<KeptVersion>>>,
}

impl SavedPackages {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the kept versions from the store.
    pub async fn refresh(
        &self,
        store: &dyn Store,
        community: CommunityId,
    ) -> Result<(), StoreError> {
        let mut tx = store.begin(community).await?;
        let kept = tx.workflows().kept_versions().await?;
        drop(tx);
        *self.kept.write().unwrap_or_else(|e| e.into_inner()) = kept;
        Ok(())
    }

    /// Adds every kept version to `catalog`: a person's current package as a package, and
    /// any other version as history.
    pub fn apply(&self, catalog: &mut Catalog) {
        let kept = self.kept.read().unwrap_or_else(|e| e.into_inner());
        for version in kept.iter() {
            let files: BTreeMap<String, String> = match serde_json::from_value(version.body.clone())
            {
                Ok(files) => files,
                Err(error) => {
                    tracing::warn!(name = %version.name, %error, "a stored package is not a file list");
                    continue;
                }
            };
            let Ok(mut package) = parse(&format!("{SAVED_LABEL}/{}", version.name), files) else {
                tracing::warn!(name = %version.name, "a stored package does not decode");
                continue;
            };
            package.builtin = version.builtin;
            if version.current && !version.builtin {
                catalog.insert(package);
            } else if !version.current {
                catalog.remember(package);
            }
        }
    }
}

/// Packages read from sources, and the ones people saved.
pub struct Layered {
    pub sources: Sources,
    pub saved: SavedPackages,
}

impl CatalogSource for Layered {
    fn load(&self) -> Catalog {
        let mut catalog = self.sources.load();
        self.saved.apply(&mut catalog);
        catalog
    }

    fn saved(&self) -> Option<&SavedPackages> {
        Some(&self.saved)
    }
}
