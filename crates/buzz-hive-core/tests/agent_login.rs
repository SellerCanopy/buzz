//! Signing an agent in to an OAuth tool server: who may, which agent and server may be named,
//! and that the link and the pasted address pass through and are kept nowhere.

mod common;

use buzz_hive_core::domain::agent_login::LoginState;
use buzz_hive_core::orchestrator::agent_login::{paste, start, status};
use buzz_hive_core::orchestrator::agent_tools::seed;
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::testing::FakeSupervisor;
use common::{agent, person, Fixture, COMMUNITY};

const ADDRESS: &str = "http://localhost:45219/callback?code=TOPSECRETCODE&state=fake";

async fn world() -> (Fixture, FakeSupervisor) {
    let f = Fixture::new();
    seed(&f.store, COMMUNITY, "coder", None).await.unwrap();
    let supervisor = FakeSupervisor::new();
    supervisor.deploy("coder", "thread", true);
    (f, supervisor)
}

#[tokio::test]
async fn a_person_starts_pastes_and_reads_the_status() {
    let (f, supervisor) = world().await;
    let s: Option<&dyn buzz_hive_core::ports::supervisor::Supervisor> = Some(&supervisor);
    let ctx = person();
    assert_eq!(
        status(&f.store, s, &ctx, "coder", "docs").await.unwrap(),
        LoginState::NeedsSignIn
    );
    let link = start(&f.store, s, &ctx, "coder", "docs").await.unwrap();
    assert_eq!(link.expose(), FakeSupervisor::link("coder", "docs"));
    // The address is trimmed, as a paste often has a trailing newline.
    paste(
        &f.store,
        s,
        &ctx,
        "coder",
        "docs",
        &format!("  {ADDRESS}\n"),
    )
    .await
    .unwrap();
    assert_eq!(supervisor.pasted(), [ADDRESS]);
    assert_eq!(
        status(&f.store, s, &ctx, "coder", "docs").await.unwrap(),
        LoginState::Connected
    );
    // A second paste has nothing waiting.
    let again = paste(&f.store, s, &ctx, "coder", "docs", ADDRESS).await;
    assert!(matches!(again, Err(OpError::NotFound(_))), "{again:?}");
}

#[tokio::test]
async fn an_agent_an_unknown_agent_a_bad_name_and_a_missing_supervisor_are_refused() {
    let (f, supervisor) = world().await;
    let s: Option<&dyn buzz_hive_core::ports::supervisor::Supervisor> = Some(&supervisor);
    // An agent, whatever its level.
    for result in [
        start(&f.store, s, &agent(), "coder", "docs")
            .await
            .map(|_| ()),
        paste(&f.store, s, &agent(), "coder", "docs", ADDRESS).await,
        status(&f.store, s, &agent(), "coder", "docs")
            .await
            .map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(OpError::PermissionDenied(_))),
            "{result:?}"
        );
    }
    // An agent with no folder, and a path or flag where a name goes.
    let unknown = start(&f.store, s, &person(), "stranger", "docs").await;
    assert!(matches!(unknown, Err(OpError::NotFound(_))), "{unknown:?}");
    for agent_name in ["../etc", "Coder", "a b"] {
        let bad = start(&f.store, s, &person(), agent_name, "docs").await;
        assert!(
            matches!(bad, Err(OpError::InvalidArgument(_))),
            "{agent_name}"
        );
    }
    for server in ["--help", "-x", "a b", "a;b", ""] {
        let bad = start(&f.store, s, &person(), "coder", server).await;
        assert!(
            matches!(bad, Err(OpError::InvalidArgument(_))),
            "{server:?}"
        );
    }
    // A wrong address is refused before the supervisor is asked, and the refusal does not
    // repeat it.
    for wrong in [
        "https://evil.example.org/x?code=TOPSECRETCODE",
        "http://localhost:1/x",
        "TOPSECRETCODE",
    ] {
        let refused = paste(&f.store, s, &person(), "coder", "docs", wrong)
            .await
            .unwrap_err();
        assert!(matches!(refused, OpError::InvalidArgument(_)));
        assert!(!refused.to_string().contains("TOPSECRETCODE"), "{refused}");
    }
    assert!(supervisor.login_calls().is_empty());
    // No supervisor at all.
    let none = start(&f.store, None, &person(), "coder", "docs").await;
    assert!(
        matches!(none, Err(OpError::FailedPrecondition(_))),
        "{none:?}"
    );
}
