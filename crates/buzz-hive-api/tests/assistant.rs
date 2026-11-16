//! The assistant's jobs: start one, take the agent's result, cancel, run out of time, and
//! decide what the agent offered.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine as _;
use buzz_hive_api::App;
use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::domain::events::{Actor, Event};
use buzz_hive_core::orchestrator::assistant::expire_jobs;
use buzz_hive_core::ports::relay::{ChannelId, Delivery, EventId, Relay, RelayError};
use buzz_hive_core::ports::store::{JobState, Pubkey, Store};
use buzz_hive_core::testing::{FakeRelay, ManualClock, MemStore};
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::load::Catalog;
use buzz_hive_core::workflow::source::{CatalogSource, Sources};
use common::{app, call, call_for, community, AGENT, HIVE, MEMBER, OWNER};
use serde_json::{json, Value};

const CHANNEL: uuid::Uuid = uuid::Uuid::from_u128(77);
/// An agent the job was not given to.
const OTHER: &str = "ff";

struct World {
    app: App,
    relay: FakeRelay,
    clock: ManualClock,
    store: MemStore,
}

/// An app whose assistant is the agent `helper` (`AGENT`), in a channel Beehive is in.
async fn world() -> World {
    let clock = ManualClock::new();
    let store = MemStore::with_clock(Arc::new(clock.clone()));
    let base = app(store.clone()).await;
    let mut tx = base.store.begin(community()).await.unwrap();
    tx.access()
        .set_level(OTHER, Level::Run, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    for (key, value) in [
        ("update_channel", json!(CHANNEL.to_string())),
        ("assistant_agent", json!("helper")),
    ] {
        tx.settings().set("engine", "", key, value).await.unwrap();
    }
    tx.commit().await.unwrap();
    let relay = FakeRelay::new();
    let directory = common::people()
        .with_agent(OTHER, OWNER)
        .with_named_agent("helper", AGENT, OWNER)
        .with_named_agent("other", OTHER, OWNER)
        .with_channel(CHANNEL, &[HIVE, AGENT, OTHER]);
    let app = App {
        directory: Arc::new(directory),
        relay: Some(Arc::new(relay.clone())),
        ..base
    };
    World {
        app,
        relay,
        clock,
        store,
    }
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// A workflow draft as the editor sends it.
fn draft() -> Value {
    json!({
        "name": "flow",
        "files": {
            "kiln-package.yaml": b64("{\"name\": \"flow\", \"version\": \"1.0.0\"}"),
            "workflow.yaml": b64("{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"helper\", \"prompt\": \"do it\"}]}"),
        }
    })
}

/// The same workflow as the agent reports it, in text.
fn offered() -> Value {
    json!({
        "name": "flow",
        "files": {
            "kiln-package.yaml": "{\"name\": \"flow\", \"version\": \"1.0.0\"}",
            "workflow.yaml": "{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"helper\", \"prompt\": \"do it better\"}]}",
        }
    })
}

async fn start_chat(w: &World, who: &str, body: Value) -> (StatusCode, Value) {
    call(&w.app, "StartChat", who, body).await
}

/// The job a chat started, by the person `MEMBER`.
async fn chat(w: &World) -> String {
    let (status, body) = start_chat(w, MEMBER, json!({"text": "hello"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["jobId"].as_str().unwrap().to_owned()
}

/// Every event after `after`, as (type, payload), and the cursor to poll from next.
async fn poll(w: &World, after: &str) -> (Vec<(String, Value)>, String) {
    let (status, reply) = call(&w.app, "PollEvents", MEMBER, json!({"afterSeq": after})).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let events = reply["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| {
            (
                e["type"].as_str().unwrap().to_owned(),
                serde_json::from_str(e["payloadJson"].as_str().unwrap()).unwrap(),
            )
        })
        .collect();
    (events, reply["nextSeq"].as_str().unwrap_or("0").to_owned())
}

fn of_kind<'a>(events: &'a [(String, Value)], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|(k, _)| k == kind)
        .map(|(_, p)| p)
        .collect()
}

async fn report(w: &World, who: &str, job: &str, body: Value) -> (StatusCode, Value) {
    let mut body = body;
    body["jobId"] = json!(job);
    call(&w.app, "AssistantResult", who, body).await
}

#[tokio::test]
async fn a_chat_starts_a_job_the_agent_is_asked_to_do_in_the_update_channel() {
    let w = world().await;
    let job = chat(&w).await;

    let posts = w.relay.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].channel, CHANNEL);
    assert_eq!(posts[0].mention.as_deref(), Some(AGENT));
    assert_eq!(posts[0].root, None);
    assert!(posts[0].text.starts_with("hello"), "{}", posts[0].text);
    assert!(posts[0].text.contains(&job), "{}", posts[0].text);

    let (events, _) = poll(&w, "0").await;
    let started = of_kind(&events, "job.started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["job_id"], job);
    assert_eq!(started[0]["kind"], "chat");
    assert_eq!(started[0]["root_event_id"], posts[0].event_id);
}

#[tokio::test]
async fn a_result_is_one_job_result_event() {
    let w = world().await;
    let job = chat(&w).await;
    let (status, body) = report(&w, AGENT, &job, json!({"message": "hi there"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (events, _) = poll(&w, "0").await;
    let results = of_kind(&events, "job.result");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["job_id"], job);
    assert_eq!(results[0]["state"], "done");
    assert_eq!(results[0]["message"], "hi there");

    // Anyone else who may read the board sees that the job ended, and not what was said to the
    // person who asked.
    let (status, reply) = call(&w.app, "PollEvents", OWNER, json!({"afterSeq": "0"})).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let seen: Vec<Value> = reply["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "job.result")
        .map(|e| serde_json::from_str(e["payloadJson"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["job_id"], job);
    assert_eq!(seen[0]["state"], "done");
    assert!(seen[0].get("message").is_none(), "{}", seen[0]);

    // A second report is refused and adds nothing.
    let (status, body) = report(&w, AGENT, &job, json!({"message": "again"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (events, _) = poll(&w, "0").await;
    assert_eq!(of_kind(&events, "job.result").len(), 1);
}

#[tokio::test]
async fn a_poll_that_dropped_resumes_from_its_cursor_and_still_gets_the_result() {
    let w = world().await;
    let job = chat(&w).await;
    let (_, cursor) = poll(&w, "0").await;

    // A long poll is parked on the cursor, then the client goes away before the result.
    let parked = {
        let app = w.app.clone();
        let cursor = cursor.clone();
        tokio::spawn(async move {
            call(
                &app,
                "PollEvents",
                MEMBER,
                json!({"afterSeq": cursor, "waitSeconds": 20}),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    parked.abort();
    let _ = parked.await;
    report(&w, AGENT, &job, json!({"message": "done"})).await;

    let (events, _) = poll(&w, &cursor).await;
    let results = of_kind(&events, "job.result");
    assert_eq!(results.len(), 1, "{events:?}");
    assert_eq!(results[0]["job_id"], job);
}

#[tokio::test]
async fn a_cancelled_job_ends_with_no_result_event_and_tells_the_agent() {
    let w = world().await;
    let job = chat(&w).await;
    let root = w.relay.posts()[0].event_id.clone();

    let (status, body) = call(&w.app, "CancelJob", MEMBER, json!({"jobId": job})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let told = w.relay.posts();
    assert_eq!(told.len(), 2);
    assert_eq!(told[1].root.as_deref(), Some(root.as_str()));
    assert_eq!(told[1].mention.as_deref(), Some(AGENT));
    assert!(told[1].text.starts_with("Cancelled"), "{}", told[1].text);

    // The agent's late result is refused, and the log has no job.result.
    let (status, _) = report(&w, AGENT, &job, json!({"message": "late"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (events, _) = poll(&w, "0").await;
    assert!(of_kind(&events, "job.result").is_empty(), "{events:?}");

    // A second cancel does nothing, and another person may not cancel it.
    let (status, _) = call(&w.app, "CancelJob", MEMBER, json!({"jobId": job})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(w.relay.posts().len(), 2);
    let (status, _) = call(&w.app, "CancelJob", OWNER, json!({"jobId": job})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_result_from_another_agent_or_a_person_is_refused() {
    let w = world().await;
    let job = chat(&w).await;
    for who in [OTHER, OWNER, MEMBER] {
        let (status, body) = report(&w, who, &job, json!({"message": "mine"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
    }
    let (events, _) = poll(&w, "0").await;
    assert!(of_kind(&events, "job.result").is_empty());

    // The job is still running: its own agent reports.
    let (status, _) = report(&w, AGENT, &job, json!({"message": "mine"})).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_job_past_its_deadline_ends_failed_with_one_result_event() {
    let w = world().await;
    let job = chat(&w).await;
    let op = w.app.op(false).unwrap();

    // The default stage timeout is 30 minutes.
    w.clock.advance(Duration::from_secs(29 * 60));
    assert!(expire_jobs(&op.env(), community(), 50)
        .await
        .unwrap()
        .is_empty());

    w.clock.advance(Duration::from_secs(2 * 60));
    let events = expire_jobs(&op.env(), community(), 50).await.unwrap();
    assert_eq!(events.len(), 1);
    op.settle(&events);
    // A second sweep finds nothing left.
    assert!(expire_jobs(&op.env(), community(), 50)
        .await
        .unwrap()
        .is_empty());

    let (events, _) = poll(&w, "0").await;
    let results = of_kind(&events, "job.result");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["job_id"], job);
    assert_eq!(results[0]["state"], "failed");

    let (status, _) = report(&w, AGENT, &job, json!({"message": "late"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn one_job_runs_in_a_thread_and_a_later_turn_replies_in_it() {
    let w = world().await;
    let first = chat(&w).await;
    let root = w.relay.posts()[0].event_id.clone();

    // The thread is busy.
    let (status, body) = start_chat(&w, MEMBER, json!({"text": "more", "rootEventId": root})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(w.relay.posts().len(), 1);

    report(&w, AGENT, &first, json!({"message": "one"})).await;

    // Someone else may not continue it.
    let (status, _) = start_chat(&w, OWNER, json!({"text": "mine", "rootEventId": root})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A root that is no assistant thread is not found.
    let (status, _) = start_chat(&w, MEMBER, json!({"text": "x", "rootEventId": "ab"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = start_chat(&w, MEMBER, json!({"text": "more", "rootEventId": root})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let second = body["jobId"].as_str().unwrap();
    assert_ne!(second, first);
    let posts = w.relay.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].root.as_deref(), Some(root.as_str()));
    assert_eq!(posts[1].mention.as_deref(), Some(AGENT));

    let (events, _) = poll(&w, "0").await;
    let started = of_kind(&events, "job.started");
    assert_eq!(started.len(), 2);
    assert_eq!(started[1]["root_event_id"], root);
}

#[tokio::test]
async fn a_start_says_what_is_missing() {
    let w = world().await;
    let (status, _) = start_chat(&w, MEMBER, json!({"text": "  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &w.app,
        "StartProposeWorkflowEdit",
        MEMBER,
        json!({"request": "change it"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an edit needs the draft");
    let (status, _) = call(
        &w.app,
        "StartAdaptSkills",
        MEMBER,
        json!({"draft": draft(), "skills": []}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an adaptation needs skills"
    );

    // No assistant agent set.
    let mut tx = w.app.store.begin(community()).await.unwrap();
    tx.settings()
        .remove("engine", "", "assistant_agent")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, body) = start_chat(&w, MEMBER, json!({"text": "hi"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("assistant_agent"));
    assert!(w.relay.posts().is_empty());

    // No relay account.
    let without = App {
        relay: None,
        ..w.app.clone()
    };
    let (status, _) = call(&without, "StartChat", MEMBER, json!({"text": "hi"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_post_the_relay_refuses_ends_the_job_and_frees_the_thread() {
    let w = world().await;
    let refused = App {
        relay: Some(Arc::new(FakeRelay::new().refusing("not a member"))),
        ..w.app.clone()
    };
    let (status, _) = call(&refused, "StartChat", MEMBER, json!({"text": "hi"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (events, _) = poll(&w, "0").await;
    assert!(events.is_empty(), "{events:?}");
}

#[tokio::test]
async fn a_workflow_the_agent_offers_is_a_proposal_for_the_person_who_asked() {
    let w = world().await;
    let (status, body) = call(
        &w.app,
        "StartProposeWorkflowEdit",
        MEMBER,
        json!({"draft": draft(), "request": "make it better"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let job = body["jobId"].as_str().unwrap().to_owned();
    let text = &w.relay.posts()[0].text;
    assert!(text.contains("make it better") && text.contains("--- workflow.yaml"));

    // An edit that offers no workflow is refused, and so is one with an error.
    let (status, _) = report(&w, AGENT, &job, json!({"message": "no change"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut broken = offered();
    broken["files"]["workflow.yaml"] = json!("{\"version\": 2, \"stages\": []}");
    let (status, _) = report(
        &w,
        AGENT,
        &job,
        json!({"message": "x", "name": "flow", "files": broken["files"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (events, _) = poll(&w, "0").await;
    assert!(
        of_kind(&events, "job.result").is_empty(),
        "the job still runs"
    );

    let mut result = offered();
    result["message"] = json!("made it better");
    result["baseHash"] = json!("abc");
    let (status, body) = report(&w, AGENT, &job, result).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let proposal = body["proposalId"].as_str().unwrap().to_owned();

    let (events, _) = poll(&w, "0").await;
    let results = of_kind(&events, "job.result");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["proposal_id"].to_string(), proposal);

    // The person who asked sees it. The files stay out of the event.
    let (status, listed) = call(&w.app, "ListProposals", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let open = listed["proposals"].as_array().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["id"], proposal);
    assert_eq!(open[0]["draft"]["name"], "flow");
    assert_eq!(open[0]["jobId"], job);
    assert_eq!(open[0]["baseHash"], "abc");
    let files = open[0]["draft"]["files"].as_object().unwrap();
    assert_eq!(files.len(), 2);
    let workflow = base64::engine::general_purpose::STANDARD
        .decode(files["workflow.yaml"].as_str().unwrap())
        .unwrap();
    assert!(String::from_utf8(workflow)
        .unwrap()
        .contains("do it better"));
    assert!(!results[0].to_string().contains("do it better"));

    // Nobody else does, and nobody else may decide it.
    let (_, other) = call(&w.app, "ListProposals", OWNER, json!({})).await;
    assert!(other.get("proposals").is_none(), "{other}");
    let (status, _) = call(
        &w.app,
        "ResolveProposal",
        OWNER,
        json!({"proposalId": proposal, "accepted": true}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Deciding takes it off the list, and deciding twice is harmless.
    for _ in 0..2 {
        let (status, decided) = call(
            &w.app,
            "ResolveProposal",
            MEMBER,
            json!({"proposalId": proposal, "accepted": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{decided}");
        assert_eq!(decided["proposal"]["state"], "accepted");
    }
    let (_, after) = call(&w.app, "ListProposals", MEMBER, json!({})).await;
    assert!(after.get("proposals").is_none(), "{after}");
    let (status, _) = call(
        &w.app,
        "ResolveProposal",
        MEMBER,
        json!({"proposalId": "9999", "accepted": false}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_agent_acting_for_a_person_neither_lists_nor_decides_their_proposals() {
    let w = world().await;
    let mut tx = w.app.store.begin(community()).await.unwrap();
    let id = tx
        .proposals()
        .insert("workflow", json!({"name": "flow", "files": {}}), MEMBER)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (status, body) = call_for(&w.app, "ListProposals", AGENT, MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call_for(
        &w.app,
        "ResolveProposal",
        AGENT,
        MEMBER,
        json!({"proposalId": id.to_string(), "accepted": false}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // The person still sees it, so the agent closed nothing.
    let (_, listed) = call(&w.app, "ListProposals", MEMBER, json!({})).await;
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 1, "{listed}");
}

/// A catalog that counts how often the packages are read.
struct Counting(Arc<std::sync::atomic::AtomicUsize>, Sources);

impl CatalogSource for Counting {
    fn load(&self) -> Catalog {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.1.load()
    }
}

#[tokio::test]
async fn cancelling_and_reporting_without_a_draft_do_not_read_the_packages() {
    use std::sync::atomic::Ordering;
    let mut w = world().await;
    let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    w.app.catalog = Arc::new(Counting(loads.clone(), Sources(vec![builtin_source()])));
    let cancelled = chat(&w).await;
    let reported = {
        // A thread has one running job, so the second chat starts after the first ends.
        let (status, _) = call(&w.app, "CancelJob", MEMBER, json!({"jobId": cancelled})).await;
        assert_eq!(status, StatusCode::OK);
        chat(&w).await
    };
    let before = loads.load(Ordering::SeqCst);
    let (status, _) = report(&w, AGENT, &reported, json!({"message": "hi"})).await;
    assert_eq!(status, StatusCode::OK);
    let again = chat(&w).await;
    let after_start = loads.load(Ordering::SeqCst);
    let (status, _) = call(&w.app, "CancelJob", MEMBER, json!({"jobId": again})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        loads.load(Ordering::SeqCst),
        after_start,
        "a cancel read no packages"
    );
    assert_eq!(
        after_start - before,
        1,
        "a report with no draft read none; only the start did"
    );
}

#[tokio::test]
async fn a_draft_too_long_to_quote_tells_the_agent_to_read_it_with_the_tool() {
    let w = world().await;
    let mut long = draft();
    long["files"]["notes.md"] = json!(b64(&"x".repeat(40 * 1024)));
    let (status, body) = call(
        &w.app,
        "StartProposeWorkflowEdit",
        MEMBER,
        json!({"draft": long, "request": "tidy it"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = &w.relay.posts()[0].text;
    assert!(
        text.contains("read it with get_workflow") && !text.contains("--- workflow.yaml"),
        "{text}"
    );
}

/// A relay that, right after it takes the agent's mention, does what a test needs the store to
/// look like by the time the post is recorded.
struct Then {
    inner: FakeRelay,
    store: MemStore,
    what: Happens,
}

enum Happens {
    /// The agent reports as soon as it is mentioned.
    TheAgentReports,
    /// The store refuses this many commits.
    TheStoreFails(usize),
}

#[async_trait::async_trait]
impl Relay for Then {
    async fn post_status(
        &self,
        channel: ChannelId,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.inner.post_status(channel, text, delivery).await
    }

    async fn post_stage_task(
        &self,
        channel: ChannelId,
        agent: &Pubkey,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        let posted = self
            .inner
            .post_stage_task(channel, agent, text, delivery)
            .await?;
        match self.what {
            Happens::TheStoreFails(commits) => self.store.fail_next_commits(commits),
            Happens::TheAgentReports => {
                let job: uuid::Uuid = delivery.key.strip_prefix("job:").unwrap().parse().unwrap();
                let mut tx = self.store.begin(community()).await.unwrap();
                tx.jobs()
                    .finish(job, JobState::Done, Some(json!({})))
                    .await
                    .unwrap();
                let actor = Actor {
                    pubkey: AGENT.into(),
                    kind: ActorKind::Agent,
                };
                let result = json!({"job_id": job, "state": "done", "requester": MEMBER});
                tx.events()
                    .append(Event::new("job.result", None, None, actor, result))
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
        }
        Ok(posted)
    }

    async fn post_reply(
        &self,
        channel: ChannelId,
        root: &EventId,
        agent: &Pubkey,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.inner
            .post_reply(channel, root, agent, text, delivery)
            .await
    }
}

fn with_happening(w: &mut World, what: Happens) {
    w.app.relay = Some(Arc::new(Then {
        inner: w.relay.clone(),
        store: w.store.clone(),
        what,
    }));
}

#[tokio::test]
async fn job_started_does_not_follow_a_job_result_that_beat_it() {
    let mut w = world().await;
    with_happening(&mut w, Happens::TheAgentReports);
    let job = chat(&w).await;
    let (events, _) = poll(&w, "0").await;
    assert_eq!(of_kind(&events, "job.result").len(), 1);
    assert!(
        of_kind(&events, "job.started").is_empty(),
        "the job had ended before it was recorded as started: {events:?}"
    );
    // The thread was still stored, so the person can reply in it.
    let root = w.relay.posts()[0].event_id.clone();
    let (status, body) = start_chat(&w, MEMBER, json!({"text": "more", "rootEventId": root})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["jobId"], json!(job));
}

#[tokio::test]
async fn a_store_that_fails_once_after_the_post_is_tried_again() {
    let mut w = world().await;
    with_happening(&mut w, Happens::TheStoreFails(1));
    let job = chat(&w).await;
    let (events, _) = poll(&w, "0").await;
    let started = of_kind(&events, "job.started");
    assert_eq!(started.len(), 1, "{events:?}");
    assert_eq!(started[0]["job_id"], job);
    assert_eq!(started[0]["root_event_id"], w.relay.posts()[0].event_id);
}

#[tokio::test]
async fn a_store_that_keeps_failing_after_the_post_ends_the_job_and_tells_the_agent() {
    let mut w = world().await;
    // Three tries to record the start fail, and the write that ends the job goes through.
    with_happening(&mut w, Happens::TheStoreFails(3));
    let (status, body) = start_chat(&w, MEMBER, json!({"text": "hello"})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    let posts = w.relay.posts();
    assert_eq!(posts.len(), 2, "the agent is told in the thread");
    assert_eq!(posts[1].root.as_deref(), Some(posts[0].event_id.as_str()));
    assert!(posts[1].text.starts_with("Cancelled"), "{}", posts[1].text);

    // Nothing is left running, so the person may ask again.
    let (events, _) = poll(&w, "0").await;
    assert!(of_kind(&events, "job.started").is_empty());
    w.app.relay = Some(Arc::new(w.relay.clone()));
    let (status, body) = start_chat(&w, MEMBER, json!({"text": "hello again"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_overdue_job_that_cannot_be_ended_does_not_hide_the_ones_that_were() {
    let w = world().await;
    for text in ["first", "second"] {
        let (status, body) = start_chat(&w, MEMBER, json!({"text": text})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let op = w.app.op(false).unwrap();
    w.clock.advance(Duration::from_secs(31 * 60));

    // The first commit fails: that job stays running, and the other is still ended and returned.
    w.store.fail_next_commits(1);
    let events = expire_jobs(&op.env(), community(), 50).await.unwrap();
    assert_eq!(events.len(), 1, "{events:?}");

    // The next sweep ends the one that was skipped.
    let events = expire_jobs(&op.env(), community(), 50).await.unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(expire_jobs(&op.env(), community(), 50)
        .await
        .unwrap()
        .is_empty());
}
