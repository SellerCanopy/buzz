//! Every rpc in `engine.proto` is routed, dropped with a reason, or still pending.
//!
//! Ported from the Go engine's `TestEveryProcedureHasAPermission`
//! (`api/permissions_test.go`). Each phase deletes the methods it serves from `PENDING`.

use std::collections::BTreeSet;

use buzz_hive_api::routes::{DROPPED, ROUTES};

const PROTO: &str = include_str!("../proto/kiln/engine/v1/engine.proto");

/// Methods a later phase serves. Each phase removes its own.
const PENDING: &[&str] = &[
    // Phase 16
    "SaveIntakeRule",
    "DeleteIntakeRule",
    "ListIntakeRules",
    "RefreshIntake",
    "PreviewIntake",
    "ListRepos",
    "ListOwners",
    "SearchIssues",
    // Phase 18
    "MergeCard",
    "SetMergeOrder",
    // Phase 19
    "GetWorkflowDraft",
    "SaveWorkflow",
    "DuplicateWorkflow",
    "DeleteWorkflow",
    "SetWorkflowOptions",
    "ExportWorkflow",
    "ImportWorkflow",
    // Phase 20
    "ProposeWorkflowEdit",
    "AdaptSkills",
    "Chat",
    "ListConversations",
    "GetConversation",
    "DeleteConversation",
    "ResolveProposal",
    // Phase 21
    "AddIssueRepo",
    "RemoveIssueRepo",
    "AcceptTicketProposal",
    // Phase 22
    "ListSkills",
];

fn proto_methods() -> BTreeSet<String> {
    let mut methods = BTreeSet::new();
    for line in PROTO.lines() {
        let Some(rest) = line.trim_start().strip_prefix("rpc ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assert!(!name.is_empty(), "unreadable rpc line: {line}");
        methods.insert(name);
    }
    methods
}

#[test]
fn the_proto_keeps_the_89_go_rpcs() {
    // New methods (PollEvents, ListAgents, GetUsage) only add to it.
    assert!(proto_methods().len() >= 89);
}

#[test]
fn every_rpc_is_routed_dropped_or_pending() {
    let all = proto_methods();
    let routed: Vec<&str> = ROUTES.iter().map(|(m, _)| *m).collect();
    let dropped: Vec<&str> = DROPPED.iter().map(|(m, _)| *m).collect();

    for (list, names) in [
        ("ROUTES", &routed),
        ("DROPPED", &dropped),
        ("PENDING", &PENDING.to_vec()),
    ] {
        let unique: BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "{list} repeats a method");
        for name in names {
            assert!(
                all.contains(*name),
                "{list} names {name}, which is not in the proto"
            );
        }
    }

    let mut seen = BTreeSet::new();
    for (list, names) in [
        ("ROUTES", &routed),
        ("DROPPED", &dropped),
        ("PENDING", &PENDING.to_vec()),
    ] {
        for name in names {
            assert!(
                seen.insert(*name),
                "{name} is in {list} and in another list"
            );
        }
    }

    let missing: Vec<_> = all.iter().filter(|m| !seen.contains(m.as_str())).collect();
    assert!(missing.is_empty(), "no disposition for {missing:?}");
}

#[test]
fn every_dropped_method_has_a_reason() {
    for (method, reason) in DROPPED {
        assert!(!reason.trim().is_empty(), "{method} has no reason");
    }
}
