//! Spike 10: a card's branch is the hand-off between stage agents that share nothing but git.
//!
//! Two clones with different identities stand for two agents on different machines. The first
//! pushes the card branch, the second fetches it, adds a commit and pushes it, and the first
//! sees that commit. The pull request is then opened against the replay server for the branch
//! the agents used. The remote is a bare repo in a temp dir, so this proves the git mechanics
//! and the call shape, not GitHub's token scopes, branch rules or the agent image's identity.

mod support;

use std::path::Path;
use std::process::Command;

use buzz_hive::github_adapter::GitHub;
use buzz_hive_core::domain::card::card_branch;
use buzz_hive_core::domain::intake::RepoRef;
use buzz_hive_core::ports::code_host::{CodeHost, NewPullRequest};
use support::github_replay::{Exchange, Replay};
use uuid::Uuid;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        // Nothing of the machine's own git setup leaks in.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn clone_as(origin: &Path, into: &Path, name: &str) {
    git(
        into.parent().unwrap(),
        &[
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            into.to_str().unwrap(),
        ],
    );
    git(into, &["config", "user.name", name]);
    git(
        into,
        &["config", "user.email", &format!("{name}@example.org")],
    );
}

#[tokio::test]
async fn two_agents_hand_a_branch_over_through_git_and_the_pull_request_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git(
        &origin,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );

    // A first commit on main, so the branch has a base.
    let seed = dir.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "seed"]);
    git(&seed, &["config", "user.email", "seed@example.org"]);
    std::fs::write(seed.join("README.md"), "base\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "base"]);
    git(
        &seed,
        &["push", "--quiet", origin.to_str().unwrap(), "main"],
    );

    let branch = card_branch(Uuid::from_u128(0x8b93fa58), "Fix login");

    // Agent one makes the branch, commits and pushes it.
    let one = dir.path().join("one");
    clone_as(&origin, &one, "builder-one");
    git(&one, &["checkout", "--quiet", "-b", &branch]);
    std::fs::write(one.join("fix.txt"), "first\n").unwrap();
    git(&one, &["add", "."]);
    git(&one, &["commit", "--quiet", "-m", "Fix the login"]);
    git(&one, &["push", "--quiet", "-u", "origin", &branch]);

    // Agent two has its own clone and gets the branch only from git.
    let two = dir.path().join("two");
    clone_as(&origin, &two, "builder-two");
    git(&two, &["fetch", "--quiet", "origin", &branch]);
    git(&two, &["checkout", "--quiet", "-b", &branch, "FETCH_HEAD"]);
    assert_eq!(
        std::fs::read_to_string(two.join("fix.txt")).unwrap(),
        "first\n"
    );
    std::fs::write(two.join("fix.txt"), "first\nsecond\n").unwrap();
    git(&two, &["commit", "--quiet", "-am", "Review changes"]);
    git(&two, &["push", "--quiet", "origin", &branch]);

    // Agent one, back for a later stage, fetches and fast-forwards onto agent two's work.
    git(&one, &["fetch", "--quiet", "origin", &branch]);
    git(&one, &["merge", "--quiet", "--ff-only", "FETCH_HEAD"]);
    assert_eq!(
        std::fs::read_to_string(one.join("fix.txt")).unwrap(),
        "first\nsecond\n"
    );
    let authors = git(&one, &["log", "--format=%an <%ae>", &branch]);
    assert!(authors.contains("builder-one <builder-one@example.org>"));
    assert!(authors.contains("builder-two <builder-two@example.org>"));

    // A clone that did not fetch first cannot push over the other's commit: the push is
    // refused, which is why the stage message tells an agent to commit and push, and the
    // worktree template fetches the branch before it starts.
    let three = dir.path().join("three");
    clone_as(&origin, &three, "builder-three");
    git(
        &three,
        &["checkout", "--quiet", "-b", &branch, "origin/main"],
    );
    std::fs::write(three.join("other.txt"), "x\n").unwrap();
    git(&three, &["add", "."]);
    git(&three, &["commit", "--quiet", "-m", "Unrelated"]);
    let refused = Command::new("git")
        .current_dir(&three)
        .args(["push", "--quiet", "origin", &branch])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "a stale branch must not overwrite"
    );

    // The branch is what the pull request asks for.
    let remote_head = git(&origin, &["rev-parse", &branch]);
    assert_eq!(remote_head, git(&one, &["rev-parse", "HEAD"]));
    let replay = Replay::start(vec![
        Exchange::get("/repos/example-org/api/pulls")
            .query("state", "open")
            .query("head", &format!("example-org:{branch}"))
            .body("[]"),
        Exchange::get("/repos/example-org/api").body(r#"{"default_branch":"main"}"#),
        Exchange::post("/repos/example-org/api/pulls").status(201).body(&format!(
            r#"{{"number":5,"state":"open","draft":true,"html_url":"https://github.example.org/example-org/api/pull/5","head":{{"ref":"{branch}"}},"base":{{"ref":"main"}}}}"#
        )),
    ])
    .await;
    let github = GitHub::new(replay.url(), "example-token").unwrap();
    let pull = github
        .open_pull_request(
            &NewPullRequest {
                repo: RepoRef::parse("example-org/api").unwrap(),
                head: branch.clone(),
                base: String::new(),
                title: "Fix login".into(),
                body: "Closes #7".into(),
                draft: true,
            },
            "kiln:card:pr:1",
        )
        .await
        .unwrap();
    assert_eq!(
        (pull.number, pull.head.as_str(), pull.draft),
        (5, branch.as_str(), true)
    );
    replay.finish();
}
