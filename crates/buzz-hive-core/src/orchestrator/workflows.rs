//! Keeping the versions of the packages every build ships.

use crate::ports::store::{CommunityId, Store, StoreError};
use crate::workflow::load::Catalog;

/// Who the store records as the author of a built-in version.
pub const BUILTIN_AUTHOR: &str = "builtin";

/// Stores each built-in package of `catalog` as a version named by its content hash, and
/// makes it the package's current one. A hash already stored is not written again, so a
/// restart changes nothing. A card keeps the hash it started with, so an older version stays.
/// Returns how many packages were recorded.
pub async fn record_builtins(
    store: &dyn Store,
    community: CommunityId,
    catalog: &Catalog,
) -> Result<usize, StoreError> {
    let mut tx = store.begin(community).await?;
    let mut recorded = 0;
    for name in catalog.names() {
        let Some(package) = catalog.get(name).filter(|p| p.builtin) else {
            continue;
        };
        let body = serde_json::to_value(&package.files)
            .map_err(|e| StoreError::Conflict(format!("package {name}: {e}")))?;
        tx.workflows()
            .record_version(name, &package.hash(), body, BUILTIN_AUTHOR, true)
            .await?;
        recorded += 1;
    }
    tx.commit().await?;
    Ok(recorded)
}
