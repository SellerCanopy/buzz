use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::access::{ActorKind, Level, RelayRole};
use crate::domain::artifact::{ArtifactInfo, ArtifactVersion};
use crate::domain::card::{Card, PullRequest};
use crate::domain::events::{check_new, Event};
use crate::domain::inbox::InboxItem;
use crate::domain::intake::{IntakeRule, Issue, IssueState, RepoRef, StoredIssue};
use crate::domain::run::{Run, RunState};
use crate::domain::secret::SecretNames;
use crate::domain::timer::{Timer, TimerKind};
use crate::domain::tool_server::ToolServer;
use crate::domain::usage::{week_start, Usage, UsageWeek};
use crate::ports::clock::{Clock, SystemClock};
use crate::ports::store::{
    AccessRepo, AccessRow, AgentInfo, AgentLink, AgentSecret, AgentToolsRepo, ArtifactSave,
    ArtifactsRepo, CardReposRepo, CardsRepo, CommunityId, Directory, EventsRepo, InboxRepo,
    IntakeRepo, IssuesRepo, Job, JobState, JobsRepo, KeptVersion, OutboxBacklog, OutboxRepo,
    OutboxRow, PersonInfo, Posting, Proposal, ProposalsRepo, Pubkey, PullRequestsRepo, RelayEvent,
    RunsRepo, SecretsRepo, SettingsRepo, StageThread, Store, StoreError, StoredPackage, TeamSecret,
    TimersRepo, Tx, UsageRepo, WorkflowsRepo, MESSAGE_KINDS,
};
use crate::workflow::options::WorkflowOptions;

#[derive(Default, Clone)]
struct State {
    access: HashMap<(CommunityId, Pubkey), (Level, ActorKind, String)>,
    settings: HashMap<(CommunityId, String, String, String), serde_json::Value>,
    /// In creation order, so the newest card is last.
    cards: Vec<(CommunityId, Card)>,
    /// `(community, name)` to `(builtin, current hash)`.
    packages: std::collections::BTreeMap<(CommunityId, String), (bool, String)>,
    versions: std::collections::BTreeMap<(CommunityId, String, String), serde_json::Value>,
    options: std::collections::BTreeMap<(CommunityId, String), WorkflowOptions>,
    /// In creation order, so the newest run is last.
    runs: Vec<(CommunityId, Run)>,
    threads: Vec<(CommunityId, StageThread)>,
    /// In queue order.
    outbox: Vec<(CommunityId, OutboxRow)>,
    next_outbox_id: i64,
    /// Each artifact with its handoff flag, and its versions in save order.
    artifacts: Vec<MemArtifact>,
    timers: Vec<(CommunityId, MemTimer)>,
    /// In the order opened.
    inbox: Vec<(CommunityId, InboxItem)>,
    /// In creation order.
    intake: Vec<(CommunityId, IntakeRule)>,
    issues: Vec<(CommunityId, StoredIssue)>,
    /// When each issue was last read, by community, repo and number.
    issue_seen:
        std::collections::BTreeMap<(CommunityId, String, u64), chrono::DateTime<chrono::Utc>>,
    pulls: std::collections::BTreeMap<(CommunityId, uuid::Uuid), PullRequest>,
    /// Proposals in creation order, with the time each was resolved.
    proposals: Vec<(CommunityId, Proposal, bool)>,
    next_proposal_id: i64,
    /// In the order added.
    card_repos: Vec<(CommunityId, uuid::Uuid, String)>,
    /// The agents seeded, and their tool servers by agent and name.
    tool_agents: std::collections::BTreeSet<(CommunityId, String)>,
    tool_servers: std::collections::BTreeMap<(CommunityId, String, String), ToolServer>,
    /// Secret names, never values: team secrets by name, agents' own by agent and name, and
    /// the grants as `(agent, var)`.
    team_secrets:
        std::collections::BTreeMap<(CommunityId, String), (String, chrono::DateTime<chrono::Utc>)>,
    agent_secrets: std::collections::BTreeMap<
        (CommunityId, String, String),
        (String, chrono::DateTime<chrono::Utc>),
    >,
    secret_grants: std::collections::BTreeSet<(CommunityId, String, String)>,
    /// In creation order, each with when it ends as failed.
    jobs: Vec<(CommunityId, Job, Option<chrono::DateTime<chrono::Utc>>)>,
}

#[derive(Clone)]
struct MemTimer {
    timer: Timer,
    fire_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
struct MemArtifact {
    community: CommunityId,
    card: uuid::Uuid,
    name: String,
    handoff: bool,
    versions: Vec<ArtifactVersion>,
}

/// The committed event log, shared by every transaction. It sits outside `State` so
/// that a commit adds to it instead of overwriting events another commit added.
#[derive(Default)]
struct Log {
    /// In seq order.
    events: Vec<(CommunityId, Event)>,
    /// Every kind ever committed, for the coverage test.
    kinds: std::collections::BTreeSet<String>,
}

/// A `Store` held in memory. `begin` copies the state and `commit` swaps the
/// copy back in, so a dropped `Tx` rolls back.
#[derive(Clone)]
pub struct MemStore {
    state: Arc<Mutex<State>>,
    log: Arc<Mutex<Log>>,
    clock: Arc<dyn Clock>,
    /// How many reads of the kept workflow versions, and how many commits, still fail, for a
    /// test of a store that cannot be reached.
    failures: Arc<Failures>,
}

#[derive(Default)]
struct Failures {
    kept_versions: std::sync::atomic::AtomicUsize,
    commits: std::sync::atomic::AtomicUsize,
}

impl Failures {
    /// Uses up one failure of `counter`, and says whether there was one.
    fn take(counter: &std::sync::atomic::AtomicUsize) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        counter
            .fetch_update(SeqCst, SeqCst, |left| left.checked_sub(1))
            .is_ok()
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store whose timers read `clock`, so a test moves time by hand.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::default(),
            log: Arc::default(),
            clock,
            failures: Arc::default(),
        }
    }

    /// Makes the next `times` reads of the kept workflow versions fail as unavailable.
    pub fn fail_kept_versions(&self, times: usize) {
        self.failures
            .kept_versions
            .store(times, std::sync::atomic::Ordering::SeqCst);
    }

    /// Makes the next `times` commits fail as unavailable, and change nothing.
    pub fn fail_next_commits(&self, times: usize) {
        self.failures
            .commits
            .store(times, std::sync::atomic::Ordering::SeqCst);
    }

    /// Puts a card on a community's board, as the newest. Later phases create cards through routes.
    pub fn seed_card(&self, community: CommunityId, card: Card) {
        self.state.lock().unwrap().cards.push((community, card));
    }

    /// Every event kind committed so far, across all communities.
    pub fn committed_kinds(&self) -> std::collections::BTreeSet<String> {
        self.log.lock().unwrap().kinds.clone()
    }
}

#[async_trait::async_trait]
impl Store for MemStore {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        let work = self.state.lock().unwrap().clone();
        Ok(Box::new(MemTx {
            community,
            work,
            pending: Vec::new(),
            savepoints: Vec::new(),
            shared: self.state.clone(),
            log: self.log.clone(),
            clock: self.clock.clone(),
            failures: self.failures.clone(),
        }))
    }
}

struct MemTx {
    community: CommunityId,
    work: State,
    /// Appended in this transaction, with no seq yet.
    pending: Vec<Event>,
    /// The state and the pending count at each open savepoint, newest last.
    savepoints: Vec<(State, usize)>,
    shared: Arc<Mutex<State>>,
    log: Arc<Mutex<Log>>,
    clock: Arc<dyn Clock>,
    failures: Arc<Failures>,
}

#[async_trait::async_trait]
impl Tx for MemTx {
    fn access(&mut self) -> &mut dyn AccessRepo {
        self
    }
    fn settings(&mut self) -> &mut dyn SettingsRepo {
        self
    }
    fn cards(&mut self) -> &mut dyn CardsRepo {
        self
    }
    fn events(&mut self) -> &mut dyn EventsRepo {
        self
    }
    fn workflows(&mut self) -> &mut dyn WorkflowsRepo {
        self
    }
    fn runs(&mut self) -> &mut dyn RunsRepo {
        self
    }
    fn outbox(&mut self) -> &mut dyn OutboxRepo {
        self
    }
    fn artifacts(&mut self) -> &mut dyn ArtifactsRepo {
        self
    }
    fn timers(&mut self) -> &mut dyn TimersRepo {
        self
    }
    fn inbox(&mut self) -> &mut dyn InboxRepo {
        self
    }
    fn usage(&mut self) -> &mut dyn UsageRepo {
        self
    }
    fn intake(&mut self) -> &mut dyn IntakeRepo {
        self
    }
    fn issues(&mut self) -> &mut dyn IssuesRepo {
        self
    }
    fn pull_requests(&mut self) -> &mut dyn PullRequestsRepo {
        self
    }
    fn proposals(&mut self) -> &mut dyn ProposalsRepo {
        self
    }
    fn card_repos(&mut self) -> &mut dyn CardReposRepo {
        self
    }
    fn agent_tools(&mut self) -> &mut dyn AgentToolsRepo {
        self
    }
    fn secrets(&mut self) -> &mut dyn SecretsRepo {
        self
    }
    fn jobs(&mut self) -> &mut dyn JobsRepo {
        self
    }
    async fn savepoint(&mut self) -> Result<(), StoreError> {
        self.savepoints
            .push((self.work.clone(), self.pending.len()));
        Ok(())
    }
    async fn rollback_savepoint(&mut self) -> Result<(), StoreError> {
        let (work, pending) = self
            .savepoints
            .pop()
            .ok_or_else(|| StoreError::Conflict("no savepoint is open".into()))?;
        self.work = work;
        self.pending.truncate(pending);
        Ok(())
    }
    async fn release_savepoint(&mut self) -> Result<(), StoreError> {
        self.savepoints
            .pop()
            .map(|_| ())
            .ok_or_else(|| StoreError::Conflict("no savepoint is open".into()))
    }
    async fn commit(self: Box<Self>) -> Result<Vec<Event>, StoreError> {
        let MemTx {
            community,
            work,
            pending,
            savepoints: _,
            shared,
            log,
            clock: _,
            failures,
        } = *self;
        if Failures::take(&failures.commits) {
            return Err(StoreError::Unavailable("the store is down".into()));
        }
        *shared.lock().unwrap() = work;
        let mut log = log.lock().unwrap();
        let mut committed = Vec::with_capacity(pending.len());
        for mut event in pending {
            event.seq = log.events.last().map_or(0, |(_, e)| e.seq) + 1;
            log.kinds.insert(event.kind.clone());
            log.events.push((community, event.clone()));
            committed.push(event);
        }
        Ok(committed)
    }
}

#[async_trait::async_trait]
impl EventsRepo for MemTx {
    /// Transactions here work on a copy and never wait on each other, so there is nothing to
    /// take. The Postgres tests show the lock holds.
    async fn lock_log(&mut self) -> Result<(), StoreError> {
        Ok(())
    }

    async fn append(&mut self, event: Event) -> Result<(), StoreError> {
        check_new(&event.kind, &event.payload).map_err(|e| StoreError::Conflict(e.to_string()))?;
        self.pending.push(event);
        Ok(())
    }

    async fn after(&mut self, seq: i64, limit: i64) -> Result<Vec<Event>, StoreError> {
        let log = self.log.lock().unwrap();
        Ok(log
            .events
            .iter()
            .filter(|(community, event)| *community == self.community && event.seq > seq)
            .take(limit.max(0) as usize)
            .map(|(_, event)| event.clone())
            .collect())
    }

    async fn latest_seq(&mut self) -> Result<i64, StoreError> {
        let log = self.log.lock().unwrap();
        Ok(log
            .events
            .iter()
            .rev()
            .find(|(community, _)| *community == self.community)
            .map_or(0, |(_, event)| event.seq))
    }

    async fn of_card(
        &mut self,
        card: uuid::Uuid,
        kind: &str,
        seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError> {
        let log = self.log.lock().unwrap();
        Ok(log
            .events
            .iter()
            .filter(|(community, event)| {
                *community == self.community
                    && event.seq > seq
                    && event.kind == kind
                    && event.card_id == Some(card)
            })
            .take(limit.max(0) as usize)
            .map(|(_, event)| event.clone())
            .collect())
    }

    async fn of_run(
        &mut self,
        card: uuid::Uuid,
        run: uuid::Uuid,
        seq: i64,
        limit: i64,
    ) -> Result<Vec<Event>, StoreError> {
        let log = self.log.lock().unwrap();
        Ok(log
            .events
            .iter()
            .filter(|(community, event)| {
                *community == self.community
                    && event.seq > seq
                    && event.card_id == Some(card)
                    && event.run_id == Some(run)
            })
            .take(limit.max(0) as usize)
            .map(|(_, event)| event.clone())
            .collect())
    }
}

#[async_trait::async_trait]
impl ArtifactsRepo for MemTx {
    async fn save(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        handoff: bool,
        save: &ArtifactSave,
    ) -> Result<ArtifactVersion, StoreError> {
        let community = self.community;
        let at = self
            .work
            .artifacts
            .iter()
            .position(|a| a.community == community && a.card == card && a.name == name);
        let at = match at {
            Some(at) => at,
            None => {
                self.work.artifacts.push(MemArtifact {
                    community,
                    card,
                    name: name.to_owned(),
                    handoff,
                    versions: Vec::new(),
                });
                self.work.artifacts.len() - 1
            }
        };
        let artifact = &mut self.work.artifacts[at];
        if let Some(last) = artifact.versions.last() {
            if last.content.kind() != save.content.kind() {
                return Err(StoreError::Conflict(format!(
                    "artifact {name} is a {} artifact",
                    last.content.kind().as_str()
                )));
            }
        }
        let version = ArtifactVersion {
            card_id: card,
            name: name.to_owned(),
            version: artifact.versions.len() as i32 + 1,
            content: save.content.clone(),
            stage: save.stage.clone(),
            visit: save.visit,
            created_by: save.created_by.clone(),
            created_at: chrono::Utc::now(),
        };
        artifact.versions.push(version.clone());
        Ok(version)
    }

    async fn list(&mut self, card: uuid::Uuid) -> Result<Vec<ArtifactInfo>, StoreError> {
        let mut out: Vec<ArtifactInfo> = self
            .work
            .artifacts
            .iter()
            .filter(|a| a.community == self.community && a.card == card)
            .filter_map(|a| {
                let last = a.versions.last()?;
                Some(ArtifactInfo {
                    name: a.name.clone(),
                    kind: last.content.kind(),
                    latest: last.version,
                    handoff: a.handoff,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn versions(
        &mut self,
        card: uuid::Uuid,
        name: &str,
    ) -> Result<Vec<ArtifactVersion>, StoreError> {
        Ok(self
            .work
            .artifacts
            .iter()
            .find(|a| a.community == self.community && a.card == card && a.name == name)
            .map(|a| a.versions.clone())
            .unwrap_or_default())
    }

    async fn version(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        version: Option<i32>,
    ) -> Result<Option<ArtifactVersion>, StoreError> {
        let versions = self.versions(card, name).await?;
        Ok(match version {
            Some(n) => versions.into_iter().find(|v| v.version == n),
            None => versions.into_iter().last(),
        })
    }

    async fn set_handoff(
        &mut self,
        card: uuid::Uuid,
        name: &str,
        handoff: bool,
    ) -> Result<(), StoreError> {
        let community = self.community;
        match self
            .work
            .artifacts
            .iter_mut()
            .find(|a| a.community == community && a.card == card && a.name == name)
        {
            Some(a) => {
                a.handoff = handoff;
                Ok(())
            }
            None => Err(StoreError::NotFound(format!("artifact {name}"))),
        }
    }

    async fn saved_in(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Vec<String>, StoreError> {
        let mut names: Vec<String> = self
            .work
            .artifacts
            .iter()
            .filter(|a| a.community == self.community && a.card == card)
            .filter(|a| {
                a.versions
                    .iter()
                    .any(|v| v.stage == stage && v.visit == visit)
            })
            .map(|a| a.name.clone())
            .collect();
        names.sort();
        Ok(names)
    }
}

#[async_trait::async_trait]
impl AccessRepo for MemTx {
    async fn level(&mut self, pubkey: &str) -> Result<Option<(Level, ActorKind)>, StoreError> {
        Ok(self
            .work
            .access
            .get(&(self.community, pubkey.to_owned()))
            .map(|(level, kind, _)| (*level, *kind)))
    }

    async fn set_level(
        &mut self,
        pubkey: &str,
        level: Level,
        kind: ActorKind,
        by: &str,
    ) -> Result<(), StoreError> {
        self.work.access.insert(
            (self.community, pubkey.to_owned()),
            (level, kind, by.to_owned()),
        );
        Ok(())
    }

    async fn list(&mut self) -> Result<Vec<AccessRow>, StoreError> {
        let mut rows: Vec<AccessRow> = self
            .work
            .access
            .iter()
            .filter(|((community, _), _)| *community == self.community)
            .map(|((_, pubkey), (level, kind, _))| AccessRow {
                pubkey: pubkey.clone(),
                level: *level,
                kind: *kind,
            })
            .collect();
        rows.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
        Ok(rows)
    }

    /// Transactions here work on a copy and never wait on each other, so there is nothing to
    /// take. Postgres serialises access changes, and its test shows it.
    async fn lock(&mut self) -> Result<(), StoreError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl SettingsRepo for MemTx {
    async fn get(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        Ok(self
            .work
            .settings
            .get(&(
                self.community,
                scope.to_owned(),
                scope_id.to_owned(),
                key.to_owned(),
            ))
            .cloned())
    }

    async fn set(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), StoreError> {
        self.work.settings.insert(
            (
                self.community,
                scope.to_owned(),
                scope_id.to_owned(),
                key.to_owned(),
            ),
            value,
        );
        Ok(())
    }

    async fn remove(&mut self, scope: &str, scope_id: &str, key: &str) -> Result<(), StoreError> {
        self.work.settings.remove(&(
            self.community,
            scope.to_owned(),
            scope_id.to_owned(),
            key.to_owned(),
        ));
        Ok(())
    }

    async fn list(
        &mut self,
        scope: &str,
        scope_id: &str,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, StoreError> {
        Ok(self
            .work
            .settings
            .iter()
            .filter(|((community, s, id, _), _)| {
                *community == self.community && s == scope && id == scope_id
            })
            .map(|((_, _, _, key), value)| (key.clone(), value.clone()))
            .collect())
    }
}

#[async_trait::async_trait]
impl RunsRepo for MemTx {
    async fn insert(&mut self, run: &Run) -> Result<(), StoreError> {
        if self.find_run(run.id).is_some() {
            return Err(StoreError::Conflict(format!("run {} exists", run.id)));
        }
        self.work.runs.push((self.community, run.clone()));
        Ok(())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        Ok(self.find_run(id).cloned())
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        Ok(self.find_run(id).cloned())
    }

    async fn update(&mut self, run: &Run) -> Result<(), StoreError> {
        let community = self.community;
        let found = self
            .work
            .runs
            .iter_mut()
            .find(|(c, r)| *c == community && r.id == run.id)
            .map(|(_, r)| r)
            .ok_or_else(|| StoreError::NotFound(format!("run {}", run.id)))?;
        *found = run.clone();
        Ok(())
    }

    async fn list(&mut self, card: Option<uuid::Uuid>) -> Result<Vec<Run>, StoreError> {
        Ok(self
            .work
            .runs
            .iter()
            .rev()
            .filter(|(c, r)| *c == self.community && card.is_none_or(|id| r.card_id == id))
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn active(&mut self) -> Result<Vec<Run>, StoreError> {
        Ok(self
            .work
            .runs
            .iter()
            .filter(|(c, r)| *c == self.community && r.state != RunState::Ended)
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn active_for_card(&mut self, card: uuid::Uuid) -> Result<Option<Run>, StoreError> {
        Ok(self
            .work
            .runs
            .iter()
            .rev()
            .find(|(c, r)| *c == self.community && r.card_id == card && r.state != RunState::Ended)
            .map(|(_, r)| r.clone()))
    }

    async fn visit_counts(
        &mut self,
        card: uuid::Uuid,
    ) -> Result<std::collections::BTreeMap<String, u32>, StoreError> {
        let mut counts = std::collections::BTreeMap::<String, u32>::new();
        let mut note = |stage: &str, visit: u32| {
            let seen = counts.entry(stage.to_owned()).or_default();
            *seen = (*seen).max(visit);
        };
        for (_, run) in self
            .work
            .runs
            .iter()
            .filter(|(c, r)| *c == self.community && r.card_id == card)
        {
            if let Some(stage) = &run.current_stage {
                note(stage, run.visit);
            }
            for step in &run.steps {
                note(&step.stage, step.visit);
            }
        }
        Ok(counts)
    }

    async fn record_thread(&mut self, thread: &StageThread) -> Result<(), StoreError> {
        let community = self.community;
        let known = self.work.threads.iter().any(|(c, t)| {
            *c == community
                && t.card_id == thread.card_id
                && t.stage == thread.stage
                && t.visit == thread.visit
        });
        if !known {
            self.work.threads.push((community, thread.clone()));
        }
        Ok(())
    }

    async fn thread(
        &mut self,
        card: uuid::Uuid,
        stage: &str,
        visit: u32,
    ) -> Result<Option<StageThread>, StoreError> {
        Ok(self
            .work
            .threads
            .iter()
            .find(|(c, t)| {
                *c == self.community && t.card_id == card && t.stage == stage && t.visit == visit
            })
            .map(|(_, t)| t.clone()))
    }

    async fn threads(&mut self, card: uuid::Uuid) -> Result<Vec<StageThread>, StoreError> {
        Ok(self
            .work
            .threads
            .iter()
            .filter(|(c, t)| *c == self.community && t.card_id == card)
            .map(|(_, t)| t.clone())
            .collect())
    }

    async fn thread_by_root(&mut self, root: &str) -> Result<Option<StageThread>, StoreError> {
        Ok(self
            .work
            .threads
            .iter()
            .find(|(c, t)| *c == self.community && t.root_event_id == root)
            .map(|(_, t)| t.clone()))
    }
}

#[async_trait::async_trait]
impl OutboxRepo for MemTx {
    async fn enqueue(
        &mut self,
        key: &str,
        card: uuid::Uuid,
        posting: &Posting,
    ) -> Result<bool, StoreError> {
        let community = self.community;
        if self
            .work
            .outbox
            .iter()
            .any(|(c, row)| *c == community && row.key == key)
        {
            return Ok(false);
        }
        self.work.next_outbox_id += 1;
        let row = OutboxRow {
            id: self.work.next_outbox_id,
            key: key.to_owned(),
            card_id: card,
            posting: posting.clone(),
            created_at: chrono::Utc::now(),
            done_at: None,
            claimed_at: None,
            attempts: 0,
            last_error: None,
        };
        self.work.outbox.push((community, row));
        Ok(true)
    }

    async fn claim_next(
        &mut self,
        skip_cards: &[uuid::Uuid],
        lease: std::time::Duration,
    ) -> Result<Option<OutboxRow>, StoreError> {
        let now = chrono::Utc::now();
        let community = self.community;
        let pending: Vec<(i64, uuid::Uuid, Option<chrono::DateTime<chrono::Utc>>)> = self
            .work
            .outbox
            .iter()
            .filter(|(c, row)| *c == community && row.done_at.is_none())
            .map(|(_, row)| (row.id, row.card_id, row.claimed_at))
            .collect();
        let live = |at: Option<chrono::DateTime<chrono::Utc>>| {
            at.is_some_and(|at| now - at < chrono::Duration::from_std(lease).unwrap_or_default())
        };
        // Rows are in id order, so the first pending row of a card is its oldest.
        let mut seen: Vec<uuid::Uuid> = Vec::new();
        let mut pick = None;
        for (id, card, claimed) in pending {
            if seen.contains(&card) {
                continue;
            }
            seen.push(card);
            if !skip_cards.contains(&card) && !live(claimed) {
                pick = Some(id);
                break;
            }
        }
        let Some(id) = pick else {
            return Ok(None);
        };
        let row = self.outbox_row(id)?;
        row.claimed_at = Some(now);
        Ok(Some(row.clone()))
    }

    async fn mark_done(&mut self, id: i64) -> Result<(), StoreError> {
        let row = self.outbox_row(id)?;
        row.done_at = Some(chrono::Utc::now());
        row.claimed_at = None;
        Ok(())
    }

    async fn mark_failed(&mut self, id: i64, error: &str) -> Result<(), StoreError> {
        let row = self.outbox_row(id)?;
        row.attempts += 1;
        row.last_error = Some(error.to_owned());
        row.claimed_at = None;
        Ok(())
    }

    async fn all(&mut self) -> Result<Vec<OutboxRow>, StoreError> {
        Ok(self
            .work
            .outbox
            .iter()
            .filter(|(c, _)| *c == self.community)
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn backlog(&mut self) -> Result<OutboxBacklog, StoreError> {
        let waiting = self
            .work
            .outbox
            .iter()
            .filter(|(c, row)| *c == self.community && row.done_at.is_none());
        let (mut pending, mut failed, mut oldest) = (0, 0, None);
        for (_, row) in waiting {
            pending += 1;
            if row.attempts > 0 {
                failed += 1;
            }
            oldest = oldest.min(Some(row.created_at)).or(Some(row.created_at));
        }
        Ok(OutboxBacklog {
            pending,
            failed,
            oldest,
        })
    }

    async fn by_key(&mut self, key: &str) -> Result<Option<OutboxRow>, StoreError> {
        Ok(self
            .work
            .outbox
            .iter()
            .find(|(c, row)| *c == self.community && row.key == key)
            .map(|(_, row)| row.clone()))
    }
}

#[async_trait::async_trait]
impl CardsRepo for MemTx {
    async fn list(&mut self, archived: bool) -> Result<Vec<Card>, StoreError> {
        Ok(self
            .work
            .cards
            .iter()
            .rev()
            .filter(|(community, card)| *community == self.community && card.archived == archived)
            .map(|(_, card)| card.clone())
            .collect())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        Ok(self.find(id).map(|(_, card)| card.clone()))
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Card>, StoreError> {
        Ok(self.find(id).map(|(_, card)| card.clone()))
    }

    async fn insert(&mut self, card: &Card) -> Result<(), StoreError> {
        if self.find(card.id).is_some() {
            return Err(StoreError::Conflict(format!("card {} exists", card.id)));
        }
        self.work.cards.push((self.community, card.clone()));
        Ok(())
    }

    async fn update(&mut self, card: &Card) -> Result<(), StoreError> {
        let found = self.find_mut(card.id)?;
        found.title = card.title.clone();
        found.description = card.description.clone();
        found.state = card.state;
        found.workflow = card.workflow.clone();
        Ok(())
    }

    async fn set_archived(&mut self, id: uuid::Uuid, archived: bool) -> Result<(), StoreError> {
        self.find_mut(id)?.archived = archived;
        Ok(())
    }

    async fn delete(&mut self, id: uuid::Uuid) -> Result<(), StoreError> {
        let before = self.work.cards.len();
        let community = self.community;
        self.work
            .cards
            .retain(|(c, card)| !(*c == community && card.id == id));
        if self.work.cards.len() == before {
            return Err(StoreError::NotFound(format!("card {id}")));
        }
        self.work
            .artifacts
            .retain(|a| !(a.community == community && a.card == id));
        self.work
            .inbox
            .retain(|(c, i)| !(*c == community && i.card_id == id));
        self.work.pulls.remove(&(community, id));
        self.work
            .card_repos
            .retain(|(c, card, _)| !(*c == community && *card == id));
        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkflowsRepo for MemTx {
    async fn record_version(
        &mut self,
        name: &str,
        hash: &str,
        body: serde_json::Value,
        _created_by: &str,
        builtin: bool,
    ) -> Result<(), StoreError> {
        let community = self.community;
        self.work
            .versions
            .entry((community, name.to_owned(), hash.to_owned()))
            .or_insert(body);
        self.work
            .packages
            .insert((community, name.to_owned()), (builtin, hash.to_owned()));
        Ok(())
    }

    async fn packages(&mut self) -> Result<Vec<StoredPackage>, StoreError> {
        Ok(self
            .work
            .packages
            .iter()
            .filter(|((community, _), _)| *community == self.community)
            .map(|((_, name), (builtin, hash))| StoredPackage {
                name: name.clone(),
                builtin: *builtin,
                current_hash: hash.clone(),
            })
            .collect())
    }

    async fn version(
        &mut self,
        name: &str,
        hash: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        Ok(self
            .work
            .versions
            .get(&(self.community, name.to_owned(), hash.to_owned()))
            .cloned())
    }

    async fn options(
        &mut self,
    ) -> Result<std::collections::BTreeMap<String, WorkflowOptions>, StoreError> {
        Ok(self
            .work
            .options
            .iter()
            .filter(|((community, _), _)| *community == self.community)
            .map(|((_, name), options)| (name.clone(), options.clone()))
            .collect())
    }

    async fn set_options(
        &mut self,
        name: &str,
        options: &WorkflowOptions,
    ) -> Result<(), StoreError> {
        self.work
            .options
            .insert((self.community, name.to_owned()), options.clone());
        Ok(())
    }

    async fn save_version(
        &mut self,
        name: &str,
        base: Option<&str>,
        hash: &str,
        body: serde_json::Value,
        _created_by: &str,
    ) -> Result<(), StoreError> {
        let key = (self.community, name.to_owned());
        match (self.work.packages.get(&key), base) {
            (None, None) => {}
            (Some((false, current)), Some(base)) if current == base => {}
            _ => {
                return Err(StoreError::Conflict(
                    "the workflow changed since you opened it; reload and try again".into(),
                ))
            }
        }
        self.work
            .versions
            .entry((self.community, name.to_owned(), hash.to_owned()))
            .or_insert(body);
        self.work.packages.insert(key, (false, hash.to_owned()));
        Ok(())
    }

    async fn delete_package(&mut self, name: &str) -> Result<(), StoreError> {
        let key = (self.community, name.to_owned());
        self.work.options.remove(&key);
        self.work
            .packages
            .remove(&key)
            .map(|_| ())
            .ok_or_else(|| StoreError::NotFound(format!("workflow {name} is not stored")))
    }

    async fn kept_versions(&mut self) -> Result<Vec<KeptVersion>, StoreError> {
        if Failures::take(&self.failures.kept_versions) {
            return Err(StoreError::Unavailable("the store is down".into()));
        }
        let community = self.community;
        let active: Vec<(String, String)> = self
            .work
            .runs
            .iter()
            .filter(|(c, run)| *c == community && run.state != RunState::Ended)
            .map(|(_, run)| (run.workflow.clone(), run.workflow_hash.clone()))
            .collect();
        let mut kept = Vec::new();
        for ((c, name, hash), body) in &self.work.versions {
            if *c != community {
                continue;
            }
            let package = self.work.packages.get(&(community, name.clone()));
            let current = package.is_some_and(|(_, current)| current == hash);
            let builtin = package.is_some_and(|(builtin, _)| *builtin);
            let used = active.iter().any(|(n, h)| n == name && h == hash);
            if (current && !builtin) || used {
                kept.push(KeptVersion {
                    name: name.clone(),
                    hash: hash.clone(),
                    body: body.clone(),
                    current,
                    builtin,
                });
            }
        }
        Ok(kept)
    }
}

#[async_trait::async_trait]
impl ProposalsRepo for MemTx {
    async fn insert(
        &mut self,
        kind: &str,
        body: serde_json::Value,
        requester: &str,
    ) -> Result<i64, StoreError> {
        self.work.next_proposal_id += 1;
        let id = self.work.next_proposal_id;
        let proposal = Proposal {
            id,
            kind: kind.to_owned(),
            body,
            requester: requester.to_owned(),
        };
        self.work.proposals.push((self.community, proposal, false));
        Ok(id)
    }

    async fn open(&mut self, kind: &str) -> Result<Vec<Proposal>, StoreError> {
        Ok(self
            .work
            .proposals
            .iter()
            .filter(|(c, p, resolved)| *c == self.community && p.kind == kind && !resolved)
            .map(|(_, p, _)| p.clone())
            .collect())
    }

    async fn open_all(&mut self) -> Result<Vec<Proposal>, StoreError> {
        Ok(self
            .work
            .proposals
            .iter()
            .filter(|(c, _, resolved)| *c == self.community && !resolved)
            .map(|(_, p, _)| p.clone())
            .collect())
    }

    async fn get(&mut self, id: i64) -> Result<Option<Proposal>, StoreError> {
        Ok(self
            .work
            .proposals
            .iter()
            .find(|(c, p, _)| *c == self.community && p.id == id)
            .map(|(_, p, _)| p.clone()))
    }

    async fn resolve(&mut self, id: i64) -> Result<bool, StoreError> {
        let community = self.community;
        let open = self
            .work
            .proposals
            .iter_mut()
            .find(|(c, p, resolved)| *c == community && p.id == id && !resolved);
        Ok(open.map(|(_, _, resolved)| *resolved = true).is_some())
    }

    async fn reopen(&mut self, id: i64) -> Result<bool, StoreError> {
        let community = self.community;
        let decided = self
            .work
            .proposals
            .iter_mut()
            .find(|(c, p, resolved)| *c == community && p.id == id && *resolved);
        Ok(decided.map(|(_, _, resolved)| *resolved = false).is_some())
    }
}

#[async_trait::async_trait]
impl CardReposRepo for MemTx {
    async fn of_card(&mut self, card: uuid::Uuid) -> Result<Vec<String>, StoreError> {
        Ok(self
            .work
            .card_repos
            .iter()
            .filter(|(c, id, _)| *c == self.community && *id == card)
            .map(|(_, _, repo)| repo.clone())
            .collect())
    }

    async fn add(&mut self, card: uuid::Uuid, repo: &str) -> Result<(), StoreError> {
        let community = self.community;
        if self.find(card).is_none() {
            return Err(StoreError::NotFound(format!("card {card}")));
        }
        if self
            .work
            .card_repos
            .iter()
            .any(|(c, id, have)| *c == community && *id == card && have == repo)
        {
            return Err(StoreError::Conflict(format!(
                "{repo} is already on the card"
            )));
        }
        self.work
            .card_repos
            .push((community, card, repo.to_owned()));
        Ok(())
    }

    async fn remove(&mut self, card: uuid::Uuid, repo: &str) -> Result<bool, StoreError> {
        let community = self.community;
        let before = self.work.card_repos.len();
        self.work
            .card_repos
            .retain(|(c, id, have)| !(*c == community && *id == card && have == repo));
        Ok(self.work.card_repos.len() < before)
    }
}

#[async_trait::async_trait]
impl AgentToolsRepo for MemTx {
    async fn agents(&mut self) -> Result<Vec<String>, StoreError> {
        Ok(self
            .work
            .tool_agents
            .iter()
            .filter(|(c, _)| *c == self.community)
            .map(|(_, agent)| agent.clone())
            .collect())
    }

    async fn list(&mut self) -> Result<Vec<(String, ToolServer)>, StoreError> {
        Ok(self
            .work
            .tool_servers
            .iter()
            .filter(|((c, _, _), _)| *c == self.community)
            .map(|((_, agent, _), server)| (agent.clone(), server.clone()))
            .collect())
    }

    async fn of_agent(&mut self, agent: &str) -> Result<Vec<ToolServer>, StoreError> {
        Ok(self
            .work
            .tool_servers
            .iter()
            .filter(|((c, a, _), _)| *c == self.community && a == agent)
            .map(|(_, server)| server.clone())
            .collect())
    }

    async fn set(&mut self, agent: &str, server: &ToolServer) -> Result<(), StoreError> {
        self.work.tool_servers.insert(
            (self.community, agent.to_owned(), server.name.clone()),
            server.clone(),
        );
        Ok(())
    }

    async fn remove(&mut self, agent: &str, name: &str) -> Result<bool, StoreError> {
        Ok(self
            .work
            .tool_servers
            .remove(&(self.community, agent.to_owned(), name.to_owned()))
            .is_some())
    }

    async fn seed(&mut self, agent: &str, servers: &[ToolServer]) -> Result<bool, StoreError> {
        if !self
            .work
            .tool_agents
            .insert((self.community, agent.to_owned()))
        {
            return Ok(false);
        }
        for server in servers {
            AgentToolsRepo::set(self, agent, server).await?;
        }
        Ok(true)
    }
}

#[async_trait::async_trait]
impl SecretsRepo for MemTx {
    async fn set_team(&mut self, var: &str, by: &str) -> Result<(), StoreError> {
        let at = self.clock.now();
        self.work
            .team_secrets
            .insert((self.community, var.to_owned()), (by.to_owned(), at));
        Ok(())
    }

    async fn set_agent(&mut self, agent: &str, var: &str, by: &str) -> Result<(), StoreError> {
        let at = self.clock.now();
        self.work.agent_secrets.insert(
            (self.community, agent.to_owned(), var.to_owned()),
            (by.to_owned(), at),
        );
        Ok(())
    }

    async fn team(&mut self) -> Result<Vec<TeamSecret>, StoreError> {
        Ok(self
            .work
            .team_secrets
            .iter()
            .filter(|((c, _), _)| *c == self.community)
            .map(|((_, var), (by, at))| TeamSecret {
                var: var.clone(),
                updated_by: by.clone(),
                updated_at: *at,
            })
            .collect())
    }

    async fn agent_secrets(&mut self) -> Result<Vec<AgentSecret>, StoreError> {
        Ok(self
            .work
            .agent_secrets
            .iter()
            .filter(|((c, _, _), _)| *c == self.community)
            .map(|((_, agent, var), (by, at))| AgentSecret {
                agent: agent.clone(),
                var: var.clone(),
                updated_by: by.clone(),
                updated_at: *at,
            })
            .collect())
    }

    async fn grants(&mut self) -> Result<Vec<(String, String)>, StoreError> {
        Ok(self
            .work
            .secret_grants
            .iter()
            .filter(|(c, _, _)| *c == self.community)
            .map(|(_, agent, var)| (agent.clone(), var.clone()))
            .collect())
    }

    async fn grant(&mut self, agent: &str, var: &str) -> Result<bool, StoreError> {
        if !self
            .work
            .team_secrets
            .contains_key(&(self.community, var.to_owned()))
        {
            return Err(StoreError::NotFound(format!("no team secret named {var}")));
        }
        Ok(self
            .work
            .secret_grants
            .insert((self.community, agent.to_owned(), var.to_owned())))
    }

    async fn revoke(&mut self, agent: &str, var: &str) -> Result<bool, StoreError> {
        Ok(self
            .work
            .secret_grants
            .remove(&(self.community, agent.to_owned(), var.to_owned())))
    }

    async fn names_of(&mut self, agent: &str) -> Result<SecretNames, StoreError> {
        Ok(SecretNames {
            own: self
                .work
                .agent_secrets
                .keys()
                .filter(|(c, a, _)| *c == self.community && a == agent)
                .map(|(_, _, var)| var.clone())
                .collect(),
            team: self
                .work
                .secret_grants
                .iter()
                .filter(|(c, a, _)| *c == self.community && a == agent)
                .map(|(_, _, var)| var.clone())
                .collect(),
        })
    }
}

impl MemTx {
    /// `Conflict` when a running job other than `id` works in the thread `root`.
    fn check_thread_free(&self, id: uuid::Uuid, root: Option<&String>) -> Result<(), StoreError> {
        let taken = root.is_some_and(|root| {
            self.work.jobs.iter().any(|(c, j, _)| {
                *c == self.community
                    && j.id != id
                    && j.state == JobState::Running
                    && j.root_event_id.as_ref() == Some(root)
            })
        });
        if taken {
            return Err(StoreError::Conflict("that already exists".into()));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl JobsRepo for MemTx {
    async fn insert(
        &mut self,
        job: &Job,
        deadline_in: Option<std::time::Duration>,
    ) -> Result<(), StoreError> {
        let taken = self
            .work
            .jobs
            .iter()
            .any(|(c, j, _)| *c == self.community && j.id == job.id);
        if taken {
            return Err(StoreError::Conflict("that already exists".into()));
        }
        if job.state == JobState::Running {
            self.check_thread_free(job.id, job.root_event_id.as_ref())?;
        }
        let deadline = deadline_in.map(|after| {
            self.clock.now() + chrono::Duration::from_std(after).expect("a duration that fits")
        });
        self.work.jobs.push((self.community, job.clone(), deadline));
        Ok(())
    }

    async fn get_for_update(&mut self, id: uuid::Uuid) -> Result<Option<Job>, StoreError> {
        Ok(self
            .work
            .jobs
            .iter()
            .find(|(c, j, _)| *c == self.community && j.id == id)
            .map(|(_, j, _)| j.clone()))
    }

    async fn latest_in_thread(&mut self, root: &str) -> Result<Option<Job>, StoreError> {
        Ok(self
            .work
            .jobs
            .iter()
            .rev()
            .find(|(c, j, _)| *c == self.community && j.root_event_id.as_deref() == Some(root))
            .map(|(_, j, _)| j.clone()))
    }

    async fn set_root(&mut self, id: uuid::Uuid, root: &str) -> Result<(), StoreError> {
        self.check_thread_free(id, Some(&root.to_owned()))?;
        let community = self.community;
        if let Some((_, job, _)) = self
            .work
            .jobs
            .iter_mut()
            .find(|(c, j, _)| *c == community && j.id == id)
        {
            job.root_event_id = Some(root.to_owned());
        }
        Ok(())
    }

    async fn finish(
        &mut self,
        id: uuid::Uuid,
        state: JobState,
        _result: Option<serde_json::Value>,
    ) -> Result<(), StoreError> {
        let community = self.community;
        if let Some((_, job, _)) = self
            .work
            .jobs
            .iter_mut()
            .find(|(c, j, _)| *c == community && j.id == id && j.state == JobState::Running)
        {
            job.state = state;
        }
        Ok(())
    }

    async fn overdue(&mut self, limit: i64) -> Result<Vec<Job>, StoreError> {
        let now = self.clock.now();
        let mut late: Vec<_> = self
            .work
            .jobs
            .iter()
            .filter(|(c, j, at)| {
                *c == self.community
                    && j.state == JobState::Running
                    && at.is_some_and(|at| at <= now)
            })
            .collect();
        late.sort_by_key(|(_, j, at)| (*at, j.id));
        Ok(late
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(0))
            .map(|(_, j, _)| j.clone())
            .collect())
    }

    async fn next_in(&mut self) -> Result<Option<std::time::Duration>, StoreError> {
        let now = self.clock.now();
        Ok(self
            .work
            .jobs
            .iter()
            .filter(|(c, j, _)| *c == self.community && j.state == JobState::Running)
            .filter_map(|(_, _, at)| *at)
            .min()
            .map(|at| (at - now).to_std().unwrap_or_default()))
    }
}

#[async_trait::async_trait]
impl TimersRepo for MemTx {
    async fn arm(
        &mut self,
        run: uuid::Uuid,
        stage: &str,
        kind: TimerKind,
        after: std::time::Duration,
    ) -> Result<(), StoreError> {
        let community = self.community;
        let fire_at = self.clock.now() + chrono::Duration::from_std(after).unwrap_or_default();
        self.work.timers.retain(|(c, t)| {
            !(*c == community
                && t.timer.run_id == run
                && t.timer.stage == stage
                && t.timer.kind == kind)
        });
        self.work.timers.push((
            community,
            MemTimer {
                timer: Timer {
                    run_id: run,
                    stage: stage.to_owned(),
                    kind,
                },
                fire_at,
            },
        ));
        Ok(())
    }

    async fn clear_run(&mut self, run: uuid::Uuid) -> Result<(), StoreError> {
        let community = self.community;
        self.work
            .timers
            .retain(|(c, t)| !(*c == community && t.timer.run_id == run));
        Ok(())
    }

    async fn due(&mut self, limit: i64) -> Result<Vec<Timer>, StoreError> {
        let community = self.community;
        let now = self.clock.now();
        let mut due: Vec<&MemTimer> = self
            .work
            .timers
            .iter()
            .filter(|(c, t)| *c == community && t.fire_at <= now)
            .map(|(_, t)| t)
            .collect();
        due.sort_by_key(|t| t.fire_at);
        Ok(due
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(0))
            .map(|t| t.timer.clone())
            .collect())
    }

    async fn take_due(
        &mut self,
        run: uuid::Uuid,
        stage: &str,
        kind: TimerKind,
    ) -> Result<bool, StoreError> {
        let community = self.community;
        let now = self.clock.now();
        let before = self.work.timers.len();
        self.work.timers.retain(|(c, t)| {
            !(*c == community
                && t.timer.run_id == run
                && t.timer.stage == stage
                && t.timer.kind == kind
                && t.fire_at <= now)
        });
        Ok(self.work.timers.len() < before)
    }

    async fn remove(
        &mut self,
        run: uuid::Uuid,
        stage: &str,
        kind: TimerKind,
    ) -> Result<(), StoreError> {
        let community = self.community;
        self.work.timers.retain(|(c, t)| {
            !(*c == community
                && t.timer.run_id == run
                && t.timer.stage == stage
                && t.timer.kind == kind)
        });
        Ok(())
    }

    async fn next_in(&mut self) -> Result<Option<std::time::Duration>, StoreError> {
        let community = self.community;
        let now = self.clock.now();
        Ok(self
            .work
            .timers
            .iter()
            .filter(|(c, _)| *c == community)
            .map(|(_, t)| t.fire_at)
            .min()
            .map(|at| (at - now).to_std().unwrap_or_default()))
    }
}

#[async_trait::async_trait]
impl UsageRepo for MemTx {
    async fn usage(&mut self, weeks: u32, engine: &str) -> Result<Usage, StoreError> {
        let community = self.community;
        let mut runs = 0u32;
        let mut ended = Vec::new();
        for (c, run) in &self.work.runs {
            if *c == community {
                runs += 1;
                if let Some(end) = run.ended_at {
                    ended.push((end - run.started_at).num_seconds().max(0) as u64);
                }
            }
        }
        // Events are stamped with the wall clock, so the weeks are the wall clock's.
        let this_week = week_start(chrono::Utc::now().date_naive());
        let mut rows: Vec<UsageWeek> = (0..weeks)
            .rev()
            .map(|back| UsageWeek {
                starts: this_week - chrono::Duration::weeks(i64::from(back)),
                cards_created_by_agents: 0,
                runs_started_by_agents: 0,
            })
            .collect();
        let log = self.log.lock().unwrap();
        for (c, event) in &log.events {
            if *c != community
                || event.actor.kind != ActorKind::Agent
                || event.actor.pubkey == engine
            {
                continue;
            }
            let starts = week_start(event.at.date_naive());
            if let Some(row) = rows.iter_mut().find(|row| row.starts == starts) {
                match event.kind.as_str() {
                    "card.created" => row.cards_created_by_agents += 1,
                    "run.started" => row.runs_started_by_agents += 1,
                    _ => {}
                }
            }
        }
        Ok(Usage {
            runs,
            ended_runs: ended.len() as u32,
            average_run_seconds: ended
                .iter()
                .sum::<u64>()
                .checked_div(ended.len() as u64)
                .unwrap_or(0),
            longest_run_seconds: ended.iter().copied().max().unwrap_or(0),
            weeks: rows,
        })
    }
}

#[async_trait::async_trait]
impl InboxRepo for MemTx {
    async fn insert(&mut self, item: &InboxItem) -> Result<(), StoreError> {
        self.work.inbox.push((self.community, item.clone()));
        Ok(())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<InboxItem>, StoreError> {
        let community = self.community;
        Ok(self
            .work
            .inbox
            .iter()
            .find(|(c, i)| *c == community && i.id == id)
            .map(|(_, i)| i.clone()))
    }

    async fn list(
        &mut self,
        card: Option<uuid::Uuid>,
        include_resolved: bool,
    ) -> Result<Vec<InboxItem>, StoreError> {
        let community = self.community;
        let mut items: Vec<InboxItem> = self
            .work
            .inbox
            .iter()
            .filter(|(c, i)| {
                *c == community
                    && card.is_none_or(|id| i.card_id == id)
                    && (include_resolved || i.is_open())
            })
            .map(|(_, i)| i.clone())
            .collect();
        // Open first, then newest first.
        items.sort_by(|a, b| {
            b.is_open()
                .cmp(&a.is_open())
                .then(b.opened_at.cmp(&a.opened_at))
        });
        Ok(items)
    }

    async fn resolve(
        &mut self,
        id: uuid::Uuid,
        answer: &str,
        by: &str,
    ) -> Result<bool, StoreError> {
        let community = self.community;
        let now = self.clock.now();
        let Some((_, item)) = self
            .work
            .inbox
            .iter_mut()
            .find(|(c, i)| *c == community && i.id == id && i.is_open())
        else {
            return Ok(false);
        };
        item.resolved_at = Some(now);
        item.resolved_by = Some(by.to_owned());
        item.answer = Some(answer.to_owned());
        Ok(true)
    }

    async fn resolve_card(
        &mut self,
        card: uuid::Uuid,
        answer: &str,
        by: &str,
    ) -> Result<Vec<InboxItem>, StoreError> {
        let community = self.community;
        let now = self.clock.now();
        let mut resolved = Vec::new();
        for (c, item) in &mut self.work.inbox {
            if *c == community && item.card_id == card && item.is_open() {
                item.resolved_at = Some(now);
                item.resolved_by = Some(by.to_owned());
                item.answer = Some(answer.to_owned());
                resolved.push(item.clone());
            }
        }
        Ok(resolved)
    }
}

impl MemTx {
    fn find_run(&self, id: uuid::Uuid) -> Option<&Run> {
        self.work
            .runs
            .iter()
            .find(|(community, run)| *community == self.community && run.id == id)
            .map(|(_, run)| run)
    }

    fn outbox_row(&mut self, id: i64) -> Result<&mut OutboxRow, StoreError> {
        let community = self.community;
        self.work
            .outbox
            .iter_mut()
            .find(|(c, row)| *c == community && row.id == id)
            .map(|(_, row)| row)
            .ok_or_else(|| StoreError::NotFound(format!("outbox row {id}")))
    }

    fn find(&self, id: uuid::Uuid) -> Option<&(CommunityId, Card)> {
        self.work
            .cards
            .iter()
            .find(|(community, card)| *community == self.community && card.id == id)
    }

    fn find_mut(&mut self, id: uuid::Uuid) -> Result<&mut Card, StoreError> {
        let community = self.community;
        self.work
            .cards
            .iter_mut()
            .find(|(c, card)| *c == community && card.id == id)
            .map(|(_, card)| card)
            .ok_or_else(|| StoreError::NotFound(format!("card {id}")))
    }
}

/// A `Directory` answering from two maps, keyed by pubkey. The community is ignored.
#[derive(Default, Clone)]
pub struct FakeDirectory {
    roles: HashMap<Pubkey, RelayRole>,
    names: HashMap<Pubkey, String>,
    owners: HashMap<Pubkey, Pubkey>,
    deactivated: Vec<Pubkey>,
    agents: Vec<AgentInfo>,
    channels: HashMap<uuid::Uuid, Vec<Pubkey>>,
    events: Vec<RelayEvent>,
}

impl FakeDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_role(mut self, pubkey: &str, role: RelayRole) -> Self {
        self.roles.insert(pubkey.to_owned(), role);
        self
    }

    /// The display name a person set.
    pub fn with_name(mut self, pubkey: &str, name: &str) -> Self {
        self.names.insert(pubkey.to_owned(), name.to_owned());
        self
    }

    pub fn with_agent(mut self, agent: &str, owner: &str) -> Self {
        self.owners.insert(agent.to_owned(), owner.to_owned());
        self
    }

    /// Switches `pubkey` off in Buzz. It keeps its owner.
    pub fn with_deactivated(mut self, pubkey: &str) -> Self {
        self.deactivated.push(pubkey.to_owned());
        self
    }

    /// An agent the relay lists under `name`. Its owner is `owner`.
    pub fn with_named_agent(mut self, name: &str, pubkey: &str, owner: &str) -> Self {
        self.owners.insert(pubkey.to_owned(), owner.to_owned());
        self.agents.push(AgentInfo {
            name: name.to_owned(),
            pubkey: pubkey.to_owned(),
            owner: Some(owner.to_owned()),
        });
        self
    }

    /// A message the relay holds.
    pub fn with_event(mut self, event: RelayEvent) -> Self {
        self.events.push(event);
        self
    }

    /// A channel that exists, with these current members.
    pub fn with_channel(mut self, channel: uuid::Uuid, members: &[&str]) -> Self {
        self.channels
            .insert(channel, members.iter().map(|m| (*m).to_owned()).collect());
        self
    }
}

#[async_trait::async_trait]
impl Directory for FakeDirectory {
    async fn relay_role(
        &self,
        _community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError> {
        Ok(self.roles.get(pubkey).copied())
    }

    async fn agent_link(
        &self,
        _community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<AgentLink>, StoreError> {
        Ok(self.owners.get(pubkey).map(|owner| AgentLink {
            owner: owner.clone(),
            deactivated: self.deactivated.iter().any(|d| d == pubkey),
        }))
    }

    async fn agents_named(
        &self,
        _community: CommunityId,
        name: &str,
    ) -> Result<Vec<AgentInfo>, StoreError> {
        Ok(self
            .agents
            .iter()
            .filter(|a| a.name.eq_ignore_ascii_case(name))
            .cloned()
            .collect())
    }

    async fn list_agents(&self, _community: CommunityId) -> Result<Vec<AgentInfo>, StoreError> {
        let mut agents = self.agents.clone();
        agents.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.pubkey.cmp(&b.pubkey)));
        Ok(agents)
    }

    async fn people(&self, _community: CommunityId) -> Result<Vec<PersonInfo>, StoreError> {
        let mut people: Vec<PersonInfo> = self
            .roles
            .iter()
            .filter(|(pubkey, _)| !self.owners.contains_key(*pubkey))
            .map(|(pubkey, role)| PersonInfo {
                pubkey: pubkey.clone(),
                name: self.names.get(pubkey).cloned(),
                role: *role,
            })
            .collect();
        people.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
        Ok(people)
    }

    async fn channel_exists(
        &self,
        _community: CommunityId,
        channel: uuid::Uuid,
    ) -> Result<bool, StoreError> {
        Ok(self.channels.contains_key(&channel))
    }

    async fn member_of_channel(
        &self,
        _community: CommunityId,
        channel: uuid::Uuid,
        pubkey: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .channels
            .get(&channel)
            .is_some_and(|members| members.iter().any(|m| m == pubkey)))
    }

    async fn event_by_id(
        &self,
        _community: CommunityId,
        id: &str,
    ) -> Result<Option<RelayEvent>, StoreError> {
        Ok(self.events.iter().find(|e| e.id == id).cloned())
    }

    async fn newer_mention(
        &self,
        _community: CommunityId,
        event: &RelayEvent,
        root: &str,
        agent: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.events.iter().any(|e| {
            !e.deleted
                && MESSAGE_KINDS.contains(&e.kind)
                && e.channel_id == event.channel_id
                && e.mentions(agent)
                && e.in_thread(root)
                && (e.created_at, &e.id) > (event.created_at, &event.id)
        }))
    }
}

#[async_trait::async_trait]
impl IntakeRepo for MemTx {
    async fn list(&mut self) -> Result<Vec<IntakeRule>, StoreError> {
        Ok(self
            .work
            .intake
            .iter()
            .filter(|(c, _)| *c == self.community)
            .map(|(_, rule)| rule.clone())
            .collect())
    }

    async fn get(&mut self, id: uuid::Uuid) -> Result<Option<IntakeRule>, StoreError> {
        Ok(self
            .work
            .intake
            .iter()
            .find(|(c, rule)| *c == self.community && rule.id == id)
            .map(|(_, rule)| rule.clone()))
    }

    async fn save(&mut self, rule: &IntakeRule) -> Result<(), StoreError> {
        let community = self.community;
        match self
            .work
            .intake
            .iter_mut()
            .find(|(c, have)| *c == community && have.id == rule.id)
        {
            Some((_, have)) => *have = rule.clone(),
            None => self.work.intake.push((community, rule.clone())),
        }
        Ok(())
    }

    async fn delete(&mut self, id: uuid::Uuid) -> Result<bool, StoreError> {
        let community = self.community;
        let before = self.work.intake.len();
        self.work
            .intake
            .retain(|(c, rule)| !(*c == community && rule.id == id));
        for (c, row) in &mut self.work.issues {
            if *c == community && row.rule_id == Some(id) {
                row.rule_id = None;
            }
        }
        Ok(self.work.intake.len() != before)
    }
}

impl MemTx {
    fn issue_row(&mut self, repo: &RepoRef, number: u64) -> Option<&mut StoredIssue> {
        let community = self.community;
        self.work
            .issues
            .iter_mut()
            .find(|(c, row)| {
                *c == community && row.issue.repo == *repo && row.issue.number == number
            })
            .map(|(_, row)| row)
    }

    fn missing_issue(repo: &RepoRef, number: u64) -> StoreError {
        StoreError::NotFound(format!("issue {repo}#{number} has not been read"))
    }
}

#[async_trait::async_trait]
impl PullRequestsRepo for MemTx {
    async fn put(&mut self, card: uuid::Uuid, pull: &PullRequest) -> Result<(), StoreError> {
        let community = self.community;
        if !self
            .work
            .cards
            .iter()
            .any(|(c, have)| *c == community && have.id == card)
        {
            return Err(StoreError::NotFound(format!("card {card}")));
        }
        self.work.pulls.insert((community, card), pull.clone());
        Ok(())
    }

    async fn by_card(&mut self, card: uuid::Uuid) -> Result<Option<PullRequest>, StoreError> {
        Ok(self.work.pulls.get(&(self.community, card)).cloned())
    }

    async fn open(&mut self) -> Result<Vec<(uuid::Uuid, PullRequest)>, StoreError> {
        Ok(self
            .work
            .pulls
            .iter()
            .filter(|((c, _), pull)| *c == self.community && pull.state == "open")
            .map(|((_, card), pull)| (*card, pull.clone()))
            .collect())
    }
}

#[async_trait::async_trait]
impl IssuesRepo for MemTx {
    async fn upsert(
        &mut self,
        issue: &Issue,
        seen_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<StoredIssue, StoreError> {
        let key = (self.community, issue.repo.to_string(), issue.number);
        // A read older than the one the row holds leaves it as it is.
        let newer = self
            .work
            .issue_seen
            .get(&key)
            .is_none_or(|have| *have <= seen_at);
        if let Some(row) = self.issue_row(&issue.repo, issue.number) {
            if newer {
                row.issue = issue.clone();
            }
            let row = row.clone();
            if newer {
                self.work.issue_seen.insert(key, seen_at);
            }
            return Ok(row);
        }
        self.work.issue_seen.insert(key, seen_at);
        let row = StoredIssue {
            issue: issue.clone(),
            card_id: None,
            rule_id: None,
            left: String::new(),
            last_comment_at: Some(seen_at),
        };
        self.work.issues.push((self.community, row.clone()));
        Ok(row)
    }

    async fn get(
        &mut self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Option<StoredIssue>, StoreError> {
        Ok(self.issue_row(repo, number).map(|row| row.clone()))
    }

    async fn get_for_update(
        &mut self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Option<StoredIssue>, StoreError> {
        Ok(self.issue_row(repo, number).map(|row| row.clone()))
    }

    async fn by_card(&mut self, card: uuid::Uuid) -> Result<Option<StoredIssue>, StoreError> {
        let community = self.community;
        Ok(self
            .work
            .issues
            .iter()
            .find(|(c, row)| *c == community && row.card_id == Some(card))
            .map(|(_, row)| row.clone()))
    }

    async fn attached(&mut self) -> Result<Vec<StoredIssue>, StoreError> {
        let community = self.community;
        Ok(self
            .work
            .issues
            .iter()
            .filter(|(c, row)| *c == community && row.card_id.is_some())
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn of_rule(&mut self, rule: uuid::Uuid) -> Result<Vec<StoredIssue>, StoreError> {
        let community = self.community;
        Ok(self
            .work
            .issues
            .iter()
            .filter(|(c, row)| {
                *c == community && row.rule_id == Some(rule) && row.card_id.is_some()
            })
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn watched(&mut self) -> Result<Vec<StoredIssue>, StoreError> {
        let community = self.community;
        Ok(self
            .work
            .issues
            .iter()
            .filter(|(c, row)| {
                *c == community
                    && row.card_id.is_some()
                    && row.left.is_empty()
                    && row.issue.state == IssueState::Open
            })
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn attach(
        &mut self,
        repo: &RepoRef,
        number: u64,
        card: uuid::Uuid,
        rule: Option<uuid::Uuid>,
    ) -> Result<(), StoreError> {
        let row = self
            .issue_row(repo, number)
            .ok_or_else(|| Self::missing_issue(repo, number))?;
        row.card_id = Some(card);
        row.rule_id = rule;
        Ok(())
    }

    async fn set_left(
        &mut self,
        repo: &RepoRef,
        number: u64,
        left: &str,
        seen_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StoreError> {
        let key = (self.community, repo.to_string(), number);
        let held = self.work.issue_seen.get(&key).copied();
        let row = self
            .issue_row(repo, number)
            .ok_or_else(|| Self::missing_issue(repo, number))?;
        if held.is_some_and(|have| have > seen_at) {
            return Ok(false);
        }
        row.left = left.to_owned();
        Ok(true)
    }

    async fn set_last_comment(
        &mut self,
        repo: &RepoRef,
        number: u64,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StoreError> {
        let row = self
            .issue_row(repo, number)
            .ok_or_else(|| Self::missing_issue(repo, number))?;
        row.last_comment_at = row.last_comment_at.max(Some(at));
        Ok(())
    }
}
