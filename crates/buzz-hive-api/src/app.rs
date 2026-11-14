//! Shared state every route reads.

use std::sync::Arc;

use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::resolve::{resolve_actor, Actor};

use crate::error::HiveError;
use crate::replay::ReplayGuard;
use crate::token::TokenMinter;

pub struct ApiConfig {
    /// The origin clients reach, with no trailing slash. NIP-98 events must name it.
    pub public_url: String,
}

#[derive(Clone)]
pub struct App {
    pub config: Arc<ApiConfig>,
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn Directory>,
    pub minter: Arc<TokenMinter>,
    pub replay: Arc<ReplayGuard>,
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
