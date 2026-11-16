//! Filing and accepting a fault while several agents or clicks race, in Postgres. The in-memory
//! store gives each transaction its own copy and never waits, so only Postgres shows the races.

mod support;

use std::sync::Arc;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::events::Actor;
use buzz_hive_core::domain::intake::RepoRef;
use buzz_hive_core::orchestrator::diagnose::{
    accept_ticket, file_ticket, Filed, Ticket, TicketKind, PROPOSAL_TICKET,
};
use buzz_hive_core::orchestrator::intake::IntakeError;
use buzz_hive_core::orchestrator::{Ctx, Env, OpError};
use buzz_hive_core::ports::code_host::HostError;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::{FakeCodeHost, FakeDirectory};
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::load::{load, Catalog};
use buzz_hive_store::pg::PgStore;
use uuid::Uuid;

const PERSON: &str = "aa";
const AGENT: &str = "dd";
const RACERS: usize = 8;

struct World {
    store: Arc<PgStore>,
    host: FakeCodeHost,
    catalog: Catalog,
    directory: FakeDirectory,
    _guard: support::Scratch,
    _relay: sqlx::PgPool,
}

async fn world() -> World {
    let (relay, guard) = support::scratch_relay_db("hive_tickets").await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    let host = FakeCodeHost::new();
    host.put_repo(&RepoRef::parse("example-org/api").unwrap());
    World {
        store: Arc::new(PgStore::new(pool)),
        host,
        catalog: load(&[builtin_source()]),
        directory: FakeDirectory::new(),
        _guard: guard,
        _relay: relay,
    }
}

impl World {
    fn env(&self) -> Env<'_> {
        Env {
            store: &*self.store,
            directory: &self.directory,
            catalog: &self.catalog,
            hive: "ee",
        }
    }
}

fn community() -> CommunityId {
    CommunityId::from_u128(1)
}

fn ctx(pubkey: &str, kind: ActorKind) -> Ctx {
    Ctx {
        community: community(),
        actor: Actor {
            pubkey: pubkey.into(),
            kind,
        },
        requester: (kind == ActorKind::Agent).then(|| PERSON.to_owned()),
    }
}

/// A card that runs in `example-org/api`.
async fn card_in_api(store: &PgStore) -> Uuid {
    let id = Uuid::from_u128(7);
    let mut tx = store.begin(community()).await.unwrap();
    tx.cards()
        .insert(&Card {
            id,
            title: "Add export".into(),
            description: String::new(),
            state: CardState::Idle,
            workflow: "demo".into(),
            archived: false,
            created_by: PERSON.into(),
        })
        .await
        .unwrap();
    tx.card_repos().add(id, "example-org/api").await.unwrap();
    tx.commit().await.unwrap();
    id
}

fn ticket(kind: TicketKind, card: Option<Uuid>) -> Ticket {
    Ticket {
        kind,
        title: "Stage never starts".into(),
        body: "The thread is not recorded.".into(),
        repos: Vec::new(),
        card,
    }
}

/// Runs `file` once per racer, all released together.
async fn racing<F, Fut, T>(file: F) -> Vec<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let start = Arc::new(tokio::sync::Barrier::new(RACERS));
    let tasks: Vec<_> = (0..RACERS)
        .map(|_| {
            let start = start.clone();
            let work = file();
            tokio::spawn(async move {
                start.wait().await;
                work.await
            })
        })
        .collect();
    let mut done = Vec::new();
    for task in tasks {
        done.push(task.await.unwrap());
    }
    done
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Postgres"]
async fn agents_filing_one_fault_at_once_make_one_card() {
    let w = world().await;
    let done = racing(|| {
        let store = w.store.clone();
        async move {
            file_ticket(
                &*store,
                None,
                &ctx(AGENT, ActorKind::Agent),
                ticket(TicketKind::Engine, None),
            )
            .await
            .unwrap()
            .0
        }
    })
    .await;
    let created = done
        .iter()
        .filter(|filed| matches!(filed, Filed::Created(_)))
        .count();
    assert_eq!(created, 1, "{done:?}");
    assert!(done
        .iter()
        .all(|filed| matches!(filed, Filed::Created(_) | Filed::Noted(_))));
    let mut tx = w.store.begin(community()).await.unwrap();
    assert_eq!(tx.cards().list(false).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Postgres"]
async fn agents_reporting_one_code_fault_at_once_leave_one_proposal() {
    let w = world().await;
    let card = card_in_api(&w.store).await;
    let host = w.host.clone();
    // Several faults in turn, since one round of the race is not always lost without the lock.
    for round in 0..10 {
        let done = racing(|| {
            let store = w.store.clone();
            let host = host.clone();
            async move {
                let mut report = ticket(TicketKind::Code, Some(card));
                report.title = format!("Drops a row {round}");
                file_ticket(&*store, Some(&host), &ctx(AGENT, ActorKind::Agent), report)
                    .await
                    .unwrap()
                    .0
            }
        })
        .await;
        let ids: Vec<_> = done
            .iter()
            .map(|filed| match filed {
                Filed::Proposed(id) => *id,
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(ids.iter().all(|id| *id == ids[0]), "round {round}: {ids:?}");
    }
    let mut tx = w.store.begin(community()).await.unwrap();
    assert_eq!(
        tx.proposals().open(PROPOSAL_TICKET).await.unwrap().len(),
        10
    );
}

async fn proposal(w: &World) -> i64 {
    let card = card_in_api(&w.store).await;
    let (filed, _) = file_ticket(
        &*w.store,
        Some(&w.host),
        &ctx(AGENT, ActorKind::Agent),
        ticket(TicketKind::Code, Some(card)),
    )
    .await
    .unwrap();
    match filed {
        Filed::Proposed(id) => id,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Postgres"]
async fn accepting_one_proposal_at_once_opens_one_issue_and_one_accept_wins() {
    let w = world().await;
    let id = proposal(&w).await;
    let env_store = w.store.clone();
    let host = w.host.clone();
    let catalog = w.catalog.clone();
    let done = racing(|| {
        let store = env_store.clone();
        let host = host.clone();
        let catalog = catalog.clone();
        async move {
            let directory = FakeDirectory::new();
            let env = Env {
                store: &*store,
                directory: &directory,
                catalog: &catalog,
                hive: "ee",
            };
            accept_ticket(
                &env,
                &host,
                &ctx(PERSON, ActorKind::Person),
                id,
                "",
                "",
                chrono::Utc::now(),
            )
            .await
            .map(|(accepted, _)| accepted.card.id)
        }
    })
    .await;
    let accepted: Vec<_> = done.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(accepted.len(), 1, "{done:?}");
    for refused in done.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            matches!(refused, IntakeError::Op(OpError::NotFound(_))),
            "{refused:?}"
        );
    }
    let opened = host
        .calls()
        .iter()
        .filter(|call| call.starts_with("create_issue"))
        .count();
    assert_eq!(opened, 1, "{:?}", host.calls());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_failed_accept_leaves_the_proposal_open_and_a_retry_opens_one_issue() {
    let w = world().await;
    let id = proposal(&w).await;
    let person = ctx(PERSON, ActorKind::Person);
    let env = w.env();
    let accept = || accept_ticket(&env, &w.host, &person, id, "", "", chrono::Utc::now());

    w.host.fail_next(HostError::Other("down".into()));
    assert!(accept().await.is_err());
    let mut tx = w.store.begin(community()).await.unwrap();
    assert_eq!(
        tx.proposals().open(PROPOSAL_TICKET).await.unwrap().len(),
        1,
        "the person can try again"
    );
    drop(tx);

    accept().await.unwrap();
    assert!(matches!(
        accept().await,
        Err(IntakeError::Op(OpError::NotFound(_)))
    ));
}
