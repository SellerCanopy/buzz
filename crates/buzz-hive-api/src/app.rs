//! Shared state every route reads.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use buzz_hive_core::bus::Bus;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::ports::code_host::CodeHost;
use buzz_hive_core::ports::relay::Relay;
use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::resolve::{resolve_actor, Actor};
use buzz_hive_core::workflow::load::Catalog;
use buzz_hive_core::workflow::source::CatalogSource;

use crate::error::HiveError;
use crate::rate::RateLimiter;
use crate::replay::ReplayGuard;
use crate::token::TokenMinter;

/// How many times a read of the saved workflows is tried after a change.
const REFRESH_TRIES: u32 = 3;

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
    /// Caps how fast `/auth/token` is served, so signed events cannot be used to flood the
    /// store with lookups.
    pub auth_limit: Arc<RateLimiter>,
    /// Wakes parked `PollEvents` calls after a commit.
    pub bus: Bus,
    /// Where workflow packages load from. Read on every call, so an edited package counts
    /// from the next one.
    pub catalog: Arc<dyn CatalogSource>,
    pub community: CommunityId,
    /// The Beehive account's pubkey, hex, empty when the service has no key. It posts to
    /// the update channel, so it must be a member of it.
    pub hive_pubkey: String,
    /// Posts that belong to no card, such as the assistant's. `None` when the service has no
    /// relay account. Posts a card makes go through the outbox instead.
    pub relay: Option<Arc<dyn Relay>>,
    /// Wakes the outbox drain after a commit that queued a post.
    pub outbox: Arc<tokio::sync::Notify>,
    /// Wakes the timer loop after a commit that may have set a timer.
    pub timers: Arc<tokio::sync::Notify>,
    /// The team's code host, `None` when no token is set. Intake and the issue pickers need it.
    pub code_host: Option<Arc<dyn CodeHost>>,
    /// Wakes the intake supervisor after a rule was saved or deleted.
    pub intake: Arc<tokio::sync::Notify>,
}

impl App {
    /// The Beehive account's pubkey, or the sentence to show when the service has no key.
    pub fn require_hive(&self) -> Result<&str, HiveError> {
        if self.hive_pubkey.is_empty() {
            return Err(HiveError::FailedPrecondition(
                "Beehive has no relay account yet: set BUZZ_HIVE_NOSTR_KEY and BUZZ_HIVE_RELAY_URL, then restart it"
                    .into(),
            ));
        }
        Ok(&self.hive_pubkey)
    }

    /// The code host, or the sentence to show when the service has no token for one.
    pub fn require_host(&self) -> Result<&dyn CodeHost, HiveError> {
        self.code_host.as_deref().ok_or_else(|| {
            HiveError::FailedPrecondition(
                "Beehive has no GitHub token yet: set GH_TOKEN, then restart it".into(),
            )
        })
    }

    /// The actor behind `pubkey`, with the level it holds right now.
    pub async fn resolve(&self, pubkey: &str) -> Result<Actor, HiveError> {
        let actor = resolve_actor(
            self.store.as_ref(),
            self.directory.as_ref(),
            self.community,
            pubkey,
        )
        .await?;
        Ok(actor)
    }

    /// Reads the saved workflows again after one changed, so the next call sees it. The change is
    /// already committed, so a read that fails is tried again a few times; when it keeps failing
    /// the caller is told, because the engine would otherwise serve the old version and refuse
    /// the person's next save as stale.
    pub async fn refresh_saved(&self) -> Result<(), HiveError> {
        let Some(saved) = self.catalog.saved() else {
            return Ok(());
        };
        let mut wait = std::time::Duration::from_millis(50);
        for attempt in 1..=REFRESH_TRIES {
            match saved.refresh(self.store.as_ref(), self.community).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    tracing::error!(%error, attempt, "could not read the saved workflows after a change");
                }
            }
            if attempt < REFRESH_TRIES {
                tokio::time::sleep(wait).await;
                wait *= 4;
            }
        }
        Err(HiveError::Unavailable(
            "saved, but the workflow list could not be refreshed; reload".into(),
        ))
    }

    /// An operation that never reads a workflow package: it skips the read of every package that
    /// [`App::op`] does, and wakes nothing.
    pub fn op_without_packages(&self) -> Op<'_> {
        Op {
            app: self,
            catalog: Catalog::default(),
            hive: self.hive_pubkey.clone(),
            queues: false,
        }
    }

    /// What an engine operation needs, and the one place that finishes it.
    ///
    /// An operation that can queue a post to the relay passes `needs_hive`, and fails here with
    /// the sentence to show when the service has no relay account. Its [`Op::settle`] wakes the
    /// outbox drain and the timer loop. The others read and write cards, artifacts and settings with no relay
    /// account at all, and wake nothing.
    pub fn op(&self, needs_hive: bool) -> Result<Op<'_>, HiveError> {
        let hive = if needs_hive {
            self.require_hive()?.to_owned()
        } else {
            self.hive_pubkey.clone()
        };
        let catalog = self.catalog.load();
        log_new_findings(&catalog);
        Ok(Op {
            app: self,
            catalog,
            hive,
            queues: needs_hive,
        })
    }
}

/// Logs each catalog finding at warn the first time it appears. The catalog is read on every
/// call, so a package that fails to load would otherwise be silent, or repeat on every call.
fn log_new_findings(catalog: &Catalog) {
    static LOGGED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let current: BTreeSet<String> = catalog
        .findings
        .iter()
        .map(|f| format!("{} [{}]: {}", f.path, f.code, f.message))
        .collect();
    let mut logged = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    for finding in current.difference(&logged) {
        tracing::warn!(%finding, "a workflow package did not load");
    }
    // Forget what is fixed, so a package that breaks again is logged again.
    *logged = current;
}

/// One engine call: the [`Env`] it runs in, and what to do once it has committed.
pub struct Op<'a> {
    app: &'a App,
    catalog: Catalog,
    hive: String,
    queues: bool,
}

impl Op<'_> {
    pub fn env(&self) -> Env<'_> {
        Env {
            store: self.app.store.as_ref(),
            directory: self.app.directory.as_ref(),
            catalog: &self.catalog,
            hive: &self.hive,
        }
    }

    /// Call after the operation committed. Wakes the parked polls for `events`, and the outbox
    /// drain when the operation may have queued a post.
    pub fn settle(&self, events: &[Event]) {
        self.app.bus.publish(events);
        if self.queues {
            self.app.outbox.notify_one();
            // A start, a report, a resume or a retry may have set a timer sooner than any the
            // loop is waiting for.
            self.app.timers.notify_one();
        }
    }
}
