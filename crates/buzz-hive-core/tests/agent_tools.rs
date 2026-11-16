//! An agent's tool servers: the seed read once, a person's change saved then applied, and
//! the settings check.

mod common;

use buzz_hive_core::domain::tool_server::{ToolServer, Transport};
use buzz_hive_core::orchestrator::agent_tools::{apply, list, remove, seed, set, settings_check};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::ports::supervisor::{Supervisor, SupervisorError};
use buzz_hive_core::testing::FakeSupervisor;
use common::{agent, person, Fixture, COMMUNITY};
use serde_json::json;

const MCP: &str = r#"{"mcpServers": {
    "filesystem": {"command": "npx", "args": ["-y", "pkg"]},
    "buzz-hive": {"command": "buzz-hive-mcp"}
}}"#;

fn docs() -> ToolServer {
    ToolServer::new(
        "docs",
        Transport::Http,
        &json!({"url": "https://mcp.example.org/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}),
    )
    .unwrap()
}

async fn seeded() -> (Fixture, FakeSupervisor) {
    let f = Fixture::new();
    seed(&f.store, COMMUNITY, "coder", Some(MCP)).await.unwrap();
    let supervisor = FakeSupervisor::new();
    supervisor.deploy("coder", "channel", false);
    (f, supervisor)
}

fn names(servers: &[(String, ToolServer)]) -> Vec<&str> {
    servers.iter().map(|(_, s)| s.name.as_str()).collect()
}

#[tokio::test]
async fn a_second_seed_changes_nothing_and_a_removed_server_stays_removed() {
    let (f, supervisor) = seeded().await;
    assert!(!seed(&f.store, COMMUNITY, "coder", Some(MCP)).await.unwrap());
    remove(
        &f.store,
        Some(&supervisor),
        &person(),
        "coder",
        "filesystem",
    )
    .await
    .unwrap();
    // A later start reads the same folder again.
    assert!(!seed(&f.store, COMMUNITY, "coder", Some(MCP)).await.unwrap());
    let listed = list(&f.store, &person()).await.unwrap();
    assert_eq!(listed.agents, ["coder"]);
    assert_eq!(names(&listed.servers), ["buzz-hive"]);
}

#[tokio::test]
async fn a_folder_with_no_file_is_a_known_agent_with_no_servers() {
    let f = Fixture::new();
    assert!(seed(&f.store, COMMUNITY, "researcher", None).await.unwrap());
    let listed = list(&f.store, &person()).await.unwrap();
    assert_eq!(listed.agents, ["researcher"]);
    assert!(listed.servers.is_empty());
}

#[tokio::test]
async fn a_bad_file_seeds_nothing() {
    let f = Fixture::new();
    for bad in [
        "not json",
        r#"{"mcpServers": {"a": {"command": "x", "env": {"KEY": "sk-live-1"}}}}"#,
        r#"{"servers": {}}"#,
    ] {
        let err = seed(&f.store, COMMUNITY, "coder", Some(bad))
            .await
            .unwrap_err();
        assert!(matches!(err, OpError::InvalidArgument(_)), "{bad}: {err}");
    }
    assert!(list(&f.store, &person()).await.unwrap().agents.is_empty());
}

#[tokio::test]
async fn set_saves_then_applies_the_whole_list_and_the_second_set_restarts_nothing() {
    let (f, supervisor) = seeded().await;
    let saved = set(&f.store, Some(&supervisor), &person(), "coder", docs())
        .await
        .unwrap();
    assert!(saved.applied.unwrap());
    assert_eq!(saved.events.len(), 1);
    assert_eq!(saved.events[0].kind, "agent_tools.changed");
    assert_eq!(
        saved.events[0].payload,
        json!({"agent": "coder", "server": "docs", "action": "set"})
    );
    let given: Vec<_> = supervisor
        .servers("coder")
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(given, ["buzz-hive", "docs", "filesystem"]);
    let again = set(&f.store, Some(&supervisor), &person(), "coder", docs())
        .await
        .unwrap();
    assert!(!again.applied.unwrap(), "the same list restarts nothing");
}

#[tokio::test]
async fn a_supervisor_that_fails_leaves_the_change_saved() {
    let (f, supervisor) = seeded().await;
    supervisor.fail_next_apply(SupervisorError::Unavailable("socket closed".into()));
    let saved = set(&f.store, Some(&supervisor), &person(), "coder", docs())
        .await
        .unwrap();
    assert!(matches!(
        saved.applied,
        Err(SupervisorError::Unavailable(_))
    ));
    assert_eq!(saved.events.len(), 1, "the saved change is still announced");
    let listed = list(&f.store, &person()).await.unwrap();
    assert!(names(&listed.servers).contains(&"docs"));
    // The next apply carries it.
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert_eq!(tx.agent_tools().of_agent("coder").await.unwrap().len(), 3);
    drop(tx);
    assert!(apply(&f.store, Some(&supervisor), COMMUNITY, "coder")
        .await
        .unwrap());
}

#[tokio::test]
async fn with_no_supervisor_a_change_is_saved_and_says_it_was_not_applied() {
    let (f, _) = seeded().await;
    let saved = set(&f.store, None, &person(), "coder", docs())
        .await
        .unwrap();
    assert!(matches!(
        saved.applied,
        Err(SupervisorError::Unavailable(_))
    ));
}

#[tokio::test]
async fn refusals_write_nothing() {
    let (f, supervisor) = seeded().await;
    let literal = ToolServer {
        name: "leaky".into(),
        transport: Transport::Stdio,
        spec: json!({"command": "x", "env": {"KEY": "sk-live-1"}}),
    };
    let err = set(&f.store, Some(&supervisor), &person(), "coder", literal)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OpError::InvalidArgument(_)), "{err}");
    let err = set(&f.store, Some(&supervisor), &person(), "stranger", docs())
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OpError::NotFound(_)), "{err}");
    for bad in ["../etc", "Coder", "", "a/b"] {
        let err = set(&f.store, Some(&supervisor), &person(), bad, docs())
            .await
            .err()
            .unwrap();
        assert!(matches!(err, OpError::InvalidArgument(_)), "{bad}: {err}");
    }
    let err = set(&f.store, Some(&supervisor), &agent(), "coder", docs())
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OpError::PermissionDenied(_)), "{err}");
    let err = remove(&f.store, Some(&supervisor), &agent(), "coder", "filesystem")
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OpError::PermissionDenied(_)), "{err}");
    let err = remove(&f.store, Some(&supervisor), &person(), "coder", "nope")
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OpError::NotFound(_)), "{err}");
    assert!(supervisor.applied().is_empty());
    assert_eq!(
        names(&list(&f.store, &person()).await.unwrap().servers),
        ["buzz-hive", "filesystem"]
    );
}

#[tokio::test]
async fn an_agent_holds_at_most_thirty_two_servers() {
    let (f, supervisor) = seeded().await;
    for i in 0..30 {
        let server =
            ToolServer::new(&format!("s{i}"), Transport::Stdio, &json!({"command": "x"})).unwrap();
        set(&f.store, Some(&supervisor), &person(), "coder", server)
            .await
            .unwrap();
    }
    let one_more = ToolServer::new("extra", Transport::Stdio, &json!({"command": "x"})).unwrap();
    let err = set(&f.store, Some(&supervisor), &person(), "coder", one_more)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OpError::FailedPrecondition(_)), "{err}");
    // Replacing one that is there is still allowed.
    let replace = ToolServer::new("s0", Transport::Stdio, &json!({"command": "y"})).unwrap();
    set(&f.store, Some(&supervisor), &person(), "coder", replace)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_settings_check_names_an_agent_not_on_thread_or_not_allowlisted() {
    let f = Fixture::new();
    for name in ["coder", "researcher", "writer", "ghost"] {
        seed(&f.store, COMMUNITY, name, None).await.unwrap();
    }
    let supervisor = FakeSupervisor::new();
    supervisor.deploy("coder", "thread", true);
    supervisor.deploy("researcher", "channel", true);
    supervisor.deploy("writer", "thread", false);
    // `ghost` has a folder but is not deployed, so it is left out.
    let (answer, issues) = settings_check(&f.store, Some(&supervisor), &person())
        .await
        .unwrap();
    assert_eq!(answer, "no");
    let by: Vec<_> = issues.iter().map(|i| i.agent.as_str()).collect();
    assert_eq!(by, ["researcher", "writer"]);
    assert_eq!(issues[0].session_policy, "channel");
    assert!(issues[0].allowlisted);
    assert!(issues[1].problem.contains("respond-to"));

    supervisor.deploy("researcher", "thread", true);
    supervisor.deploy("writer", "thread", true);
    let (answer, issues) = settings_check(&f.store, Some(&supervisor), &person())
        .await
        .unwrap();
    assert_eq!((answer.as_str(), issues.len()), ("yes", 0));
}

#[tokio::test]
async fn the_settings_check_is_unknown_without_a_supervisor_or_when_it_fails() {
    let f = Fixture::new();
    seed(&f.store, COMMUNITY, "coder", None).await.unwrap();
    let (answer, _) = settings_check(&f.store, None, &person()).await.unwrap();
    assert_eq!(answer, "unknown");

    struct Down;
    #[async_trait::async_trait]
    impl Supervisor for Down {
        async fn apply(
            &self,
            _: &str,
            _: &[ToolServer],
            _: &buzz_hive_core::domain::secret::SecretNames,
        ) -> Result<bool, SupervisorError> {
            Err(SupervisorError::Unavailable("down".into()))
        }
        async fn settings(
            &self,
            _: &str,
        ) -> Result<buzz_hive_core::ports::supervisor::AgentSettings, SupervisorError> {
            Err(SupervisorError::Unavailable("down".into()))
        }
    }
    let (answer, issues) = settings_check(&f.store, Some(&Down), &person())
        .await
        .unwrap();
    assert_eq!((answer.as_str(), issues.len()), ("unknown", 0));
}
