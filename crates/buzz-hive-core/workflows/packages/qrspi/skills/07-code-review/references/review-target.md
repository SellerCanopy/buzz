# Resolving the review target

Read this at step 1 of `/07-code-review`. It decides three things before anything is reviewed: which change is under review (the current branch or a pull request), which base it is diffed against, and which checkout the reviewers read.

## Mode, from `$ARGUMENTS`

- **PR mode**: a pull request number (`123` or `#123`), a pull request URL (`.../pull/123`), or `--pr` alone, meaning the open PR for the current branch (`gh pr view --json number`).
- **Branch mode**: anything else. That covers no argument, or only a task-dir path.
- A task-dir path and `--base <branch>` may accompany either mode.

## Branch mode

Reviews what is committed on the current branch, including commits not yet pushed.

**Base, first hit wins** (the order `/pr` uses, plus the branch's own open PR):

1. `--base <branch>`.
2. `git config --get branch.<current>.qrspiBase`: the stamp `/git-branch` writes on an epic phase branch.
3. The base of the current branch's open PR: `gh pr view --json baseRefName,state`, used only when `state` is `OPEN`.
4. In a worktree (CWD under `.claude/worktrees/<name>/`): the `**Feature branch**:` line in `./work-summary.md`. Skip it when it equals the current branch, since a standalone worktree records its own branch there.
5. The `## Integration branch` in the epic's `_epic.md` (the task dir's parent).
6. The repo default: `gh repo view --json defaultBranchRef --jq .defaultBranchRef.name`. If `gh` fails, use `git symbolic-ref --short refs/remotes/origin/HEAD` without the `origin/` prefix. If that also fails, use `main`.

Record which rule matched; the confirm block names it. If the current branch is the base itself, stop: there is nothing to diff.

**Diff against the remote copy of the base.** Run `git fetch origin <base>` and use `origin/<base>` as the base ref. A stale local base makes the merge base older than it should be, which pulls commits already merged upstream into the review. If there is no remote or the fetch fails, use the local `<base>` and say so.

```
git diff --name-only <base-ref>...HEAD
git diff <base-ref>...HEAD
```

Only committed changes are reviewed. If `git status --porcelain` shows modified tracked files, say in the confirm block that they are not part of the review.

Workflow args: `diffBase: "<base-ref>"`, `headRef: "HEAD"`, `branch`. No `reviewRoot`.

## PR mode

1. **Look it up:** `gh pr view <N> --json number,url,title,state,headRefName,headRefOid,baseRefName`. Stop if it does not resolve. A closed or merged PR can still be reviewed; show its state in the confirm block.
2. **Base:** the PR's `baseRefName`. The PR is the source of truth for its own base. An explicit `--base` overrides it, with a one-line note.
3. **Fetch the PR's head** from the remote that points at the PR's repository (usually `origin`; `upstream` when `origin` is your fork):
   ```
   git fetch <remote> pull/<N>/head
   HEAD_SHA=$(git rev-parse FETCH_HEAD)
   ```
   `pull/<N>/head` exists on the base repository even for a PR from a fork.
4. **Pick the checkout the reviewers read.**
   - If the current `HEAD` is `HEAD_SHA` (you are on the PR's branch and it is up to date), review in place. Pass no `reviewRoot`.
   - Otherwise, check out the PR's head in a temporary worktree:
     ```
     ROOT="$(git rev-parse --show-toplevel)/.claude/worktrees/review-pr-<N>"
     git worktree remove --force "$ROOT" 2>/dev/null
     git worktree add --detach "$ROOT" "$HEAD_SHA"
     ```
     Pass `reviewRoot: "$ROOT"`. Your own branch and uncommitted edits are never touched.
5. **Diff, taken from GitHub** so it matches what the PR shows:
   ```
   gh pr diff <N> --name-only
   gh pr diff <N>
   ```
   Do not compute a PR's diff locally. Once a PR is merged with a merge commit, its head is already in the base, so `git diff <base>...<head>` comes back empty. GitHub refuses very large diffs. For an open PR, fall back to `git fetch <remote> <baseRefName>` and `git diff <remote>/<baseRefName>...$HEAD_SHA`. For a merged PR, stop and tell the user the diff is too large to review this way.
6. **Workflow args:** `diffBase: "<remote>/<baseRefName>"`, `headRef: "<HEAD_SHA>"`, `branch: "<headRefName>"`, `pr: "#<N> <url>"`, plus `reviewRoot` when step 4 made a worktree.
7. **Clean up** once the workflow has finished, failed, or been cancelled: `git worktree remove --force "$ROOT"`. If removal fails, tell the user the path so it is not left behind silently.

## Where the plan lands

- **Branch mode:** the task-dir argument, else the SKILL.md step 2 discovery.
- **PR mode:** the task-dir argument. Else, when the PR's head branch is the current branch, the SKILL.md step 2 discovery. Otherwise use `.claude/reviews/pr-<N>-qa-plan.md` under the current checkout's root, with no `planPath` (so no plan-fidelity). Never pick a task dir by recency for someone else's PR, because that attaches the review to an unrelated ticket.
