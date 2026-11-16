//! The two one-shot commands of the deploy: what `keys` prints and what `apply-agents` counts.

use std::process::Command;

use buzz_hive::commands::{apply_all, key_lines};
use buzz_hive::supervisor_client::UnixSupervisor;
use buzz_hive_core::orchestrator::agent_tools::seed;
use buzz_hive_core::ports::store::CommunityId;
use buzz_hive_core::ports::supervisor::SupervisorError;
use buzz_hive_core::supervisor_wire::Signer;
use buzz_hive_core::testing::{FakeSupervisor, MemStore};

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const COMMUNITY: CommunityId = CommunityId::from_u128(1);

#[test]
fn the_key_lines_hold_the_signing_key_and_the_account_key_and_nothing_secret() {
    let account = nostr::Keys::generate();
    let nsec = nostr::ToBech32::to_bech32(account.secret_key()).unwrap();
    let hex_secret = account.secret_key().to_secret_hex();
    let expected = [
        format!(
            "BUZZ_HIVE_SUPERVISOR_PUBKEY={}",
            Signer::from_secret(SECRET.as_bytes()).unwrap().public_hex()
        ),
        format!(
            "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY={}",
            account.public_key().to_hex()
        ),
    ];
    // The signing key is the one the engine's client signs with.
    assert_eq!(
        expected[0].split('=').nth(1).unwrap(),
        UnixSupervisor::new("/none".into(), SECRET.as_bytes())
            .unwrap()
            .public_hex()
    );
    for form in [nsec.clone(), hex_secret.clone()] {
        let lines = key_lines(SECRET, &form).unwrap();
        assert_eq!(lines, expected);
        let printed = lines.join("\n");
        for secret in [SECRET, nsec.as_str(), hex_secret.as_str()] {
            assert!(!printed.contains(secret));
        }
    }
    let error = key_lines(SECRET, "not a key").unwrap_err();
    assert!(error.contains("BUZZ_HIVE_NOSTR_KEY"), "{error}");
}

#[test]
fn the_built_command_prints_the_lines_and_refuses_what_it_does_not_know() {
    let binary = env!("CARGO_BIN_EXE_buzz-hive");
    let account = nostr::Keys::generate();
    let keys = Command::new(binary)
        .arg("keys")
        .env("BUZZ_HIVE_TOKEN_SECRET", SECRET)
        .env("BUZZ_HIVE_NOSTR_KEY", account.secret_key().to_secret_hex())
        .output()
        .unwrap();
    assert!(keys.status.success(), "{keys:?}");
    let printed = String::from_utf8(keys.stdout).unwrap();
    let expected = key_lines(SECRET, &account.secret_key().to_secret_hex()).unwrap();
    assert_eq!(printed, format!("{}\n{}\n", expected[0], expected[1]));

    let missing = Command::new(binary)
        .arg("keys")
        .env_remove("BUZZ_HIVE_TOKEN_SECRET")
        .env("BUZZ_HIVE_NOSTR_KEY", account.secret_key().to_secret_hex())
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("BUZZ_HIVE_TOKEN_SECRET"));
    assert!(missing.stdout.is_empty());

    let unknown = Command::new(binary).arg("explode").output().unwrap();
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown command explode"));
}

#[tokio::test]
async fn an_agent_that_fails_is_counted_and_does_not_stop_the_ones_after_it() {
    let store = MemStore::new();
    let supervisor = FakeSupervisor::new();
    for agent in ["coder", "researcher", "writer"] {
        seed(
            &store,
            COMMUNITY,
            agent,
            Some(r#"{"mcpServers": {"fs": {"command": "npx"}}}"#),
        )
        .await
        .unwrap();
        supervisor.deploy(agent, "channel", false);
    }
    let agents: Vec<String> = ["coder", "researcher", "writer"]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();

    let all = apply_all(&store, &supervisor, COMMUNITY, &agents).await;
    assert_eq!(all.failed, 0);
    assert_eq!(
        all.lines,
        [
            "coder: applied, restarted: true",
            "researcher: applied, restarted: true",
            "writer: applied, restarted: true"
        ]
    );
    assert_eq!(all.result(3), Ok(()));

    // The first fails; the other two are still applied, and the count says how many.
    supervisor.fail_next_apply(SupervisorError::Unavailable("the restart failed".into()));
    let some = apply_all(&store, &supervisor, COMMUNITY, &agents).await;
    assert_eq!(some.failed, 1);
    assert!(
        some.lines[0].starts_with("coder: not applied: "),
        "{:?}",
        some.lines
    );
    assert!(
        some.lines[0].ends_with("the restart failed"),
        "{:?}",
        some.lines
    );
    assert_eq!(some.lines[1], "researcher: applied, restarted: false");
    assert_eq!(some.lines[2], "writer: applied, restarted: false");
    assert_eq!(
        some.result(3),
        Err("1 of 3 agents were not applied".to_owned())
    );

    // A supervisor that does not know an agent is a failure for it alone.
    let mut more = agents.clone();
    more.push("stranger".into());
    let with_stranger = apply_all(&store, &supervisor, COMMUNITY, &more).await;
    assert_eq!(with_stranger.failed, 1);
    assert!(with_stranger.lines[3].starts_with("stranger: not applied"));
}
