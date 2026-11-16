//! Secrets: a person sets and grants, the engine keeps names only, and the supervisor is told
//! which names an agent gets. No value appears in an event, an error or a return value.

mod common;

use buzz_hive_core::domain::secret::SecretNames;
use buzz_hive_core::orchestrator::agent_tools::seed;
use buzz_hive_core::orchestrator::secrets::{grant, list, revoke, set_agent, set_team, Saved};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::secrets::{SecretError, SecretScope, SecretString};
use buzz_hive_core::testing::{FakeSecretSink, FakeSupervisor};
use common::{agent, person, Fixture, COMMUNITY};
use serde_json::json;

const VALUE: &str = "ghp_known-secret-value-1234";

async fn world() -> (Fixture, FakeSupervisor, FakeSecretSink) {
    let f = Fixture::new();
    seed(&f.store, COMMUNITY, "coder", None).await.unwrap();
    seed(&f.store, COMMUNITY, "researcher", None).await.unwrap();
    let supervisor = FakeSupervisor::new();
    supervisor.deploy("coder", "thread", true);
    supervisor.deploy("researcher", "thread", true);
    (f, supervisor, FakeSecretSink::new())
}

fn secret() -> SecretString {
    SecretString::new(VALUE)
}

fn names(own: &[&str], team: &[&str]) -> SecretNames {
    SecretNames {
        own: own.iter().map(|s| (*s).to_owned()).collect(),
        team: team.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Everything a change returned, as text, to look for the value in.
fn shown(saved: &Saved) -> String {
    let events: Vec<_> = saved.events.iter().map(|e| e.payload.to_string()).collect();
    let applied: Vec<_> = saved
        .applied
        .iter()
        .map(|a| {
            format!(
                "{} {:?}",
                a.agent,
                a.result.as_ref().map_err(ToString::to_string)
            )
        })
        .collect();
    format!("{events:?} {applied:?}")
}

#[tokio::test]
async fn a_team_secret_is_put_in_the_store_and_only_its_name_is_kept() {
    let (f, supervisor, sink) = world().await;
    let saved = set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap();
    assert_eq!(
        sink.value(&SecretScope::Team, "GH_TOKEN").as_deref(),
        Some(VALUE)
    );
    assert_eq!(saved.events.len(), 1);
    assert_eq!(saved.events[0].kind, "secret.changed");
    assert_eq!(
        saved.events[0].payload,
        json!({"scope": "team", "var": "GH_TOKEN", "action": "set"})
    );
    assert_eq!(saved.events[0].actor.pubkey, person().actor.pubkey);
    assert!(!shown(&saved).contains(VALUE));
    // No agent is granted it, so nothing was applied.
    assert!(saved.applied.is_empty());

    let listed = list(&f.store, &person()).await.unwrap();
    assert_eq!(listed.team.len(), 1);
    assert_eq!(listed.team[0].var, "GH_TOKEN");
    assert_eq!(listed.team[0].updated_by, person().actor.pubkey);
    assert!(!format!("{:?}", listed.team).contains(VALUE));
}

#[tokio::test]
async fn an_agent_with_no_grant_gets_no_team_secret_and_a_granted_one_gets_its_name() {
    let (f, supervisor, sink) = world().await;
    set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap();
    set_agent(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "coder",
        "DOCS_TOKEN",
        SecretString::new("docs-value"),
    )
    .await
    .unwrap();
    assert_eq!(
        sink.value(&SecretScope::Agent("coder".into()), "DOCS_TOKEN")
            .as_deref(),
        Some("docs-value")
    );
    // The agent's own secret reaches only that agent.
    assert_eq!(
        supervisor.secrets("coder"),
        Some(names(&["DOCS_TOKEN"], &[]))
    );
    assert_eq!(supervisor.secrets("researcher"), Some(names(&[], &[])));

    let saved = grant(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
    assert_eq!(saved.events[0].payload["action"], "granted");
    assert_eq!(saved.applied.len(), 1);
    assert!(saved.applied[0].result.as_ref().unwrap());
    assert_eq!(
        supervisor.secrets("coder"),
        Some(names(&["DOCS_TOKEN"], &["GH_TOKEN"]))
    );
    assert_eq!(supervisor.secrets("researcher"), Some(names(&[], &[])));

    // A second grant changes nothing and appends no event.
    let again = grant(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
    assert!(again.events.is_empty());
    assert!(!again.applied[0].result.as_ref().unwrap());

    let revoked = revoke(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
    assert_eq!(revoked.events[0].payload["action"], "revoked");
    assert_eq!(
        supervisor.secrets("coder"),
        Some(names(&["DOCS_TOKEN"], &[]))
    );
    let err = revoke(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap_err();
    assert!(matches!(err, OpError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn replacing_a_team_secret_applies_every_agent_that_holds_it() {
    let (f, supervisor, sink) = world().await;
    set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap();
    grant(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
    let before = supervisor.applied().len();
    let saved = set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        SecretString::new("ghp_rotated"),
    )
    .await
    .unwrap();
    assert_eq!(
        sink.value(&SecretScope::Team, "GH_TOKEN").as_deref(),
        Some("ghp_rotated")
    );
    assert_eq!(saved.applied.len(), 1);
    assert_eq!(saved.applied[0].agent, "coder");
    assert_eq!(supervisor.applied().len(), before + 1);
}

#[tokio::test]
async fn a_grant_needs_a_team_secret_and_a_known_agent() {
    let (f, supervisor, sink) = world().await;
    let err = grant(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap_err();
    assert!(matches!(err, OpError::Store(_)), "{err}");
    set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap();
    let err = grant(&f.store, Some(&supervisor), &person(), "nobody", "GH_TOKEN")
        .await
        .unwrap_err();
    assert!(matches!(err, OpError::NotFound(_)), "{err}");
    assert!(list(&f.store, &person()).await.unwrap().grants.is_empty());
}

#[tokio::test]
async fn a_bad_name_or_value_is_refused_before_anything_is_stored_and_never_echoed() {
    let (f, supervisor, sink) = world().await;
    for var in [
        "BUZZ_PRIVATE_KEY",
        "BUZZ_ACP_SESSION_POLICY",
        "gh_token",
        "",
    ] {
        let err = set_team(
            &f.store,
            Some(&sink),
            Some(&supervisor),
            &person(),
            var,
            secret(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpError::InvalidArgument(_)), "{var}: {err}");
        assert!(!err.to_string().contains(VALUE));
    }
    for value in [
        "",
        "has a space",
        "line\nbreak",
        "dollar$HOME",
        &"a".repeat(4097),
    ] {
        let err = set_team(
            &f.store,
            Some(&sink),
            Some(&supervisor),
            &person(),
            "GH_TOKEN",
            SecretString::new(value),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpError::InvalidArgument(_)), "{err}");
        assert!(value.is_empty() || !err.to_string().contains(value));
    }
    assert!(sink.paths().is_empty());
    assert!(list(&f.store, &person()).await.unwrap().team.is_empty());
}

#[tokio::test]
async fn an_agent_may_not_manage_secrets() {
    let (f, supervisor, sink) = world().await;
    let err = set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &agent(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OpError::PermissionDenied(_)), "{err}");
    assert!(sink.paths().is_empty());
    assert!(matches!(
        list(&f.store, &agent()).await.err(),
        Some(OpError::PermissionDenied(_))
    ));
}

#[tokio::test]
async fn with_no_secret_store_nothing_is_saved_and_a_failed_put_keeps_no_name() {
    let (f, supervisor, sink) = world().await;
    let err = set_team(
        &f.store,
        None,
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OpError::FailedPrecondition(_)), "{err}");

    sink.fail_next(SecretError::Unavailable("down".into()));
    let err = set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap_err();
    assert!(!err.to_string().contains(VALUE));
    assert!(list(&f.store, &person()).await.unwrap().team.is_empty());
}

#[tokio::test]
async fn a_change_is_kept_when_the_supervisor_cannot_apply_it() {
    let (f, supervisor, sink) = world().await;
    set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap();
    let saved = grant(&f.store, None, &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
    assert!(saved.applied[0].result.is_err());
    assert_eq!(
        list(&f.store, &person()).await.unwrap().grants,
        [("coder".to_owned(), "GH_TOKEN".to_owned())]
    );
}

#[tokio::test]
async fn a_failed_read_of_the_grants_cannot_leave_a_saved_change_that_reports_an_error() {
    let (f, supervisor, sink) = world().await;
    set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        secret(),
    )
    .await
    .unwrap();
    grant(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
    // The grants are read with the change, so a store that cannot read them saves nothing and
    // says so, instead of committing the change and then returning an error for it.
    f.store.fail_grant_reads(1);
    let other = common::ctx_for("cc", buzz_hive_core::access::ActorKind::Person);
    let err = set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &other,
        "GH_TOKEN",
        SecretString::new("a-rotated-value-9876"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OpError::Store(_)), "{err}");
    let kept = list(&f.store, &person()).await.unwrap();
    assert_eq!(kept.team[0].updated_by, person().actor.pubkey);
}

/// A secret store that, while a value is being put, gives the agent one more team secret, as
/// a grant from another admin would.
struct RacingSink {
    store: buzz_hive_core::testing::MemStore,
}

#[async_trait::async_trait]
impl buzz_hive_core::ports::secrets::SecretSink for RacingSink {
    async fn put(&self, _: SecretScope, _: &str, _: SecretString) -> Result<(), SecretError> {
        let mut tx = buzz_hive_core::ports::store::Store::begin(&self.store, COMMUNITY)
            .await
            .unwrap();
        tx.secrets().set_team("TEAM_RACE", "aa").await.unwrap();
        tx.secrets().grant("coder", "TEAM_RACE").await.unwrap();
        tx.commit().await.unwrap();
        Ok(())
    }
}

#[tokio::test]
async fn an_agent_secret_is_refused_when_a_grant_took_the_last_room_while_it_was_being_put() {
    use buzz_hive_core::domain::secret::MAX_SECRETS_PER_AGENT;
    let (f, supervisor, _) = world().await;
    // One name short of the most: the count before the put passes.
    let mut tx = buzz_hive_core::ports::store::Store::begin(&f.store, COMMUNITY)
        .await
        .unwrap();
    for i in 0..MAX_SECRETS_PER_AGENT - 1 {
        let var = format!("TEAM_{i}");
        tx.secrets().set_team(&var, "aa").await.unwrap();
        tx.secrets().grant("coder", &var).await.unwrap();
    }
    tx.commit().await.unwrap();
    let racing = RacingSink {
        store: f.store.clone(),
    };
    let err = set_agent(
        &f.store,
        Some(&racing),
        Some(&supervisor),
        &person(),
        "coder",
        "DOCS_TOKEN",
        secret(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OpError::FailedPrecondition(_)), "{err}");
    assert!(list(&f.store, &person()).await.unwrap().agents.is_empty());
}
