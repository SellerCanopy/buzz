//! The supervisor's three calls: what is refused, what is written, and when it restarts.

mod common;

use std::os::unix::fs::PermissionsExt;

use buzz_hive_core::domain::tool_server::{parse_mcp, Transport};
use buzz_hive_core::supervisor_wire::{Call, Signer, WINDOW_MS};
use common::{base_env, golden, servers, Rig, HIVE, NOW, OTHER};
use serde_json::json;

fn ok(reply: &buzz_hive_core::supervisor_wire::Reply) -> &serde_json::Value {
    assert!(reply.ok, "{reply:?}");
    reply.result.as_ref().unwrap()
}

#[tokio::test]
async fn an_unsigned_badly_signed_stale_replayed_and_unknown_call_is_refused_before_any_work() {
    let rig = Rig::new();
    let call = Call::ApplyAgentConfig {
        agent: "coder".into(),
        servers: servers(),
        secrets: Default::default(),
    };
    let now = NOW;
    let line = rig.signer.sign(&call, now);
    let unsigned = {
        let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
        v.as_object_mut().unwrap().remove("signature");
        v.to_string()
    };
    let forged = {
        let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
        v["signature"] = json!("00".repeat(64));
        v.to_string()
    };
    let stale = Signer::from_secret(common::SECRET)
        .unwrap()
        .sign(&call, now - WINDOW_MS - 1);
    let other_key = Signer::from_secret(b"some other secret that is also long enough")
        .unwrap()
        .sign(&call, now);
    let unknown = {
        // A correctly signed line for a method that is not one of the three.
        let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
        v["method"] = json!("RunShell");
        v.to_string()
    };
    for (what, line, code) in [
        ("unsigned", &unsigned, "unauthenticated"),
        ("not json", &"hello".to_owned(), "unauthenticated"),
        ("forged", &forged, "unauthenticated"),
        ("another key", &other_key, "unauthenticated"),
        ("stale", &stale, "stale"),
        (
            "a method that was not signed for",
            &unknown,
            "unauthenticated",
        ),
    ] {
        let reply = rig.service.handle(line, now).await;
        assert_eq!(reply.error.as_deref(), Some(code), "{what}: {reply:?}");
    }
    // The real line works once, and the same line again is a replay.
    assert!(rig.service.handle(&line, now).await.ok);
    let again = rig.service.handle(&line, now).await;
    assert_eq!(again.error.as_deref(), Some("replayed"));
    assert_eq!(
        rig.restarts(),
        ["coder"],
        "only the one good call did anything"
    );
}

#[tokio::test]
async fn a_method_outside_the_closed_set_is_refused_even_when_signed() {
    let rig = Rig::new();
    for method in ["RunShell", "ReadFile", "LoginRun", "applyagentconfig", ""] {
        let line = rig
            .signer
            .sign_raw(method, r#"{"agent":"coder"}"#, rig.now());
        let reply = rig.service.handle(&line, rig.now()).await;
        assert_eq!(
            reply.error.as_deref(),
            Some("unknown_method"),
            "{method}: {reply:?}"
        );
    }
    // A known method with a body that is not its call is refused too.
    let line = rig
        .signer
        .sign_raw("RestartAgent", r#"{"command":"ls"}"#, rig.now() + 1);
    let reply = rig.service.handle(&line, rig.now()).await;
    assert_eq!(reply.error.as_deref(), Some("bad_request"), "{reply:?}");
    assert!(rig.restarts().is_empty());
}

#[tokio::test]
async fn an_agent_with_no_folder_is_refused_and_nothing_is_written_or_restarted() {
    let rig = Rig::new();
    for agent in [
        "stranger",
        "../src",
        "Coder",
        "",
        "coder/../researcher",
        "a b",
    ] {
        for call in [
            Call::ApplyAgentConfig {
                agent: agent.into(),
                servers: servers(),
                secrets: Default::default(),
            },
            Call::RestartAgent {
                agent: agent.into(),
            },
            Call::AgentSettings {
                agent: agent.into(),
            },
        ] {
            let reply = rig.call(&call).await;
            assert_eq!(
                reply.error.as_deref(),
                Some("not_found"),
                "{agent:?}: {reply:?}"
            );
        }
    }
    assert!(rig.restarts().is_empty());
    assert!(!rig.render("stranger.env").exists());
    assert_eq!(
        std::fs::read_to_string(rig.render("coder.env")).unwrap(),
        base_env()
    );
}

#[tokio::test]
async fn apply_writes_the_golden_files_with_the_right_modes_and_restarts_once() {
    let rig = Rig::new();
    let reply = rig.apply("coder", servers()).await;
    assert_eq!(ok(&reply), &json!({"restarted": true}));
    let mcp = std::fs::read_to_string(rig.render("coder/config/mcp.json")).unwrap();
    let env = std::fs::read_to_string(rig.render("coder.env")).unwrap();
    golden("apply/mcp.json", &mcp);
    golden("apply/coder.env", &env);
    let mode = |path: &str| {
        std::fs::metadata(rig.render(path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("coder/config/mcp.json"), 0o644);
    assert_eq!(mode("coder/config"), 0o755);
    assert_eq!(mode("coder.env"), 0o600);
    assert_eq!(rig.restarts(), ["coder"]);
    // No temporary file is left behind.
    let leftovers: Vec<_> = walk(&rig.render(""))
        .into_iter()
        .filter(|p| p.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path.display().to_string());
        }
    }
    out
}

#[tokio::test]
async fn the_golden_mcp_json_holds_an_http_entry_in_claude_codes_shape() {
    // Spike 7. The file is Claude Code's `.mcp.json`, read by the Claude adapter in the agent
    // container. buzz-acp never parses it, so there is no Rust loader to call here: this checks
    // the shape the adapter documents (`type`, `url`, `headers`) and that the engine's own
    // parser reads it back.
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/apply/mcp.json"),
    )
    .unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        file["mcpServers"]["docs"],
        json!({"type": "http", "url": "https://mcp.example.org/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}})
    );
    assert!(file["mcpServers"]["filesystem"].get("type").is_none());
    let read = parse_mcp(&text).unwrap();
    assert_eq!(
        read.iter().find(|s| s.name == "docs").unwrap().transport,
        Transport::Http
    );
}

#[tokio::test]
async fn render_keeps_the_agents_other_settings_and_its_allowlist() {
    let rig = Rig::new();
    rig.apply("coder", servers()).await;
    let env = std::fs::read_to_string(rig.render("coder.env")).unwrap();
    assert!(env.contains("BUZZ_ACP_SESSION_POLICY=thread\n"));
    assert!(env.contains("BUZZ_ACP_RESPOND_TO=allowlist\n"));
    assert!(env.contains(&format!("BUZZ_ACP_RESPOND_TO_ALLOWLIST={HIVE},{OTHER}\n")));
    assert!(env.contains("BUZZ_RELAY_URL=wss://example.org\n"));
    assert!(env.contains("BUZZ_ACP_AGENT_COMMAND=claude-agent-acp\n"));
}

#[tokio::test]
async fn an_agent_restarts_only_when_what_it_would_run_changed() {
    let rig = Rig::new();
    assert_eq!(
        ok(&rig.apply("coder", servers()).await),
        &json!({"restarted": true})
    );
    // The same list again: no restart, no rewrite.
    assert_eq!(
        ok(&rig.apply("coder", servers()).await),
        &json!({"restarted": false})
    );
    assert_eq!(rig.restarts().len(), 1);
    // A different list restarts it.
    let mut fewer = servers();
    fewer.pop();
    assert_eq!(
        ok(&rig.apply("coder", fewer.clone()).await),
        &json!({"restarted": true})
    );
    assert_eq!(rig.restarts().len(), 2);
    // The deploy renders the agent's env file again without the settings: the next apply puts
    // them back and restarts, since the container was just recreated without them.
    std::fs::write(rig.render("coder.env"), base_env()).unwrap();
    assert_eq!(
        ok(&rig.apply("coder", fewer.clone()).await),
        &json!({"restarted": true})
    );
    assert_eq!(rig.restarts().len(), 3);
    assert_eq!(
        ok(&rig.apply("coder", fewer).await),
        &json!({"restarted": false})
    );
    assert_eq!(rig.restarts().len(), 3);
}

#[tokio::test]
async fn a_restart_that_failed_is_tried_again_by_the_next_apply() {
    let rig = Rig::new();
    rig.make_restart_fail(true);
    let reply = rig.apply("coder", servers()).await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    assert!(reply.message.unwrap().contains("refusing coder"));
    // The files are already right, so only the unfinished restart can make this one run.
    assert!(rig.render("coder/config/mcp.json").exists());
    assert!(rig.restarts().is_empty());
    rig.make_restart_fail(false);
    assert_eq!(
        ok(&rig.apply("coder", servers()).await),
        &json!({"restarted": true})
    );
    assert_eq!(rig.restarts(), ["coder"]);
    assert_eq!(
        ok(&rig.apply("coder", servers()).await),
        &json!({"restarted": false})
    );
}

#[tokio::test]
async fn an_agent_with_no_rendered_environment_is_not_found_and_gets_no_files() {
    let rig = Rig::new();
    let reply = rig.apply("researcher", servers()).await;
    assert_eq!(reply.error.as_deref(), Some("not_found"), "{reply:?}");
    assert!(!rig.render("researcher").exists());
    assert!(rig.restarts().is_empty());
}

#[tokio::test]
async fn restart_agent_restarts_every_time() {
    let rig = Rig::new();
    for _ in 0..2 {
        let reply = rig
            .call(&Call::RestartAgent {
                agent: "coder".into(),
            })
            .await;
        assert_eq!(ok(&reply), &json!({"restarted": true}));
    }
    assert_eq!(rig.restarts(), ["coder", "coder"]);
}

#[tokio::test]
async fn agent_settings_reads_the_rendered_env_and_changes_nothing() {
    let rig = Rig::new();
    let settings = || Call::AgentSettings {
        agent: "coder".into(),
    };
    // As the deploy rendered it: channel, and the Beehive account is not in the list.
    assert_eq!(
        ok(&rig.call(&settings()).await),
        &json!({"session_policy": "channel", "allowlisted": false})
    );
    assert!(rig.restarts().is_empty());
    rig.apply("coder", servers()).await;
    assert_eq!(
        ok(&rig.call(&settings()).await),
        &json!({"session_policy": "thread", "allowlisted": true})
    );
    // No session policy at all reads as the default, channel.
    std::fs::write(
        rig.render("coder.env"),
        "BUZZ_RELAY_URL=wss://example.org\n",
    )
    .unwrap();
    assert_eq!(
        ok(&rig.call(&settings()).await),
        &json!({"session_policy": "channel", "allowlisted": false})
    );
    let none = rig
        .call(&Call::AgentSettings {
            agent: "researcher".into(),
        })
        .await;
    assert_eq!(none.error.as_deref(), Some("not_found"));
}

#[tokio::test]
async fn applies_that_arrive_together_take_turns_and_restart_the_agent_once() {
    let rig = Rig::new();
    // The first restart is held, so the others are all waiting for their turn when it ends.
    rig.hold_restarts(true);
    let release = rig.release_restarts_once_one_has_begun();
    let (a, b, c, d, e, ()) = tokio::join!(
        rig.apply("coder", servers()),
        rig.apply("coder", servers()),
        rig.apply("coder", servers()),
        rig.apply("coder", servers()),
        rig.apply("coder", servers()),
        release,
    );
    let restarted = [a, b, c, d, e]
        .iter()
        .filter(|reply| ok(reply) == &json!({"restarted": true}))
        .count();
    assert_eq!(restarted, 1);
    assert_eq!(rig.restarts(), ["coder"]);
    golden(
        "apply/mcp.json",
        &std::fs::read_to_string(rig.render("coder/config/mcp.json")).unwrap(),
    );
    golden(
        "apply/coder.env",
        &std::fs::read_to_string(rig.render("coder.env")).unwrap(),
    );
}

#[tokio::test]
async fn a_call_that_waits_too_long_for_the_change_ahead_of_it_fails_and_does_nothing() {
    let rig = Rig::with_lock_wait(std::time::Duration::from_millis(300));
    rig.hold_restarts(true);
    let mut fewer = servers();
    fewer.pop();
    let (first, second) = tokio::join!(rig.apply("coder", servers()), async {
        // Begins once the first has its turn, and is let go after the second gave up.
        while !rig.restart_started() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let second = rig.apply("coder", fewer.clone()).await;
        rig.hold_restarts(false);
        second
    });
    assert!(first.ok, "{first:?}");
    assert_eq!(second.error.as_deref(), Some("failed"), "{second:?}");
    assert!(
        second.message.as_deref().unwrap().contains("try again"),
        "{second:?}"
    );
    assert_eq!(rig.restarts(), ["coder"]);
    // The late call wrote nothing: the agent still has the first call's list.
    golden(
        "apply/mcp.json",
        &std::fs::read_to_string(rig.render("coder/config/mcp.json")).unwrap(),
    );
}

#[tokio::test]
async fn a_file_that_cannot_be_read_is_a_failure_that_names_it_and_not_a_missing_agent() {
    let rig = Rig::new();
    assert!(rig.apply("coder", servers()).await.ok);
    let is_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| o.stdout == b"0\n")
        .unwrap_or(false);

    // An env file that is a folder cannot be read, and is not "run a deploy".
    let env = rig.render("coder.env");
    let saved = std::fs::read(&env).unwrap();
    std::fs::remove_file(&env).unwrap();
    std::fs::create_dir(&env).unwrap();
    let settings = rig
        .call(&Call::AgentSettings {
            agent: "coder".into(),
        })
        .await;
    assert_eq!(settings.error.as_deref(), Some("failed"), "{settings:?}");
    assert!(
        settings.message.as_deref().unwrap().contains("coder.env"),
        "{settings:?}"
    );
    std::fs::remove_dir(&env).unwrap();
    std::fs::write(&env, saved).unwrap();
    // A file that is not there is the case "run a deploy" is for.
    let missing = rig
        .call(&Call::AgentSettings {
            agent: "researcher".into(),
        })
        .await;
    assert_eq!(missing.error.as_deref(), Some("not_found"), "{missing:?}");

    // An mcp.json that does not parse, or cannot be read, is named by a sign-in call.
    let mcp = rig.render("coder/config/mcp.json");
    let status = Call::LoginStatus {
        agent: "coder".into(),
        server: "docs".into(),
    };
    std::fs::write(&mcp, "{ not json").unwrap();
    let broken = rig.call(&status).await;
    assert_eq!(broken.error.as_deref(), Some("failed"), "{broken:?}");
    assert!(
        broken.message.as_deref().unwrap().contains("mcp.json"),
        "{broken:?}"
    );
    if !is_root {
        std::fs::write(&mcp, "{}").unwrap();
        std::fs::set_permissions(&mcp, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = rig.call(&status).await;
        assert_eq!(
            unreadable.error.as_deref(),
            Some("failed"),
            "{unreadable:?}"
        );
    }
    // No file at all has no such server.
    std::fs::remove_file(&mcp).unwrap();
    let none = rig.call(&status).await;
    assert_eq!(none.error.as_deref(), Some("not_found"), "{none:?}");
}
