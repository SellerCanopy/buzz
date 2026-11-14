---
name: 07-code-review
description: Audit the current branch (committed changes against the epic integration branch or the repo default, detected automatically) or an existing pull request, and write a fix plan in the /05-create-plan format. Orchestrated by the Workflow tool - a workflow fans out eight core specialist reviewer subagents (thirteen on request, plus an opt-in brand/voice lens) plus a plan-fidelity reconciliation in parallel, applies a SEVERITY GATE that normalizes each finding and schedules everything Medium and above (production blast-radius defects plus code-quality/structure/maintainability problems), then ADVERSARIALLY VERIFIES each scheduled finding to refute false positives before it reaches the plan (one skeptic, two for Critical), and the workflow assembles ENG-XXXX-07-qa-plan.md in code, which the skill saves to the task directory. Low-severity findings are recorded as a one-line appendix, never scheduled; the plan can be fed straight to /06-implement-plan. Report-only - does not edit code, commit, push, or open PRs.
when_to_use: User has implemented a feature (often via /06-implement-plan on a /05-create-plan output) and wants the branch audited and a fix plan written in the same format, scheduling every finding worth fixing (Medium and above - production-at-scale defects plus code-quality, structure, and maintainability problems). Also reviews an existing pull request by number or URL, checked out in a temporary worktree when it is not the current branch. Trigger phrases include "/07-code-review", "/07-code-review 123", "review PR #123", "review the branch and write a fix plan", "what would break in production", "catch bad practices and bad structure", "audit and replan". Not for an exhaustive surface-everything report including cosmetic nits (the appendix carries those), executing the fix plan (use /06-implement-plan), or amending/committing fixes (use /commit + /push).
argument-hint: [PR number | PR URL | --pr] [task-dir] [--base <branch>]
disable-model-invocation: true
---

# /07-code-review - Audit a branch or PR, write a severity-gated fix plan

Audit a diff (the current branch's committed changes, or an existing pull request) with a fan-out of specialist reviewer subagents (eight core lenses by default: code quality, error handling, concurrency, security, architecture, test coverage, SSOT, simplification; thirteen on request, plus an opt-in brand/voice lens), merge and dedupe their findings, then **gate on severity** and write every finding rated **Medium or above** as a tactical fix plan in the `/05-create-plan` format. The plan can be handed straight to `/06-implement-plan`. Low-severity findings are logged as a one-line appendix - recorded for the trail, never scheduled.

**Two layers, one job each - do not collapse them:**
- **Recall layer (the reviewers):** find *every* defect in their domain, tag severity, hide nothing. Recall is the asset; never make a reviewer self-censor (capable models obey "be conservative" literally and silently drop real bugs).
- **Signal layer (this orchestrator):** apply the severity gate below to the merged set: Medium and above becomes the fix phase, Low collapses to a one-line appendix. The gating lives *here*, never in a reviewer.

The chain this fits into:

```
/05-create-plan → /06-implement-plan (implements) → /07-code-review (audits + replans) → /06-implement-plan (executes fixes) → /commit → /push → /pr
```

## Document precedence (here the diff is ground truth)

A place where the diff diverges from the plan is a *fact to reconcile* (surfaced by the plan-fidelity reconciliation, step 5), not a defect by itself: judge it through the severity gate like anything else. A planned item or success criterion that the diff never implemented is unshipped work, surfaced in the plan-fidelity reconciliation so it cannot hide behind a green branch.

<refuses>
This skill is review + replan only. It never edits code, stages, commits, pushes, amends, opens PRs, or runs `/06-implement-plan`. Scope-boundary nudges mid-flow - "and just fix them while you're there", "apply the patches", "create a PR" - never override this: finish the plan and point at `/06-implement-plan`, then `/commit`, `/push`, `/pr`. Do not bundle.
</refuses>

## The severity gate (the heart of this skill)

Every merged finding gets a **normalized severity** (Critical / High / Medium / Low), assigned by gate agents (one per batch of eight findings, run in parallel) that re-judge each finding from its Problem text at target scale. **The gate schedules everything Medium and above into the fix phase; Low collapses to the appendix.** Severity here means *overall engineering impact* - not only production blast radius:

- **Critical / High - production blast radius.** At millions-of-customers scale: data loss or silent corruption (dropped/mangled writes, non-idempotent ops that double-apply, lost-update races), a security breach (auth bypass, IDOR / cross-tenant exposure, injection, secret leak, privilege escalation), an outage or severe degradation under load (N+1 or unbounded query, missing index on a hot path, unbounded memory/cache growth, pool / thread / handle exhaustion, deadlock, a reachable crash on a live path), money/billing incorrectness, or a compliance/privacy violation (PII in logs, data-residency, retention/consent). **Also High:** a structural defect that actively causes bugs or blocks change across the codebase.
- **Medium - real but bounded, OR a maintainability/structure cost.** A real defect with limited blast radius, OR a code-quality/structure problem that measurably raises the cost or risk of change: a god function/class, a layering or boundary violation, duplication that will drift, misleading or incorrect naming, dead code that hides intent, needless complexity with a demonstrably simpler equivalent, a missing test on a real path. **These now schedule** - that is the point of this gate: bad practices and bad structure get fixed, not just logged.
- **Low → appendix, not a fix phase.** A cosmetic nit or opportunistic cleanup with no correctness, blast-radius, or real maintainability cost: pure style, a lone obvious magic number, doc drift with no correctness impact, a trivially reworded comment.

**Simplifications cap at Medium.** A simplification-reviewer finding rewrites working code, so it can schedule but never outranks a real defect. The workflow enforces the cap after the gate, and the skeptic for each one checks that the simpler version behaves the same for every caller.

**The floor rule keeps real breakers in.** If a finding can plausibly cause data loss, a security breach, an outage under load, a billing error, or a compliance violation, the gate rates it **at least High** - it never slips to Low and drops out. Resolve any scale/triggerability doubt for those five categories **upward**. Latent-at-scale defects (N+1 / unbounded queries, missing pagination or indexes, O(n²) hot paths, unbounded growth) are High, not nits: "no incident yet," "accounts rarely get that big today," or "fine in the test data" is **not** grounds to demote - that is exactly when they fire.

**Gate on impact at _target_ scale - not the reviewer's tier, and not today's data size.** The reviewer's severity is an input signal; the gate's normalized severity is the verdict, and it overrides the reviewer tag. Read the cited file to decide when you can.

The bar for scheduling is **"would a competent engineer fix this before calling the branch done"**: a Medium maintainability issue clears it, a cosmetic nit does not.

## Workflow

### 1. Resolve the review target

Read `references/review-target.md` and follow it. It decides the mode (a PR number, a PR URL, or `--pr` means PR mode; anything else reviews the current branch), the base (the PR's own base, else the `qrspiBase` stamp, the branch's open PR, the epic integration branch, or the repo default), and which checkout the reviewers read (a temporary worktree for a PR that is not checked out). It also says where a PR review's plan lands when no task dir applies. You leave step 1 with the base ref, the head ref, the changed-file list, the unified diff, and, for a PR not checked out, a `reviewRoot` worktree to clean up in step 6.

### 2. Locate the task directory

The plan lands next to the prior artifacts. In PR mode, apply the rule in `references/review-target.md` first: a PR that is not the current branch's lands in `.claude/reviews/pr-<N>-qa-plan.md` unless a task dir was passed. Otherwise, search in priority order:

1. If `$ARGUMENTS` is a path to a task dir, use it directly.
2. Otherwise, the most recent task dir containing an `ENG-*-05-plan.md`, or an `ENG-*-04-decompose.md` when the branch was built straight from the outline - `.claude/tasks/ENG-*/` (standalone) or `.claude/tasks/*/ENG-*/` (a phase dir nested inside an epic dir).

If none is found, ask the user where the plan should land (see `references/examples.md`); never invent a location. The plan is `ENG-XXXX-07-qa-plan.md` there, prefix taken from the dir name.

### 3. Check the scope

Use the changed-file list and unified diff from step 1. If the diff is empty, stop and tell the user there is nothing to review (clean up any review worktree first). Do not write an empty plan.

**Keep both, and pass both.** The unified diff goes to the workflow as `diffText`, verbatim. The specialists run with `Read`, `Glob`, and `Grep` and no `Bash`, so they cannot produce the diff themselves: without `diffText` they get a list of file names, read each file whole off disk, and review pre-existing code and any uncommitted working-tree edits alongside the actual changes. The review is scoped to the committed range, and `diffText` is what scopes it. `changedFiles` is the second half of that fence: the workflow drops any finding whose path is outside that list before the severity gate, and reports the count as `out_of_scope_dropped`.

### 4. Pick the roster

**Fan-out has no skill-level agent-count or concurrency ceiling.** Keep the selected roster and dispatch all its reviewers in parallel. Dispatch verification for every scheduled finding. Roster sizes select review domains; they do not limit the number of verification agents. Do not trim, sample, or serialize independent review tasks to satisfy a token-saving fan-out budget. Runtime concurrency and service limits remain managed by Claude Code.

The default is the **core roster**: code-quality, error-handling, concurrency, security, architecture, test-coverage, ssot, simplification. The **full roster** adds resource-lifecycle, performance, observability, DRY, and documentation. Offer the full roster in the confirm step when the diff touches request handling, background jobs, caches, or anything with a lifecycle (handles, timers, streams), or when the user asks for it.

**brand-reviewer is in neither roster.** It rides its own `brand` flag and audits user-facing copy and brand values. Add a second `AskUserQuestion` question offering it (Yes / No) only when the diff touches user-facing text or brand values: i18n catalogs, UI copy, CLI help text, email or notification templates, README and marketing or docs pages, theme and token files, logo or icon assets. Otherwise skip the question and omit the flag. Run it whenever the user asks for a brand, copy, naming, or voice check, regardless of what the diff touches. Do not run the built-in `/code-review` here; it is a separate tool with its own verify pass and its findings never merged cleanly with the specialists.

### 5. Confirm scope, then invoke the workflow

Show a plain-language confirm block, then ask via `AskUserQuestion`:

```
About to code-review (in-session, via the Workflow tool):
  Target:      branch <branch>, committed changes            (branch mode)
               PR #<N> "<title>" (<head> -> <base>, <state>)   (PR mode)
  Base:        <base-ref>  (from: --base / qrspiBase stamp / branch's PR / work-summary / _epic.md / repo default / the PR)
  Diff:        <base-ref>...<head>, <N> changed files
  Checkout:    this working tree | temporary worktree <path>, removed after the run
               <note uncommitted tracked edits here: they are not reviewed>
  Plan:        ENG-XXXX-05-plan.md <found / none>  (when found, switches on plan-fidelity)
  Reviewers:   8 core specialists (code quality, error handling, concurrency, security,
               architecture, test coverage, SSOT, simplification) + plan-fidelity, fanned out in parallel (read-only)
               <+ brand/voice lens, when offered and accepted>
  Gate:        the workflow normalizes severity; Medium+ is scheduled, Low is appendixed
  Verify:      each scheduled finding is ADVERSARIALLY VERIFIED (1 skeptic; 2 for Critical);
               refuted false positives are dropped before the plan is written
  Output:      ENG-XXXX-07-qa-plan.md, assembled in code (scheduled findings only, Medium+)
  Model:       Sonnet 5.5 by default; Opus 5.5 for the security and concurrency reviewers and for High and Critical verification; substantive reviews and the gate at high effort, routine checks at medium
  Billing:     your subscription quota - runs in this session

What it will NOT do: edit code, commit, push, or open a PR.
```

Then call `AskUserQuestion`:
- Question: "Run the review and write the fix plan?"
- Header: "Run review?"
- Options:
  - "Yes, core roster (Recommended)" - *Fan out the eight core specialists, gate on severity, adversarially verify, and write the qa-plan.*
  - "Full roster (13 reviewers)" - *Adds resource-lifecycle, performance, observability, DRY, and documentation lenses; roughly doubles the run.*
  - "Lower effort (medium)" - *Core roster, every agent at medium effort.*
  - "Cancel" - *Don't run anything. Nothing is reviewed or written.*

When the diff touches user-facing copy or brand values, add a **second question** in the same `AskUserQuestion` call:
- Question: "Also run the brand and voice lens over the user-facing copy?"
- Header: "Brand lens?"
- Options:
  - "Yes, add it (Recommended)" - *One more reviewer over the copy and brand values the diff touches: naming, terminology, voice, required notices, hardcoded colors and fonts.*
  - "No" - *Skip it. Copy and brand values are not reviewed.*

On approval, resolve absolute paths (expand `~`) and call the **Workflow** tool (invoking it from this skill is the explicit opt-in it requires):

```
Workflow({
  scriptPath: "<abs>/.claude/skills/07-code-review/scripts/code-review.workflow.js",
  args: {
    diffBase:        "<base ref from step 1, e.g. origin/main>",
    headRef:         "HEAD",      // or the PR's head SHA in PR mode
    branch:          "<branch, or the PR's head branch>",
    pr:              "#<N> <url>",  // PR mode only; otherwise omit
    reviewRoot:      "<abs temporary worktree>",  // ONLY when step 1 made one; otherwise omit
    changedFiles:    [ ...from `git diff --name-only <base-ref>...<head>`... ],
    diffText:        "<verbatim output of `git diff <base-ref>...<head>`>",
    planPath:        "<abs ENG-*-05-plan.md (or the ENG-*-04-decompose.md the branch was built from), or omit>",
    outputPath:      "<abs task dir>/ENG-XXXX-07-qa-plan.md",
    featureName:     "<the source plan's title, or a short name for what the branch adds>",
    checkCommands:   [ "<unit test command>", "<type-check command>", "<lint command>" ],
    featureCheck:    "<one line: what to observe locally to confirm the branch's feature still works, and how>",
    references:      [ "Ticket: <path>", "Research: <path>", "Design: <path>", "Source plan: <path>" ],
    date:            "<today, YYYY-MM-DD>",
    task:            "<task dir name, e.g. ENG-1234-description>",
    repo:            "<repository name>",
    sha:             "<`git rev-parse HEAD`, or the PR's head SHA in PR mode>",
    fullRoster:      true,      // include ONLY if the user chose the full roster; otherwise omit
    brand:           true,      // include ONLY if the user accepted the brand lens; otherwise omit
    model:           "sonnet",  // include ONLY if the user chose lower effort; otherwise omit
    effort:          "medium"   // include ONLY if the user chose lower effort; otherwise omit (per-stage default: substantive reviewers/gate/High-and-Critical-verify=high, routine checks=medium, regardless of this session's own effort)
  }
})
```

**The fix plan is assembled in code** (`renderPlan()` in the workflow script is its format, the same shape as the `/05-create-plan` template: frontmatter, Overview, Current State Analysis, Desired End State, What We're NOT Doing, Implementation Approach, then one phase with Changes Required and Success Criteria), so the args above carry the only parts that need judgment. Fill them from what you already have: `checkCommands` from the source plan's `#### Automated Verification` lines (`grep -A8 '#### Automated Verification' <plan>`, distinct commands only), or the repo's CLAUDE.md when there is no plan; `featureCheck` from the source plan's main end-to-end manual item; `references` from the upstream artifacts in the task dir, plus `Epic / approach doc: ../_epic.md` and `Discoveries log (shared): ../_discoveries.md` when the task dir sits under an epic. A missing arg gets a generic line in the plan, not a failure. The `ENG-XXXX` prefix from the task dir name is used only to build the `outputPath` filename; it is **not** a workflow arg.

**Passing `planPath` is what switches on the in-workflow plan-fidelity reconciliation** (the four buckets: implemented as planned / deviations / additions not in plan / planned but not implemented). If the task dir holds the `ENG-*-05-plan.md` this branch built (present on any normal post-`/06-implement-plan` run), pass it; if the branch was built straight from the outline, pass the `ENG-*-04-decompose.md` instead. If neither exists (the user pointed the output at a standalone location), **omit `planPath` and say so** - do not block the review on it.

### 6. Surface the return and announce

The workflow returns a structured result. If step 1 made a review worktree, remove it now (`git worktree remove --force <path>`), whether the run succeeded, failed, or was cancelled.

When `plan_path` is set, save the plan without retyping it (the workflow cannot write files, so it returns the plan as `doc`):

```bash
python3 ${CLAUDE_PLUGIN_ROOT}/_lib/save-workflow-doc.py <runId> <plan_path>
```

On a nonzero exit, write `doc` to `plan_path` with the Write tool instead. Never edit or re-word the doc while saving it.

```
{ plan_path, doc, scheduled:[{title,file,severity,category,source}], scheduled_severity_counts, scheduled_category_counts,
  refuted:[{title,file,severity,refute_votes}], non_blocking:[{category,title,file,severity}], plan_fidelity,
  engine_breakdown:{ reviewers, roster, out_of_scope_dropped, findings, gated_in, confirmed, refuted } }
```

**If `plan_path` is null** (nothing reached Medium+, or every scheduled finding was refuted): no fix plan was written, but the review still *found things* - surface them in full. Never let "ship-clean" read as "nothing to improve." Report, in this order:

1. **The headline, as a gate verdict not a quality verdict**: nothing was scheduled - the branch has no Medium-or-above findings to fix. State it plainly, then immediately pivot to what was found.
2. **What reached the gate and why it was dropped**: list each refuted candidate (`title`, `file`, `refute_votes`) with a one-line reason it was judged not a real Medium+ finding. These were *identified* - the user must see them, and you must flag any whose refutation rests on an assumption worth a second look (e.g. "refuted because it's not on a hot path" - is that proven?).
3. **Every low-severity finding, in full** - grouped by area or category, each with its `file:line`. List them; do **not** collapse to a category count (`naming x2`). This is the recall the reviewers paid for; surfacing it *is* the deliverable when no plan was written.
4. **An actionable shortlist**: which low-severity items are genuine improvements worth doing anyway (separated from cosmetic/doc-drift record-keeping). Give the user your prioritized read, not just the raw dump.
5. **Plan-fidelity** reconciliation inline (the one-liner below; name any unshipped planned item).

No file was written, so there is nothing for `/06-implement-plan` to run: point at `/commit`, `/push`, then `/pr`, and offer to make the shortlist improvements if the user wants.

**Otherwise announce:**
- The plan path.
- The scheduled counts by severity: `N scheduled (X Critical, Y High, Z Medium)` from `scheduled_severity_counts`; optionally the category split from `scheduled_category_counts`.
- **Engine breakdown**, formatted as: `<reviewers> reviewers (<roster> roster): F findings; gated-in G, confirmed K, refuted R` (from `engine_breakdown`). The `refuted R` count is the false positives the adversarial verify stage excluded from the plan. When `out_of_scope_dropped` is non-zero, add `, dropped D outside the diff` so a reviewer wandering off the branch is visible rather than silent.
- **Plan fidelity** (if `plan_fidelity` is non-null): the one-liner `P implemented as planned, Q deviations, R additions, S planned-but-not-implemented`. If S > 0, name the unshipped planned items explicitly (a dropped item is the headline, not a footnote).
- **Surface the refuted + low-severity findings inline, don't just count them**: the refuted candidates (title + one-line reason) and at least the *actionable* low-severity items (with `file:line`), then point at the appendix for the full set.
- Next step: *"Spot-check the plan, then `/06-implement-plan <plan-path>` to execute the fixes, then `/commit`, `/push`, and `/pr`."* In PR mode, fixes run on the PR's branch: if it is not checked out, say to check it out first (for a teammate's PR, the plan may be better handed to its author). The PR already exists, so point at `/pr --update` instead of `/pr`.

Do **not** invoke `/06-implement-plan` yourself.

## Companion files

- `references/review-target.md` - step 1: mode detection, base resolution, the PR worktree, and where a PR review's plan lands.
- `scripts/code-review.workflow.js` - the Workflow this skill invokes (step 5): fan-out, merge/dedupe, batched severity gate, tiered adversarial verify, and the fix plan's format (`renderPlan()`). The schedule threshold is `SCHEDULE_AT_OR_ABOVE`; the args contract is at the top of the file.
- `reviewer-contract.md` - severity guide and reviewer domain map (which eight are core). The workflow's reviewer prompts and schema are derived from it.

## Guidelines

- **The gate trims the plan, never the report.** The fix plan schedules the Medium-and-above findings only (a wall of cosmetic nits is a worse fix plan than a focused list), but every identified finding - scheduled, refuted, and low-severity alike - is surfaced to the user with `file:line`, grouped, never reduced to a count. "Nothing scheduled" is the gate verdict, never a quality verdict.
- **The gate's normalized severity is load-bearing, not the reviewer's tier.** The single fix phase is every Medium-and-above finding; order it Critical-first, then High, then Medium. Never let a reviewer's raw tier alone decide inclusion - the gate re-judges each finding's severity and that verdict governs.
- **Reviewers own non-overlapping categories.** The fourteen specialists are non-overlapping; don't duplicate categories or introduce new reviewer types here. A finding that fits none of the categories still gets normalized to the closest one for the plan, not dropped.
- **Review-only.** The skill writes a plan; it never edits the code it reviewed. Scope-boundary nudges never override this (`<refuses>`).
- **Empty diff → stop. Zero Medium+ findings → no plan file, full report** (step 6's null-`plan_path` path).

## Examples

Worked examples (standard run, clean result, scope-boundary nudges, edge cases) are in `references/examples.md` - consult when unsure how a flow should read.
