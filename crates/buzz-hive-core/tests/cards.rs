//! Card operations over `MemStore`.

mod common;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::orchestrator::cards::{
    add_note, archive_cards, create_card, delete_card, restore_card, update_card, CardPatch,
    NewCard,
};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::testing::MemStore;
use common::{agent, person, COMMUNITY};
use uuid::Uuid;

fn new_card(title: &str) -> NewCard {
    NewCard {
        title: title.into(),
        description: "why".into(),
        workflow: "rpi".into(),
    }
}

#[tokio::test]
async fn create_card_appends_card_created_with_the_actor() {
    let store = MemStore::new();
    let (card, events) = create_card(&store, &agent(), new_card("  Fix it  "))
        .await
        .unwrap();
    assert_eq!(card.title, "Fix it", "the title is trimmed");
    assert_eq!(card.created_by, "bb");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.kind, "card.created");
    assert_eq!(event.card_id, Some(card.id));
    assert_eq!(event.seq, 1);
    assert_eq!(event.actor.pubkey, "bb");
    assert_eq!(event.actor.kind, ActorKind::Agent);
    assert_eq!(
        event.payload,
        serde_json::json!({"title": "Fix it", "workflow": "rpi", "state": "idle"})
    );
    let mut tx = store.begin(COMMUNITY).await.unwrap();
    assert_eq!(tx.cards().get(card.id).await.unwrap(), Some(card));
}

#[tokio::test]
async fn create_card_refuses_a_bad_title_or_description() {
    let store = MemStore::new();
    for title in ["", "   ", &"x".repeat(201)] {
        let err = create_card(&store, &person(), new_card(title))
            .await
            .unwrap_err();
        assert!(matches!(err, OpError::InvalidArgument(_)), "{title:?}");
    }
    let long = NewCard {
        description: "x".repeat(65_537),
        ..new_card("ok")
    };
    assert!(matches!(
        create_card(&store, &person(), long).await.unwrap_err(),
        OpError::InvalidArgument(_)
    ));
    let max = NewCard {
        title: "é".repeat(200),
        description: "x".repeat(65_536),
        workflow: String::new(),
    };
    create_card(&store, &person(), max)
        .await
        .expect("the limits count characters");
    assert!(
        store.events_after(COMMUNITY, 0, 10).await.unwrap().len() == 1,
        "refused calls append nothing"
    );
}

#[tokio::test]
async fn a_failed_operation_leaves_no_event_behind() {
    let store = MemStore::new();
    let err = archive_cards(&store, &person(), &[Uuid::new_v4()])
        .await
        .unwrap_err();
    assert!(matches!(err, OpError::NotFound(_)));
    assert!(store
        .events_after(COMMUNITY, 0, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn update_card_appends_moved_and_state_only_for_what_changed() {
    let store = MemStore::new();
    let (card, _) = create_card(&store, &person(), new_card("a")).await.unwrap();

    let (updated, events) = update_card(
        &store,
        &person(),
        card.id,
        CardPatch {
            workflow: Some("review".into()),
            state: Some(CardState::Running),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        (updated.workflow.as_str(), updated.state),
        ("review", CardState::Running)
    );
    let kinds: Vec<_> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["card.moved", "card.state"]);
    assert_eq!(
        events[0].payload,
        serde_json::json!({"workflow": "review", "from": "rpi"})
    );
    assert_eq!(
        events[1].payload,
        serde_json::json!({"state": "running", "from": "idle"})
    );

    let (_, none) = update_card(
        &store,
        &person(),
        card.id,
        CardPatch {
            workflow: Some("review".into()),
            state: Some(CardState::Running),
        },
    )
    .await
    .unwrap();
    assert!(
        none.is_empty(),
        "a patch that changes nothing appends nothing"
    );
}

#[tokio::test]
async fn archive_is_all_or_nothing_and_skips_cards_already_archived() {
    let store = MemStore::new();
    let (a, _) = create_card(&store, &person(), new_card("a")).await.unwrap();
    let (b, _) = create_card(&store, &person(), new_card("b")).await.unwrap();
    let (busy, _) = create_card(&store, &person(), new_card("busy"))
        .await
        .unwrap();
    update_card(
        &store,
        &person(),
        busy.id,
        CardPatch {
            state: Some(CardState::Running),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let err = archive_cards(&store, &person(), &[a.id, busy.id])
        .await
        .unwrap_err();
    assert!(matches!(err, OpError::FailedPrecondition(_)));
    let mut tx = store.begin(COMMUNITY).await.unwrap();
    assert!(
        !tx.cards().get(a.id).await.unwrap().unwrap().archived,
        "nothing was archived"
    );
    drop(tx);

    let events = archive_cards(&store, &person(), &[a.id, b.id, a.id])
        .await
        .unwrap();
    assert_eq!(events.len(), 2, "a repeated id archives once");
    assert!(events.iter().all(|e| e.kind == "card.archived"));
    let again = archive_cards(&store, &person(), &[a.id]).await.unwrap();
    assert!(again.is_empty(), "a card already archived is left alone");
}

#[tokio::test]
async fn restore_returns_an_archived_card_to_backlog() {
    let store = MemStore::new();
    let (card, _) = create_card(&store, &person(), new_card("a")).await.unwrap();
    let err = restore_card(&store, &person(), card.id).await.unwrap_err();
    assert!(
        matches!(err, OpError::FailedPrecondition(_)),
        "not archived yet"
    );

    archive_cards(&store, &person(), &[card.id]).await.unwrap();
    let (restored, events) = restore_card(&store, &person(), card.id).await.unwrap();
    assert!(!restored.archived);
    assert_eq!(restored.workflow, "");
    assert_eq!(events[0].kind, "card.restored");
    assert_eq!(events[0].payload["workflow"], "");
}

#[tokio::test]
async fn restore_makes_a_card_idle_whatever_state_it_was_archived_in() {
    for state in [CardState::Done, CardState::Stopped, CardState::Failed] {
        let store = MemStore::new();
        let id = Uuid::from_u128(7);
        store.seed_card(
            COMMUNITY,
            buzz_hive_core::domain::card::Card {
                id,
                title: "finished".into(),
                description: String::new(),
                state,
                workflow: "rpi".into(),
                archived: true,
                created_by: "aa".into(),
            },
        );
        let (restored, events) = restore_card(&store, &person(), id).await.unwrap();
        assert_eq!(restored.state, CardState::Idle, "from {state:?}");
        assert_eq!(restored.workflow, "", "from {state:?}");
        assert_eq!(events[0].payload["state"], "idle", "from {state:?}");
    }
}

#[tokio::test]
async fn an_archived_card_cannot_be_changed() {
    let store = MemStore::new();
    let (card, _) = create_card(&store, &person(), new_card("a")).await.unwrap();
    archive_cards(&store, &person(), &[card.id]).await.unwrap();
    let err = update_card(
        &store,
        &person(),
        card.id,
        CardPatch {
            workflow: Some("x".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OpError::FailedPrecondition(_)));
}

#[tokio::test]
async fn delete_removes_a_card_unless_work_is_in_flight() {
    let store = MemStore::new();
    let (card, _) = create_card(&store, &person(), new_card("a")).await.unwrap();
    update_card(
        &store,
        &person(),
        card.id,
        CardPatch {
            state: Some(CardState::Waiting),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        delete_card(&store, &person(), card.id).await.unwrap_err(),
        OpError::FailedPrecondition(_)
    ));
    update_card(
        &store,
        &person(),
        card.id,
        CardPatch {
            state: Some(CardState::Stopped),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let events = delete_card(&store, &person(), card.id).await.unwrap();
    assert_eq!(events[0].kind, "card.removed");
    assert!(matches!(
        delete_card(&store, &person(), card.id).await.unwrap_err(),
        OpError::NotFound(_)
    ));
}

#[tokio::test]
async fn a_note_is_an_activity_event_with_the_actor() {
    let store = MemStore::new();
    let (card, _) = create_card(&store, &person(), new_card("a")).await.unwrap();
    let event = add_note(&store, &agent(), card.id, " Looks good ", "details")
        .await
        .unwrap();
    assert_eq!(event.kind, "activity.added");
    assert_eq!(event.actor.kind, ActorKind::Agent);
    assert_eq!(event.payload["summary"], "Looks good");
    assert_eq!(event.payload["detail"], "details");

    assert!(matches!(
        add_note(&store, &person(), card.id, "", "")
            .await
            .unwrap_err(),
        OpError::InvalidArgument(_)
    ));
    assert!(matches!(
        add_note(&store, &person(), card.id, "ok", &"x".repeat(10_001))
            .await
            .unwrap_err(),
        OpError::InvalidArgument(_)
    ));
    assert!(matches!(
        add_note(&store, &person(), Uuid::new_v4(), "ok", "")
            .await
            .unwrap_err(),
        OpError::NotFound(_)
    ));
}

#[tokio::test]
async fn events_come_back_in_order_and_only_for_their_community() {
    let store = MemStore::new();
    let other = {
        let mut ctx = person();
        ctx.community = buzz_hive_core::ports::store::CommunityId::from_u128(2);
        ctx
    };
    create_card(&store, &person(), new_card("one"))
        .await
        .unwrap();
    create_card(&store, &other, new_card("elsewhere"))
        .await
        .unwrap();
    create_card(&store, &person(), new_card("two"))
        .await
        .unwrap();

    let events = store.events_after(COMMUNITY, 0, 10).await.unwrap();
    let seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 3]);
    let after = store.events_after(COMMUNITY, 1, 10).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(
        store.events_after(COMMUNITY, 0, 1).await.unwrap().len(),
        1,
        "limit"
    );
    let mut tx = store.begin(COMMUNITY).await.unwrap();
    assert_eq!(tx.events().latest_seq().await.unwrap(), 3);
}

#[tokio::test]
async fn an_unlisted_kind_cannot_be_appended() {
    use buzz_hive_core::domain::events::Event;
    let store = MemStore::new();
    let mut tx = store.begin(COMMUNITY).await.unwrap();
    let event = Event::new(
        "card.exploded",
        None,
        None,
        person().actor,
        serde_json::json!({}),
    );
    assert!(tx.events().append(event).await.is_err());
}
