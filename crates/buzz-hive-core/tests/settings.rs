//! Reading and changing settings through the orchestrator.

mod common;

use buzz_hive_core::config::LayerKind;
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::settings::{get_settings, set_setting, Scope, JOIN_MESSAGE};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::testing::FakeDirectory;
use common::{person, Fixture, CHANNEL, HIVE};
use serde_json::{json, Value};

fn channel() -> Value {
    json!(CHANNEL.to_string())
}

fn invalid(error: OpError) -> String {
    match error {
        OpError::InvalidArgument(m) | OpError::FailedPrecondition(m) | OpError::NotFound(m) => m,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_update_channel_is_saved_once_the_beehive_account_is_in_it() {
    let f = Fixture::new();
    let (entry, events) = set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(channel()),
    )
    .await
    .unwrap();
    let entry = entry.unwrap();
    assert_eq!(entry.value, channel());
    assert_eq!(entry.source.layer, LayerKind::User);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "setting.changed");
    let read = get_settings(&f.env(), &person(), &Scope::User, None)
        .await
        .unwrap();
    assert_eq!(read.value("update_channel"), Some(&channel()));
}

#[tokio::test]
async fn a_channel_the_beehive_account_has_not_joined_gets_the_join_message() {
    let directory = FakeDirectory::new().with_channel(CHANNEL, &["someone"]);
    let f = Fixture::new().with_directory(directory);
    let error = set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(channel()),
    )
    .await
    .unwrap_err();
    assert_eq!(invalid(error), JOIN_MESSAGE);
    assert_eq!(
        JOIN_MESSAGE,
        "add the Beehive account to this channel, then save again"
    );
    let read = get_settings(&f.env(), &person(), &Scope::User, None)
        .await
        .unwrap();
    assert_eq!(read.value("update_channel"), None, "nothing was saved");
}

#[tokio::test]
async fn a_channel_that_does_not_exist_or_is_not_an_id_is_refused() {
    let f = Fixture::new();
    let ghost = json!(uuid::Uuid::from_u128(99).to_string());
    let message = invalid(
        set_setting(
            &f.env(),
            &person(),
            &Scope::User,
            "update_channel",
            Some(ghost),
        )
        .await
        .unwrap_err(),
    );
    assert!(message.contains("does not exist"), "{message}");
    let message = invalid(
        set_setting(
            &f.env(),
            &person(),
            &Scope::User,
            "update_channel",
            Some(json!("general")),
        )
        .await
        .unwrap_err(),
    );
    assert!(message.contains("channel id"), "{message}");
    assert_eq!(HIVE, "ee");
}

#[tokio::test]
async fn an_empty_value_removes_the_setting() {
    let f = Fixture::new();
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(channel()),
    )
    .await
    .unwrap();
    let (entry, _) = set_setting(&f.env(), &person(), &Scope::User, "update_channel", None)
        .await
        .unwrap();
    assert_eq!(entry.unwrap().value, Value::Null);
    let read = get_settings(&f.env(), &person(), &Scope::User, None)
        .await
        .unwrap();
    assert_eq!(read.value("update_channel"), None);
}

#[tokio::test]
async fn a_key_belongs_only_to_the_scopes_it_allows() {
    let f = Fixture::new();
    let workflow = Scope::Workflow("demo".into());
    let message = invalid(
        set_setting(
            &f.env(),
            &person(),
            &workflow,
            "update_channel",
            Some(channel()),
        )
        .await
        .unwrap_err(),
    );
    assert!(message.contains("cannot be set on a workflow"), "{message}");
    let message = invalid(
        set_setting(&f.env(), &person(), &Scope::User, "nope", Some(json!(1)))
            .await
            .unwrap_err(),
    );
    assert!(message.contains("unknown setting"), "{message}");
}

#[tokio::test]
async fn workflow_and_card_layers_stack_and_a_stage_override_wins() {
    let f = Fixture::new();
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "c".into(),
            workflow: "demo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let workflow = Scope::Workflow("demo".into());
    let on_card = Scope::Card(card.id);
    set_setting(
        &f.env(),
        &person(),
        &workflow,
        "execution.timeout",
        Some(json!("10m")),
    )
    .await
    .unwrap();
    set_setting(
        &f.env(),
        &person(),
        &workflow,
        "params.depth",
        Some(json!("deep")),
    )
    .await
    .unwrap();
    let (entry, _) = set_setting(
        &f.env(),
        &person(),
        &on_card,
        "stage.research.execution.timeout",
        Some(json!("45m")),
    )
    .await
    .unwrap();
    let entry = entry.unwrap();
    assert_eq!(entry.value, json!("45m"));
    assert_eq!(entry.source.layer, LayerKind::Card);

    let read = get_settings(&f.env(), &person(), &on_card, Some("research"))
        .await
        .unwrap();
    assert_eq!(read.value("execution.timeout"), Some(&json!("45m")));
    assert_eq!(read.value("params.depth"), Some(&json!("deep")));
    let plain = get_settings(&f.env(), &person(), &on_card, None)
        .await
        .unwrap();
    assert_eq!(plain.value("execution.timeout"), Some(&json!("10m")));
    // The engine keys are not part of a card's settings.
    assert!(plain.entry("update_channel").is_none());
}

#[tokio::test]
async fn parameters_are_checked_against_what_the_workflow_declares() {
    let f = Fixture::new();
    let workflow = Scope::Workflow("demo".into());
    for (key, value, want) in [
        ("params.depth", json!("shallow"), "want one of"),
        ("params.colour", json!("red"), "not declared"),
        ("params.depth", json!(3), "want a string"),
    ] {
        let message = invalid(
            set_setting(&f.env(), &person(), &workflow, key, Some(value))
                .await
                .unwrap_err(),
        );
        assert!(message.contains(want), "{key}: {message}");
    }
    let message = invalid(
        set_setting(
            &f.env(),
            &person(),
            &Scope::User,
            "params.depth",
            Some(json!("deep")),
        )
        .await
        .unwrap_err(),
    );
    assert!(message.contains("workflow or a card"), "{message}");
}

#[tokio::test]
async fn an_unknown_workflow_card_or_stage_is_not_found() {
    let f = Fixture::new();
    for scope in [
        Scope::Workflow("gone".into()),
        Scope::Card(uuid::Uuid::from_u128(5)),
    ] {
        let error = get_settings(&f.env(), &person(), &scope, None)
            .await
            .unwrap_err();
        assert!(matches!(error, OpError::NotFound(_)), "{error:?}");
    }
    let error = set_setting(
        &f.env(),
        &person(),
        &Scope::Workflow("demo".into()),
        "stage.nope.execution.timeout",
        Some(json!("1m")),
    )
    .await
    .unwrap_err();
    assert!(invalid(error).contains("has no stage nope"));
}
