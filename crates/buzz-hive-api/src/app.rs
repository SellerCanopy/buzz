//! Shared state every route reads.

use std::sync::Arc;

use buzz_hive_core::bus::Bus;
use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::resolve::{resolve_actor, Actor};
use buzz_hive_core::workflow::source::CatalogSource;

use crate::error::HiveError;
use crate::replay::ReplayGuard;
use crate::token::TokenMinter;

pub struct ApiConfig {
    /// The origin clients reach, with no trailing slash. NIP-98 events must name it.
    pub public_url: String,
    /// Origins allowed to call from a browser. Empty means no cross-origin access.
    pub allowed_origins: Vec<String>,
}

#[derive(Clone)]
pub struct App {
    pub config: Arc<ApiConfig>,
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn Directory>,
    pub minter: Arc<TokenMinter>,
    pub replay: Arc<ReplayGuard>,
    /// Wakes parked `PollEvents` calls after a commit.
    pub bus: Bus,
    /// Where workflow packages load from. Read on every call, so an edited package counts
    /// from the next one.
    pub catalog: Arc<dyn CatalogSource>,
    pub community: CommunityId,
}

impl App {
    /// The actor behind `pubkey`, with the level it holds right now.
    pub async fn resolve(&self, pubkey: &str) -> Result<Actor, HiveError> {
        let mut tx = self.store.begin(self.community).await?;
        let actor =
            resolve_actor(&mut *tx, self.directory.as_ref(), self.community, pubkey).await?;
        Ok(actor)
    }
}
