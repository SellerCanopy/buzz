# 07-code-review reference: Worked examples

Extracted verbatim from 07-code-review/SKILL.md (context-diet split, 2026-07-23). Consult when unsure how a flow or a scope-boundary nudge should read.

## Examples

### Standard post-implementation review

User: *"`/07-code-review`"* (after `/06-implement-plan` finished)

Flow: locate `.claude/tasks/ENG-1599-ads-budget-sync/` → resolve the base (no stamp, no epic: repo default `main`) and diff `origin/main...HEAD` (6 files) → confirm scope via `AskUserQuestion` (core roster) → pull the check commands and the end-to-end check from the source plan and invoke the Workflow (passing `planPath`, `checkCommands`, `featureCheck`, `references`). The workflow fans out the seven core specialists + plan-fidelity in parallel and dedupes = 16 distinct findings. The severity gate normalizes them to 1 Critical, 2 High, 5 Medium, 8 Low → 8 scheduled (Medium+), 8 to the appendix. Adversarial verify runs tiered: the Critical gets 2 skeptics (survives), the 2 High and 5 Medium get 1 skeptic each (2 refuted as false positives) → 6 survive (1 IDOR, 1 unbounded query, 1 double-charge race, a god handler function, a layering violation, a missing test on the sync path). The workflow returns `ENG-1599-07-qa-plan.md` as `doc` and the skill saves it: one fix phase (Critical first, then High, then Medium) + an 8-line low-severity appendix → announce `6 scheduled (1 Critical, 1 High, 4 Medium), 8 low-severity; 7 reviewers (core roster): 16 findings; gated-in 8, confirmed 6, refuted 2` and hand off to `/06-implement-plan`.

### Scope-boundary nudge

User: *"`/07-code-review` and then apply the fixes."*

Response:
```
I can run the review and write the fix plan as ENG-XXXX-07-qa-plan.md, but applying the
fixes is a separate step on purpose - the plan goes through spot-check first, then
/06-implement-plan executes it. Want me to run the review now and hand you the plan?
```
Wait. Do not bundle.

### Nothing to schedule

User: *"`/07-code-review`"* on a branch where nothing reaches Medium+ (the one candidate that did is refuted).

Lead with the gate outcome, then surface the full recall - refuted candidates with reasoning, every low-severity finding (not a count), and a prioritized actionable shortlist. "Nothing scheduled" is the gate verdict, never a quality verdict:
```
Nothing to schedule - no finding reached Medium or above (the one candidate that did was refuted).
The review still found low-severity items worth seeing; none needs a fix phase.

Reached the gate at High, then refuted:
- "unbounded list_sessions drains memory" (store.py:749) - refuted: per-actor set is bounded,
  not on a request path. Re-confirm if list_* is ever called per-invocation.

Low-severity (recorded, not scheduled) - full list:
- naming: `handleReq` reads as a handler but only validates (api/x.go:40)
- docs: README omits the now-required content_node_id (README.md:181)
- style: a two-line guard could fold into the existing early-return (store.py:88)
- ...(every finding, grouped by area, each with file:line)

Worth doing before commit (still not blockers): the naming fix and the README update.
The remainder is cosmetic.

Plan fidelity: 7 implemented as planned, 1 deviation (rate limiter reused the existing middleware
instead of a new helper), 0 additions, 0 planned-but-unshipped.
Next: /commit and /push when ready - or say the word and I'll make the shortlist improvements.
```

### No task dir found

User: *"`/07-code-review`"* with no `.claude/tasks/` task directory.

Briefly explain there's no task dir, then call `AskUserQuestion` (Header: `"Plan location"`) with two options, each described by its real consequence:
- **"Create a new task dir for this branch (Recommended)"** - *`mkdir .claude/tasks/<branch-name>/`; plan lands inside as `<branch-name>-07-qa-plan.md`, where `/06-implement-plan` can find it; future review/fix rounds get a home.*
- **"I'll name a specific path"** - *Ask where, then save there; `/06-implement-plan` runs it via explicit path.*

Wait for the answer. Do not invent a location.
