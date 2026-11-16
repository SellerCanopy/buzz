//! Saving, copying, deleting, exporting and importing workflows, and the agent's
//! `edit_workflow`, over `MemStore` and a catalog that reads saved packages from it.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use base64::Engine as _;
use buzz_hive_api::App;
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::saved::{Layered, SavedPackages};
use buzz_hive_core::workflow::source::Sources;
use common::{app_with, call, call_for, call_unclaimed, community, AGENT, HIVE, MEMBER, OWNER};
use serde_json::{json, Value};

/// A person at Run who owns the agent `mine`.
const RUNNER: &str = "ff";
/// The agent `mine`, owned by `RUNNER`.
const MINE: &str = "b1";
/// The agent `theirs`, owned by `OWNER`.
const THEIRS: &str = "b2";
const CHANNEL: &str = "00000000-0000-0000-0000-00000000004d";

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// An app whose catalog holds the built-ins and what has been saved.
async fn saving_app() -> App {
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(AGENT, Level::Run, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    tx.access()
        .set_level(RUNNER, Level::Run, ActorKind::Person, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_role(RUNNER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
        .with_named_agent("mine", MINE, RUNNER)
        .with_named_agent("theirs", THEIRS, OWNER)
        .with_named_agent("spare", "b3", OWNER)
        .with_channel(
            uuid::Uuid::parse_str(CHANNEL).unwrap(),
            &[HIVE, MINE, THEIRS],
        );
    App {
        catalog: Arc::new(Layered {
            sources: Sources(vec![builtin_source()]),
            saved: SavedPackages::new(),
        }),
        ..app_with(directory, store)
    }
}

/// The JSON body of a draft named `name` whose one stage runs on `agent`.
fn draft(name: &str, agent: &str) -> Value {
    json!({
        "name": name,
        "files": {
            "kiln-package.yaml": b64(&format!("{{\"name\": \"{name}\", \"version\": \"1.0.0\"}}")),
            "workflow.yaml": b64(&format!(
                "{{\"version\": 2, \"stages\": [{{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"{agent}\", \"prompt\": \"do it\"}}]}}"
            )),
        }
    })
}

async fn save(app: &App, who: &str, name: &str, agent: &str, base: &str) -> (StatusCode, Value) {
    call(
        app,
        "SaveWorkflow",
        who,
        json!({"draft": draft(name, agent), "baseHash": base}),
    )
    .await
}

/// The hash a saved workflow is at now, read the way the editor reads it.
async fn hash_of(app: &App, name: &str) -> String {
    let (status, body) = call(app, "GetWorkflowDraft", RUNNER, json!({"name": name})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["hash"].as_str().unwrap().to_owned()
}

/// How many packages the store holds a version of, whatever their name.
async fn stored(app: &App) -> Vec<String> {
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.workflows()
        .packages()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

#[tokio::test]
async fn a_person_saves_a_new_workflow_and_opens_it_again() {
    let app = saving_app().await;
    let (status, body) = save(&app, RUNNER, "mine-flow", "mine", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["workflow"]["name"], "mine-flow");
    assert_eq!(body["workflow"]["own"], true);
    let hash = body["hash"].as_str().unwrap().to_owned();

    let (_, list) = call(&app, "ListWorkflows", MEMBER, json!({})).await;
    let listed = list["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "mine-flow")
        .unwrap_or_else(|| panic!("the saved workflow is listed: {list}"));
    assert_eq!(listed["stages"][0]["agent"], "mine");

    let (status, opened) = call(
        &app,
        "GetWorkflowDraft",
        MEMBER,
        json!({"name": "mine-flow"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(
        opened["hash"], hash,
        "a draft opens at the hash a save returned"
    );
    assert!(opened["draft"]["files"]["workflow.yaml"].is_string());
}

#[tokio::test]
async fn a_stale_save_is_refused_and_the_newer_version_stays() {
    let app = saving_app().await;
    let (_, first) = save(&app, RUNNER, "flow", "mine", "").await;
    let v1 = first["hash"].as_str().unwrap().to_owned();
    // A second person's save from v1 lands first.
    let changed = json!({
        "name": "flow",
        "files": {
            "kiln-package.yaml": b64("{\"name\": \"flow\", \"version\": \"2.0.0\"}"),
            "workflow.yaml": draft("flow", "mine")["files"]["workflow.yaml"].clone(),
        }
    });
    let changed_files = changed["files"].as_object().unwrap().clone();
    let mut files = changed_files;
    files.insert(
        "workflow.yaml".into(),
        json!(b64("{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"mine\", \"prompt\": \"do it twice\"}]}")),
    );
    let (status, second) = call(
        &app,
        "SaveWorkflow",
        RUNNER,
        json!({"draft": {"name": "flow", "files": files}, "baseHash": v1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let v2 = second["hash"].as_str().unwrap().to_owned();
    assert_ne!(v1, v2);

    let (status, stale) = save(&app, RUNNER, "flow", "mine", &v1).await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "failed_precondition");
    assert!(
        stale["message"].as_str().unwrap().contains("reload"),
        "{stale}"
    );
    assert_eq!(
        hash_of(&app, "flow").await,
        v2,
        "the refused save wrote nothing"
    );
}

#[tokio::test]
async fn a_new_workflow_cannot_take_a_name_that_is_in_use() {
    let app = saving_app().await;
    assert_eq!(
        save(&app, RUNNER, "flow", "mine", "").await.0,
        StatusCode::OK
    );
    let (status, body) = save(&app, RUNNER, "flow", "mine", "").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_exists");
    let (status, body) = save(&app, RUNNER, "rpi", "mine", "").await;
    assert_eq!(
        body["code"], "already_exists",
        "a built-in's name is taken: {body}"
    );
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_name_that_cannot_be_a_folder_is_refused() {
    let app = saving_app().await;
    for name in ["Upper", "has space", "-lead", &"x".repeat(33)] {
        let (status, body) = save(&app, RUNNER, name, "mine", "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
    }
    assert!(stored(&app).await.is_empty());
}

#[tokio::test]
async fn a_draft_with_errors_is_not_written() {
    let app = saving_app().await;
    let broken = json!({
        "name": "broken",
        "files": {
            "kiln-package.yaml": b64("{\"name\": \"broken\", \"version\": \"1.0.0\"}"),
            "workflow.yaml": b64("{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"mine\", \"prompt\": \"x\", \"on\": {\"passed\": \"nowhere\"}}]}"),
        }
    });
    let (status, body) = call(&app, "SaveWorkflow", RUNNER, json!({"draft": broken})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("route_target_unknown"),
        "the refusal names the finding code: {body}"
    );
    assert!(stored(&app).await.is_empty(), "nothing was written");
    let (_, list) = call(&app, "ListWorkflows", MEMBER, json!({})).await;
    assert!(list["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|w| w["name"] != "broken"));
}

#[tokio::test]
async fn an_agent_no_buzz_agent_is_named_is_a_finding_and_a_refusal() {
    let app = saving_app().await;
    let (status, checked) = call(
        &app,
        "CheckWorkflow",
        MEMBER,
        json!({"draft": draft("flow", "nobody")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    let codes: Vec<&str> = checked["packages"][0]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"agent_unknown"), "{checked}");
    let (status, body) = save(&app, RUNNER, "flow", "nobody", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("agent_unknown"));
}

#[tokio::test]
async fn an_agent_the_saver_does_not_own_is_refused_by_name_unless_they_are_admin() {
    let app = saving_app().await;
    let (status, body) = save(&app, RUNNER, "flow", "theirs", "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("theirs"),
        "{body}"
    );
    assert!(stored(&app).await.is_empty());
    // The owner of the agent, who is an admin, may.
    let (status, body) = save(&app, OWNER, "flow", "theirs", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_edit_that_leaves_an_agent_alone_does_not_ask_for_it_again() {
    let app = saving_app().await;
    // The admin builds a workflow on someone else's agent, then the runner edits its prompt.
    let (_, first) = save(&app, OWNER, "flow", "theirs", "").await;
    let base = first["hash"].as_str().unwrap();
    let edited = json!({
        "name": "flow",
        "files": {
            "kiln-package.yaml": draft("flow", "theirs")["files"]["kiln-package.yaml"].clone(),
            "workflow.yaml": b64("{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"theirs\", \"prompt\": \"do it better\"}]}"),
        }
    });
    let (status, body) = call(
        &app,
        "SaveWorkflow",
        RUNNER,
        json!({"draft": edited, "baseHash": base}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Adding a stage on an agent the workflow did not name before is checked, and refused.
    let moved = |extra: &str| {
        json!({
            "name": "flow",
            "files": {
                "kiln-package.yaml": draft("flow", "x")["files"]["kiln-package.yaml"].clone(),
                "workflow.yaml": b64(&format!("{{\"version\": 2, \"stages\": [{{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"theirs\", \"prompt\": \"x\"}}, {{\"id\": \"b\", \"kind\": \"prompt\", \"agent\": \"{extra}\", \"prompt\": \"y\"}}]}}")),
            }
        })
    };
    let (_, current) = call(&app, "GetWorkflowDraft", RUNNER, json!({"name": "flow"})).await;
    let (status, body) = call(
        &app,
        "SaveWorkflow",
        RUNNER,
        json!({"draft": moved("spare"), "baseHash": current["hash"]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("spare"),
        "{body}"
    );
    // An agent they own is fine, and the one the workflow already named stays.
    let (status, body) = call(
        &app,
        "SaveWorkflow",
        RUNNER,
        json!({"draft": moved("mine"), "baseHash": current["hash"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_built_in_cannot_be_saved_over_or_deleted_but_can_be_copied() {
    let app = saving_app().await;
    let (_, opened) = call(&app, "GetWorkflowDraft", RUNNER, json!({"name": "task"})).await;
    let (status, body) = call(
        &app,
        "SaveWorkflow",
        OWNER,
        json!({"draft": opened["draft"], "baseHash": opened["hash"]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call(&app, "DeleteWorkflow", OWNER, json!({"name": "task"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The built-in names agents the directory does not have, so a copy names them too.
    let (status, body) = call(
        &app,
        "DuplicateWorkflow",
        OWNER,
        json!({"name": "task", "newName": "task-copy"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("agent_unknown"),
        "{body}"
    );
}

#[tokio::test]
async fn a_copy_is_the_persons_own_and_can_be_deleted() {
    let app = saving_app().await;
    save(&app, RUNNER, "flow", "mine", "").await;
    let (status, copy) = call(
        &app,
        "DuplicateWorkflow",
        RUNNER,
        json!({"name": "flow", "newName": "flow-copy"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{copy}");
    assert_eq!(copy["name"], "flow-copy");
    assert_eq!(copy["own"], true);
    let (status, again) = call(
        &app,
        "DuplicateWorkflow",
        RUNNER,
        json!({"name": "flow", "newName": "flow-copy"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["code"], "already_exists");

    let (status, body) = call(&app, "DeleteWorkflow", RUNNER, json!({"name": "flow-copy"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = call(
        &app,
        "GetWorkflowDraft",
        RUNNER,
        json!({"name": "flow-copy"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DeleteWorkflow", RUNNER, json!({"name": "flow-copy"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_workflow_a_card_uses_is_not_deleted() {
    let app = saving_app().await;
    save(&app, RUNNER, "flow", "mine", "").await;
    let (status, card) = call(
        &app,
        "CreateCard",
        RUNNER,
        json!({"title": "Fix it", "description": "", "workflow": "flow"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    let (status, body) = call(&app, "DeleteWorkflow", RUNNER, json!({"name": "flow"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"].as_str().unwrap().contains("card"), "{body}");
}

#[tokio::test]
async fn an_agent_may_not_use_the_people_routes() {
    let app = saving_app().await;
    for (method, body) in [
        ("SaveWorkflow", json!({"draft": draft("flow", "mine")})),
        ("DuplicateWorkflow", json!({"name": "task", "newName": "x"})),
        ("DeleteWorkflow", json!({"name": "task"})),
        ("SetWorkflowOptions", json!({"workflow": "task"})),
    ] {
        let (status, body) = call(&app, method, AGENT, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
    }
    assert!(stored(&app).await.is_empty());
}

#[tokio::test]
async fn a_reader_cannot_save() {
    let app = saving_app().await;
    let (status, body) = save(&app, MEMBER, "flow", "mine", "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn options_replace_pauses_and_next_workflow() {
    let app = saving_app().await;
    save(&app, RUNNER, "flow", "mine", "").await;
    save(&app, RUNNER, "other", "mine", "").await;
    let (status, set) = call(
        &app,
        "SetWorkflowOptions",
        RUNNER,
        json!({"workflow": "flow", "pausedStages": ["a"], "nextWorkflow": "other", "labels": [" bug ", ""]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["pausedStages"], json!(["a"]));
    assert_eq!(set["nextWorkflow"], "other");
    assert_eq!(set["labels"], json!(["bug"]));

    let (status, body) = call(
        &app,
        "SetWorkflowOptions",
        RUNNER,
        json!({"workflow": "flow", "pausedStages": ["nope"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // other -> flow would close a loop with flow -> other.
    let (status, body) = call(
        &app,
        "SetWorkflowOptions",
        RUNNER,
        json!({"workflow": "other", "nextWorkflow": "flow"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = call(
        &app,
        "SetWorkflowOptions",
        RUNNER,
        json!({"workflow": "missing"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_workflow_exports_to_a_zip_and_imports_under_a_new_name() {
    let app = saving_app().await;
    save(&app, RUNNER, "flow", "mine", "").await;
    let (status, exported) = call(&app, "ExportWorkflow", RUNNER, json!({"name": "flow"})).await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let (status, imported) = call(
        &app,
        "ImportWorkflow",
        RUNNER,
        json!({"zip": exported["zip"], "name": "flow-again"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["name"], "flow-again");
    assert_eq!(imported["own"], true);
    let (status, taken) = call(
        &app,
        "ImportWorkflow",
        RUNNER,
        json!({"zip": exported["zip"], "name": "flow-again"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{taken}");
    let (status, bad) = call(
        &app,
        "ImportWorkflow",
        RUNNER,
        json!({"zip": b64("not a zip")}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
}

#[tokio::test]
async fn edit_workflow_saves_for_a_requester_at_run_and_proposes_for_anyone_else() {
    let app = saving_app().await;
    let files = |agent: &str| {
        json!({
            "kiln-package.yaml": "{\"name\": \"flow\", \"version\": \"1.0.0\"}",
            "workflow.yaml": format!("{{\"version\": 2, \"stages\": [{{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"{agent}\", \"prompt\": \"do it\"}}]}}"),
        })
    };

    // No requester behind the agent: a proposal, and no version row.
    let (status, body) = call_unclaimed(
        &app,
        "EditWorkflow",
        AGENT,
        json!({"name": "flow", "files": files("mine")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("saved").is_none(), "saved is false: {body}");
    assert!(
        body["proposalId"].as_str().is_some() || body["proposalId"].is_number(),
        "{body}"
    );
    assert!(
        stored(&app).await.is_empty(),
        "a proposal writes no version"
    );
    let open = {
        let mut tx = app.store.begin(community()).await.unwrap();
        tx.proposals().open("workflow").await.unwrap()
    };
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].body["name"], "flow");

    // A requester who may only read: a proposal too, kept for that person.
    let (_, body) = call_for(
        &app,
        "EditWorkflow",
        AGENT,
        MEMBER,
        json!({"name": "flow", "files": files("mine")}),
    )
    .await;
    assert!(body.get("saved").is_none(), "{body}");
    assert!(stored(&app).await.is_empty());

    // A requester at Run: saved, as that person, so they must own the agent.
    let (status, body) = call_for(
        &app,
        "EditWorkflow",
        AGENT,
        RUNNER,
        json!({"name": "flow", "files": files("theirs")}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call_for(
        &app,
        "EditWorkflow",
        AGENT,
        RUNNER,
        json!({"name": "flow", "files": files("mine")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["saved"], true);
    assert_eq!(stored(&app).await, ["flow"]);

    // The agent reads it back as text, and the hash is the base for the next edit.
    let (status, read) = call_for(
        &app,
        "GetWorkflowDraft",
        AGENT,
        RUNNER,
        json!({"name": "flow", "text": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["hash"], body["hash"]);
    assert!(read["textFiles"]["workflow.yaml"]
        .as_str()
        .unwrap()
        .contains("do it"));
    assert!(
        read["draft"].get("files").is_none(),
        "no bytes in text mode: {read}"
    );
}

#[tokio::test]
async fn a_proposal_with_an_error_is_refused_and_stores_nothing() {
    let app = saving_app().await;
    let (status, body) = call_unclaimed(
        &app,
        "EditWorkflow",
        AGENT,
        json!({"name": "flow", "files": {
            "kiln-package.yaml": "{\"name\": \"flow\", \"version\": \"1.0.0\"}",
            "workflow.yaml": "{\"version\": 2, \"stages\": []}",
        }}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let mut tx = app.store.begin(community()).await.unwrap();
    assert!(tx.proposals().open("workflow").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_running_card_keeps_the_version_it_started_with() {
    let app = saving_app().await;
    let (_, saved) = save(&app, RUNNER, "flow", "mine", "").await;
    let v1 = saved["hash"].as_str().unwrap().to_owned();
    let (status, body) = call(
        &app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, card) = call(
        &app,
        "CreateCard",
        RUNNER,
        json!({"title": "Fix it", "description": "", "workflow": "flow"}),
    )
    .await;
    let (status, run) = call(&app, "StartStage", RUNNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{run}");

    // The workflow changes while the card runs.
    let edited = json!({
        "name": "flow",
        "files": {
            "kiln-package.yaml": draft("flow", "mine")["files"]["kiln-package.yaml"].clone(),
            "workflow.yaml": b64("{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"mine\", \"prompt\": \"a new prompt\"}]}"),
        }
    });
    let (status, body) = call(
        &app,
        "SaveWorkflow",
        RUNNER,
        json!({"draft": edited, "baseHash": v1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v2 = body["hash"].as_str().unwrap().to_owned();
    assert_ne!(v1, v2);

    let catalog = app.catalog.load();
    let run_id = uuid::Uuid::parse_str(run["id"].as_str().unwrap()).unwrap();
    let mut tx = app.store.begin(community()).await.unwrap();
    let stored_run = tx.runs().get(run_id).await.unwrap().unwrap();
    assert_eq!(
        stored_run.workflow_hash, v1,
        "the run keeps the hash it started with"
    );
    let pinned = catalog
        .pinned(&stored_run.workflow, &stored_run.workflow_hash)
        .unwrap();
    assert_eq!(
        pinned.hash(),
        v1,
        "the run loads the version it started with"
    );
    assert!(pinned.files["workflow.yaml"].contains("do it"));
    assert_eq!(
        catalog.get("flow").unwrap().hash(),
        v2,
        "a new card gets the new one"
    );
}
