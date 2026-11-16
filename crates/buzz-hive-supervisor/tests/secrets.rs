//! Filling an agent's env file from the secret store: who gets what, what a revoke and a
//! rotation do, and that no value reaches a reply or a log line.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use buzz_hive_core::domain::secret::SecretNames;
use buzz_hive_core::ports::secrets::SecretScope;
use buzz_hive_supervisor::calls::Service;
use buzz_hive_supervisor::config::Secrets;
use buzz_hive_supervisor::secrets::{FileSecretSource, SecretSource, SsmSecretSource};
use common::{golden, servers, Rig};
use serde_json::json;

const TEAM_VALUE: &str = "ghp_TeamValue123";
const OWN_VALUE: &str = "docs.OwnValue456";

fn names(own: &[&str], team: &[&str]) -> SecretNames {
    SecretNames {
        own: own.iter().map(|s| (*s).to_owned()).collect(),
        team: team.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn env_of(rig: &Rig) -> String {
    std::fs::read_to_string(rig.render("coder.env")).unwrap()
}

#[tokio::test]
async fn an_agent_with_no_grant_gets_no_team_secret_and_a_granted_one_is_filled_in() {
    let rig = Rig::new();
    rig.put_secret("team/env/GH_TOKEN", TEAM_VALUE);
    rig.put_secret("agents/coder/env/DOCS_TOKEN", &format!("{OWN_VALUE}\n"));

    // Nothing granted: the team secret is in the store and nowhere in the file.
    assert!(rig.apply("coder", servers()).await.ok);
    assert!(!env_of(&rig).contains("GH_TOKEN"));
    assert!(!env_of(&rig).contains(TEAM_VALUE));

    // Granted, and one of its own.
    let reply = rig
        .apply_with("coder", servers(), names(&["DOCS_TOKEN"], &["GH_TOKEN"]))
        .await;
    assert_eq!(reply.result, Some(json!({"restarted": true})), "{reply:?}");
    golden("apply/coder-secrets.env", &env_of(&rig));
    assert_eq!(
        std::fs::metadata(rig.render("coder.env"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(rig.restarts().len(), 2);

    // The same again changes nothing.
    let again = rig
        .apply_with("coder", servers(), names(&["DOCS_TOKEN"], &["GH_TOKEN"]))
        .await;
    assert_eq!(again.result, Some(json!({"restarted": false})));
}

#[tokio::test]
async fn a_revoke_takes_the_secret_out_and_restarts_and_a_rotation_restarts() {
    let rig = Rig::new();
    rig.put_secret("team/env/GH_TOKEN", TEAM_VALUE);
    rig.apply_with("coder", servers(), names(&[], &["GH_TOKEN"]))
        .await;
    assert!(env_of(&rig).contains(&format!("GH_TOKEN={TEAM_VALUE}\n")));
    assert_eq!(rig.restarts().len(), 1);

    // A new value in the store, the same names: the agent restarts with it.
    rig.put_secret("team/env/GH_TOKEN", "ghp_Rotated789");
    let rotated = rig
        .apply_with("coder", servers(), names(&[], &["GH_TOKEN"]))
        .await;
    assert_eq!(
        rotated.result,
        Some(json!({"restarted": true})),
        "{rotated:?}"
    );
    assert!(env_of(&rig).contains("GH_TOKEN=ghp_Rotated789\n"));
    assert_eq!(rig.restarts().len(), 2);

    // Revoked: gone from the file, the marker and the agent's environment.
    let revoked = rig.apply_with("coder", servers(), names(&[], &[])).await;
    assert_eq!(
        revoked.result,
        Some(json!({"restarted": true})),
        "{revoked:?}"
    );
    let env = env_of(&rig);
    assert!(
        !env.contains("GH_TOKEN") && !env.contains("buzz-hive-injected"),
        "{env}"
    );
    assert_eq!(
        env,
        common::base_env()
            .replace("owner-only", "allowlist")
            .replace(
                "BUZZ_ACP_SESSION_POLICY=channel",
                "BUZZ_ACP_SESSION_POLICY=thread"
            )
            .replace(
                &format!("BUZZ_ACP_RESPOND_TO_ALLOWLIST={}", common::OTHER),
                &format!(
                    "BUZZ_ACP_RESPOND_TO_ALLOWLIST={},{}",
                    common::HIVE,
                    common::OTHER
                ),
            )
    );
    assert_eq!(rig.restarts().len(), 3);
}

#[tokio::test]
async fn an_own_secret_wins_over_a_team_secret_of_the_same_name() {
    let rig = Rig::new();
    rig.put_secret("team/env/API_TOKEN", "team.value");
    rig.put_secret("agents/coder/env/API_TOKEN", "own.value");
    rig.apply_with("coder", servers(), names(&["API_TOKEN"], &["API_TOKEN"]))
        .await;
    let env = env_of(&rig);
    assert!(env.contains("API_TOKEN=own.value\n"), "{env}");
    assert!(!env.contains("team.value"));
}

#[tokio::test]
async fn a_secret_that_is_not_set_fails_the_apply_by_name_and_changes_nothing() {
    let rig = Rig::new();
    let before = env_of(&rig);
    let reply = rig
        .apply_with("coder", servers(), names(&[], &["MISSING_TOKEN"]))
        .await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    assert!(reply.message.unwrap().contains("MISSING_TOKEN"));
    assert_eq!(env_of(&rig), before);
    assert!(rig.restarts().is_empty());
    assert!(!rig.render("coder/config/mcp.json").exists());
}

#[tokio::test]
async fn a_value_the_env_file_could_not_hold_is_refused_without_repeating_it() {
    let rig = Rig::new();
    rig.put_secret("team/env/BAD_TOKEN", "has space and $VAR");
    let reply = rig
        .apply_with("coder", servers(), names(&[], &["BAD_TOKEN"]))
        .await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    let message = reply.message.unwrap();
    assert!(
        message.contains("BAD_TOKEN") && !message.contains("has space"),
        "{message}"
    );
    assert!(rig.restarts().is_empty());
}

#[tokio::test]
async fn a_reserved_name_is_refused_by_the_wire_and_by_the_supervisor() {
    let rig = Rig::new();
    let line = rig.signer.sign_raw(
        "ApplyAgentConfig",
        r#"{"agent":"coder","servers":[],"secrets":{"own":["BUZZ_PRIVATE_KEY"],"team":[]}}"#,
        common::NOW,
    );
    let reply = rig.service.handle(&line, common::NOW).await;
    assert_eq!(reply.error.as_deref(), Some("bad_request"), "{reply:?}");
    assert!(rig.restarts().is_empty());
}

/// A subscriber that keeps everything it is told, at every level.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn no_value_reaches_a_reply_or_a_log_line() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let rig = Rig::new();
    rig.put_secret("team/env/GH_TOKEN", TEAM_VALUE);
    let mut replies = Vec::new();
    replies.push(
        rig.apply_with("coder", servers(), names(&[], &["GH_TOKEN"]))
            .await,
    );
    replies.push(
        rig.apply_with("coder", servers(), names(&[], &["GH_TOKEN"]))
            .await,
    );
    replies.push(
        rig.apply_with("coder", servers(), names(&[], &["MISSING"]))
            .await,
    );
    rig.put_secret("team/env/BAD", "bad value");
    replies.push(
        rig.apply_with("coder", servers(), names(&[], &["BAD"]))
            .await,
    );
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("call done"),
        "the capture saw nothing: {logs}"
    );
    let replies = format!("{replies:?}");
    for text in [&logs, &replies] {
        assert!(
            !text.contains(TEAM_VALUE) && !text.contains("bad value"),
            "{text}"
        );
    }
}

#[tokio::test]
async fn the_file_source_reads_by_the_engines_paths_and_says_nothing_for_a_missing_one() {
    let rig = Rig::new();
    rig.put_secret("agents/coder/env/ONLY_MINE", "mine");
    let Secrets::Dir(dir) = rig.config.secrets.clone() else {
        panic!()
    };
    let source = FileSecretSource { dir };
    let read = |scope: SecretScope, var: &'static str| {
        let source = &source;
        async move {
            source
                .read(&scope, var)
                .await
                .unwrap()
                .map(|v| v.expose().to_owned())
        }
    };
    assert_eq!(
        read(SecretScope::Agent("coder".into()), "ONLY_MINE")
            .await
            .as_deref(),
        Some("mine")
    );
    assert_eq!(read(SecretScope::Team, "ONLY_MINE").await, None);
    assert_eq!(
        read(SecretScope::Agent("researcher".into()), "ONLY_MINE").await,
        None
    );
}

/// A stand-in for the `aws` program that answers `ssm get-parameter` from a folder of files
/// and records the arguments it was given. It never reaches a network.
fn stub_aws(root: &std::path::Path) -> std::path::PathBuf {
    let program = root.join("aws");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n\
             echo \"$@\" >> '{args}'\n\
             [ \"$1 $2\" = 'ssm get-parameter' ] || exit 2\n\
             while [ $# -gt 0 ]; do [ \"$1\" = --name ] && name=$2; shift; done\n\
             case \"$(cat '{mode}' 2>/dev/null)\" in\n\
               denied) echo 'An error occurred (AccessDeniedException) when calling the GetParameter operation: not allowed' >&2; exit 254;;\n\
               hang) sleep 30;;\n\
             esac\n\
             f='{store}'\"$name\"\n\
             if [ -f \"$f\" ]; then cat \"$f\"; echo; exit 0; fi\n\
             echo 'An error occurred (ParameterNotFound) when calling the GetParameter operation' >&2\n\
             exit 254\n",
            args = root.join("aws-args.log").display(),
            store = root.join("ssm").display(),
            mode = root.join("aws-mode").display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

#[tokio::test]
async fn the_ssm_source_asks_the_aws_program_for_the_decrypted_parameter() {
    let rig = Rig::new();
    let cli = stub_aws(rig.root());
    let dir = rig.root().join("ssm/buzz/team/env");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("GH_TOKEN"), TEAM_VALUE).unwrap();

    let service =
        Service::with_secrets(rig.config.clone(), Arc::new(SsmSecretSource::new(cli))).unwrap();
    let now = rig.now() + 1000;
    let line = rig.signer.sign(
        &buzz_hive_core::supervisor_wire::Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers: servers(),
            secrets: names(&[], &["GH_TOKEN"]),
        },
        now,
    );
    let reply = service.handle(&line, now).await;
    assert!(reply.ok, "{reply:?}");
    assert!(env_of(&rig).contains(&format!("GH_TOKEN={TEAM_VALUE}\n")));
    let args = std::fs::read_to_string(rig.root().join("aws-args.log")).unwrap();
    assert!(
        args.contains("ssm get-parameter --with-decryption --name /buzz/team/env/GH_TOKEN"),
        "{args}"
    );

    // A parameter that was never set is a name that is granted but missing.
    let line = rig.signer.sign(
        &buzz_hive_core::supervisor_wire::Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers: servers(),
            secrets: names(&[], &["GH_TOKEN", "NOPE_TOKEN"]),
        },
        now + 1000,
    );
    let reply = service.handle(&line, now + 1000).await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    assert!(reply.message.unwrap().contains("NOPE_TOKEN"));
}

#[tokio::test]
async fn a_supervisor_with_no_store_fails_a_named_secret_and_not_an_apply_that_names_none() {
    let rig = Rig::new();
    let mut config = rig.config.clone();
    config.secrets = Secrets::None;
    let service = Service::new(config).unwrap();
    let now = rig.now() + 1000;
    let call = |secrets| buzz_hive_core::supervisor_wire::Call::ApplyAgentConfig {
        agent: "coder".into(),
        servers: servers(),
        secrets,
    };
    let none = service
        .handle(&rig.signer.sign(&call(names(&[], &[])), now), now)
        .await;
    assert!(none.ok, "{none:?}");
    let some = service
        .handle(
            &rig.signer
                .sign(&call(names(&[], &["GH_TOKEN"])), now + 1000),
            now + 1000,
        )
        .await;
    assert_eq!(some.error.as_deref(), Some("failed"), "{some:?}");
}

#[tokio::test]
async fn an_ssm_failure_that_is_not_a_missing_parameter_is_an_error_and_not_an_unset_secret() {
    let rig = Rig::new();
    let cli = stub_aws(rig.root());
    let before = env_of(&rig);
    let scope = SecretScope::Team;
    let source = SsmSecretSource::new(cli.clone());

    // AccessDenied is a fault with the reason, which is what an admin needs to read.
    std::fs::write(rig.root().join("aws-mode"), "denied").unwrap();
    let error = source.read(&scope, "GH_TOKEN").await.unwrap_err();
    assert!(error.contains("failed"), "{error}");
    assert!(error.contains("AccessDeniedException"), "{error}");
    assert!(error.contains("/buzz/team/env/GH_TOKEN"), "{error}");

    // And an apply reports it and leaves the agent's file and process alone.
    let service =
        Service::with_secrets(rig.config.clone(), Arc::new(SsmSecretSource::new(cli))).unwrap();
    let now = rig.now() + 1000;
    let line = rig.signer.sign(
        &buzz_hive_core::supervisor_wire::Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers: servers(),
            secrets: names(&[], &["GH_TOKEN"]),
        },
        now,
    );
    let reply = service.handle(&line, now).await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    assert!(
        reply.message.as_deref().unwrap().contains("AccessDenied"),
        "{reply:?}"
    );
    assert_eq!(env_of(&rig), before);
    assert!(rig.restarts().is_empty());
}

#[tokio::test]
async fn a_read_that_hangs_or_a_program_that_is_not_there_is_an_error() {
    let rig = Rig::new();
    let scope = SecretScope::Team;

    let missing = SsmSecretSource::new(rig.root().join("no-such-aws"));
    let error = missing.read(&scope, "GH_TOKEN").await.unwrap_err();
    assert!(error.contains("could not run"), "{error}");

    let cli = stub_aws(rig.root());
    std::fs::write(rig.root().join("aws-mode"), "hang").unwrap();
    let mut slow = SsmSecretSource::new(cli);
    slow.timeout = std::time::Duration::from_millis(300);
    let error = slow.read(&scope, "GH_TOKEN").await.unwrap_err();
    assert!(error.contains("took too long"), "{error}");
}

#[tokio::test]
async fn a_secret_path_that_is_a_folder_is_an_error_and_not_an_unset_secret() {
    let rig = Rig::new();
    let dir = rig.root().join("secrets");
    std::fs::create_dir_all(dir.join("team/env/GH_TOKEN")).unwrap();
    let source = FileSecretSource { dir };
    let error = source
        .read(&SecretScope::Team, "GH_TOKEN")
        .await
        .unwrap_err();
    assert!(error.contains("could not read"), "{error}");
    assert!(error.contains("GH_TOKEN"), "{error}");
}
