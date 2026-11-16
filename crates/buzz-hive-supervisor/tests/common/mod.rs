//! A supervisor over temporary folders, with a restart script that writes down each restart.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use buzz_hive_core::domain::secret::SecretNames;
use buzz_hive_core::domain::tool_server::{ToolServer, Transport};
use buzz_hive_core::supervisor_wire::{Call, Reply, Signer};
use buzz_hive_supervisor::calls::Service;
use buzz_hive_supervisor::config::{Config, Exec, Restart, Secrets};
use buzz_hive_supervisor::login::Limits;
use serde_json::json;

pub const HIVE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
pub const OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
pub const NOW: i64 = 1_800_000_000_000;
pub const SECRET: &[u8] = b"a test secret that is long enough to use";

/// What the deploy renders for an agent before the supervisor has touched it.
pub fn base_env() -> String {
    format!(
        "BUZZ_ACP_AGENT_COMMAND=claude-agent-acp\n\
         BUZZ_ACP_RESPOND_TO=owner-only\n\
         BUZZ_ACP_RESPOND_TO_ALLOWLIST={OTHER}\n\
         BUZZ_ACP_SESSION_POLICY=channel\n\
         BUZZ_PRIVATE_KEY=example-not-a-key\n\
         BUZZ_RELAY_URL=wss://example.org\n"
    )
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub service: Service,
    pub signer: Signer,
    pub config: Config,
    clock: std::sync::atomic::AtomicI64,
}

impl Rig {
    pub fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    /// A rig whose sign-ins use `limits`, so a test can expire one in a moment.
    pub fn with_limits(limits: Limits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for agent in ["coder", "researcher"] {
            std::fs::create_dir_all(root.join("src/agents").join(agent)).unwrap();
        }
        std::fs::create_dir_all(root.join("render")).unwrap();
        std::fs::write(root.join("render/coder.env"), base_env()).unwrap();
        let restart = root.join("restart.sh");
        std::fs::write(
            &restart,
            format!(
                "#!/bin/sh\n[ $# -eq 1 ] || exit 2\n\
                 if [ -e '{fail}' ]; then echo \"refusing $1\" >&2; exit 1; fi\n\
                 echo \"$1\" >> '{log}'\n",
                fail = root.join("restart-fails").display(),
                log = root.join("restarts.log").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&restart, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The sign-in stubs: an exec runner that stands in for `docker compose exec`, and a
        // stand-in for the Claude CLI next to it.
        for script in ["exec-stub.sh", "claude-stub.sh"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/support")
                    .join(script),
                root.join(script),
            )
            .unwrap();
        }
        let signer = Signer::from_secret(SECRET).unwrap();
        let config = Config {
            socket: root.join("supervisor.sock"),
            engine_key: signer.public_hex(),
            hive_pubkey: HIVE.into(),
            agents_src: root.join("src/agents"),
            render_dir: root.join("render"),
            restart: Restart::Command(restart),
            exec: Exec::Command(root.join("exec-stub.sh")),
            secrets: Secrets::Dir(root.join("secrets")),
            socket_uid: None,
        };
        Self {
            service: Service::new(config.clone())
                .unwrap()
                .with_login_limits(limits),
            signer,
            config,
            dir,
            clock: NOW.into(),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Signs and sends a call. Each call is a second after the one before.
    pub async fn call(&self, call: &Call) -> Reply {
        let now = self
            .clock
            .fetch_add(1000, std::sync::atomic::Ordering::SeqCst);
        self.service.handle(&self.signer.sign(call, now), now).await
    }

    /// Signs `body` as `method` with the engine's key and sends it, for a call the typed
    /// `Call` cannot say (a body the wire refuses, a method that does not exist).
    pub async fn raw(&self, method: &str, body: &str) -> Reply {
        let now = self
            .clock
            .fetch_add(1000, std::sync::atomic::Ordering::SeqCst);
        self.service
            .handle(&self.signer.sign_raw(method, body, now), now)
            .await
    }

    pub fn now(&self) -> i64 {
        self.clock.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn apply(&self, agent: &str, servers: Vec<ToolServer>) -> Reply {
        self.apply_with(agent, servers, SecretNames::default())
            .await
    }

    pub async fn apply_with(
        &self,
        agent: &str,
        servers: Vec<ToolServer>,
        secrets: SecretNames,
    ) -> Reply {
        self.call(&Call::ApplyAgentConfig {
            agent: agent.into(),
            servers,
            secrets,
        })
        .await
    }

    /// Puts a value where the file secret source reads it: `team/env/<var>` or
    /// `agents/<agent>/env/<var>` under the rig's secrets folder.
    pub fn put_secret(&self, relative: &str, value: &str) {
        let path = self.root().join("secrets").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }

    /// The agents restarted so far, in order.
    pub fn restarts(&self) -> Vec<String> {
        std::fs::read_to_string(self.root().join("restarts.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn render(&self, relative: &str) -> PathBuf {
        self.root().join("render").join(relative)
    }

    /// A file the stub CLI keeps, under `stub-state`.
    pub fn stub(&self, relative: &str) -> PathBuf {
        self.root().join("stub-state").join(relative)
    }

    /// Makes the stub CLI `normal`, `reject`, `hang`, `silent` or `list-failed`.
    pub fn set_stub_mode(&self, mode: &str) {
        std::fs::create_dir_all(self.root().join("stub-state")).unwrap();
        std::fs::write(self.stub("mode"), mode).unwrap();
    }

    /// What the stub CLI was run with, one invocation a line: `<agent> mcp login ...`.
    pub fn stub_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.stub("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn make_restart_fail(&self, fails: bool) {
        let marker = self.root().join("restart-fails");
        if fails {
            std::fs::write(marker, "").unwrap();
        } else {
            let _ = std::fs::remove_file(marker);
        }
    }
}

pub fn servers() -> Vec<ToolServer> {
    vec![
        ToolServer::new(
            "buzz-hive",
            Transport::Stdio,
            &json!({"command": "buzz-hive-mcp"}),
        )
        .unwrap(),
        ToolServer::new(
            "filesystem",
            Transport::Stdio,
            &json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem@1.0.0", "/home/agent"]}),
        )
        .unwrap(),
        ToolServer::new(
            "docs",
            Transport::Http,
            &json!({"url": "https://mcp.example.org/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}),
        )
        .unwrap(),
    ]
}

/// Compares `actual` with `tests/golden/<name>`. `UPDATE_GOLDEN=1` rewrites the file.
pub fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display()));
    assert_eq!(actual, want, "{name} differs from its golden file");
}
