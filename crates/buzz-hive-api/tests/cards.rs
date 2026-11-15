//! The card procedures over `MemStore`.

mod common;

use axum::http::StatusCode;
use buzz_hive_core::testing::MemStore;
use common::{app, call, community, AGENT, MEMBER, OWNER, STRANGER};
use serde_json::json;

async fn create(app: &buzz_hive_api::App, who: &str, title: &str) -> serde_json::Value {
    let (status, card) = call(
        app,
        "CreateCard",
        who,
        json!({"title": title, "description": "details", "workflow": "rpi"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    card
}

#[tokio::test]
async fn a_created_card_has_a_uuid_and_shows_on_the_board_with_its_event() {
    let app = app(MemStore::new()).await;
    let card = create(&app, OWNER, "Fix the build").await;
    let id = card["id"].as_str().unwrap();
    assert!(
        uuid::Uuid::parse_str(id).is_ok(),
        "the id is a uuid string: {id}"
    );
    assert_eq!(card["title"], "Fix the build");
    assert_eq!(card["state"], "idle");
    assert_eq!(card["workflow"], "rpi");
    assert!(card.get("body").is_none(), "only GetCard sets the body");

    let (_, list) = call(&app, "ListCards", MEMBER, json!({})).await;
    assert_eq!(list["cards"][0]["id"], id);
    assert_eq!(
        list["seq"], "1",
        "int64 is a JSON string: the newest event seq"
    );

    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    let event = &polled["events"][0];
    assert_eq!(event["type"], "card.created");
    assert_eq!(event["cardId"], id);
    assert_eq!(event["actorPubkey"], OWNER);
    assert_eq!(event["actorKind"], "person");
    assert_eq!(polled["nextSeq"], "1");
}

#[tokio::test]
async fn an_agent_with_run_creates_cards_and_the_event_names_it() {
    let app = app(MemStore::new()).await;
    create(&app, AGENT, "From an agent").await;
    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    assert_eq!(polled["events"][0]["actorKind"], "agent");
    assert_eq!(polled["events"][0]["actorPubkey"], AGENT);
}

#[tokio::test]
async fn creating_needs_run_and_reading_needs_read() {
    let app = app(MemStore::new()).await;
    for method in [
        "CreateCard",
        "CreateTask",
        "ArchiveCards",
        "RestoreCard",
        "RemoveCard",
        "AddNote",
    ] {
        let (status, body) = call(&app, method, MEMBER, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
        assert_eq!(body["message"], "needs Run, you have Read");
    }
    for method in ["GetCard", "PollEvents", "ListCards"] {
        let (status, _) = call(&app, method, STRANGER, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}");
    }
}

#[tokio::test]
async fn bad_input_is_invalid_argument_or_not_found() {
    let app = app(MemStore::new()).await;
    let cases = [
        ("CreateCard", json!({"title": ""}), "invalid_argument"),
        (
            "CreateCard",
            json!({"title": "x".repeat(201)}),
            "invalid_argument",
        ),
        (
            "CreateCard",
            json!({"title": "ok", "issue": {"host": "h"}}),
            "invalid_argument",
        ),
        (
            "CreateTask",
            json!({"sentence": "ok", "repo": "a/b"}),
            "invalid_argument",
        ),
        (
            "CreateTask",
            json!({"sentence": "ok", "parameters": {"a": "b"}}),
            "invalid_argument",
        ),
        ("GetCard", json!({}), "invalid_argument"),
        ("GetCard", json!({"cardId": "7"}), "invalid_argument"),
        (
            "GetCard",
            json!({"cardId": uuid::Uuid::new_v4().to_string()}),
            "not_found",
        ),
        ("ArchiveCards", json!({}), "invalid_argument"),
        (
            "ArchiveCards",
            json!({"cardIds": [uuid::Uuid::new_v4().to_string()]}),
            "not_found",
        ),
        (
            "RestoreCard",
            json!({"cardId": uuid::Uuid::new_v4().to_string()}),
            "not_found",
        ),
        (
            "RemoveCard",
            json!({"cardId": uuid::Uuid::new_v4().to_string()}),
            "not_found",
        ),
        (
            "AddNote",
            json!({"cardId": uuid::Uuid::new_v4().to_string(), "summary": "x"}),
            "not_found",
        ),
    ];
    for (method, body, code) in cases {
        let (_, reply) = call(&app, method, OWNER, body.clone()).await;
        assert_eq!(reply["code"], code, "{method} {body}: {reply}");
    }
}

#[tokio::test]
async fn create_task_makes_a_card_from_a_sentence() {
    let app = app(MemStore::new()).await;
    let (status, card) = call(
        &app,
        "CreateTask",
        OWNER,
        json!({"sentence": "Write the docs", "description": "soon", "workflow": ""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    assert_eq!(card["title"], "Write the docs");
    let (_, got) = call(&app, "GetCard", MEMBER, json!({"cardId": card["id"]})).await;
    assert_eq!(got["body"], "soon", "GetCard sets the body");
}

#[tokio::test]
async fn archive_restore_and_remove_each_publish_their_event() {
    let app = app(MemStore::new()).await;
    let id = create(&app, OWNER, "a").await["id"].clone();

    let (status, _) = call(&app, "ArchiveCards", OWNER, json!({"cardIds": [id]})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, board) = call(&app, "ListCards", MEMBER, json!({})).await;
    assert!(board.get("cards").is_none(), "the board is empty");
    let (_, archive) = call(&app, "ListCards", MEMBER, json!({"archived": true})).await;
    assert_eq!(archive["cards"][0]["id"], id);

    let (status, restored) = call(&app, "RestoreCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(
        restored["workflow"].as_str().unwrap_or(""),
        "",
        "back to Backlog"
    );
    let (status, again) = call(&app, "RestoreCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");

    let (status, _) = call(&app, "RemoveCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    let kinds: Vec<_> = polled["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        [
            "card.created",
            "card.archived",
            "card.restored",
            "card.removed"
        ]
    );
}

#[tokio::test]
async fn add_note_returns_the_entry_and_logs_it() {
    let app = app(MemStore::new()).await;
    let id = create(&app, OWNER, "a").await["id"].clone();
    let (status, reply) = call(
        &app,
        "AddNote",
        AGENT,
        json!({"cardId": id, "summary": "Checked the logs", "detail": "all green"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let entry = &reply["entry"];
    assert_eq!(entry["type"], "note");
    assert_eq!(entry["summary"], "Checked the logs");
    assert_eq!(entry["cardId"], id);
    assert_eq!(entry["author"], json!({"kind": "agent", "id": AGENT}));
    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({"afterSeq": "1"})).await;
    assert_eq!(polled["events"][0]["type"], "activity.added");
}

#[tokio::test]
async fn other_communities_cards_and_events_stay_out_of_view() {
    use buzz_hive_core::domain::events::Actor;
    use buzz_hive_core::orchestrator::{cards, Ctx};
    use buzz_hive_core::ports::store::CommunityId;
    let store = MemStore::new();
    let elsewhere = Ctx {
        community: CommunityId::from_u128(2),
        actor: Actor {
            pubkey: "ee".into(),
            kind: buzz_hive_core::access::ActorKind::Person,
        },
        requester: None,
    };
    cards::create_card(
        &store,
        &elsewhere,
        cards::NewCard {
            title: "other".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let app = app(store).await;
    let (_, list) = call(&app, "ListCards", MEMBER, json!({})).await;
    assert!(list.get("cards").is_none());
    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    assert!(polled.get("events").is_none());
    assert_eq!(community().as_u128(), 1);
}
