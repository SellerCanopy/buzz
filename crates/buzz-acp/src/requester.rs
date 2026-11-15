//! The message that started an agent's turn, kept where its tool servers can read it.
//!
//! A tool server is started once, when a session is created, so what it knows is fixed then.
//! The message that started a turn is only known later, so the harness names a file at
//! session creation (`BUZZ_REQUESTER_FILE`) and rewrites it when a turn starts. A tool server
//! that needs to know who is asking reads the file on each call and asks the service that
//! enforces access to confirm it. The file is a hint, never the proof: whatever it holds, the
//! service accepts only a message that really mentions the agent, from the relay.

use std::path::{Path, PathBuf};

use crate::acp::{EnvVar, McpServer};
use crate::queue::BatchEvent;
use crate::scope::SessionScope;

/// The environment variable that names the requester file.
pub const REQUESTER_FILE_ENV: &str = "BUZZ_REQUESTER_FILE";

/// The requester file of `scope` for the worker `agent_index` of the agent `agent_pubkey`.
/// One file per worker and scope, so two sessions never write the same one.
pub fn requester_path(agent_pubkey_hex: &str, agent_index: usize, scope: &SessionScope) -> PathBuf {
    let name = match scope.root_event_id() {
        Some(root) => format!("{}-{root}", scope.channel_id()),
        None => scope.channel_id().to_string(),
    };
    std::env::temp_dir()
        .join("buzz-requester")
        .join(agent_pubkey_hex)
        .join(agent_index.to_string())
        .join(name)
}

/// `servers` with [`REQUESTER_FILE_ENV`] set to `path` on each.
pub fn with_requester_file(servers: Vec<McpServer>, path: &Path) -> Vec<McpServer> {
    let value = path.display().to_string();
    servers
        .into_iter()
        .map(|mut server| {
            server.env.retain(|var| var.name != REQUESTER_FILE_ENV);
            server.env.push(EnvVar {
                name: REQUESTER_FILE_ENV.into(),
                value: value.clone(),
            });
            server
        })
        .collect()
}

/// Writes `event_id` to `path`. The write is atomic, so a tool server never reads half an
/// id, and the directory is private to the user.
pub fn record_requester(path: &Path, event_id: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, event_id)?;
    std::fs::rename(tmp, path)
}

/// The event of a new turn's batch that asks `agent_pubkey_hex` to act: the last one that
/// mentions it, else the last one. A batch of several messages is one turn, and the newest
/// mention is the one the service accepts.
pub fn requester_event(batch: &[BatchEvent], agent_pubkey_hex: &str) -> Option<String> {
    let mentions = |be: &&BatchEvent| {
        be.event.tags.iter().any(|tag| {
            let tag = tag.as_slice();
            tag.first().map(String::as_str) == Some("p")
                && tag.get(1).map(String::as_str) == Some(agent_pubkey_hex)
        })
    };
    batch
        .iter()
        .rev()
        .find(mentions)
        .or_else(|| batch.last())
        .map(|be| be.routing_event_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn scope(root: Option<&str>) -> SessionScope {
        let channel_id = Uuid::from_u128(7);
        match root {
            Some(root) => SessionScope::Thread {
                channel_id,
                root_event_id: root.to_owned(),
            },
            None => SessionScope::Conversation { channel_id },
        }
    }

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("buzz-requester-test-{name}-{}", Uuid::new_v4()))
    }

    #[test]
    fn each_worker_and_scope_has_its_own_file() {
        let a = requester_path("aa", 0, &scope(None));
        assert_ne!(a, requester_path("aa", 1, &scope(None)));
        assert_ne!(a, requester_path("aa", 0, &scope(Some("r1"))));
        assert_ne!(
            requester_path("aa", 0, &scope(Some("r1"))),
            requester_path("aa", 0, &scope(Some("r2")))
        );
        assert_ne!(a, requester_path("bb", 0, &scope(None)));
    }

    #[test]
    fn the_file_is_named_for_every_tool_server_and_replaces_a_stale_name() {
        let servers = vec![
            McpServer {
                name: "one".into(),
                command: "one".into(),
                args: vec![],
                env: vec![EnvVar {
                    name: REQUESTER_FILE_ENV.into(),
                    value: "/stale".into(),
                }],
            },
            McpServer {
                name: "two".into(),
                command: "two".into(),
                args: vec![],
                env: vec![],
            },
        ];
        let path = PathBuf::from("/tmp/requester");
        for server in with_requester_file(servers, &path) {
            let named: Vec<_> = server
                .env
                .iter()
                .filter(|var| var.name == REQUESTER_FILE_ENV)
                .collect();
            assert_eq!(named.len(), 1, "{}", server.name);
            assert_eq!(named[0].value, "/tmp/requester");
        }
    }

    #[test]
    fn a_turn_start_leaves_its_event_id_in_the_file_and_a_new_turn_replaces_it() {
        let path = scratch("write").join("w").join("requester");
        record_requester(&path, &"ab".repeat(32)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ab".repeat(32));
        record_requester(&path, &"cd".repeat(32)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "cd".repeat(32));
        // The write goes through a temporary file that is renamed, so none is left behind.
        assert!(!path.with_extension("tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_reader_sees_a_whole_id_while_the_file_is_replaced() {
        let path = scratch("atomic").join("requester");
        let (first, second) = ("ab".repeat(32), "cd".repeat(32));
        record_requester(&path, &first).unwrap();
        let reader = {
            let path = path.clone();
            let (first, second) = (first.clone(), second.clone());
            std::thread::spawn(move || {
                for _ in 0..2000 {
                    let text = std::fs::read_to_string(&path).unwrap();
                    assert!(text == first || text == second, "half a value: {text:?}");
                }
            })
        };
        for i in 0..2000 {
            record_requester(&path, if i % 2 == 0 { &second } else { &first }).unwrap();
        }
        reader.join().unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn batch_event(agent: Option<&nostr::PublicKey>) -> BatchEvent {
        let keys = nostr::Keys::generate();
        let mut builder = nostr::EventBuilder::new(nostr::Kind::Custom(9), "hi");
        if let Some(agent) = agent {
            builder = builder.tag(nostr::Tag::public_key(*agent));
        }
        BatchEvent {
            event: builder.sign_with_keys(&keys).unwrap(),
            prompt_tag: String::new(),
            received_at: std::time::Instant::now(),
            edit: None,
        }
    }

    #[test]
    fn a_batch_names_its_newest_mention_of_the_agent() {
        let agent = nostr::Keys::generate().public_key();
        let hex = agent.to_hex();
        let (first, chatter, second) = (
            batch_event(Some(&agent)),
            batch_event(None),
            batch_event(Some(&agent)),
        );
        let batch = vec![first, second, chatter];
        let want = batch[1].event.id.to_hex();
        assert_eq!(requester_event(&batch, &hex), Some(want));
        // With no mention in it, the last message stands in and the service decides.
        let batch = vec![batch_event(None), batch_event(None)];
        assert_eq!(
            requester_event(&batch, &hex),
            Some(batch[1].event.id.to_hex())
        );
        assert_eq!(requester_event(&[], &hex), None);
    }
}
